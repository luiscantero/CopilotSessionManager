//! VS Code local chat sessions ("Local" in VS Code's sessions view).
//!
//! Each workspace keeps its chats under `User/workspaceStorage/<hash>/chatSessions` (chats
//! without a folder under `User/globalStorage/emptyWindowChatSessions`), either as one JSON
//! document (`.json`, older) or as an append-only mutation log (`.jsonl`): an initial snapshot
//! followed by set / push / delete operations on object paths. Titles, the archived ("done")
//! flag and change counts come from the workspace's `state.vscdb`.
//!
//! [`delete`] removes a chat the way VS Code does (its files and its `state.vscdb` entries), but
//! only while no running VS Code window has the chat's workspace open: such a window keeps the
//! chat index in memory and would write the chat back.

use crate::{
    file_stamp, first_line_title, is_session_id, non_empty, ChatMessage, Entry, FileStamp,
    ParsedEvents, Project, SessionSummary,
};
use rayon::prelude::*;
use rusqlite::{Connection, OpenFlags, OptionalExtension, TransactionBehavior};
use serde::de::{
    value::{MapAccessDeserializer, SeqAccessDeserializer},
    IgnoredAny, MapAccess, SeqAccess, Visitor,
};
use serde::{Deserialize, Deserializer};
use serde_json::value::RawValue;
use serde_json::Value;
use std::collections::{HashMap, HashSet};
use std::fmt;
use std::fs::{self, File};
use std::io::{BufRead, BufReader};
use std::marker::PhantomData;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, SystemTime};

/// `state.vscdb` keys that hold per-chat entries.
const INDEX_KEY: &str = "chat.ChatSessionStore.index";
const AGENT_SESSION_CACHES: [&str; 2] = ["agentSessions.model.cache", "agentSessions.state.cache"];
const TODO_KEY: &str = "memento/chat-todo-list";
const TERMINALS_KEY: &str = "chat.terminalSessions";

/// Parsed chat files and workspace metadata, reused while their files are unchanged.
#[derive(Default)]
pub struct Cache {
    chats: HashMap<PathBuf, Arc<ParsedChat>>,
    metadata: HashMap<PathBuf, (FileStamp, Arc<HashMap<String, ChatMeta>>)>,
}

pub struct ParsedChat {
    events: Arc<ParsedEvents>,
    custom_title: Option<String>,
    created_ms: Option<f64>,
    last_message_ms: Option<f64>,
}

#[derive(Default)]
struct ChatMeta {
    title: Option<String>,
    archived: bool,
    additions: Option<u64>,
    deletions: Option<u64>,
}

struct Workspace {
    /// Folder holding the workspace's `state.vscdb`.
    storage_dir: PathBuf,
    /// Folder the chats belong to (first folder of a multi-root workspace).
    folder: Option<String>,
    is_git_root: bool,
    open_command: Option<String>,
    /// A running VS Code window has this workspace (or, for no-folder chats, an empty window) open.
    window_open: bool,
}

struct ChatFile {
    id: String,
    path: PathBuf,
    stamp: FileStamp,
    workspace: Arc<Workspace>,
    /// Workspace storages that also hold a copy of the chat.
    copies: Vec<ChatCopy>,
}

struct ChatCopy {
    path: PathBuf,
    stamp: FileStamp,
    workspace: Arc<Workspace>,
}

/// Indexes all VS Code local chats with at least one message.
pub fn index(cache: &mut Cache, config_dirs: &[PathBuf]) -> Vec<Entry> {
    let files: Vec<ChatFile> = discover(config_dirs).into_values().collect();

    let storage_dirs: Vec<PathBuf> = {
        let mut dirs: Vec<PathBuf> = files
            .iter()
            .map(|file| file.workspace.storage_dir.clone())
            .collect();
        dirs.sort();
        dirs.dedup();
        dirs
    };
    let previous_meta = &cache.metadata;
    let metadata: HashMap<PathBuf, (FileStamp, Arc<HashMap<String, ChatMeta>>)> = storage_dirs
        .into_par_iter()
        .map(|dir| {
            let stamp = file_stamp(&dir.join("state.vscdb"));
            let meta = match previous_meta.get(&dir) {
                Some((cached, meta)) if *cached == stamp => meta.clone(),
                _ => Arc::new(read_metadata(&dir.join("state.vscdb"))),
            };
            (dir, (stamp, meta))
        })
        .collect();

    let previous_chats = &cache.chats;
    let parsed: Vec<(ChatFile, Arc<ParsedChat>)> = files
        .into_par_iter()
        .map(|file| {
            let chat = match previous_chats.get(&file.path) {
                Some(chat) if chat.events.stamp == file.stamp => chat.clone(),
                _ => Arc::new(parse_chat_file(&file.path, file.stamp)),
            };
            (file, chat)
        })
        .collect();

    let empty = ChatMeta::default();
    let entries = parsed
        .iter()
        .filter(|(_, chat)| !chat.events.messages.is_empty())
        .map(|(file, chat)| {
            let meta = metadata
                .get(&file.workspace.storage_dir)
                .and_then(|(_, meta)| meta.get(&file.id))
                .unwrap_or(&empty);
            build_entry(file, chat, meta)
        })
        .collect();

    cache.chats = parsed
        .into_iter()
        .map(|(file, chat)| (file.path, chat))
        .collect();
    cache.metadata = metadata;
    entries
}

fn build_entry(file: &ChatFile, chat: &ParsedChat, meta: &ChatMeta) -> Entry {
    let events = &chat.events;
    let first_user = events
        .messages
        .iter()
        .find(|m| m.role == "user")
        .map(|m| m.content.as_str());
    let title_source = chat
        .custom_title
        .as_deref()
        .or(meta.title.as_deref())
        .or(first_user)
        .unwrap_or(&file.id);
    let title = first_line_title(title_source);
    let created_at = chat
        .created_ms
        .and_then(iso_from_millis)
        .or_else(|| events.first_timestamp.clone());
    let updated_at = [
        chat.last_message_ms.and_then(iso_from_millis),
        events.last_timestamp.clone(),
        created_at.clone(),
    ]
    .into_iter()
    .flatten()
    .max();
    let workspace = &file.workspace;
    Entry {
        title_lower: title.to_lowercase(),
        base_project: Project::default(),
        summary: SessionSummary {
            id: file.id.clone(),
            title,
            source: "vscode",
            cwd: workspace.folder.clone(),
            repository: None,
            branch: None,
            project: Project::default(),
            created_at,
            updated_at,
            message_count: events.messages.len(),
            additions: meta.additions,
            deletions: meta.deletions,
            done: meta.archived,
            in_use: false,
            window_open: file.copies.iter().any(|copy| copy.workspace.window_open),
            protection: None,
            resume_command: workspace.open_command.clone(),
        },
        git_root: workspace
            .is_git_root
            .then(|| workspace.folder.clone())
            .flatten(),
        owner_app: None,
        events: events.clone(),
    }
}

/// Finds every chat file, keyed by lowercase chat ID.
fn discover(config_dirs: &[PathBuf]) -> HashMap<String, ChatFile> {
    let mut chat_dirs: Vec<(PathBuf, Arc<Workspace>)> = Vec::new();
    for config_dir in config_dirs {
        let windows = open_windows(config_dir);
        let user_dir = config_dir.join("User");
        if let Ok(entries) = fs::read_dir(user_dir.join("workspaceStorage")) {
            for entry in entries.flatten() {
                let storage_dir = entry.path();
                let chat_dir = storage_dir.join("chatSessions");
                if chat_dir.is_dir() {
                    let workspace = read_workspace(storage_dir, windows.as_ref());
                    chat_dirs.push((chat_dir, Arc::new(workspace)));
                }
            }
        }
        let global = user_dir.join("globalStorage");
        let empty_window = global.join("emptyWindowChatSessions");
        if empty_window.is_dir() {
            let workspace = Workspace {
                storage_dir: global,
                folder: None,
                is_git_root: false,
                open_command: None,
                window_open: windows.as_ref().is_some_and(|open| open.all || open.empty),
            };
            chat_dirs.push((empty_window, Arc::new(workspace)));
        }
    }

    let mut copies: HashMap<String, Vec<ChatCopy>> = HashMap::new();
    for (dir, workspace) in chat_dirs {
        let Ok(entries) = fs::read_dir(&dir) else {
            continue;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            let is_chat = matches!(
                path.extension().and_then(|ext| ext.to_str()),
                Some("json" | "jsonl")
            );
            let Some(id) = path
                .file_stem()
                .and_then(|stem| stem.to_str())
                .filter(|stem| is_chat && is_session_id(stem))
                .map(str::to_ascii_lowercase)
            else {
                continue;
            };
            let stamp = file_stamp(&path);
            copies.entry(id).or_default().push(ChatCopy {
                path,
                stamp,
                workspace: workspace.clone(),
            });
        }
    }
    copies
        .into_iter()
        .filter_map(|(id, copies)| Some((id.clone(), pick_copy(id, copies)?)))
        .collect()
}

/// VS Code copies chats into other workspace storages, e.g. into untitled workspaces of
/// transient windows that are later deleted. Read the most recently written copy, but group it
/// under the newest copy stored for a real folder.
fn pick_copy(id: String, copies: Vec<ChatCopy>) -> Option<ChatFile> {
    let has_folder = |copy: &&ChatCopy| copy.workspace.folder.is_some();
    let newest = |a: &&ChatCopy, b: &&ChatCopy| {
        (a.stamp.1, has_folder(a), &a.path).cmp(&(b.stamp.1, has_folder(b), &b.path))
    };
    let workspace = copies
        .iter()
        .filter(has_folder)
        .max_by(newest)
        .map(|copy| copy.workspace.clone());
    let content = copies.iter().max_by(newest)?;
    let (path, stamp) = (content.path.clone(), content.stamp);
    let workspace = workspace.unwrap_or_else(|| content.workspace.clone());
    Some(ChatFile {
        id,
        path,
        stamp,
        workspace,
        copies,
    })
}

/// Workspaces open in a running VS Code. VS Code saves its window list to `storage.json` whenever
/// a window loses focus or closes, so it is current by the time this app has the focus.
struct OpenWindows {
    uris: HashSet<String>,
    /// A window without folder or workspace is open (it uses the no-folder chats).
    empty: bool,
    /// The window list could not be read: treat every workspace as open.
    all: bool,
}

fn open_windows(config_dir: &Path) -> Option<OpenWindows> {
    if !vscode_running(config_dir) {
        return None;
    }
    let state = fs::read_to_string(config_dir.join("User/globalStorage/storage.json"))
        .ok()
        .and_then(|text| serde_json::from_str::<Value>(&text).ok())
        .map(|value| value["windowsState"].clone())
        .filter(Value::is_object);
    let mut open = OpenWindows {
        uris: HashSet::new(),
        empty: false,
        all: state.is_none(),
    };
    let state = state.unwrap_or(Value::Null);
    let windows = state["openedWindows"]
        .as_array()
        .into_iter()
        .flatten()
        .chain([&state["lastActiveWindow"]])
        .filter(|window| window.is_object());
    for window in windows {
        let uri = window["folder"]
            .as_str()
            .or_else(|| window["workspaceIdentifier"]["configURIPath"].as_str());
        match uri {
            Some(uri) => {
                open.uris.insert(normalize_uri(uri));
            }
            None => open.empty = true,
        }
    }
    Some(open)
}

fn normalize_uri(uri: &str) -> String {
    percent_decode(uri).trim_end_matches('/').to_lowercase()
}

/// VS Code writes its main process ID to `code.lock` while it runs and removes the file on exit.
fn vscode_running(config_dir: &Path) -> bool {
    let lock = config_dir.join("code.lock");
    let Some(pid) = fs::read_to_string(&lock)
        .ok()
        .and_then(|text| text.trim().parse::<u32>().ok())
    else {
        return false;
    };
    let written = fs::metadata(&lock).and_then(|meta| meta.modified()).ok();
    process_running_since(pid, written)
}

/// Whether process `pid` runs and was started no later than `lock_written` (a newer process
/// reused the PID of a VS Code that crashed without removing its lock file).
#[cfg(windows)]
fn process_running_since(pid: u32, lock_written: Option<SystemTime>) -> bool {
    use windows_sys::Win32::Foundation::{
        CloseHandle, GetLastError, ERROR_ACCESS_DENIED, FILETIME,
    };
    use windows_sys::Win32::System::Threading::{
        GetExitCodeProcess, GetProcessTimes, OpenProcess, PROCESS_QUERY_LIMITED_INFORMATION,
    };
    const STILL_ACTIVE: u32 = 259;
    let zero = || FILETIME {
        dwLowDateTime: 0,
        dwHighDateTime: 0,
    };
    let (mut created, mut exited, mut kernel, mut user) = (zero(), zero(), zero(), zero());
    let mut exit_code = 0u32;
    // SAFETY: plain Win32 queries on a process handle that is opened and closed here.
    let (alive, has_times) = unsafe {
        let handle = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, 0, pid);
        if handle.is_null() {
            // An elevated VS Code cannot be inspected; assume it is the one running.
            return GetLastError() == ERROR_ACCESS_DENIED;
        }
        let alive = GetExitCodeProcess(handle, &mut exit_code) != 0 && exit_code == STILL_ACTIVE;
        let has_times =
            GetProcessTimes(handle, &mut created, &mut exited, &mut kernel, &mut user) != 0;
        CloseHandle(handle);
        (alive, has_times)
    };
    if !alive {
        return false;
    }
    // FILETIME counts 100 ns intervals since 1601-01-01.
    const UNIX_EPOCH_TICKS: u64 = 116_444_736_000_000_000;
    let ticks = (u64::from(created.dwHighDateTime) << 32) | u64::from(created.dwLowDateTime);
    let started = ticks
        .checked_sub(UNIX_EPOCH_TICKS)
        .map(|ticks| SystemTime::UNIX_EPOCH + Duration::from_nanos(ticks.saturating_mul(100)));
    match (has_times.then_some(started).flatten(), lock_written) {
        (Some(started), Some(written)) => started <= written + Duration::from_secs(60),
        _ => true,
    }
}

#[cfg(not(windows))]
fn process_running_since(pid: u32, _lock_written: Option<SystemTime>) -> bool {
    std::process::Command::new("kill")
        .args(["-0", &pid.to_string()])
        .stderr(std::process::Stdio::null())
        .status()
        .is_ok_and(|status| status.success())
}

/// Deletes a chat the way VS Code does: every copy of its file, its editing-session folder and
/// its entries in the `state.vscdb` of each workspace holding a copy.
pub fn delete(config_dirs: &[PathBuf], id: &str) -> Result<(), String> {
    let id = id.to_ascii_lowercase();
    if !is_session_id(&id) {
        return Err("Invalid session id".to_string());
    }
    let chat = discover(config_dirs)
        .remove(&id)
        .ok_or_else(|| format!("Chat {id} was not found"))?;
    if let Some(copy) = chat.copies.iter().find(|copy| copy.workspace.window_open) {
        let place = copy
            .workspace
            .folder
            .as_deref()
            .unwrap_or("a window without folder");
        return Err(format!(
            "This chat is open in VS Code ({place}). Close that VS Code window and try again."
        ));
    }
    let mut storage_dirs: Vec<&Path> = chat
        .copies
        .iter()
        .map(|copy| copy.workspace.storage_dir.as_path())
        .collect();
    storage_dirs.sort();
    storage_dirs.dedup();
    // Entries go first: VS Code ignores a chat file without an index entry, but lists an entry
    // without a file as a chat that cannot be opened.
    for dir in &storage_dirs {
        remove_chat_state(&dir.join("state.vscdb"), &id)?;
    }
    for copy in &chat.copies {
        match fs::remove_file(&copy.path) {
            Err(err) if err.kind() != std::io::ErrorKind::NotFound => {
                return Err(format!("Unable to delete {}: {err}", copy.path.display()));
            }
            _ => {}
        }
    }
    for dir in &storage_dirs {
        let editing = dir.join("chatEditingSessions").join(&id);
        if editing.is_dir() {
            fs::remove_dir_all(&editing)
                .map_err(|err| format!("Unable to delete {}: {err}", editing.display()))?;
        }
    }
    Ok(())
}

fn remove_chat_state(db_path: &Path, id: &str) -> Result<(), String> {
    if !db_path.is_file() {
        return Ok(());
    }
    let fail = |err: rusqlite::Error| format!("Unable to update {}: {err}", db_path.display());
    let flags = OpenFlags::SQLITE_OPEN_READ_WRITE | OpenFlags::SQLITE_OPEN_NO_MUTEX;
    let mut conn = Connection::open_with_flags(db_path, flags).map_err(fail)?;
    conn.busy_timeout(Duration::from_secs(5)).map_err(fail)?;
    let tx = conn
        .transaction_with_behavior(TransactionBehavior::Immediate)
        .map_err(fail)?;
    for key in [INDEX_KEY, TODO_KEY, TERMINALS_KEY]
        .into_iter()
        .chain(AGENT_SESSION_CACHES)
    {
        let value: Option<String> = tx
            .query_row("SELECT value FROM ItemTable WHERE key = ?1", [key], |row| {
                row.get(0)
            })
            .optional()
            .map_err(fail)?;
        let Some(mut value) = value.and_then(|text| serde_json::from_str::<Value>(&text).ok())
        else {
            continue;
        };
        if remove_chat_from(key, &mut value, id) {
            tx.execute(
                "UPDATE ItemTable SET value = ?1 WHERE key = ?2",
                (value.to_string(), key),
            )
            .map_err(fail)?;
        }
    }
    tx.commit().map_err(fail)
}

/// Removes chat `id` (lowercase) from the `state.vscdb` value stored under `key`; returns
/// whether anything changed.
fn remove_chat_from(key: &str, value: &mut Value, id: &str) -> bool {
    let is_id = |text: &str| text.eq_ignore_ascii_case(id);
    let retain_map = |map: Option<&mut serde_json::Map<String, Value>>,
                      keep: &dyn Fn(&String, &Value) -> bool| {
        map.is_some_and(|map| {
            let before = map.len();
            map.retain(|key, value| keep(key, value));
            map.len() != before
        })
    };
    match key {
        INDEX_KEY => retain_map(
            value.get_mut("entries").and_then(Value::as_object_mut),
            &|key, _| !is_id(key),
        ),
        TODO_KEY => retain_map(value.as_object_mut(), &|key, _| !is_id(key)),
        TERMINALS_KEY => retain_map(value.as_object_mut(), &|_, terminal| {
            !terminal["sessionId"].as_str().is_some_and(is_id)
        }),
        _ => value.as_array_mut().is_some_and(|items| {
            let before = items.len();
            items.retain(|item| {
                item["resource"]
                    .as_str()
                    .and_then(local_session_id)
                    .as_deref()
                    != Some(id)
            });
            items.len() != before
        }),
    }
}

fn read_workspace(storage_dir: PathBuf, windows: Option<&OpenWindows>) -> Workspace {
    let value = fs::read_to_string(storage_dir.join("workspace.json"))
        .ok()
        .and_then(|text| serde_json::from_str::<Value>(&text).ok())
        .unwrap_or(Value::Null);
    let uri = value["folder"]
        .as_str()
        .or_else(|| value["workspace"].as_str());
    let window_open = windows.is_some_and(|open| {
        open.all || uri.is_some_and(|uri| open.uris.contains(&normalize_uri(uri)))
    });
    let (folder, open_command) = if let Some(uri) = value["folder"].as_str() {
        match file_uri_to_path(uri) {
            Some(path) => (Some(path.clone()), Some(format!("code \"{path}\""))),
            None => (
                remote_uri_path(uri),
                Some(format!("code --folder-uri \"{uri}\"")),
            ),
        }
    } else if let Some(uri) = value["workspace"].as_str() {
        match file_uri_to_path(uri) {
            Some(path) => (
                first_workspace_folder(Path::new(&path)),
                Some(format!("code \"{path}\"")),
            ),
            None => (None, Some(format!("code --file-uri \"{uri}\""))),
        }
    } else {
        (None, None)
    };
    let is_git_root = folder
        .as_deref()
        .is_some_and(|folder| Path::new(folder).join(".git").exists());
    Workspace {
        storage_dir,
        folder,
        is_git_root,
        open_command,
        window_open,
    }
}

/// First folder of a `.code-workspace` file (plain JSON only; JSONC comments are not parsed).
fn first_workspace_folder(file: &Path) -> Option<String> {
    let text = fs::read_to_string(file).ok()?;
    let value = serde_json::from_str::<Value>(&text).ok()?;
    let folder = value["folders"].as_array()?.first()?;
    if let Some(path) = folder["path"].as_str() {
        let path = Path::new(path);
        let path = if path.is_absolute() {
            path.to_path_buf()
        } else {
            file.parent()?.join(path)
        };
        return path.to_str().map(str::to_owned);
    }
    folder["uri"]
        .as_str()
        .and_then(|uri| file_uri_to_path(uri).or_else(|| remote_uri_path(uri)))
}

fn percent_decode(text: &str) -> String {
    let bytes = text.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        let hex = |b: u8| (b as char).to_digit(16);
        if bytes[i] == b'%' && i + 2 < bytes.len() {
            if let (Some(hi), Some(lo)) = (hex(bytes[i + 1]), hex(bytes[i + 2])) {
                out.push((hi * 16 + lo) as u8);
                i += 3;
                continue;
            }
        }
        out.push(bytes[i]);
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

/// Converts a `file://` URI to a local path (`file:///c%3A/x` -> `c:\x` on Windows).
fn file_uri_to_path(uri: &str) -> Option<String> {
    let rest = percent_decode(uri.strip_prefix("file://")?);
    let separator = if cfg!(windows) { "\\" } else { "/" };
    let path = match rest.strip_prefix('/') {
        Some(local) if local.as_bytes().get(1) == Some(&b':') => local.to_string(),
        Some(local) => format!("/{local}"),
        // `file://server/share/...` is a UNC path.
        None => format!("//{rest}"),
    };
    Some(path.replace('/', separator))
}

/// Path of a remote URI such as `vscode-remote://wsl%2Bubuntu/home/me/repo`.
fn remote_uri_path(uri: &str) -> Option<String> {
    let (_, rest) = uri.split_once("://")?;
    let path = &rest[rest.find('/')?..];
    non_empty(Some(&percent_decode(path)))
}

/// Reads titles, archived flags and change counts from a VS Code `state.vscdb`.
fn read_metadata(db_path: &Path) -> HashMap<String, ChatMeta> {
    let mut metas: HashMap<String, ChatMeta> = HashMap::new();
    if !db_path.is_file() {
        return metas;
    }
    let flags = OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NO_MUTEX;
    let Ok(conn) = Connection::open_with_flags(db_path, flags) else {
        return metas;
    };
    let _ = conn.busy_timeout(Duration::from_millis(200));
    let read = |key: &str| -> Value {
        conn.query_row("SELECT value FROM ItemTable WHERE key = ?1", [key], |row| {
            row.get::<_, String>(0)
        })
        .ok()
        .and_then(|text| serde_json::from_str(&text).ok())
        .unwrap_or(Value::Null)
    };

    if let Some(entries) = read(INDEX_KEY)["entries"].as_object() {
        for (id, entry) in entries {
            if is_session_id(id) {
                metas.entry(id.to_ascii_lowercase()).or_default().title =
                    non_empty(entry["title"].as_str());
            }
        }
    }
    // Both caches are lists keyed by `vscode-chat-session://local/<base64 session id>`.
    for key in AGENT_SESSION_CACHES {
        for item in read(key).as_array().into_iter().flatten() {
            let Some(id) = item["resource"].as_str().and_then(local_session_id) else {
                continue;
            };
            let meta = metas.entry(id).or_default();
            if meta.title.is_none() {
                meta.title = non_empty(item["label"].as_str());
            }
            meta.archived |= item["archived"].as_bool().unwrap_or(false);
            let changes = &item["changes"];
            if let Some(added) = changes["insertions"].as_u64() {
                meta.additions = Some(added);
            }
            if let Some(deleted) = changes["deletions"].as_u64() {
                meta.deletions = Some(deleted);
            }
        }
    }
    metas
}

fn local_session_id(resource: &str) -> Option<String> {
    let encoded = resource.strip_prefix("vscode-chat-session://local/")?;
    let decoded = String::from_utf8(base64_decode(encoded)?).ok()?;
    is_session_id(&decoded).then(|| decoded.to_ascii_lowercase())
}

fn base64_decode(text: &str) -> Option<Vec<u8>> {
    let value = |c: u8| -> Option<u32> {
        Some(match c {
            b'A'..=b'Z' => c - b'A',
            b'a'..=b'z' => c - b'a' + 26,
            b'0'..=b'9' => c - b'0' + 52,
            b'+' | b'-' => 62,
            b'/' | b'_' => 63,
            _ => return None,
        } as u32)
    };
    let mut out = Vec::with_capacity(text.len() * 3 / 4);
    let mut buffer = 0u32;
    let mut bits = 0;
    for c in text.trim_end_matches('=').bytes() {
        buffer = (buffer << 6) | value(c)?;
        bits += 6;
        if bits >= 8 {
            bits -= 8;
            out.push((buffer >> bits) as u8);
            buffer &= (1 << bits) - 1;
        }
    }
    Some(out)
}

/// Formats Unix epoch milliseconds like the Copilot CLI (`2026-01-31T12:34:56.789Z`).
fn iso_from_millis(ms: f64) -> Option<String> {
    if !ms.is_finite() || ms <= 0.0 {
        return None;
    }
    let ms = ms as i64;
    let (days, ms_of_day) = (ms.div_euclid(86_400_000), ms.rem_euclid(86_400_000));
    // Civil-from-days (Howard Hinnant).
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = doy - (153 * mp + 2) / 5 + 1;
    let month = if mp < 10 { mp + 3 } else { mp - 9 };
    let year = yoe + era * 400 + i64::from(month <= 2);
    Some(format!(
        "{year:04}-{month:02}-{day:02}T{:02}:{:02}:{:02}.{:03}Z",
        ms_of_day / 3_600_000,
        ms_of_day / 60_000 % 60,
        ms_of_day / 1000 % 60,
        ms_of_day % 1000
    ))
}

// ---------------------------------------------------------------------------------------------
// Chat file parsing. Chat files can be hundreds of MB (tool results, rendered prompts), so only
// the needed fields are deserialized; everything else is skipped without allocating. Field
// types are lenient so an unexpected shape drops a value instead of the whole file.

/// A string, or `None` for any other JSON value.
#[derive(Default)]
struct Text(Option<String>);

impl<'de> Deserialize<'de> for Text {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        struct TextVisitor;
        impl<'de> Visitor<'de> for TextVisitor {
            type Value = Text;
            fn expecting(&self, f: &mut fmt::Formatter) -> fmt::Result {
                f.write_str("any value")
            }
            fn visit_str<E>(self, v: &str) -> Result<Text, E> {
                Ok(Text(Some(v.to_owned())))
            }
            fn visit_string<E>(self, v: String) -> Result<Text, E> {
                Ok(Text(Some(v)))
            }
            fn visit_bool<E>(self, _: bool) -> Result<Text, E> {
                Ok(Text(None))
            }
            fn visit_i64<E>(self, _: i64) -> Result<Text, E> {
                Ok(Text(None))
            }
            fn visit_u64<E>(self, _: u64) -> Result<Text, E> {
                Ok(Text(None))
            }
            fn visit_f64<E>(self, _: f64) -> Result<Text, E> {
                Ok(Text(None))
            }
            fn visit_unit<E>(self) -> Result<Text, E> {
                Ok(Text(None))
            }
            fn visit_seq<A: SeqAccess<'de>>(self, mut seq: A) -> Result<Text, A::Error> {
                while seq.next_element::<IgnoredAny>()?.is_some() {}
                Ok(Text(None))
            }
            fn visit_map<A: MapAccess<'de>>(self, mut map: A) -> Result<Text, A::Error> {
                while map.next_entry::<IgnoredAny, IgnoredAny>()?.is_some() {}
                Ok(Text(None))
            }
        }
        deserializer.deserialize_any(TextVisitor)
    }
}

/// A number, or `None` for any other JSON value.
#[derive(Default, Clone, Copy)]
struct Num(Option<f64>);

impl<'de> Deserialize<'de> for Num {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        struct NumVisitor;
        impl<'de> Visitor<'de> for NumVisitor {
            type Value = Num;
            fn expecting(&self, f: &mut fmt::Formatter) -> fmt::Result {
                f.write_str("any value")
            }
            fn visit_i64<E>(self, v: i64) -> Result<Num, E> {
                Ok(Num(Some(v as f64)))
            }
            fn visit_u64<E>(self, v: u64) -> Result<Num, E> {
                Ok(Num(Some(v as f64)))
            }
            fn visit_f64<E>(self, v: f64) -> Result<Num, E> {
                Ok(Num(Some(v)))
            }
            fn visit_bool<E>(self, _: bool) -> Result<Num, E> {
                Ok(Num(None))
            }
            fn visit_str<E>(self, _: &str) -> Result<Num, E> {
                Ok(Num(None))
            }
            fn visit_unit<E>(self) -> Result<Num, E> {
                Ok(Num(None))
            }
            fn visit_seq<A: SeqAccess<'de>>(self, mut seq: A) -> Result<Num, A::Error> {
                while seq.next_element::<IgnoredAny>()?.is_some() {}
                Ok(Num(None))
            }
            fn visit_map<A: MapAccess<'de>>(self, mut map: A) -> Result<Num, A::Error> {
                while map.next_entry::<IgnoredAny, IgnoredAny>()?.is_some() {}
                Ok(Num(None))
            }
        }
        deserializer.deserialize_any(NumVisitor)
    }
}

/// An object or array deserialized as `T`, or `None` for scalars and `null`.
struct Lenient<T>(Option<T>);

impl<T> Default for Lenient<T> {
    fn default() -> Self {
        Self(None)
    }
}

impl<'de, T: Deserialize<'de>> Deserialize<'de> for Lenient<T> {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        struct LenientVisitor<T>(PhantomData<T>);
        impl<'de, T: Deserialize<'de>> Visitor<'de> for LenientVisitor<T> {
            type Value = Lenient<T>;
            fn expecting(&self, f: &mut fmt::Formatter) -> fmt::Result {
                f.write_str("any value")
            }
            fn visit_map<A: MapAccess<'de>>(self, map: A) -> Result<Self::Value, A::Error> {
                T::deserialize(MapAccessDeserializer::new(map)).map(|v| Lenient(Some(v)))
            }
            fn visit_seq<A: SeqAccess<'de>>(self, seq: A) -> Result<Self::Value, A::Error> {
                T::deserialize(SeqAccessDeserializer::new(seq)).map(|v| Lenient(Some(v)))
            }
            fn visit_bool<E>(self, _: bool) -> Result<Self::Value, E> {
                Ok(Lenient(None))
            }
            fn visit_i64<E>(self, _: i64) -> Result<Self::Value, E> {
                Ok(Lenient(None))
            }
            fn visit_u64<E>(self, _: u64) -> Result<Self::Value, E> {
                Ok(Lenient(None))
            }
            fn visit_f64<E>(self, _: f64) -> Result<Self::Value, E> {
                Ok(Lenient(None))
            }
            fn visit_str<E>(self, _: &str) -> Result<Self::Value, E> {
                Ok(Lenient(None))
            }
            fn visit_unit<E>(self) -> Result<Self::Value, E> {
                Ok(Lenient(None))
            }
        }
        deserializer.deserialize_any(LenientVisitor(PhantomData))
    }
}

type Parts = Lenient<Vec<Lenient<ResponsePart>>>;

#[derive(Deserialize, Default)]
#[serde(default, rename_all = "camelCase")]
struct ChatData {
    requests: Lenient<Vec<Lenient<ChatRequest>>>,
    custom_title: Text,
    creation_date: Num,
    last_message_date: Num,
}

#[derive(Deserialize, Default)]
#[serde(default, rename_all = "camelCase")]
struct ChatRequest {
    message: Lenient<RequestMessage>,
    timestamp: Num,
    model_state: Lenient<ModelState>,
    response: Parts,
}

#[derive(Deserialize, Default)]
#[serde(default)]
struct RequestMessage {
    text: Text,
}

#[derive(Deserialize, Default)]
#[serde(default, rename_all = "camelCase")]
struct ModelState {
    completed_at: Num,
}

#[derive(Deserialize, Default)]
#[serde(default, rename_all = "camelCase")]
struct ResponsePart {
    kind: Text,
    value: Text,
    content: Lenient<MarkdownContent>,
    name: Text,
    inline_reference: Lenient<InlineReference>,
}

#[derive(Deserialize, Default)]
#[serde(default)]
struct MarkdownContent {
    value: Text,
}

#[derive(Deserialize, Default)]
#[serde(default)]
struct InlineReference {
    name: Text,
    uri: Lenient<UriParts>,
    location: Lenient<Location>,
}

#[derive(Deserialize, Default)]
#[serde(default)]
struct Location {
    uri: Lenient<UriParts>,
}

#[derive(Deserialize, Default)]
#[serde(default, rename_all = "camelCase")]
struct UriParts {
    path: Text,
    fs_path: Text,
}

#[derive(Default)]
struct RequestState {
    text: Option<String>,
    timestamp: Option<f64>,
    completed_at: Option<f64>,
    parts: Vec<Part>,
}

enum Part {
    Markdown(String),
    Reference(String),
    /// Tool calls, thinking, edits, ...: separates the surrounding text.
    Other,
}

#[derive(Default)]
struct ChatState {
    custom_title: Option<String>,
    created_ms: Option<f64>,
    last_message_ms: Option<f64>,
    requests: Vec<RequestState>,
}

impl From<ChatData> for ChatState {
    fn from(data: ChatData) -> Self {
        Self {
            custom_title: data.custom_title.0,
            created_ms: data.creation_date.0,
            last_message_ms: data.last_message_date.0,
            requests: convert_requests(data.requests),
        }
    }
}

fn convert_requests(requests: Lenient<Vec<Lenient<ChatRequest>>>) -> Vec<RequestState> {
    requests
        .0
        .unwrap_or_default()
        .into_iter()
        .map(|request| convert_request(request.0.unwrap_or_default()))
        .collect()
}

fn convert_request(request: ChatRequest) -> RequestState {
    RequestState {
        text: request.message.0.and_then(|message| message.text.0),
        timestamp: request.timestamp.0,
        completed_at: request.model_state.0.and_then(|state| state.completed_at.0),
        parts: convert_parts(request.response),
    }
}

fn convert_parts(parts: Parts) -> Vec<Part> {
    parts
        .0
        .unwrap_or_default()
        .into_iter()
        .map(|part| convert_part(part.0.unwrap_or_default()))
        .collect()
}

fn convert_part(part: ResponsePart) -> Part {
    match part.kind.0.as_deref() {
        None => part.value.0.map_or(Part::Other, Part::Markdown),
        Some("markdownContent") => part
            .content
            .0
            .and_then(|content| content.value.0)
            .map_or(Part::Other, Part::Markdown),
        Some("inlineReference") => {
            let reference = part.inline_reference.0.unwrap_or_default();
            let uri = reference
                .uri
                .0
                .or_else(|| reference.location.0.and_then(|location| location.uri.0));
            let file_name = uri
                .and_then(|uri| uri.path.0.or(uri.fs_path.0))
                .map(|path| crate::last_segment(&path));
            part.name
                .0
                .or(reference.name.0)
                .or(file_name)
                .filter(|name| !name.trim().is_empty())
                .map_or(Part::Other, Part::Reference)
        }
        Some(_) => Part::Other,
    }
}

#[derive(Deserialize)]
struct LogLine<'a> {
    kind: u8,
    #[serde(default)]
    k: Vec<Value>,
    #[serde(borrow)]
    v: Option<&'a RawValue>,
    i: Option<usize>,
}

enum Key<'a> {
    Name(&'a str),
    Index(usize),
}

fn keys(path: &[Value]) -> Vec<Key<'_>> {
    path.iter()
        .map(|key| match key {
            Value::String(name) => Key::Name(name),
            other => Key::Index(other.as_u64().map_or(usize::MAX, |i| i as usize)),
        })
        .collect()
}

fn decode<'a, T: Deserialize<'a> + Default>(raw: Option<&'a RawValue>) -> T {
    raw.and_then(|raw| serde_json::from_str(raw.get()).ok())
        .unwrap_or_default()
}

/// Replaces or appends an element, matching how VS Code sets `array[index]`.
fn set_at<T>(items: &mut Vec<T>, index: usize, value: T) {
    if index < items.len() {
        items[index] = value;
    } else if index == items.len() {
        items.push(value);
    }
}

fn splice<T>(items: &mut Vec<T>, start: Option<usize>, values: impl IntoIterator<Item = T>) {
    if let Some(start) = start {
        items.truncate(start);
    }
    items.extend(values);
}

/// Applies one mutation-log entry; only paths that affect the shown conversation matter.
fn apply(state: &mut ChatState, line: &LogLine) {
    use Key::{Index, Name};
    let path = keys(&line.k);
    match (line.kind, path.as_slice()) {
        (0, []) => *state = decode::<ChatData>(line.v).into(),
        (1, [Name("customTitle")]) => state.custom_title = decode::<Text>(line.v).0,
        (3, [Name("customTitle")]) => state.custom_title = None,
        (1, [Name("creationDate")]) => state.created_ms = decode::<Num>(line.v).0,
        (1, [Name("lastMessageDate")]) => state.last_message_ms = decode::<Num>(line.v).0,
        (1, [Name("requests")]) => state.requests = convert_requests(decode(line.v)),
        (2, [Name("requests")]) => {
            let requests = convert_requests(Lenient(Some(decode(line.v))));
            splice(&mut state.requests, line.i, requests);
        }
        (1, [Name("requests"), Index(index)]) => {
            set_at(&mut state.requests, *index, convert_request(decode(line.v)));
        }
        (kind, [Name("requests"), Index(index), rest @ ..]) => {
            let Some(request) = state.requests.get_mut(*index) else {
                return;
            };
            match (kind, rest) {
                (1, [Name("message")]) => {
                    request.text = decode::<RequestMessage>(line.v).text.0;
                }
                (1, [Name("timestamp")]) => request.timestamp = decode::<Num>(line.v).0,
                (1, [Name("modelState")]) => {
                    request.completed_at = decode::<ModelState>(line.v).completed_at.0;
                }
                (1, [Name("response")]) => request.parts = convert_parts(decode(line.v)),
                (2, [Name("response")]) => {
                    let parts = convert_parts(Lenient(Some(decode(line.v))));
                    splice(&mut request.parts, line.i, parts);
                }
                (1, [Name("response"), Index(part)]) => {
                    set_at(&mut request.parts, *part, convert_part(decode(line.v)));
                }
                _ => {}
            }
        }
        _ => {}
    }
}

fn read_chat_state(path: &Path) -> Option<ChatState> {
    if path.extension().and_then(|ext| ext.to_str()) == Some("json") {
        let text = fs::read(path).ok()?;
        return serde_json::from_slice::<ChatData>(&text)
            .ok()
            .map(ChatState::from);
    }
    let file = File::open(path).ok()?;
    let mut reader = BufReader::with_capacity(1 << 20, file);
    let mut state = ChatState::default();
    let mut line = Vec::new();
    loop {
        line.clear();
        match reader.read_until(b'\n', &mut line) {
            Ok(0) | Err(_) => break,
            Ok(_) => {}
        }
        // A trailing partial line (still being written) fails to parse and is skipped.
        if let Ok(entry) = serde_json::from_slice::<LogLine>(&line) {
            apply(&mut state, &entry);
        }
    }
    Some(state)
}

fn assistant_text(parts: &[Part]) -> String {
    let mut text = String::new();
    let mut separated = false;
    for part in parts {
        match part {
            Part::Markdown(markdown) => {
                if separated && !text.trim().is_empty() {
                    let trimmed = text.trim_end().len();
                    text.truncate(trimmed);
                    text.push_str("\n\n");
                }
                text.push_str(markdown);
                separated = false;
            }
            Part::Reference(name) => {
                text.push('`');
                text.push_str(name);
                text.push('`');
                separated = false;
            }
            Part::Other => separated = true,
        }
    }
    text.trim().to_string()
}

fn parse_chat_file(path: &Path, stamp: FileStamp) -> ParsedChat {
    let state = read_chat_state(path).unwrap_or_default();
    let mut messages = Vec::new();
    for request in &state.requests {
        let timestamp = request.timestamp.and_then(iso_from_millis);
        if let Some(text) = non_empty(request.text.as_deref()) {
            messages.push(ChatMessage {
                role: "user",
                content: text,
                timestamp: timestamp.clone(),
            });
        }
        let reply = assistant_text(&request.parts);
        if !reply.is_empty() {
            messages.push(ChatMessage {
                role: "assistant",
                content: reply,
                timestamp: request.completed_at.and_then(iso_from_millis).or(timestamp),
            });
        }
    }
    ParsedChat {
        events: Arc::new(ParsedEvents::new(stamp, messages)),
        custom_title: non_empty(state.custom_title.as_deref()),
        created_ms: state.created_ms,
        last_message_ms: state.last_message_ms,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_path(name: &str) -> PathBuf {
        std::env::temp_dir().join(format!(
            "copilot-session-search-{}-{name}",
            std::process::id()
        ))
    }

    const CHAT: &str = "24c442d3-ada5-4d9a-b945-c448f7d78023";
    const OTHER: &str = "0be69b86-e4aa-4c13-9b3d-85a782ab7fe8";

    fn chat_resource(id: &str) -> String {
        let encoded: String = {
            const ALPHABET: &[u8] =
                b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
            id.as_bytes()
                .chunks(3)
                .flat_map(|chunk| {
                    let bytes = [
                        chunk[0],
                        *chunk.get(1).unwrap_or(&0),
                        *chunk.get(2).unwrap_or(&0),
                    ];
                    let n = (u32::from(bytes[0]) << 16)
                        | (u32::from(bytes[1]) << 8)
                        | u32::from(bytes[2]);
                    (0..=chunk.len())
                        .map(move |i| ALPHABET[(n >> (18 - 6 * i) & 63) as usize] as char)
                })
                .collect()
        };
        format!("vscode-chat-session://local/{encoded}")
    }

    /// Creates a workspace storage with a chat file, editing session and `state.vscdb` entries.
    fn write_workspace(config_dir: &Path, hash: &str, folder_uri: &str, ids: &[&str]) -> PathBuf {
        let storage = config_dir.join("User/workspaceStorage").join(hash);
        fs::create_dir_all(storage.join("chatSessions")).unwrap();
        fs::write(
            storage.join("workspace.json"),
            serde_json::json!({ "folder": folder_uri }).to_string(),
        )
        .unwrap();
        for id in ids {
            fs::write(
                storage.join("chatSessions").join(format!("{id}.jsonl")),
                "{}",
            )
            .unwrap();
            fs::create_dir_all(storage.join("chatEditingSessions").join(id)).unwrap();
        }
        let entries: serde_json::Map<String, Value> = ids
            .iter()
            .map(|id| (id.to_string(), serde_json::json!({ "sessionId": id })))
            .collect();
        let resources: Vec<Value> = ids
            .iter()
            .map(|id| serde_json::json!({ "resource": chat_resource(id), "archived": false }))
            .collect();
        let todos: serde_json::Map<String, Value> = ids
            .iter()
            .map(|id| (id.to_string(), serde_json::json!([])))
            .collect();
        let terminals: serde_json::Map<String, Value> = ids
            .iter()
            .enumerate()
            .map(|(pid, id)| (pid.to_string(), serde_json::json!({ "sessionId": id })))
            .collect();
        let conn = Connection::open(storage.join("state.vscdb")).unwrap();
        conn.execute_batch(
            "CREATE TABLE ItemTable (key TEXT UNIQUE ON CONFLICT REPLACE, value BLOB)",
        )
        .unwrap();
        for (key, value) in [
            (
                INDEX_KEY,
                serde_json::json!({ "version": 1, "entries": entries }),
            ),
            (AGENT_SESSION_CACHES[0], Value::Array(resources.clone())),
            (AGENT_SESSION_CACHES[1], Value::Array(resources)),
            (TODO_KEY, Value::Object(todos)),
            (TERMINALS_KEY, Value::Object(terminals)),
            ("unrelated", serde_json::json!({ "keep": CHAT })),
        ] {
            conn.execute(
                "INSERT INTO ItemTable (key, value) VALUES (?1, ?2)",
                (key, value.to_string()),
            )
            .unwrap();
        }
        storage
    }

    fn stored_ids(storage: &Path) -> Vec<String> {
        read_metadata(&storage.join("state.vscdb"))
            .into_keys()
            .collect()
    }

    #[test]
    fn deletes_every_copy_and_its_state() {
        let config_dir = temp_path("delete-chat");
        let _ = fs::remove_dir_all(&config_dir);
        let main = write_workspace(&config_dir, "a", "file:///c%3A/Repos/app", &[CHAT, OTHER]);
        let copy = write_workspace(&config_dir, "b", "file:///c%3A/Repos/untitled", &[CHAT]);
        assert_eq!(
            chat_resource(CHAT).len(),
            "vscode-chat-session://local/".len() + 48
        );
        assert_eq!(
            local_session_id(&chat_resource(CHAT)).as_deref(),
            Some(CHAT)
        );

        delete(std::slice::from_ref(&config_dir), &CHAT.to_uppercase()).unwrap();

        for storage in [&main, &copy] {
            assert!(!storage.join(format!("chatSessions/{CHAT}.jsonl")).exists());
            assert!(!storage.join("chatEditingSessions").join(CHAT).exists());
        }
        assert!(main.join(format!("chatSessions/{OTHER}.jsonl")).exists());
        assert!(main.join("chatEditingSessions").join(OTHER).exists());
        assert_eq!(stored_ids(&main), vec![OTHER.to_string()]);
        assert!(stored_ids(&copy).is_empty());
        let conn = Connection::open(main.join("state.vscdb")).unwrap();
        let read = |key: &str| -> String {
            conn.query_row("SELECT value FROM ItemTable WHERE key = ?1", [key], |row| {
                row.get(0)
            })
            .unwrap()
        };
        assert!(!read(TODO_KEY).contains(CHAT) && read(TODO_KEY).contains(OTHER));
        assert!(!read(TERMINALS_KEY).contains(CHAT) && read(TERMINALS_KEY).contains(OTHER));
        assert!(read("unrelated").contains(CHAT));
        drop(conn);

        assert!(delete(std::slice::from_ref(&config_dir), CHAT)
            .unwrap_err()
            .contains("not found"));
        fs::remove_dir_all(&config_dir).unwrap();
    }

    #[test]
    fn refuses_chats_of_workspaces_open_in_vscode() {
        let config_dir = temp_path("delete-open-chat");
        let _ = fs::remove_dir_all(&config_dir);
        let storage = write_workspace(&config_dir, "a", "file:///c%3A/Repos/App", &[CHAT]);
        let global = config_dir.join("User/globalStorage");
        fs::create_dir_all(&global).unwrap();
        fs::write(
            global.join("storage.json"),
            serde_json::json!({ "windowsState": {
                "lastActiveWindow": { "folder": "file:///c:/repos/app/" },
                "openedWindows": [{ "workspaceIdentifier": { "id": "x", "configURIPath": "file:///c%3A/w.code-workspace" } }]
            } })
            .to_string(),
        )
        .unwrap();
        let config_dirs = std::slice::from_ref(&config_dir);

        // Not running: the window list is stale.
        assert!(discover(config_dirs)[CHAT]
            .copies
            .iter()
            .all(|c| !c.workspace.window_open));

        // This test process stands in for a running VS Code.
        fs::write(config_dir.join("code.lock"), std::process::id().to_string()).unwrap();
        let chats = discover(config_dirs);
        assert!(chats[CHAT].copies[0].workspace.window_open);
        let err = delete(config_dirs, CHAT).unwrap_err();
        assert!(err.contains("Close that VS Code window"), "{err}");
        assert!(storage.join(format!("chatSessions/{CHAT}.jsonl")).exists());
        assert_eq!(stored_ids(&storage), vec![CHAT.to_string()]);

        let open = open_windows(&config_dir).unwrap();
        assert!(!open.empty && !open.all);
        assert!(open.uris.contains("file:///c:/w.code-workspace"));

        fs::write(config_dir.join("code.lock"), "4294967294").unwrap();
        delete(config_dirs, CHAT).unwrap();
        fs::remove_dir_all(&config_dir).unwrap();
    }

    #[test]
    fn removes_chat_from_state_values() {
        let mut index = serde_json::json!({ "version": 1, "entries": { CHAT: {}, OTHER: {} } });
        assert!(remove_chat_from(INDEX_KEY, &mut index, CHAT));
        assert!(!remove_chat_from(INDEX_KEY, &mut index, CHAT));
        assert_eq!(index["entries"].as_object().unwrap().len(), 1);

        let mut cache = serde_json::json!([
            { "resource": chat_resource(CHAT) },
            { "resource": chat_resource(OTHER) },
            { "resource": "vscode-chat-session://copilotcli/abc" }
        ]);
        assert!(remove_chat_from(AGENT_SESSION_CACHES[0], &mut cache, CHAT));
        assert_eq!(cache.as_array().unwrap().len(), 2);

        let mut terminals =
            serde_json::json!({ "1": { "sessionId": CHAT }, "2": { "sessionId": OTHER } });
        assert!(remove_chat_from(TERMINALS_KEY, &mut terminals, CHAT));
        assert_eq!(terminals.as_object().unwrap().len(), 1);

        let mut not_a_map = serde_json::json!([CHAT]);
        assert!(!remove_chat_from(TODO_KEY, &mut not_a_map, CHAT));
    }

    #[test]
    fn replays_mutation_log() {
        let path = temp_path("24c442d3-ada5-4d9a-b945-c448f7d78023.jsonl");
        let lines = [
            r#"{"kind":0,"v":{"version":3,"creationDate":1784095273625,"requests":[],"inputState":{"inputText":"draft"}}}"#,
            r#"{"kind":1,"k":["customTitle"],"v":"Invalid start parameter"}"#,
            r#"{"kind":2,"k":["requests"],"v":[{"requestId":"r1","timestamp":1784095300000,"message":{"text":"Why does it fail?","parts":[]},"response":[],"modelState":{"value":0}}]}"#,
            r#"{"kind":2,"k":["requests",0,"response"],"v":[{"kind":"thinking","value":["hidden"]},{"value":"See ","supportHtml":false},{"kind":"inlineReference","name":"src/Program.cs#L1-L2","inlineReference":{"uri":{"path":"/c:/x/src/Program.cs"}}},{"value":" first."}]}"#,
            r#"{"kind":2,"k":["requests",0,"response"],"v":[{"kind":"toolInvocationSerialized","invocationMessage":{"value":"Reading"},"toolSpecificData":{"value":1}},{"value":"Fixed it."}]}"#,
            r#"{"kind":1,"k":["requests",0,"modelState"],"v":{"value":1,"completedAt":1784095400000}}"#,
            r#"{"kind":1,"k":["requests",0,"result"],"v":{"metadata":{"huge":[1,2,3]}}}"#,
            r#"{"kind":2,"k":["requests"],"v":[{"requestId":"r2","timestamp":1784095500000,"message":{"text":"dropped"},"response":[]}]}"#,
            r#"{"kind":2,"k":["requests"],"i":1}"#,
            r#"{"kind":2,"k":["requests"],"v":[{"requestId":"r3","message":{"text":"Thanks"},"response":[{"kind":"markdownContent","content":{"value":"You're welcome"}}]}]}"#,
            r#"{"kind":1,"k":["requests",1,"response",0],"v":{"value":"Any time"}}"#,
            r#"{"kind":2,"k":["requests",0,"respo"#,
        ];
        fs::write(&path, lines.join("\n")).unwrap();
        let chat = parse_chat_file(&path, (0, None));
        fs::remove_file(&path).unwrap();

        let messages: Vec<_> = chat
            .events
            .messages
            .iter()
            .map(|m| (m.role, m.content.as_str()))
            .collect();
        assert_eq!(
            messages,
            [
                ("user", "Why does it fail?"),
                (
                    "assistant",
                    "See `src/Program.cs#L1-L2` first.\n\nFixed it."
                ),
                ("user", "Thanks"),
                ("assistant", "Any time"),
            ]
        );
        assert_eq!(
            chat.custom_title.as_deref(),
            Some("Invalid start parameter")
        );
        assert_eq!(chat.created_ms, Some(1_784_095_273_625.0));
        let first = &chat.events.messages[0];
        assert_eq!(
            first.timestamp.as_deref(),
            iso_from_millis(1_784_095_300_000.0).as_deref()
        );
        assert_eq!(
            chat.events.messages[1].timestamp.as_deref(),
            iso_from_millis(1_784_095_400_000.0).as_deref()
        );
    }

    #[test]
    fn parses_json_documents_leniently() {
        let path = temp_path("24c442d3-ada5-4d9a-b945-c448f7d78023.json");
        fs::write(
            &path,
            r#"{"version":3,"requests":[{"message":{"text":"Hello"},"timestamp":1763375822228,"response":[{"value":"Hi","baseUri":{"path":"/x"}},{"kind":"toolInvocationSerialized","value":{"nested":true},"name":7}],"result":{"metadata":{"renderedUserMessage":[{"text":"big"}]}}},{"message":null,"response":null}],"customTitle":null,"lastMessageDate":1763376071136}"#,
        )
        .unwrap();
        let chat = parse_chat_file(&path, (0, None));
        fs::remove_file(&path).unwrap();
        let contents: Vec<_> = chat
            .events
            .messages
            .iter()
            .map(|m| m.content.as_str())
            .collect();
        assert_eq!(contents, ["Hello", "Hi"]);
        assert_eq!(chat.custom_title, None);
        assert_eq!(chat.last_message_ms, Some(1_763_376_071_136.0));
    }

    #[test]
    fn converts_uris_and_ids() {
        if cfg!(windows) {
            assert_eq!(
                file_uri_to_path("file:///c%3A/Users/me/Repos/my%20repo").as_deref(),
                Some(r"c:\Users\me\Repos\my repo")
            );
            assert_eq!(
                file_uri_to_path("file://server/share/repo").as_deref(),
                Some(r"\\server\share\repo")
            );
        } else {
            assert_eq!(
                file_uri_to_path("file:///home/me/repo").as_deref(),
                Some("/home/me/repo")
            );
        }
        assert_eq!(file_uri_to_path("vscode-remote://wsl%2Bubuntu/home"), None);
        assert_eq!(
            remote_uri_path("vscode-remote://wsl%2Bubuntu/mnt/c/Repos/app").as_deref(),
            Some("/mnt/c/Repos/app")
        );
        assert_eq!(
            local_session_id(
                "vscode-chat-session://local/OTQ1ODZiMjQtODQ2My00ZTY2LWJkZTctOTRiNzFhMGMwNjJm"
            )
            .as_deref(),
            Some("94586b24-8463-4e66-bde7-94b71a0c062f")
        );
        assert_eq!(local_session_id("agent-host-copilotcli:/94586b24"), None);
    }

    #[test]
    fn formats_epoch_millis() {
        assert_eq!(iso_from_millis(0.0), None);
        assert_eq!(
            iso_from_millis(951_782_400_123.0).as_deref(),
            Some("2000-02-29T00:00:00.123Z")
        );
        assert_eq!(
            iso_from_millis(1_761_741_873_508.0).as_deref(),
            Some("2025-10-29T12:44:33.508Z")
        );
    }
}

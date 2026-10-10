//! VS Code local chat sessions ("Local" in VS Code's sessions view).
//!
//! Each workspace keeps its chats under `User/workspaceStorage/<hash>/chatSessions` (chats
//! without a folder under `User/globalStorage/emptyWindowChatSessions`), either as one JSON
//! document (`.json`, older) or as an append-only mutation log (`.jsonl`): an initial snapshot
//! followed by set / push / delete operations on object paths. Titles, the archived ("done")
//! flag and change counts come from the workspace's `state.vscdb`. Everything is read-only;
//! VS Code owns these files and their index, so sessions are never deleted here.

use crate::{
    file_stamp, first_line_title, is_session_id, non_empty, ChatMessage, Entry, FileStamp,
    ParsedEvents, Project, SessionSummary,
};
use rayon::prelude::*;
use rusqlite::{Connection, OpenFlags};
use serde::de::{
    value::{MapAccessDeserializer, SeqAccessDeserializer},
    IgnoredAny, MapAccess, SeqAccess, Visitor,
};
use serde::{Deserialize, Deserializer};
use serde_json::value::RawValue;
use serde_json::Value;
use std::collections::HashMap;
use std::fmt;
use std::fs::{self, File};
use std::io::{BufRead, BufReader};
use std::marker::PhantomData;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

pub const PROTECTION: &str =
    "VS Code chat sessions are managed by VS Code. Delete or archive them in VS Code's Chat view.";

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
}

struct ChatFile {
    id: String,
    path: PathBuf,
    stamp: FileStamp,
    workspace: Arc<Workspace>,
}

/// Indexes all VS Code local chats with at least one message.
pub fn index(cache: &mut Cache, config_dirs: &[PathBuf]) -> Vec<Entry> {
    let files = discover(config_dirs);

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
            protection: Some(PROTECTION.to_string()),
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

fn discover(config_dirs: &[PathBuf]) -> Vec<ChatFile> {
    let mut chat_dirs: Vec<(PathBuf, Arc<Workspace>)> = Vec::new();
    for config_dir in config_dirs {
        let user_dir = config_dir.join("User");
        if let Ok(entries) = fs::read_dir(user_dir.join("workspaceStorage")) {
            for entry in entries.flatten() {
                let storage_dir = entry.path();
                let chat_dir = storage_dir.join("chatSessions");
                if chat_dir.is_dir() {
                    let workspace = read_workspace(storage_dir);
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
            };
            chat_dirs.push((empty_window, Arc::new(workspace)));
        }
    }

    // VS Code copies chats into other workspace storages, e.g. into untitled workspaces of
    // transient windows that are later deleted. Read the most recently written copy, but group
    // it under the newest copy stored for a real folder.
    let mut copies: HashMap<String, Vec<ChatFile>> = HashMap::new();
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
            copies.entry(id.clone()).or_default().push(ChatFile {
                id,
                path,
                stamp,
                workspace: workspace.clone(),
            });
        }
    }
    copies.into_values().filter_map(pick_copy).collect()
}

fn pick_copy(copies: Vec<ChatFile>) -> Option<ChatFile> {
    let has_folder = |file: &&ChatFile| file.workspace.folder.is_some();
    let newest = |a: &&ChatFile, b: &&ChatFile| {
        (a.stamp.1, has_folder(a), &a.path).cmp(&(b.stamp.1, has_folder(b), &b.path))
    };
    let workspace = copies
        .iter()
        .filter(has_folder)
        .max_by(newest)
        .map(|file| file.workspace.clone());
    let content = copies.iter().max_by(newest)?;
    Some(ChatFile {
        id: content.id.clone(),
        path: content.path.clone(),
        stamp: content.stamp,
        workspace: workspace.unwrap_or_else(|| content.workspace.clone()),
    })
}

fn read_workspace(storage_dir: PathBuf) -> Workspace {
    let value = fs::read_to_string(storage_dir.join("workspace.json"))
        .ok()
        .and_then(|text| serde_json::from_str::<Value>(&text).ok())
        .unwrap_or(Value::Null);
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

    if let Some(entries) = read("chat.ChatSessionStore.index")["entries"].as_object() {
        for (id, entry) in entries {
            if is_session_id(id) {
                metas.entry(id.to_ascii_lowercase()).or_default().title =
                    non_empty(entry["title"].as_str());
            }
        }
    }
    // Both caches are lists keyed by `vscode-chat-session://local/<base64 session id>`.
    for key in ["agentSessions.model.cache", "agentSessions.state.cache"] {
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

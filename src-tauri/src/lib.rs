use rayon::prelude::*;
use rusqlite::{Connection, OpenFlags};
use serde::Serialize;
use serde_json::Value;
use std::collections::HashMap;
use std::fs::{self, File};
use std::io::{BufRead, BufReader};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::{Duration, SystemTime};
use tauri::{Manager, State};

mod window_state;

const TITLE_MAX_CHARS: usize = 200;
const SNIPPET_BEFORE_CHARS: usize = 60;
const SNIPPET_TOTAL_CHARS: usize = 220;

#[derive(Clone, Serialize)]
#[serde(rename_all = "camelCase")]
struct ChatMessage {
    role: &'static str,
    content: String,
    timestamp: Option<String>,
}

#[derive(Clone, Serialize)]
#[serde(rename_all = "camelCase")]
struct SessionSummary {
    id: String,
    title: String,
    cwd: Option<String>,
    repository: Option<String>,
    branch: Option<String>,
    project: Project,
    created_at: Option<String>,
    updated_at: Option<String>,
    message_count: usize,
    additions: Option<u64>,
    deletions: Option<u64>,
    done: bool,
    in_use: bool,
}

/// Repository (or folder) a session belongs to, used to group and filter sessions.
#[derive(Clone, Default, Debug, PartialEq, Serialize)]
struct Project {
    /// Stable grouping key; "none" when the session has no folder information.
    key: String,
    name: String,
    /// Full repository name or folder path.
    detail: String,
}

#[derive(Serialize)]
struct Snippet {
    role: &'static str,
    text: String,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct SearchHit {
    #[serde(flatten)]
    session: SessionSummary,
    snippet: Option<Snippet>,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct SessionDetail {
    session: SessionSummary,
    messages: Vec<ChatMessage>,
}

type FileStamp = (u64, Option<SystemTime>);

struct ParsedEvents {
    stamp: FileStamp,
    messages: Vec<ChatMessage>,
    lowered: Vec<String>,
    first_timestamp: Option<String>,
    last_timestamp: Option<String>,
}

struct Entry {
    summary: SessionSummary,
    git_root: Option<String>,
    title_lower: String,
    events: Arc<ParsedEvents>,
}

#[derive(Default)]
struct Index {
    parsed: HashMap<String, Arc<ParsedEvents>>,
    vscode_links: HashMap<PathBuf, VsCodeLink>,
    entries: Vec<Entry>,
}

#[derive(Clone)]
struct VsCodeLink {
    stamp: FileStamp,
    vscode_id: String,
    sdk_id: Option<String>,
}

#[derive(Default)]
struct AppState {
    index: Arc<Mutex<Index>>,
}

#[derive(Default)]
struct WorkspaceMeta {
    name: Option<String>,
    summary: Option<String>,
    cwd: Option<String>,
    git_root: Option<String>,
    repository: Option<String>,
    branch: Option<String>,
    created_at: Option<String>,
    updated_at: Option<String>,
}

struct VsCodeState {
    summary: Option<String>,
    done: bool,
    additions: Option<u64>,
    deletions: Option<u64>,
}

fn lock(index: &Mutex<Index>) -> MutexGuard<'_, Index> {
    index
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

fn home_dir() -> Option<PathBuf> {
    std::env::var_os("USERPROFILE")
        .or_else(|| std::env::var_os("HOME"))
        .map(PathBuf::from)
}

fn copilot_dir() -> Result<PathBuf, String> {
    if let Some(dir) = std::env::var_os("COPILOT_HOME") {
        return Ok(PathBuf::from(dir));
    }
    home_dir()
        .map(|home| home.join(".copilot"))
        .ok_or_else(|| "Unable to determine the user home directory".to_string())
}

fn session_root() -> Result<PathBuf, String> {
    Ok(copilot_dir()?.join("session-state"))
}

fn vscode_config_dirs() -> Vec<PathBuf> {
    let base = if cfg!(windows) {
        std::env::var_os("APPDATA").map(PathBuf::from)
    } else if cfg!(target_os = "macos") {
        home_dir().map(|home| home.join("Library").join("Application Support"))
    } else {
        std::env::var_os("XDG_CONFIG_HOME")
            .map(PathBuf::from)
            .or_else(|| home_dir().map(|home| home.join(".config")))
    };
    base.map(|base| {
        ["Code", "Code - Insiders"]
            .iter()
            .map(|name| base.join(name))
            .collect()
    })
    .unwrap_or_default()
}

fn is_session_id(id: &str) -> bool {
    id.len() == 36
        && id.chars().enumerate().all(|(i, c)| match i {
            8 | 13 | 18 | 23 => c == '-',
            _ => c.is_ascii_hexdigit(),
        })
}

fn non_empty(value: Option<&str>) -> Option<String> {
    value
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_owned)
}

fn first_line_title(text: &str) -> String {
    let line = text
        .lines()
        .map(str::trim)
        .find(|line| !line.is_empty())
        .unwrap_or("");
    let mut title: String = line.chars().take(TITLE_MAX_CHARS).collect();
    if line.chars().count() > TITLE_MAX_CHARS {
        title.push('…');
    }
    title
}

fn read_workspace(dir: &Path) -> WorkspaceMeta {
    let Ok(text) = fs::read_to_string(dir.join("workspace.yaml")) else {
        return WorkspaceMeta::default();
    };
    let Ok(serde_yaml::Value::Mapping(map)) = serde_yaml::from_str::<serde_yaml::Value>(&text)
    else {
        return WorkspaceMeta::default();
    };
    let get = |key: &str| -> Option<String> {
        match map.get(key)? {
            serde_yaml::Value::String(s) => non_empty(Some(s)),
            serde_yaml::Value::Number(n) => Some(n.to_string()),
            serde_yaml::Value::Bool(b) => Some(b.to_string()),
            _ => None,
        }
    };
    WorkspaceMeta {
        name: get("name"),
        summary: get("summary"),
        cwd: get("cwd"),
        git_root: get("git_root"),
        repository: get("repository"),
        branch: get("branch"),
        created_at: get("created_at"),
        updated_at: get("updated_at"),
    }
}

/// Reads VS Code's agent-host database read-only. "Set as done" is stored as `isArchived`.
fn read_vscode_state() -> HashMap<String, VsCodeState> {
    let mut states = HashMap::new();
    for config_dir in vscode_config_dirs() {
        let path = config_dir
            .join("User")
            .join("globalStorage")
            .join("agent-host.db");
        if !path.exists() {
            continue;
        }
        let flags = OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NO_MUTEX;
        let Ok(conn) = Connection::open_with_flags(&path, flags) else {
            continue;
        };
        let _ = conn.busy_timeout(Duration::from_millis(500));
        let Ok(mut stmt) = conn.prepare(
            "SELECT session_uri, payload FROM sessions_v2 \
             WHERE provider = 'copilotcli' AND payload IS NOT NULL",
        ) else {
            continue;
        };
        let Ok(rows) = stmt.query_map([], |row| {
            Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
        }) else {
            continue;
        };
        for (uri, payload) in rows.flatten() {
            let Some(id) = uri.rsplit('/').next().filter(|id| is_session_id(id)) else {
                continue;
            };
            let Ok(value) = serde_json::from_str::<Value>(&payload) else {
                continue;
            };
            let data = &value["data"];
            states
                .entry(id.to_ascii_lowercase())
                .or_insert_with(|| VsCodeState {
                    summary: non_empty(data["summary"].as_str()),
                    done: data["isArchived"].as_bool().unwrap_or(false),
                    additions: data["changes"]["additions"].as_u64(),
                    deletions: data["changes"]["deletions"].as_u64(),
                });
        }
    }
    states
}

fn parse_events(path: &Path, stamp: FileStamp) -> ParsedEvents {
    let mut messages = Vec::new();
    if let Ok(file) = File::open(path) {
        let mut reader = BufReader::with_capacity(1 << 20, file);
        let mut line = Vec::new();
        loop {
            line.clear();
            match reader.read_until(b'\n', &mut line) {
                Ok(0) | Err(_) => break,
                Ok(_) => {}
            }
            // Events are serialized with `type` first, so most large tool events are skipped
            // without JSON parsing.
            let role = if line.starts_with(br#"{"type":"user.message""#) {
                "user"
            } else if line.starts_with(br#"{"type":"assistant.message""#) {
                "assistant"
            } else {
                continue;
            };
            let Ok(event) = serde_json::from_slice::<Value>(&line) else {
                continue;
            };
            let data = &event["data"];
            let is_subagent = data["parentToolCallId"]
                .as_str()
                .is_some_and(|s| !s.is_empty());
            let is_injected_prompt = data["source"].as_str().is_some_and(|s| s != "user");
            if (role == "assistant" && is_subagent) || (role == "user" && is_injected_prompt) {
                continue;
            }
            let Some(content) = non_empty(data["content"].as_str()) else {
                continue;
            };
            messages.push(ChatMessage {
                role,
                content,
                timestamp: event["timestamp"].as_str().map(str::to_owned),
            });
        }
    }
    let lowered = messages.iter().map(|m| m.content.to_lowercase()).collect();
    ParsedEvents {
        stamp,
        first_timestamp: messages.first().and_then(|m| m.timestamp.clone()),
        last_timestamp: messages.last().and_then(|m| m.timestamp.clone()),
        messages,
        lowered,
    }
}

fn hold_files(dir: &Path) -> Vec<PathBuf> {
    let Ok(entries) = fs::read_dir(dir) else {
        return Vec::new();
    };
    entries
        .flatten()
        .filter(|entry| {
            entry.file_name().to_str().is_some_and(|name| {
                name.strip_prefix("inuse.")
                    .and_then(|rest| rest.strip_suffix(".hold"))
                    .is_some_and(|pid| pid.parse::<u32>().is_ok())
            })
        })
        .map(|entry| entry.path())
        .collect()
}

/// A live Copilot runtime keeps `inuse.<pid>.hold` open without exclusive sharing; stale holds
/// left by exited processes can be opened exclusively. PID checks are unreliable because the
/// runtime can be hidden from normal process enumeration.
#[cfg(windows)]
fn hold_is_active(path: &Path) -> bool {
    use std::os::windows::fs::OpenOptionsExt;
    const ERROR_SHARING_VIOLATION: i32 = 32;
    const ERROR_LOCK_VIOLATION: i32 = 33;
    match fs::OpenOptions::new().read(true).share_mode(0).open(path) {
        Ok(_) => false,
        Err(err) => matches!(
            err.raw_os_error(),
            Some(ERROR_SHARING_VIOLATION | ERROR_LOCK_VIOLATION)
        ),
    }
}

#[cfg(not(windows))]
fn hold_is_active(path: &Path) -> bool {
    path.file_name()
        .and_then(|name| name.to_str())
        .and_then(|name| name.strip_prefix("inuse."))
        .and_then(|rest| rest.strip_suffix(".hold"))
        .is_some_and(|pid| Path::new("/proc").join(pid).exists())
}

fn is_in_use(dir: &Path) -> bool {
    hold_files(dir).iter().any(|path| hold_is_active(path))
}

fn max_timestamp(values: &[Option<&String>]) -> Option<String> {
    values.iter().flatten().max().map(|s| (*s).clone())
}

fn file_stamp(path: &Path) -> FileStamp {
    fs::metadata(path)
        .map(|m| (m.len(), m.modified().ok()))
        .unwrap_or((0, None))
}

fn read_sdk_session_id(db_path: &Path) -> Option<String> {
    let flags = OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NO_MUTEX;
    let conn = Connection::open_with_flags(db_path, flags).ok()?;
    let _ = conn.busy_timeout(Duration::from_millis(200));
    let value: String = conn
        .query_row(
            "SELECT value FROM session_metadata WHERE key = 'defaultChatProviderData'",
            [],
            |row| row.get(0),
        )
        .ok()?;
    let value = serde_json::from_str::<Value>(&value).ok()?;
    value["sdkSessionId"]
        .as_str()
        .filter(|id| is_session_id(id))
        .map(str::to_ascii_lowercase)
}

/// VS Code agent sessions use their own IDs; each per-session database records the Copilot
/// SDK session ID that names the folder in `~/.copilot/session-state`.
fn refresh_vscode_links(cache: &mut HashMap<PathBuf, VsCodeLink>) -> HashMap<String, String> {
    let candidates: Vec<(PathBuf, String)> = vscode_config_dirs()
        .into_iter()
        .filter_map(|config_dir| fs::read_dir(config_dir.join("agentSessionData")).ok())
        .flat_map(|entries| entries.flatten())
        .filter_map(|entry| {
            let id = entry.file_name().into_string().ok()?;
            let db_path = entry.path().join("session.db");
            (is_session_id(&id) && db_path.is_file()).then(|| (db_path, id.to_ascii_lowercase()))
        })
        .collect();

    let previous = &*cache;
    let links: Vec<(PathBuf, VsCodeLink)> = candidates
        .into_par_iter()
        .map(|(db_path, vscode_id)| {
            let stamp = file_stamp(&db_path);
            let link = match previous.get(&db_path) {
                Some(link) if link.sdk_id.is_some() || link.stamp == stamp => link.clone(),
                _ => VsCodeLink {
                    stamp,
                    sdk_id: read_sdk_session_id(&db_path),
                    vscode_id,
                },
            };
            (db_path, link)
        })
        .collect();

    *cache = links.into_iter().collect();
    let mut sdk_to_vscode = HashMap::new();
    for link in cache.values() {
        if let Some(sdk_id) = &link.sdk_id {
            sdk_to_vscode
                .entry(sdk_id.clone())
                .or_insert_with(|| link.vscode_id.clone());
        }
    }
    sdk_to_vscode
}

fn normalize_path(path: &str) -> String {
    let path = path.trim().replace('\\', "/");
    let trimmed = path.trim_end_matches('/');
    let path = if trimmed.is_empty() || trimmed.ends_with(':') {
        format!("{trimmed}/")
    } else {
        trimmed.to_string()
    };
    if cfg!(windows) {
        path.to_lowercase()
    } else {
        path
    }
}

fn last_segment(text: &str) -> String {
    text.trim()
        .trim_end_matches(['/', '\\'])
        .rsplit(['/', '\\'])
        .find(|segment| !segment.is_empty())
        .map(str::to_owned)
        .unwrap_or_else(|| text.trim().to_string())
}

fn repository_project(repository: &str) -> Project {
    Project {
        key: format!("repo:{}", repository.trim().to_lowercase()),
        name: last_segment(repository),
        detail: repository.trim().to_string(),
    }
}

fn folder_project(path: &str) -> Project {
    Project {
        key: format!("path:{}", normalize_path(path)),
        name: last_segment(path),
        detail: path.trim().to_string(),
    }
}

/// Groups sessions by repository. Sessions without repository metadata (for example started
/// before the folder was a git repo, or in a subfolder) are matched to a known git root that
/// contains their working directory, falling back to the working directory itself.
fn assign_projects(entries: &mut [Entry]) {
    let mut roots: HashMap<String, Project> = HashMap::new();
    for entry in entries.iter() {
        if let (Some(root), Some(repository)) = (&entry.git_root, &entry.summary.repository) {
            roots
                .entry(normalize_path(root))
                .or_insert_with(|| repository_project(repository));
        }
    }
    for entry in entries.iter() {
        if let (Some(root), None) = (&entry.git_root, &entry.summary.repository) {
            roots
                .entry(normalize_path(root))
                .or_insert_with(|| folder_project(root));
        }
    }

    let containing_root = |path: &str| -> Option<Project> {
        let path = normalize_path(path);
        roots
            .iter()
            .filter(|(root, _)| {
                path == **root
                    || path
                        .strip_prefix(root.as_str())
                        .is_some_and(|rest| root.ends_with('/') || rest.starts_with('/'))
            })
            .max_by_key(|(root, _)| root.len())
            .map(|(_, project)| project.clone())
    };

    for entry in entries.iter_mut() {
        let summary = &entry.summary;
        entry.summary.project = if let Some(repository) = &summary.repository {
            repository_project(repository)
        } else if let Some(root) = entry.git_root.as_deref().or(summary.cwd.as_deref()) {
            containing_root(root).unwrap_or_else(|| folder_project(root))
        } else {
            Project {
                key: "none".to_string(),
                name: "No folder".to_string(),
                detail: String::new(),
            }
        };
    }
}

fn rebuild(index: &mut Index) -> Result<(), String> {
    let root = session_root()?;
    let dirs: Vec<(String, PathBuf)> = match fs::read_dir(&root) {
        Ok(entries) => entries
            .flatten()
            .filter(|entry| entry.file_type().is_ok_and(|t| t.is_dir()))
            .filter_map(|entry| {
                let id = entry.file_name().into_string().ok()?;
                is_session_id(&id).then(|| (id.to_ascii_lowercase(), entry.path()))
            })
            .collect(),
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => Vec::new(),
        Err(err) => return Err(format!("Unable to read {}: {err}", root.display())),
    };

    let vscode = read_vscode_state();
    let sdk_to_vscode = refresh_vscode_links(&mut index.vscode_links);
    let previous = &index.parsed;

    let mut entries: Vec<Entry> = dirs
        .par_iter()
        .filter_map(|(id, dir)| {
            let events_path = dir.join("events.jsonl");
            let stamp = file_stamp(&events_path);
            let events = match previous.get(id) {
                Some(parsed) if parsed.stamp == stamp => parsed.clone(),
                _ => Arc::new(parse_events(&events_path, stamp)),
            };
            if events.messages.is_empty() {
                return None;
            }

            let meta = read_workspace(dir);
            let vs = sdk_to_vscode
                .get(id)
                .and_then(|vscode_id| vscode.get(vscode_id))
                .or_else(|| vscode.get(id));
            let first_user = events
                .messages
                .iter()
                .find(|m| m.role == "user")
                .map(|m| m.content.as_str());
            let title_source = vs
                .and_then(|s| s.summary.clone())
                .or_else(|| meta.name.clone())
                .or_else(|| meta.summary.clone())
                .or_else(|| first_user.map(str::to_owned))
                .unwrap_or_else(|| id.clone());
            let title = first_line_title(&title_source);
            let in_use = is_in_use(dir);

            let summary = SessionSummary {
                id: id.clone(),
                title: title.clone(),
                cwd: meta.cwd,
                repository: meta.repository,
                branch: meta.branch,
                project: Project::default(),
                created_at: meta
                    .created_at
                    .clone()
                    .or_else(|| events.first_timestamp.clone()),
                updated_at: max_timestamp(&[
                    meta.updated_at.as_ref(),
                    events.last_timestamp.as_ref(),
                ]),
                message_count: events.messages.len(),
                additions: vs.and_then(|s| s.additions),
                deletions: vs.and_then(|s| s.deletions),
                done: vs.is_some_and(|s| s.done),
                in_use,
            };
            Some(Entry {
                title_lower: title.to_lowercase(),
                summary,
                git_root: meta.git_root,
                events,
            })
        })
        .collect();

    assign_projects(&mut entries);
    entries.sort_by(|a, b| b.summary.updated_at.cmp(&a.summary.updated_at));
    index.parsed = entries
        .iter()
        .map(|entry| (entry.summary.id.clone(), entry.events.clone()))
        .collect();
    index.entries = entries;
    Ok(())
}

fn make_snippet(message: &ChatMessage, lowered: &str, term: &str) -> Snippet {
    let byte_index = lowered.find(term).unwrap_or(0);
    let char_index = lowered[..byte_index].chars().count();
    let start = char_index.saturating_sub(SNIPPET_BEFORE_CHARS);
    let raw: String = message
        .content
        .chars()
        .skip(start)
        .take(SNIPPET_TOTAL_CHARS)
        .collect();
    let mut text = raw.split_whitespace().collect::<Vec<_>>().join(" ");
    if start > 0 {
        text.insert(0, '…');
    }
    Snippet {
        role: message.role,
        text,
    }
}

fn search(index: &Index, query: &str) -> Vec<SearchHit> {
    let lowered_query = query.to_lowercase();
    let terms: Vec<&str> = lowered_query.split_whitespace().collect();
    index
        .entries
        .iter()
        .filter_map(|entry| {
            let mut snippet = None;
            for term in &terms {
                if entry.title_lower.contains(term) {
                    continue;
                }
                let position = entry
                    .events
                    .lowered
                    .iter()
                    .position(|message| message.contains(term))?;
                if snippet.is_none() {
                    snippet = Some(make_snippet(
                        &entry.events.messages[position],
                        &entry.events.lowered[position],
                        term,
                    ));
                }
            }
            Some(SearchHit {
                session: entry.summary.clone(),
                snippet,
            })
        })
        .collect()
}

async fn with_index<T, F>(state: &AppState, f: F) -> Result<T, String>
where
    T: Send + 'static,
    F: FnOnce(&mut Index) -> Result<T, String> + Send + 'static,
{
    let index = state.index.clone();
    tauri::async_runtime::spawn_blocking(move || f(&mut lock(&index)))
        .await
        .map_err(|err| err.to_string())?
}

#[tauri::command]
async fn refresh_sessions(state: State<'_, AppState>) -> Result<usize, String> {
    with_index(&state, |index| {
        rebuild(index)?;
        Ok(index.entries.len())
    })
    .await
}

#[tauri::command]
async fn search_sessions(
    state: State<'_, AppState>,
    query: String,
) -> Result<Vec<SearchHit>, String> {
    with_index(&state, move |index| Ok(search(index, &query))).await
}

#[tauri::command]
async fn get_session(state: State<'_, AppState>, id: String) -> Result<SessionDetail, String> {
    with_index(&state, move |index| {
        let entry = index
            .entries
            .iter()
            .find(|entry| entry.summary.id == id)
            .ok_or_else(|| format!("Session {id} was not found"))?;
        Ok(SessionDetail {
            session: entry.summary.clone(),
            messages: entry.events.messages.clone(),
        })
    })
    .await
}

fn delete_session_dir(root: &Path, id: &str) -> Result<(), String> {
    if !is_session_id(id) {
        return Err("Invalid session id".to_string());
    }
    let dir = root.join(id);
    if !dir.is_dir() {
        return Err(format!("Session folder {} does not exist", dir.display()));
    }
    if is_in_use(&dir) {
        return Err("This session is currently in use. Close it before deleting.".to_string());
    }
    fs::remove_dir_all(&dir).map_err(|err| format!("Unable to delete {}: {err}", dir.display()))
}

#[tauri::command]
async fn delete_session(state: State<'_, AppState>, id: String) -> Result<(), String> {
    let id = id.to_ascii_lowercase();
    if !is_session_id(&id) {
        return Err("Invalid session id".to_string());
    }
    with_index(&state, move |index| {
        delete_session_dir(&session_root()?, &id)?;
        index.entries.retain(|entry| entry.summary.id != id);
        index.parsed.remove(&id);
        Ok(())
    })
    .await
}

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    tauri::Builder::default()
        .manage(AppState::default())
        .manage(window_state::WindowSizeState::default())
        .setup(|app| {
            if let Some(window) = app.get_webview_window("main") {
                if let Err(err) = window_state::restore(&window) {
                    eprintln!("Unable to restore window size: {err}");
                    window.show()?;
                }
            }
            Ok(())
        })
        .on_window_event(window_state::handle_event)
        .invoke_handler(tauri::generate_handler![
            refresh_sessions,
            search_sessions,
            get_session,
            delete_session
        ])
        .run(tauri::generate_context!())
        .expect("error while running Copilot Session Search");
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_file(name: &str, content: &str) -> PathBuf {
        let path = std::env::temp_dir().join(format!(
            "copilot-session-search-{}-{name}",
            std::process::id()
        ));
        fs::write(&path, content).unwrap();
        path
    }

    #[test]
    #[ignore = "indexes the local Copilot and VS Code session stores"]
    fn indexes_local_sessions() {
        let started = std::time::Instant::now();
        let mut index = Index::default();
        rebuild(&mut index).unwrap();
        let cold = started.elapsed();
        let started = std::time::Instant::now();
        rebuild(&mut index).unwrap();
        let warm = started.elapsed();
        let with_vscode_state = index
            .entries
            .iter()
            .filter(|entry| entry.summary.additions.is_some())
            .count();
        let in_use: Vec<_> = index
            .entries
            .iter()
            .filter(|entry| entry.summary.in_use)
            .map(|entry| entry.summary.id.as_str())
            .collect();
        println!(
            "sessions={} with_vscode_state={with_vscode_state} cold={cold:?} warm={warm:?} in_use={in_use:?}",
            index.entries.len()
        );
        assert!(!index.entries.is_empty());
    }

    #[test]
    fn validates_session_ids() {
        assert!(is_session_id("24c442d3-ada5-4d9a-b945-c448f7d78023"));
        assert!(!is_session_id("../c442d3-ada5-4d9a-b945-c448f7d78023"));
        assert!(!is_session_id("24c442d3ada54d9ab945c448f7d78023"));
    }

    #[test]
    #[cfg(windows)]
    fn delete_refuses_live_sessions_and_removes_stale_ones() {
        let id = "00000000-0000-4000-8000-000000000001";
        let root = std::env::temp_dir().join(format!(
            "copilot-session-search-root-{}",
            std::process::id()
        ));
        let dir = root.join(id);
        fs::create_dir_all(&dir).unwrap();
        fs::write(dir.join("events.jsonl"), "").unwrap();
        fs::write(dir.join("inuse.1234.lock"), "").unwrap();

        assert!(delete_session_dir(&root, "../outside").is_err());

        let live_hold = File::create(dir.join("inuse.1234.hold")).unwrap();
        assert!(is_in_use(&dir));
        assert!(delete_session_dir(&root, id).is_err());
        assert!(dir.is_dir());

        drop(live_hold);
        assert!(!is_in_use(&dir));
        delete_session_dir(&root, id).unwrap();
        assert!(!dir.exists());
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn parses_only_main_conversation_messages() {
        let path = temp_file(
            "events.jsonl",
            concat!(
                r#"{"type":"session.start","data":{}}"#,
                "\n",
                r#"{"type":"user.message","data":{"content":"Find flaky tests"},"timestamp":"2026-01-01T00:00:00Z"}"#,
                "\n",
                r#"{"type":"user.message","data":{"content":"injected","source":"agent-1"}}"#,
                "\n",
                r#"{"type":"assistant.message","data":{"content":"","toolRequests":[]}}"#,
                "\n",
                r#"{"type":"assistant.message","data":{"content":"subagent","parentToolCallId":"call-1"}}"#,
                "\n",
                r#"{"type":"assistant.message","data":{"content":"Found the cause"},"timestamp":"2026-01-01T00:01:00Z"}"#,
                "\n"
            ),
        );
        let parsed = parse_events(&path, (0, None));
        fs::remove_file(path).unwrap();

        let contents: Vec<_> = parsed.messages.iter().map(|m| m.content.as_str()).collect();
        assert_eq!(contents, ["Find flaky tests", "Found the cause"]);
        assert_eq!(
            parsed.first_timestamp.as_deref(),
            Some("2026-01-01T00:00:00Z")
        );
        assert_eq!(
            parsed.last_timestamp.as_deref(),
            Some("2026-01-01T00:01:00Z")
        );
    }

    #[test]
    fn search_requires_all_terms_across_title_and_messages() {
        let messages = vec![ChatMessage {
            role: "assistant",
            content: "The Broker pod restarted".to_string(),
            timestamp: None,
        }];
        let entry = test_entry("Failing integration tests", messages, None, None, None);
        let index = Index {
            entries: vec![entry],
            ..Index::default()
        };

        let hits = search(&index, "TESTS broker");
        assert_eq!(hits.len(), 1);
        assert_eq!(
            hits[0].snippet.as_ref().unwrap().text,
            "The Broker pod restarted"
        );
        assert!(search(&index, "tests dataflow").is_empty());
        assert_eq!(search(&index, "").len(), 1);
    }

    fn test_entry(
        title: &str,
        messages: Vec<ChatMessage>,
        cwd: Option<&str>,
        git_root: Option<&str>,
        repository: Option<&str>,
    ) -> Entry {
        Entry {
            summary: SessionSummary {
                id: "24c442d3-ada5-4d9a-b945-c448f7d78023".to_string(),
                title: title.to_string(),
                cwd: cwd.map(str::to_owned),
                repository: repository.map(str::to_owned),
                branch: None,
                project: Project::default(),
                created_at: None,
                updated_at: None,
                message_count: messages.len(),
                additions: None,
                deletions: None,
                done: false,
                in_use: false,
            },
            git_root: git_root.map(str::to_owned),
            title_lower: title.to_lowercase(),
            events: Arc::new(ParsedEvents {
                stamp: (0, None),
                lowered: messages.iter().map(|m| m.content.to_lowercase()).collect(),
                messages,
                first_timestamp: None,
                last_timestamp: None,
            }),
        }
    }

    #[test]
    fn groups_sessions_by_repository_or_folder() {
        let entry = |cwd, git_root, repository| test_entry("t", Vec::new(), cwd, git_root, repository);
        let mut entries = vec![
            entry(
                Some(r"c:\Repos\aio-tests"),
                Some(r"C:\Repos\aio-tests"),
                Some("DefaultCollection/One/aio-tests"),
            ),
            // Same repo, but started before it was a git repo.
            entry(Some(r"C:\repos\AIO-tests\"), None, None),
            // Subfolder of the repo without metadata.
            entry(Some(r"C:\Repos\aio-tests\src\deep"), None, None),
            // Sibling folder sharing the prefix must not match.
            entry(Some(r"C:\Repos\aio-tests-old"), None, None),
            // Local git repo without a remote.
            entry(Some(r"C:\Repos\local\sub"), Some(r"C:\Repos\local"), None),
            entry(None, None, None),
            entry(Some(r"C:\"), None, None),
        ];
        assign_projects(&mut entries);
        let projects: Vec<_> = entries
            .iter()
            .map(|e| (e.summary.project.key.as_str(), e.summary.project.name.as_str()))
            .collect();
        let repo = ("repo:defaultcollection/one/aio-tests", "aio-tests");
        assert_eq!(projects[0], repo);
        if cfg!(windows) {
            assert_eq!(projects[1], repo);
        }
        assert_eq!(projects[2], repo);
        assert_eq!(projects[3].1, "aio-tests-old");
        assert_ne!(projects[3].0, repo.0);
        assert_eq!(projects[4].1, "local");
        assert!(projects[4].0.starts_with("path:"));
        assert_eq!(projects[5], ("none", "No folder"));
        assert_eq!(projects[6].1, "C:");
    }
}

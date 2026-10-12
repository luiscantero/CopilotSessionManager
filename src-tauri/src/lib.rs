use rayon::prelude::*;
use rusqlite::{Connection, OpenFlags};
use serde::Serialize;
use serde_json::Value;
use std::collections::HashMap;
use std::fs::{self, File};
use std::io::{BufRead, BufReader};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, SystemTime};

#[cfg(feature = "gui")]
mod gui;
mod project_merges;
mod sidecar;
mod vscode_chat;
mod watcher;
#[cfg(feature = "gui")]
mod window_state;

#[cfg(feature = "gui")]
pub use gui::run;
pub use sidecar::run as run_sidecar;

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
    /// "cli" for Copilot CLI / agent sessions, "vscode" for VS Code local chats.
    source: &'static str,
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
    /// VS Code chat whose workspace is open in a running VS Code window, which would write the
    /// chat back if it were deleted now.
    window_open: bool,
    /// Why the session must not be deleted or changed here (it belongs to another app).
    protection: Option<String>,
    /// Command that continues the session (or opens it in its app).
    resume_command: Option<String>,
}

/// Repository (or folder) a session belongs to, used to group and filter sessions.
#[derive(Clone, Default, Debug, PartialEq, Serialize)]
struct Project {
    /// Stable grouping key; "none" when the session has no folder information.
    key: String,
    name: String,
    /// Full repository name or folder path.
    detail: String,
    /// No session of this repository/folder group has an existing folder any more (it was
    /// probably renamed or moved), so it is a candidate for merging into the new project.
    missing: bool,
    /// Projects merged into this one.
    merged: Vec<project_merges::ProjectRef>,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct ProjectInfo {
    #[serde(flatten)]
    project: Project,
    sessions: usize,
    updated_at: Option<String>,
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

impl ParsedEvents {
    fn new(stamp: FileStamp, messages: Vec<ChatMessage>) -> Self {
        Self {
            stamp,
            lowered: messages.iter().map(|m| m.content.to_lowercase()).collect(),
            first_timestamp: messages.first().and_then(|m| m.timestamp.clone()),
            last_timestamp: messages.last().and_then(|m| m.timestamp.clone()),
            messages,
        }
    }
}

struct Entry {
    summary: SessionSummary,
    /// Project from the session's own repository/folder, before user-defined merges.
    base_project: Project,
    git_root: Option<String>,
    /// App that owns the session; such sessions are grouped by app instead of folder.
    owner_app: Option<String>,
    title_lower: String,
    events: Arc<ParsedEvents>,
}

#[derive(Default)]
struct Index {
    parsed: HashMap<String, Arc<ParsedEvents>>,
    vscode_links: HashMap<PathBuf, VsCodeLink>,
    vscode_chats: vscode_chat::Cache,
    merges: project_merges::Merges,
    entries: Vec<Entry>,
}

#[derive(Clone)]
struct VsCodeLink {
    stamp: FileStamp,
    vscode_id: String,
    sdk_id: Option<String>,
}

#[derive(Default)]
struct WorkspaceMeta {
    name: Option<String>,
    summary: Option<String>,
    cwd: Option<String>,
    git_root: Option<String>,
    repository: Option<String>,
    branch: Option<String>,
    client_name: Option<String>,
    created_at: Option<String>,
    updated_at: Option<String>,
}

/// Clients whose sessions are regular user conversations (Copilot CLI and VS Code).
const USER_CLIENTS: &[&str] = &["github/cli", "vscode-agent-host"];

/// Friendly names for apps known to drive Copilot sessions in the background.
const KNOWN_CLIENTS: &[(&str, &str)] = &[("wta", "Windows Terminal Agent")];

/// Folders that identify sessions owned by other apps or by the system.
struct ProtectedDirs {
    /// Per-user application data (`%APPDATA%`, `~/.config`, ...); the first sub folder names
    /// the owning app.
    app_data: Vec<String>,
    /// System and program folders (`C:\Windows`, `Program Files`, `/usr`, ...).
    system: Vec<String>,
}

impl ProtectedDirs {
    fn detect() -> Self {
        let env_dir = |name: &str| std::env::var_os(name).map(PathBuf::from);
        let home = home_dir();
        let mut app_data = Vec::new();
        let mut system = Vec::new();
        if cfg!(windows) {
            app_data.extend(
                ["APPDATA", "LOCALAPPDATA"]
                    .iter()
                    .filter_map(|n| env_dir(n)),
            );
            system.extend(
                [
                    "SystemRoot",
                    "windir",
                    "ProgramFiles",
                    "ProgramFiles(x86)",
                    "ProgramW6432",
                    "ProgramData",
                ]
                .iter()
                .filter_map(|n| env_dir(n)),
            );
        } else {
            if let Some(home) = &home {
                if cfg!(target_os = "macos") {
                    app_data.push(home.join("Library").join("Application Support"));
                }
                app_data.push(home.join(".config"));
                app_data.push(home.join(".local").join("share"));
            }
            app_data.extend(
                ["XDG_CONFIG_HOME", "XDG_DATA_HOME"]
                    .iter()
                    .filter_map(|n| env_dir(n)),
            );
            system.extend(
                [
                    "/usr", "/etc", "/var", "/opt", "/bin", "/sbin", "/System", "/Library",
                ]
                .iter()
                .map(PathBuf::from),
            );
        }
        app_data.extend(copilot_dir().ok());
        let normalize = |dirs: Vec<PathBuf>| -> Vec<String> {
            let mut dirs: Vec<String> = dirs
                .iter()
                .filter_map(|dir| dir.to_str())
                .filter(|dir| !dir.trim().is_empty())
                .map(normalize_path)
                .collect();
            dirs.sort();
            dirs.dedup();
            dirs
        };
        Self {
            app_data: normalize(app_data),
            system: normalize(system),
        }
    }
}

/// Explains why a Copilot CLI session belongs to another app, together with that app's name.
/// Such sessions are part of the app's own state, so deleting them could break it.
fn session_owner(
    client_name: Option<&str>,
    cwd: Option<&str>,
    dirs: &ProtectedDirs,
) -> Option<(String, String)> {
    let client = client_name
        .map(str::trim)
        .filter(|client| !client.is_empty())
        .filter(|client| {
            !USER_CLIENTS
                .iter()
                .any(|user| client.eq_ignore_ascii_case(user))
        });
    if let Some(client) = client {
        let lowered = client.to_ascii_lowercase();
        let app = KNOWN_CLIENTS
            .iter()
            .find(|(prefix, _)| lowered.starts_with(prefix))
            .map_or_else(|| client.to_string(), |(_, name)| (*name).to_string());
        let reason = if app == client {
            format!("Created by the app \"{client}\". Deleting it could break that app.")
        } else {
            format!("Created by {app} ({client}). Deleting it could break that app.")
        };
        return Some((reason, app));
    }

    let cwd = cwd.map(str::trim).filter(|cwd| !cwd.is_empty())?;
    let path = normalize_path(cwd);
    for root in &dirs.app_data {
        if let Some(rest) = path_within(&path, root) {
            // Keep the original casing: the segment right below the root names the app.
            let depth = root.split('/').filter(|s| !s.is_empty()).count();
            let app = cwd
                .split(['/', '\\'])
                .filter(|segment| !segment.is_empty())
                .nth(depth)
                .filter(|_| !rest.is_empty())
                .map_or_else(|| last_segment(cwd), str::to_owned);
            return Some((
                format!("Its folder is inside {app}'s application data. Deleting it could break that app."),
                app,
            ));
        }
    }
    dirs.system
        .iter()
        .any(|root| path_within(&path, root).is_some())
        .then(|| {
            (
                format!("Started in the system folder {cwd}, typically by a background tool. Deleting it could break that tool."),
                "System".to_string(),
            )
        })
}

/// Returns the remainder of a normalized `path` below the normalized `root`, if it is inside.
fn path_within<'a>(path: &'a str, root: &str) -> Option<&'a str> {
    if path == root {
        return Some("");
    }
    let rest = path.strip_prefix(root)?;
    if root.ends_with('/') {
        Some(rest)
    } else {
        rest.strip_prefix('/')
    }
}

struct VsCodeState {
    summary: Option<String>,
    done: bool,
    additions: Option<u64>,
    deletions: Option<u64>,
}

fn home_dir() -> Option<PathBuf> {
    std::env::var_os("USERPROFILE")
        .or_else(|| std::env::var_os("HOME"))
        .map(PathBuf::from)
}

/// Per-user application config folder (`%APPDATA%`, `~/Library/Application Support`,
/// `$XDG_CONFIG_HOME` or `~/.config`).
fn config_base_dir() -> Option<PathBuf> {
    if cfg!(windows) {
        std::env::var_os("APPDATA").map(PathBuf::from)
    } else if cfg!(target_os = "macos") {
        home_dir().map(|home| home.join("Library").join("Application Support"))
    } else {
        std::env::var_os("XDG_CONFIG_HOME")
            .map(PathBuf::from)
            .or_else(|| home_dir().map(|home| home.join(".config")))
    }
}

/// Same as `identifier` in `tauri.conf.json`. The desktop app and the VS Code extension share
/// this config folder, so project merges apply to both.
const APP_IDENTIFIER: &str = "com.luiscantero.copilot-session-manager";
/// Former identifiers, newest first: data in newer folders wins over older ones.
const LEGACY_APP_IDENTIFIERS: [&str; 2] = [
    "com.lcantero.copilot-session-manager",
    "com.lcantero.copilot-session-search",
];

/// Per-user folder for local (non-roaming) app data, where Tauri keeps the WebView storage.
fn local_data_base_dir() -> Option<PathBuf> {
    if cfg!(windows) {
        std::env::var_os("LOCALAPPDATA").map(PathBuf::from)
    } else if cfg!(target_os = "macos") {
        config_base_dir()
    } else {
        std::env::var_os("XDG_DATA_HOME")
            .map(PathBuf::from)
            .or_else(|| home_dir().map(|home| home.join(".local").join("share")))
    }
}

fn app_config_path(file: &str) -> Option<PathBuf> {
    config_base_dir().map(|dir| dir.join(APP_IDENTIFIER).join(file))
}

/// Moves the settings, project merges and WebView data saved under the app's former identifiers
/// to the current folders. Must run before the WebView starts.
fn migrate_legacy_app_dirs() {
    for base in [config_base_dir(), local_data_base_dir()]
        .into_iter()
        .flatten()
    {
        for legacy in LEGACY_APP_IDENTIFIERS {
            migrate_dir(&base.join(legacy), &base.join(APP_IDENTIFIER));
        }
    }
}

/// Moves every entry of `legacy` into `current` unless `current` already has one with that name,
/// then removes `legacy` if it is empty. Entries that are in use stay and are retried next time.
fn migrate_dir(legacy: &Path, current: &Path) {
    let Ok(entries) = fs::read_dir(legacy) else {
        return;
    };
    if let Err(err) = fs::create_dir_all(current) {
        eprintln!("Unable to create {}: {err}", current.display());
        return;
    }
    for entry in entries.flatten() {
        let target = current.join(entry.file_name());
        if target.exists() {
            continue;
        }
        if let Err(err) = fs::rename(entry.path(), &target) {
            eprintln!(
                "Unable to move {} to {}: {err}",
                entry.path().display(),
                target.display()
            );
        }
    }
    let _ = fs::remove_dir(legacy);
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
    config_base_dir()
        .map(|base| {
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
        client_name: get("client_name"),
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
    ParsedEvents::new(stamp, messages)
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
        ..Project::default()
    }
}

fn folder_project(path: &str) -> Project {
    Project {
        key: format!("path:{}", normalize_path(path)),
        name: last_segment(path),
        detail: path.trim().to_string(),
        ..Project::default()
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
        let project = if let Some(repository) = &summary.repository {
            repository_project(repository)
        } else if let Some(app) = &entry.owner_app {
            Project {
                key: format!("app:{}", app.to_lowercase()),
                name: app.clone(),
                detail: summary.cwd.clone().unwrap_or_default(),
                ..Project::default()
            }
        } else if let Some(root) = entry.git_root.as_deref().or(summary.cwd.as_deref()) {
            containing_root(root).unwrap_or_else(|| folder_project(root))
        } else {
            Project {
                key: "none".to_string(),
                name: "No folder".to_string(),
                ..Project::default()
            }
        };
        entry.summary.project = project.clone();
        entry.base_project = project;
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
    let protected_dirs = ProtectedDirs::detect();
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
            let owner = session_owner(
                meta.client_name.as_deref(),
                meta.cwd.as_deref(),
                &protected_dirs,
            );
            let (protection, owner_app) = owner.unzip();

            let summary = SessionSummary {
                id: id.clone(),
                title: title.clone(),
                source: "cli",
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
                window_open: false,
                protection,
                resume_command: Some(format!("copilot --resume={id}")),
            };
            Some(Entry {
                title_lower: title.to_lowercase(),
                summary,
                base_project: Project::default(),
                git_root: meta.git_root,
                owner_app,
                events,
            })
        })
        .collect();

    entries.extend(vscode_chat::index(
        &mut index.vscode_chats,
        &vscode_config_dirs(),
    ));
    assign_projects(&mut entries);
    project_merges::apply(&mut entries, &index.merges);
    entries.sort_by(|a, b| b.summary.updated_at.cmp(&a.summary.updated_at));
    index.parsed = entries
        .iter()
        .filter(|entry| entry.summary.source == "cli")
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

fn session_detail(index: &Index, id: &str) -> Result<SessionDetail, String> {
    let entry = index
        .entries
        .iter()
        .find(|entry| entry.summary.id == id)
        .ok_or_else(|| format!("Session {id} was not found"))?;
    Ok(SessionDetail {
        session: entry.summary.clone(),
        messages: entry.events.messages.clone(),
    })
}

fn delete_session_dir(root: &Path, id: &str, protected_dirs: &ProtectedDirs) -> Result<(), String> {
    if !is_session_id(id) {
        return Err("Invalid session id".to_string());
    }
    let dir = root.join(id);
    if !dir.is_dir() {
        return Err(format!("Session folder {} does not exist", dir.display()));
    }
    let meta = read_workspace(&dir);
    if let Some((reason, _)) = session_owner(
        meta.client_name.as_deref(),
        meta.cwd.as_deref(),
        protected_dirs,
    ) {
        return Err(format!("This session is protected. {reason}"));
    }
    if is_in_use(&dir) {
        return Err("This session is currently in use. Close it before deleting.".to_string());
    }
    fs::remove_dir_all(&dir).map_err(|err| format!("Unable to delete {}: {err}", dir.display()))
}

/// Deletes a Copilot CLI session folder or a VS Code chat and drops it from the index.
fn delete_entry(index: &mut Index, id: &str) -> Result<(), String> {
    let id = id.to_ascii_lowercase();
    if !is_session_id(&id) {
        return Err("Invalid session id".to_string());
    }
    let entry = index.entries.iter().find(|entry| entry.summary.id == id);
    if let Some(reason) = entry.and_then(|entry| entry.summary.protection.clone()) {
        return Err(format!("This session is protected. {reason}"));
    }
    let source = entry.map_or("cli", |entry| entry.summary.source);
    if source == "vscode" {
        vscode_chat::delete(&vscode_config_dirs(), &id)?;
    } else {
        delete_session_dir(&session_root()?, &id, &ProtectedDirs::detect())?;
        index.parsed.remove(&id);
    }
    index
        .entries
        .retain(|entry| !(entry.summary.id == id && entry.summary.source == source));
    Ok(())
}

fn project_ref(project: &Project) -> project_merges::ProjectRef {
    project_merges::ProjectRef {
        key: project.key.clone(),
        name: project.name.clone(),
        detail: project.detail.clone(),
    }
}

fn list_project_infos(index: &Index) -> Vec<ProjectInfo> {
    let mut projects: HashMap<&str, ProjectInfo> = HashMap::new();
    for entry in &index.entries {
        let project = &entry.summary.project;
        let info = projects
            .entry(project.key.as_str())
            .or_insert_with(|| ProjectInfo {
                project: project.clone(),
                sessions: 0,
                updated_at: None,
            });
        info.sessions += 1;
        if entry.summary.updated_at > info.updated_at {
            info.updated_at = entry.summary.updated_at.clone();
        }
    }
    let mut projects: Vec<ProjectInfo> = projects.into_values().collect();
    projects.sort_by(|a, b| b.updated_at.cmp(&a.updated_at));
    projects
}

/// Groups the sessions of project `from` under project `into` (both current group keys) and
/// saves the merges to `path`. Returns the key of the resulting group.
fn merge_projects(
    index: &mut Index,
    path: &Path,
    from: &str,
    into: &str,
) -> Result<String, String> {
    let find = |key: &str| {
        index
            .entries
            .iter()
            .map(|entry| &entry.summary.project)
            .find(|project| project.key == key)
            .map(project_ref)
            .ok_or_else(|| format!("Project {key} was not found"))
    };
    let (from, into) = (find(from)?, find(into)?);
    if from.key == into.key {
        return Err("Choose a different project to merge into".to_string());
    }
    let mut merges = index.merges.clone();
    merges.add(from, into.clone())?;
    merges.save(path)?;
    index.merges = merges;
    project_merges::apply(&mut index.entries, &index.merges);
    Ok(into.key)
}

/// Undoes the merge of project `key` (it becomes its own group again) and saves the merges.
fn unmerge_project(index: &mut Index, path: &Path, key: &str) -> Result<(), String> {
    let mut merges = index.merges.clone();
    if !merges.remove(key) {
        return Err(format!("Project {key} is not merged"));
    }
    merges.save(path)?;
    index.merges = merges;
    project_merges::apply(&mut index.entries, &index.merges);
    Ok(())
}

/// Watches the Copilot and VS Code session stores and calls `notify` after changes.
fn spawn_watcher(notify: impl Fn() + Send + 'static) {
    watcher::spawn(
        notify,
        session_root().ok(),
        vscode_config_dirs()
            .into_iter()
            .map(|dir| dir.join("User").join("globalStorage"))
            .collect(),
        vscode_config_dirs()
            .into_iter()
            .flat_map(|dir| {
                let user = dir.join("User");
                [
                    user.join("workspaceStorage"),
                    user.join("globalStorage").join("emptyWindowChatSessions"),
                ]
            })
            .collect(),
    );
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_file(name: &str, content: &str) -> PathBuf {
        let path = std::env::temp_dir().join(format!(
            "copilot-session-manager-{}-{name}",
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
        let protected: Vec<_> = index
            .entries
            .iter()
            .filter(|entry| entry.summary.protection.is_some() && entry.summary.source == "cli")
            .map(|entry| entry.summary.project.name.as_str())
            .collect();
        let mut projects: HashMap<(&str, &str), usize> = HashMap::new();
        for entry in &index.entries {
            *projects
                .entry((entry.summary.project.name.as_str(), entry.summary.source))
                .or_default() += 1;
        }
        let mut projects: Vec<_> = projects.into_iter().collect();
        projects.sort();
        let mut missing: Vec<_> = index
            .entries
            .iter()
            .filter(|entry| entry.summary.project.missing)
            .map(|entry| entry.summary.project.detail.as_str())
            .collect();
        missing.sort();
        missing.dedup();
        let mut window_open: Vec<_> = index
            .entries
            .iter()
            .filter(|entry| entry.summary.window_open)
            .map(|entry| entry.summary.project.name.as_str())
            .collect();
        window_open.sort();
        window_open.dedup();
        println!("missing_folders={missing:?}");
        println!("vscode_window_open={window_open:?}");
        println!(
            "sessions={} vscode_chats={} with_vscode_state={with_vscode_state} cold={cold:?} warm={warm:?} in_use={in_use:?} protected_cli={protected:?}\nprojects={projects:?}",
            index.entries.len(),
            index.entries.iter().filter(|entry| entry.summary.source == "vscode").count()
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
    fn migrates_legacy_app_folder_without_overwriting() {
        let root = std::env::temp_dir().join(format!(
            "copilot-session-manager-migrate-{}",
            std::process::id()
        ));
        let _ = fs::remove_dir_all(&root);
        let legacy = root.join("legacy");
        let current = root.join("current");
        fs::create_dir_all(legacy.join("EBWebView")).unwrap();
        fs::write(legacy.join("EBWebView").join("Local State"), "old").unwrap();
        fs::write(legacy.join("project-merges.json"), "merges").unwrap();
        fs::create_dir_all(&current).unwrap();
        fs::write(current.join("window-state.json"), "new").unwrap();
        fs::write(legacy.join("window-state.json"), "old").unwrap();

        migrate_dir(&legacy, &current);

        let read = |path: PathBuf| fs::read_to_string(path).unwrap();
        assert_eq!(read(current.join("project-merges.json")), "merges");
        assert_eq!(read(current.join("EBWebView").join("Local State")), "old");
        assert_eq!(read(current.join("window-state.json")), "new");
        // The entry that was not moved keeps the legacy folder alive.
        assert_eq!(read(legacy.join("window-state.json")), "old");
        assert!(!legacy.join("project-merges.json").exists());

        fs::remove_file(legacy.join("window-state.json")).unwrap();
        migrate_dir(&legacy, &current);
        assert!(!legacy.exists());
        migrate_dir(&root.join("missing"), &current);
        fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn delete_refuses_live_sessions_and_removes_stale_ones() {
        let id = "00000000-0000-4000-8000-000000000001";
        let root = std::env::temp_dir().join(format!(
            "copilot-session-manager-root-{}",
            std::process::id()
        ));
        let dir = root.join(id);
        fs::create_dir_all(&dir).unwrap();
        fs::write(dir.join("events.jsonl"), "").unwrap();
        fs::write(dir.join("inuse.1234.lock"), "").unwrap();

        assert!(delete_session_dir(&root, "../outside", &no_dirs()).is_err());

        let live_hold = File::create(dir.join("inuse.1234.hold")).unwrap();
        assert!(is_in_use(&dir));
        assert!(delete_session_dir(&root, id, &no_dirs()).is_err());
        assert!(dir.is_dir());

        drop(live_hold);
        assert!(!is_in_use(&dir));
        fs::write(
            dir.join("workspace.yaml"),
            "cwd: C:\\Windows\\System32\nclient_name: wta-master\n",
        )
        .unwrap();
        let err = delete_session_dir(&root, id, &no_dirs()).unwrap_err();
        assert!(err.contains("protected"), "{err}");
        assert!(dir.is_dir());

        fs::write(
            dir.join("workspace.yaml"),
            "cwd: C:\\Repos\\app\nclient_name: github/cli\n",
        )
        .unwrap();
        delete_session_dir(&root, id, &no_dirs()).unwrap();
        assert!(!dir.exists());
        fs::remove_dir_all(root).unwrap();
    }

    fn no_dirs() -> ProtectedDirs {
        ProtectedDirs {
            app_data: Vec::new(),
            system: Vec::new(),
        }
    }

    #[test]
    fn detects_sessions_owned_by_other_apps() {
        let dirs = ProtectedDirs {
            app_data: vec![normalize_path(r"C:\Users\me\AppData\Roaming")],
            system: vec![normalize_path(r"C:\WINDOWS")],
        };
        let owner = |client, cwd| session_owner(client, cwd, &dirs).map(|(_, app)| app);

        assert_eq!(
            owner(Some("vscode-agent-host"), Some(r"C:\Repos\app")),
            None
        );
        assert_eq!(owner(Some("github/cli"), Some(r"C:\Repos\app")), None);
        assert_eq!(owner(None, Some(r"C:\Repos\app")), None);
        assert_eq!(owner(None, None), None);
        assert_eq!(
            owner(Some("wta-master"), Some(r"C:\Users\me")).as_deref(),
            Some("Windows Terminal Agent")
        );
        assert_eq!(owner(Some("my-tool"), None).as_deref(), Some("my-tool"));
        if cfg!(windows) {
            assert_eq!(
                owner(None, Some(r"C:\Windows\system32")).as_deref(),
                Some("System")
            );
            assert_eq!(
                owner(
                    None,
                    Some(r"C:\Users\me\AppData\Roaming\Open Design\projects\bee7043a-0fe8-4bb2-bb5b-42a1c833ed26")
                )
                .as_deref(),
                Some("Open Design")
            );
        }
        // Sibling folders sharing a prefix are not protected.
        assert_eq!(owner(None, Some(r"C:\WINDOWS-old\repo")), None);
        assert_eq!(owner(None, Some(r"C:\Users\me\AppData\RoamingStuff")), None);
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
                source: "cli",
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
                window_open: false,
                protection: None,
                resume_command: None,
            },
            base_project: Project::default(),
            git_root: git_root.map(str::to_owned),
            owner_app: None,
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
        let entry =
            |cwd, git_root, repository| test_entry("t", Vec::new(), cwd, git_root, repository);
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
            // Sessions owned by another app are grouped by that app.
            Entry {
                owner_app: Some("Open Design".to_string()),
                ..entry(Some(r"C:\AppData\Open Design\projects\1"), None, None)
            },
        ];
        assign_projects(&mut entries);
        let projects: Vec<_> = entries
            .iter()
            .map(|e| {
                (
                    e.summary.project.key.as_str(),
                    e.summary.project.name.as_str(),
                )
            })
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
        assert_eq!(projects[7], ("app:open design", "Open Design"));
    }

    #[test]
    fn merges_renamed_projects_and_flags_missing_folders() {
        let current = std::env::temp_dir().join(format!(
            "copilot-session-manager-merge-{}",
            std::process::id()
        ));
        fs::create_dir_all(&current).unwrap();
        let current_path = current.to_str().unwrap();
        let gone = |name: &str| current.join("gone").join(name);
        let notebook = gone("ShotNotebook");
        let gallery = gone("ShotGallery");
        let entry =
            |cwd, git_root, repository| test_entry("t", Vec::new(), cwd, git_root, repository);
        let mut entries = vec![
            entry(notebook.to_str(), notebook.to_str(), None),
            entry(gallery.to_str(), gallery.to_str(), Some("me/ShotGallery")),
            entry(gallery.to_str(), None, None),
            entry(
                Some(current_path),
                Some(current_path),
                Some("me/ShotCollector"),
            ),
            entry(None, None, None),
        ];
        assign_projects(&mut entries);
        project_merges::apply(&mut entries, &project_merges::Merges::default());
        let summary = |entries: &[Entry]| -> Vec<(String, bool, usize)> {
            entries
                .iter()
                .map(|e| {
                    let p = &e.summary.project;
                    (p.name.clone(), p.missing, p.merged.len())
                })
                .collect()
        };
        assert_eq!(
            summary(&entries),
            [
                ("ShotNotebook".into(), true, 0),
                ("ShotGallery".into(), true, 0),
                ("ShotGallery".into(), true, 0),
                ("ShotCollector".into(), false, 0),
                ("No folder".into(), false, 0),
            ]
        );

        let refs: Vec<_> = entries
            .iter()
            .map(|e| project_ref(&e.summary.project))
            .collect();
        let mut merges = project_merges::Merges::default();
        merges.add(refs[0].clone(), refs[1].clone()).unwrap();
        merges.add(refs[1].clone(), refs[3].clone()).unwrap();
        project_merges::apply(&mut entries, &merges);
        let collector = entries[3].summary.project.clone();
        assert!(entries[..4].iter().all(|e| e.summary.project == collector));
        let merged: Vec<_> = collector.merged.iter().map(|p| p.name.as_str()).collect();
        assert_eq!(merged, ["ShotGallery", "ShotNotebook"]);
        assert!(!collector.missing);
        assert_eq!(entries[4].summary.project.key, "none");

        // Undoing the middle merge keeps ShotNotebook under ShotGallery.
        merges.remove(&refs[1].key);
        project_merges::apply(&mut entries, &merges);
        assert_eq!(entries[0].summary.project.name, "ShotGallery");
        assert!(entries[0].summary.project.missing);
        assert_eq!(entries[3].summary.project.merged, []);
        fs::remove_dir_all(&current).unwrap();
    }
}

//! Moves the sessions of a merged project to the folder of the project it was merged into, so
//! that VS Code and the Copilot CLI list them there too, and moves them back on unmerge.
//!
//! A Copilot CLI session is moved by rewriting `cwd`, `git_root` and `repository` in its
//! `workspace.yaml`, and the working directories that VS Code's agent host caches for it in
//! `agent-host.db`. VS Code local chats are moved between workspace storages by
//! [`crate::vscode_chat::move_chat`].

use crate::vscode_chat::file_uri_to_path;
use crate::{last_segment, normalize_path, path_within};
use rusqlite::{Connection, OpenFlags, OptionalExtension, TransactionBehavior};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use sha2::{Digest, Sha256};
use std::fs;
use std::path::{Path, PathBuf};
use std::time::Duration;

/// `workspace.yaml` fields that place a Copilot CLI session in a folder.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct CliFolder {
    pub cwd: Option<String>,
    pub git_root: Option<String>,
    pub repository: Option<String>,
}

/// A session's `agent-host.db` fields before it was moved.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct AgentHostSnapshot {
    pub db: PathBuf,
    pub working_directories: Value,
    pub project: Value,
    pub github_data: Value,
}

/// What was changed to move one session, so that it can be moved back.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum MovedSession {
    Cli {
        id: String,
        before: CliFolder,
        after: CliFolder,
        #[serde(default)]
        agent_host: Vec<AgentHostSnapshot>,
    },
    VscodeChat {
        id: String,
        from_storage: PathBuf,
        into_storage: PathBuf,
    },
}

impl MovedSession {
    pub fn id(&self) -> &str {
        match self {
            MovedSession::Cli { id, .. } | MovedSession::VscodeChat { id, .. } => id,
        }
    }

    /// Whether `later` moved the same session again from where this move put it, so this move
    /// can only be undone after `later`.
    pub fn continued_by(&self, later: &MovedSession) -> bool {
        match (self, later) {
            (
                MovedSession::Cli { id, after, .. },
                MovedSession::Cli {
                    id: later_id,
                    before,
                    ..
                },
            ) => id == later_id && after == before,
            (
                MovedSession::VscodeChat {
                    id, into_storage, ..
                },
                MovedSession::VscodeChat {
                    id: later_id,
                    from_storage,
                    ..
                },
            ) => id == later_id && into_storage == from_storage,
            _ => false,
        }
    }
}

/// Why a session could not be moved (or moved back).
#[derive(Debug, PartialEq)]
pub enum MoveError {
    /// Not possible right now, for example while the session is in use; try again later.
    Blocked(String),
    /// The session is no longer where it was moved to (deleted or changed since), so there is
    /// nothing to move back.
    Gone(String),
}

impl MoveError {
    pub fn reason(&self) -> &str {
        match self {
            MoveError::Blocked(reason) | MoveError::Gone(reason) => reason,
        }
    }
}

const AGENT_HOST_DB: &str = "User/globalStorage/agent-host.db";

/// Moves the Copilot CLI session in `dir` from `old_root` to `new_root`, keeping its sub
/// folder when that exists below `new_root`.
pub fn move_cli_session(
    dir: &Path,
    config_dirs: &[PathBuf],
    old_root: &str,
    new_root: &str,
    repository: Option<&str>,
) -> Result<MovedSession, String> {
    let id = session_id(dir)?;
    let path = dir.join("workspace.yaml");
    let text = fs::read_to_string(&path)
        .map_err(|err| format!("Unable to read {}: {err}", path.display()))?;
    let before = read_cli_folder(&text);
    let cwd = before
        .cwd
        .as_deref()
        .and_then(|cwd| relocate(cwd, old_root, new_root))
        .unwrap_or_else(|| new_root.to_string());
    let git_root = (before.git_root.is_some() || Path::new(new_root).join(".git").exists())
        .then(|| new_root.to_string());
    let after = CliFolder {
        cwd: Some(cwd),
        git_root,
        repository: repository.map(str::to_owned),
    };
    write_file(&path, &set_cli_folder(&text, &after))?;
    match move_agent_host(config_dirs, &id, old_root, new_root) {
        Ok(agent_host) => Ok(MovedSession::Cli {
            id,
            before,
            after,
            agent_host,
        }),
        Err(err) => {
            let _ = write_file(&path, &text);
            Err(err)
        }
    }
}

/// Moves a Copilot CLI session back to the folder it had before [`move_cli_session`].
pub fn restore_cli_session(
    dir: &Path,
    before: &CliFolder,
    after: &CliFolder,
    agent_host: &[AgentHostSnapshot],
) -> Result<(), MoveError> {
    let path = dir.join("workspace.yaml");
    let Ok(text) = fs::read_to_string(&path) else {
        return Err(MoveError::Gone("the session was deleted".to_string()));
    };
    if read_cli_folder(&text) != *after {
        return Err(MoveError::Gone(
            "its folder was changed since, so it was left as is".to_string(),
        ));
    }
    write_file(&path, &set_cli_folder(&text, before)).map_err(MoveError::Blocked)?;
    for snapshot in agent_host {
        if let Err(err) = restore_agent_host(snapshot, &session_id(dir).unwrap_or_default()) {
            let _ = write_file(&path, &text);
            return Err(MoveError::Blocked(err));
        }
    }
    Ok(())
}

fn session_id(dir: &Path) -> Result<String, String> {
    dir.file_name()
        .and_then(|name| name.to_str())
        .map(str::to_ascii_lowercase)
        .ok_or_else(|| format!("Invalid session folder {}", dir.display()))
}

/// Writes through a temporary file so that a failed write cannot leave a truncated file.
fn write_file(path: &Path, text: &str) -> Result<(), String> {
    let temp = path.with_extension("yaml.tmp");
    fs::write(&temp, text)
        .and_then(|()| fs::rename(&temp, path))
        .map_err(|err| {
            let _ = fs::remove_file(&temp);
            format!("Unable to write {}: {err}", path.display())
        })
}

pub fn read_cli_folder(text: &str) -> CliFolder {
    let Ok(serde_yaml::Value::Mapping(map)) = serde_yaml::from_str::<serde_yaml::Value>(text)
    else {
        return CliFolder::default();
    };
    let get = |key: &str| match map.get(key) {
        Some(serde_yaml::Value::String(value)) if !value.trim().is_empty() => {
            Some(value.trim().to_string())
        }
        _ => None,
    };
    CliFolder {
        cwd: get("cwd"),
        git_root: get("git_root"),
        repository: get("repository"),
    }
}

/// Sets the folder fields of a `workspace.yaml`, keeping every other line as it is. Missing
/// fields are removed.
pub fn set_cli_folder(text: &str, folder: &CliFolder) -> String {
    let fields = [
        ("cwd", &folder.cwd),
        ("git_root", &folder.git_root),
        ("repository", &folder.repository),
    ];
    let line = |key: &str, value: &str| format!("{key}: {}", yaml_scalar(value));
    let mut lines: Vec<String> = Vec::new();
    let mut found = [false; 3];
    let mut in_replaced_value = false;
    for text_line in text.lines() {
        // Continuation lines of a replaced multi-line value.
        if in_replaced_value && text_line.starts_with([' ', '\t']) {
            continue;
        }
        in_replaced_value = false;
        let field = fields.iter().position(|(key, _)| {
            text_line
                .strip_prefix(key)
                .is_some_and(|rest| rest.starts_with(':'))
        });
        match field {
            Some(index) => {
                in_replaced_value = true;
                found[index] = true;
                if let Some(value) = fields[index].1 {
                    lines.push(line(fields[index].0, value));
                }
            }
            None => lines.push(text_line.to_string()),
        }
    }
    for (index, (key, value)) in fields.iter().enumerate() {
        let (false, Some(value)) = (found[index], value) else {
            continue;
        };
        let after = lines.iter().rposition(|text_line| {
            fields[..index]
                .iter()
                .any(|(earlier, _)| text_line.starts_with(&format!("{earlier}:")))
        });
        let position = after.map_or(lines.len(), |position| position + 1);
        lines.insert(position, line(key, value));
    }
    let newline = if text.contains("\r\n") { "\r\n" } else { "\n" };
    let mut result = lines.join(newline);
    if text.ends_with('\n') || text.is_empty() {
        result.push_str(newline);
    }
    result
}

fn yaml_scalar(value: &str) -> String {
    serde_yaml::to_string(value)
        .map(|text| text.trim_end_matches('\n').to_string())
        .unwrap_or_else(|_| serde_json::Value::String(value.to_string()).to_string())
}

/// `path` moved from below `old_root` to the same place below `new_root`; `new_root` itself
/// when that sub folder does not exist there. None when `path` is not inside `old_root`.
pub fn relocate(path: &str, old_root: &str, new_root: &str) -> Option<String> {
    let normalized = normalize_path(path);
    let rest = path_within(&normalized, &normalize_path(old_root))?;
    let depth = rest.split('/').filter(|part| !part.is_empty()).count();
    let parts: Vec<&str> = path
        .split(['/', '\\'])
        .filter(|part| !part.is_empty())
        .collect();
    if depth == 0 || depth > parts.len() {
        return Some(new_root.to_string());
    }
    let mut moved = PathBuf::from(new_root);
    moved.extend(&parts[parts.len() - depth..]);
    Some(
        moved
            .to_str()
            .filter(|_| moved.is_dir())
            .map_or_else(|| new_root.to_string(), str::to_owned),
    )
}

/// `file://` URI of a local path, encoded like VS Code's `URI.file(path).toString()`.
pub fn file_uri(path: &str) -> String {
    let path = path.replace('\\', "/");
    let (authority, path) = match path.strip_prefix("//") {
        Some(unc) => match unc.find('/') {
            Some(slash) => (unc[..slash].to_lowercase(), unc[slash..].to_string()),
            None => (unc.to_lowercase(), "/".to_string()),
        },
        None => {
            let bytes = path.as_bytes();
            if bytes.len() >= 2 && bytes[0].is_ascii_alphabetic() && bytes[1] == b':' {
                (
                    String::new(),
                    format!("/{}{}", path[..1].to_ascii_lowercase(), &path[1..]),
                )
            } else if path.starts_with('/') {
                (String::new(), path)
            } else {
                (String::new(), format!("/{path}"))
            }
        }
    };
    let mut encoded = String::with_capacity(path.len());
    for byte in path.bytes() {
        if byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'.' | b'_' | b'~' | b'/') {
            encoded.push(byte as char);
        } else {
            encoded.push_str(&format!("%{byte:02X}"));
        }
    }
    format!("file://{authority}{encoded}")
}

fn relocate_uri(uri: &str, old_root: &str, new_root: &str) -> Option<String> {
    let path = file_uri_to_path(uri)?;
    relocate(&path, old_root, new_root).map(|moved| file_uri(&moved))
}

/// Points the working directories and project of agent-host session data from `old_root` to
/// `new_root`. Returns whether anything changed.
fn move_payload(data: &mut Value, old_root: &str, new_root: &str) -> bool {
    let mut changed = false;
    if let Some(dirs) = data
        .get_mut("workingDirectories")
        .and_then(Value::as_array_mut)
    {
        for dir in dirs {
            if let Some(moved) = dir
                .as_str()
                .and_then(|uri| relocate_uri(uri, old_root, new_root))
            {
                changed |= dir.as_str() != Some(moved.as_str());
                *dir = Value::String(moved);
            }
        }
    }
    if let Some(project) = data.get_mut("project").and_then(Value::as_object_mut) {
        let moved = project
            .get("uri")
            .and_then(Value::as_str)
            .and_then(|uri| relocate_uri(uri, old_root, new_root));
        if let Some(moved) = moved {
            if project.get("uri").and_then(Value::as_str) != Some(moved.as_str()) {
                let name = file_uri_to_path(&moved).map(|path| last_segment(&path));
                project.insert("uri".to_string(), Value::String(moved));
                if let Some(name) = name {
                    project.insert("displayName".to_string(), Value::String(name));
                }
                changed = true;
            }
        }
    }
    // GitHub data of the old folder; VS Code looks it up again for the new one.
    if let Some(github) = data
        .get_mut("_meta")
        .and_then(|meta| meta.get_mut("githubData"))
        .and_then(Value::as_object_mut)
    {
        let before = github.len();
        github.retain(|uri, _| {
            file_uri_to_path(uri).is_none_or(|path| {
                path_within(&normalize_path(&path), &normalize_path(old_root)).is_none()
            })
        });
        changed |= github.len() != before;
    }
    changed
}

fn snapshot(db: &Path, data: &Value) -> AgentHostSnapshot {
    AgentHostSnapshot {
        db: db.to_path_buf(),
        working_directories: data["workingDirectories"].clone(),
        project: data["project"].clone(),
        github_data: data["_meta"]["githubData"].clone(),
    }
}

fn apply_snapshot(data: &mut Value, snapshot: &AgentHostSnapshot) {
    let Some(data) = data.as_object_mut() else {
        return;
    };
    let set = |map: &mut serde_json::Map<String, Value>, key: &str, value: &Value| {
        if value.is_null() {
            map.remove(key);
        } else {
            map.insert(key.to_string(), value.clone());
        }
    };
    set(data, "workingDirectories", &snapshot.working_directories);
    set(data, "project", &snapshot.project);
    if let Some(meta) = data.get_mut("_meta").and_then(Value::as_object_mut) {
        set(meta, "githubData", &snapshot.github_data);
    } else if !snapshot.github_data.is_null() {
        data.insert(
            "_meta".to_string(),
            serde_json::json!({ "githubData": snapshot.github_data }),
        );
    }
}

/// Moves the session in every VS Code installation's agent host. VS Code lists agent-host
/// sessions under these cached working directories, not under the `workspace.yaml` folder.
fn move_agent_host(
    config_dirs: &[PathBuf],
    id: &str,
    old_root: &str,
    new_root: &str,
) -> Result<Vec<AgentHostSnapshot>, String> {
    let mut snapshots = Vec::new();
    for db in config_dirs.iter().map(|dir| dir.join(AGENT_HOST_DB)) {
        if !db.is_file() {
            continue;
        }
        let mut taken = None;
        let result = update_agent_host(&db, id, |data| {
            let before = snapshot(&db, data);
            let changed = move_payload(data, old_root, new_root);
            if changed {
                taken = Some(before);
            }
            changed
        });
        if let Err(err) = result {
            for snapshot in &snapshots {
                let _ = restore_agent_host(snapshot, id);
            }
            return Err(err);
        }
        snapshots.extend(taken);
    }
    Ok(snapshots)
}

fn restore_agent_host(snapshot: &AgentHostSnapshot, id: &str) -> Result<(), String> {
    if !snapshot.db.is_file() {
        return Ok(());
    }
    update_agent_host(&snapshot.db, id, |data| {
        apply_snapshot(data, snapshot);
        true
    })
    .map(|_| ())
}

/// Edits the payload `data` of agent-host session `id` and stores it with a matching hash.
/// Returns whether the session exists and `edit` changed it.
fn update_agent_host(
    db: &Path,
    id: &str,
    edit: impl FnOnce(&mut Value) -> bool,
) -> Result<bool, String> {
    let fail = |err: rusqlite::Error| format!("Unable to update {}: {err}", db.display());
    let flags = OpenFlags::SQLITE_OPEN_READ_WRITE | OpenFlags::SQLITE_OPEN_NO_MUTEX;
    let mut conn = Connection::open_with_flags(db, flags).map_err(fail)?;
    conn.busy_timeout(Duration::from_secs(5)).map_err(fail)?;
    let tx = conn
        .transaction_with_behavior(TransactionBehavior::Immediate)
        .map_err(fail)?;
    let has_table = tx
        .query_row(
            "SELECT 1 FROM sqlite_master WHERE type = 'table' AND name = 'sessions_v2'",
            [],
            |_| Ok(()),
        )
        .optional()
        .map_err(fail)?
        .is_some();
    if !has_table {
        return Ok(false);
    }
    let uri = format!("copilotcli:/{id}");
    let payload: Option<String> = tx
        .query_row(
            "SELECT payload FROM sessions_v2 WHERE session_uri = ?1 AND payload IS NOT NULL",
            [&uri],
            |row| row.get(0),
        )
        .optional()
        .map_err(fail)?;
    let Some(mut payload) = payload.and_then(|text| serde_json::from_str::<Value>(&text).ok())
    else {
        return Ok(false);
    };
    let Some(data) = payload.get_mut("data").filter(|data| data.is_object()) else {
        return Ok(false);
    };
    if !edit(data) {
        return Ok(false);
    }
    let text = canonical_json(&payload);
    let hash = Sha256::digest(text.as_bytes())
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect::<String>();
    tx.execute(
        "UPDATE sessions_v2 SET payload = ?1, payload_hash = ?2 WHERE session_uri = ?3",
        (&text, &hash, &uri),
    )
    .map_err(fail)?;
    tx.commit().map_err(fail)?;
    Ok(true)
}

/// Compact JSON with sorted keys, the form in which VS Code stores and hashes payloads.
fn canonical_json(value: &Value) -> String {
    fn write(value: &Value, out: &mut String) {
        match value {
            Value::Object(map) => {
                let mut keys: Vec<&String> = map.keys().collect();
                keys.sort_by(|a, b| a.encode_utf16().cmp(b.encode_utf16()));
                out.push('{');
                for (index, key) in keys.into_iter().enumerate() {
                    if index > 0 {
                        out.push(',');
                    }
                    out.push_str(&Value::String(key.clone()).to_string());
                    out.push(':');
                    write(&map[key], out);
                }
                out.push('}');
            }
            Value::Array(items) => {
                out.push('[');
                for (index, item) in items.iter().enumerate() {
                    if index > 0 {
                        out.push(',');
                    }
                    write(item, out);
                }
                out.push(']');
            }
            other => out.push_str(&other.to_string()),
        }
    }
    let mut out = String::new();
    write(value, &mut out);
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    const ID: &str = "2527405f-a368-4adf-9fc9-7170d4b6c6c4";

    fn temp_dir(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "copilot-session-manager-{}-{name}",
            std::process::id()
        ));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn path_str(path: &Path) -> String {
        path.to_str().unwrap().to_string()
    }

    #[test]
    fn rewrites_workspace_yaml_fields_only() {
        let text = "id: x\ncwd: C:\\Repos\\old\\sub\ngit_root: C:\\Repos\\old\nrepository: me/old\nhost_type: github\nname: 'a: b'\nsummary: |\n  line one\n  line two\n";
        let folder = CliFolder {
            cwd: Some("C:\\Repos\\new folder\\sub".to_string()),
            git_root: Some("C:\\Repos\\new folder".to_string()),
            repository: None,
        };
        let moved = set_cli_folder(text, &folder);
        assert_eq!(read_cli_folder(&moved), folder);
        assert!(moved.contains("host_type: github\nname: 'a: b'\nsummary: |\n  line one\n"));
        assert!(!moved.contains("repository"));

        let restored = set_cli_folder(
            &moved,
            &CliFolder {
                cwd: Some("C:\\Repos\\old\\sub".to_string()),
                git_root: Some("C:\\Repos\\old".to_string()),
                repository: Some("me/old".to_string()),
            },
        );
        assert_eq!(restored, text);

        let added = set_cli_folder(
            "id: x\ncwd: /a\nname: n",
            &CliFolder {
                cwd: Some("/b: c".to_string()),
                git_root: Some("/b".to_string()),
                repository: Some("me/b".to_string()),
            },
        );
        assert_eq!(
            read_cli_folder(&added),
            CliFolder {
                cwd: Some("/b: c".to_string()),
                git_root: Some("/b".to_string()),
                repository: Some("me/b".to_string()),
            }
        );
        assert!(added.starts_with("id: x\ncwd: '/b: c'\ngit_root: /b\nrepository: me/b\nname: n"));
        assert_eq!(
            set_cli_folder("cwd: a\r\nname: n\r\n", &CliFolder::default()),
            "name: n\r\n"
        );
    }

    #[test]
    fn relocates_paths_into_existing_sub_folders() {
        let root = temp_dir("relocate");
        let old = path_str(&root.join("Old"));
        let new = path_str(&root.join("New"));
        fs::create_dir_all(root.join("New").join("src")).unwrap();
        let old_src = path_str(&root.join("Old").join("src"));
        assert_eq!(
            relocate(&old_src, &old, &new),
            Some(path_str(&root.join("New").join("src")))
        );
        let old_docs = path_str(&root.join("Old").join("docs"));
        assert_eq!(relocate(&old_docs, &old, &new), Some(new.clone()));
        assert_eq!(relocate(&old, &old, &new), Some(new.clone()));
        assert_eq!(relocate(&path_str(&root.join("Older")), &old, &new), None);
        fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn encodes_file_uris_like_vscode() {
        assert_eq!(
            file_uri("C:\\Users\\me\\My Repo (1)"),
            "file:///c%3A/Users/me/My%20Repo%20%281%29"
        );
        assert_eq!(file_uri("/home/me/ä"), "file:///home/me/%C3%A4");
        assert_eq!(file_uri("\\\\Server\\share\\x"), "file://server/share/x");
        if cfg!(windows) {
            assert_eq!(
                file_uri_to_path(&file_uri("C:\\Users\\me\\My Repo")).as_deref(),
                Some("c:\\Users\\me\\My Repo")
            );
        }
    }

    #[test]
    fn detects_sessions_moved_again() {
        let folder = |cwd: &str| CliFolder {
            cwd: Some(cwd.to_string()),
            ..CliFolder::default()
        };
        let cli = |before: &str, after: &str| MovedSession::Cli {
            id: ID.to_string(),
            before: folder(before),
            after: folder(after),
            agent_host: Vec::new(),
        };
        assert!(cli("/a", "/b").continued_by(&cli("/b", "/c")));
        assert!(!cli("/b", "/c").continued_by(&cli("/a", "/b")));
        let chat = |from: &str, into: &str| MovedSession::VscodeChat {
            id: ID.to_string(),
            from_storage: from.into(),
            into_storage: into.into(),
        };
        assert!(chat("a", "b").continued_by(&chat("b", "c")));
        assert!(!chat("b", "c").continued_by(&chat("a", "b")));
        assert!(!cli("/a", "/b").continued_by(&chat("b", "c")));
    }

    #[test]
    fn hashes_canonical_payloads() {
        let payload = json!({ "z": 1, "a": { "b": [true, null, "é\n"], "A": 1.5 } });
        assert_eq!(
            canonical_json(&payload),
            r#"{"a":{"A":1.5,"b":[true,null,"é\n"]},"z":1}"#
        );
    }

    #[test]
    #[ignore = "reads the local VS Code agent-host.db"]
    fn reproduces_local_agent_host_payloads() {
        let mut checked = 0;
        for db in crate::vscode_config_dirs()
            .iter()
            .map(|dir| dir.join(AGENT_HOST_DB))
            .filter(|db| db.is_file())
        {
            let flags = OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NO_MUTEX;
            let conn = Connection::open_with_flags(&db, flags).unwrap();
            let mut stmt = conn
                .prepare("SELECT payload, payload_hash FROM sessions_v2 WHERE payload IS NOT NULL")
                .unwrap();
            let rows = stmt
                .query_map([], |row| {
                    Ok((row.get::<_, String>(0)?, row.get::<_, Option<String>>(1)?))
                })
                .unwrap();
            for (text, hash) in rows.flatten() {
                let value: Value = serde_json::from_str(&text).unwrap();
                assert_eq!(canonical_json(&value), text);
                if let Some(hash) = hash {
                    let digest = Sha256::digest(text.as_bytes())
                        .iter()
                        .map(|byte| format!("{byte:02x}"))
                        .collect::<String>();
                    assert_eq!(digest, hash);
                }
                checked += 1;
            }
        }
        println!("checked {checked} payloads");
    }

    fn agent_host_db(config_dir: &Path, payload: &Value) -> PathBuf {
        let db = config_dir.join(AGENT_HOST_DB);
        fs::create_dir_all(db.parent().unwrap()).unwrap();
        let conn = Connection::open(&db).unwrap();
        conn.execute_batch(
            "CREATE TABLE sessions_v2 (session_uri TEXT PRIMARY KEY NOT NULL, provider TEXT NOT NULL,
             payload_hash TEXT, payload TEXT)",
        )
        .unwrap();
        let text = canonical_json(payload);
        conn.execute(
            "INSERT INTO sessions_v2 VALUES (?1, 'copilotcli', 'old', ?2)",
            (format!("copilotcli:/{ID}"), text),
        )
        .unwrap();
        db
    }

    fn stored_payload(db: &Path) -> (Value, String, String) {
        let conn = Connection::open(db).unwrap();
        let (text, hash): (String, String) = conn
            .query_row("SELECT payload, payload_hash FROM sessions_v2", [], |row| {
                Ok((row.get(0)?, row.get(1)?))
            })
            .unwrap();
        (serde_json::from_str(&text).unwrap(), text, hash)
    }

    #[test]
    fn moves_cli_sessions_and_back() {
        let root = temp_dir("move-cli");
        let old = root.join("Old");
        let new = root.join("New");
        fs::create_dir_all(new.join("sub")).unwrap();
        fs::create_dir_all(new.join(".git")).unwrap();
        let (old_s, new_s) = (path_str(&old), path_str(&new));
        let dir = root.join("session-state").join(ID);
        fs::create_dir_all(&dir).unwrap();
        let yaml = format!(
            "id: {ID}\ncwd: {}\ngit_root: {old_s}\nrepository: me/old\nbranch: main\n",
            path_str(&old.join("sub"))
        );
        fs::write(dir.join("workspace.yaml"), &yaml).unwrap();

        let config_dir = root.join("Code");
        let old_uri = file_uri(&old_s);
        let payload = json!({
            "data": {
                "workingDirectories": [file_uri(&path_str(&old.join("sub")))],
                "project": { "displayName": "Old", "uri": old_uri },
                "_meta": { "githubData": { old_uri.to_lowercase(): { "owner": "me", "repo": "old" } }, "git": { "branchName": "main" } },
                "title": "t"
            },
            "payloadVersion": 2
        });
        let db = agent_host_db(&config_dir, &payload);
        let original = stored_payload(&db).1;

        let moved = move_cli_session(
            &dir,
            std::slice::from_ref(&config_dir),
            &old_s,
            &new_s,
            Some("me/new"),
        )
        .unwrap();
        let MovedSession::Cli {
            before,
            after,
            agent_host,
            ..
        } = &moved
        else {
            panic!("expected a CLI session");
        };
        assert_eq!(
            *after,
            CliFolder {
                cwd: Some(path_str(&new.join("sub"))),
                git_root: Some(new_s.clone()),
                repository: Some("me/new".to_string()),
            }
        );
        let text = fs::read_to_string(dir.join("workspace.yaml")).unwrap();
        assert_eq!(read_cli_folder(&text), *after);
        assert!(text.contains("branch: main"));

        let (data, text, hash) = stored_payload(&db);
        assert_eq!(
            data["data"]["workingDirectories"],
            json!([file_uri(&path_str(&new.join("sub")))])
        );
        assert_eq!(data["data"]["project"]["uri"], json!(file_uri(&new_s)));
        assert_eq!(data["data"]["project"]["displayName"], json!("New"));
        assert_eq!(data["data"]["_meta"]["githubData"], json!({}));
        assert_eq!(data["data"]["_meta"]["git"]["branchName"], json!("main"));
        let expected_hash = Sha256::digest(text.as_bytes())
            .iter()
            .map(|byte| format!("{byte:02x}"))
            .collect::<String>();
        assert_eq!(hash, expected_hash);

        // The record survives a save and load.
        let saved: MovedSession =
            serde_json::from_str(&serde_json::to_string(&moved).unwrap()).unwrap();
        assert_eq!(saved, moved);

        restore_cli_session(&dir, before, after, agent_host).unwrap();
        assert_eq!(
            fs::read_to_string(dir.join("workspace.yaml")).unwrap(),
            yaml
        );
        assert_eq!(stored_payload(&db).1, original);

        // Changed since: left alone.
        assert!(matches!(
            restore_cli_session(&dir, before, after, agent_host),
            Err(MoveError::Gone(_))
        ));
        fs::remove_dir_all(&root).unwrap();
    }
}

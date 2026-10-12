//! Notifies the frontend when sessions change on disk.
//!
//! A native file watcher covers `~/.copilot/session-state`, VS Code's `agent-host.db` and
//! `storage.json` (its open windows) and the VS Code chat session folders
//! (`workspaceStorage/*/chatSessions`, `emptyWindowChatSessions`).
//! Bursts of changes (an active session appends to `events.jsonl` constantly) are debounced
//! and throttled. A slow periodic poll covers anything the watcher cannot see, such as a
//! Copilot process exiting without removing its `inuse.<pid>.hold` lock file, or VS Code
//! archiving a chat (stored in its frequently written `state.vscdb`).
//! The frontend is notified through a callback (a Tauri event in the desktop app, a JSON line
//! from the sidecar used by the VS Code extension).

use notify::{Event, EventKind, RecommendedWatcher, RecursiveMode, Watcher};
use std::path::{Path, PathBuf};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError};
use std::time::{Duration, Instant};

pub const CHANGED_EVENT: &str = "sessions-changed";

/// Quiet time collected after the first change before notifying.
const DEBOUNCE: Duration = Duration::from_millis(1500);
/// Minimum time between two notifications while changes keep arriving.
const MIN_INTERVAL: Duration = Duration::from_secs(5);
/// Fallback refresh interval when nothing was reported by the watcher.
const POLL_INTERVAL: Duration = Duration::from_secs(45);

type EventResult = notify::Result<Event>;

struct Targets {
    session_root: Option<PathBuf>,
    /// VS Code `User/globalStorage` folders containing `agent-host.db`.
    storage_dirs: Vec<PathBuf>,
    /// VS Code folders holding chat session files, watched recursively.
    chat_dirs: Vec<PathBuf>,
}

pub fn spawn(
    notify: impl Fn() + Send + 'static,
    session_root: Option<PathBuf>,
    storage_dirs: Vec<PathBuf>,
    chat_dirs: Vec<PathBuf>,
) {
    let targets = Targets {
        session_root,
        storage_dirs,
        chat_dirs,
    };
    let spawned = std::thread::Builder::new()
        .name("session-watcher".to_string())
        .spawn(move || run(&notify, &targets));
    if let Err(err) = spawned {
        eprintln!("Unable to start the session watcher: {err}");
    }
}

fn run(notify: &dyn Fn(), targets: &Targets) {
    let (tx, rx) = mpsc::channel::<EventResult>();
    let mut watcher = match notify::recommended_watcher(tx.clone()) {
        Ok(watcher) => Some(watcher),
        Err(err) => {
            eprintln!("File watcher unavailable, falling back to polling: {err}");
            None
        }
    };
    // Folders that do not exist yet (no Copilot or VS Code sessions so far) are retried later.
    let mut watched: Vec<PathBuf> = Vec::new();
    let mut last_emit = Instant::now();

    loop {
        if let Some(watcher) = watcher.as_mut() {
            add_watches(watcher, targets, &mut watched);
        }
        let next_poll = last_emit + POLL_INTERVAL;
        let timeout = next_poll.saturating_duration_since(Instant::now());
        match rx.recv_timeout(timeout) {
            Ok(Ok(event)) if is_relevant(&event, targets.session_root.as_deref()) => {
                let deadline = (Instant::now() + DEBOUNCE).max(last_emit + MIN_INTERVAL);
                drain_until(&rx, deadline);
            }
            Ok(Ok(_)) => continue,
            Ok(Err(err)) => {
                eprintln!("File watcher error: {err}");
                continue;
            }
            Err(RecvTimeoutError::Timeout) => {}
            // Unreachable while `tx` is alive; stop rather than spin.
            Err(RecvTimeoutError::Disconnected) => return,
        }
        notify();
        last_emit = Instant::now();
    }
}

fn add_watches(watcher: &mut RecommendedWatcher, targets: &Targets, watched: &mut Vec<PathBuf>) {
    let wanted = targets
        .session_root
        .iter()
        .map(|root| (root, RecursiveMode::Recursive))
        .chain(
            targets
                .storage_dirs
                .iter()
                .map(|dir| (dir, RecursiveMode::NonRecursive)),
        )
        .chain(
            targets
                .chat_dirs
                .iter()
                .map(|dir| (dir, RecursiveMode::Recursive)),
        );
    for (path, mode) in wanted {
        if watched.contains(path) || !path.is_dir() {
            continue;
        }
        match watcher.watch(path, mode) {
            Ok(()) => watched.push(path.clone()),
            Err(err) => eprintln!("Unable to watch {}: {err}", path.display()),
        }
    }
}

fn drain_until(rx: &Receiver<EventResult>, deadline: Instant) {
    while let Some(left) = deadline.checked_duration_since(Instant::now()) {
        if let Err(RecvTimeoutError::Disconnected) = rx.recv_timeout(left) {
            return;
        }
    }
}

fn is_relevant(event: &Event, session_root: Option<&Path>) -> bool {
    if matches!(event.kind, EventKind::Access(_)) {
        return false;
    }
    event.need_rescan()
        || event
            .paths
            .iter()
            .any(|path| is_relevant_path(path, session_root))
}

/// Only files the index reads matter; session folders also hold checkpoints and artifacts.
/// `agent-host.db-shm` is ignored because our own read-only queries touch it.
fn is_relevant_path(path: &Path, session_root: Option<&Path>) -> bool {
    let name = path
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or_default();
    if let Some(relative) = session_root.and_then(|root| path.strip_prefix(root).ok()) {
        return match relative.components().count() {
            1 => true,
            2 => {
                name == "events.jsonl"
                    || name == "workspace.yaml"
                    || (name.starts_with("inuse.") && name.ends_with(".hold"))
            }
            _ => false,
        };
    }
    let parent = path
        .parent()
        .and_then(|parent| parent.file_name())
        .and_then(|name| name.to_str());
    if matches!(parent, Some("chatSessions" | "emptyWindowChatSessions")) {
        return name.ends_with(".json") || name.ends_with(".jsonl");
    }
    // `storage.json` lists the open VS Code windows; chats of open workspaces cannot be deleted.
    name == "agent-host.db" || name == "agent-host.db-wal" || name == "storage.json"
}

#[cfg(test)]
mod tests {
    use super::*;
    use notify::event::{AccessKind, CreateKind, ModifyKind};

    #[test]
    fn filters_relevant_paths() {
        let root = Path::new("/home/me/.copilot/session-state");
        let session = root.join("24c442d3-ada5-4d9a-b945-c448f7d78023");
        let relevant = |path: &Path| is_relevant_path(path, Some(root));

        assert!(relevant(&session));
        assert!(relevant(&session.join("events.jsonl")));
        assert!(relevant(&session.join("workspace.yaml")));
        assert!(relevant(&session.join("inuse.1234.hold")));
        assert!(!relevant(&session.join("plan.md")));
        assert!(!relevant(&session.join("files").join("events.jsonl")));

        let storage = Path::new("/home/me/.config/Code/User/globalStorage");
        assert!(relevant(&storage.join("agent-host.db")));
        assert!(relevant(&storage.join("agent-host.db-wal")));
        assert!(!relevant(&storage.join("agent-host.db-shm")));
        assert!(!relevant(&storage.join("state.vscdb")));
        assert!(relevant(&storage.join("storage.json")));

        let chats = Path::new("/home/me/.config/Code/User/workspaceStorage/abc/chatSessions");
        assert!(relevant(
            &chats.join("24c442d3-ada5-4d9a-b945-c448f7d78023.jsonl")
        ));
        assert!(relevant(
            &chats.join("24c442d3-ada5-4d9a-b945-c448f7d78023.json")
        ));
        assert!(!relevant(&chats.join("notes.txt")));
        assert!(!relevant(&chats.parent().unwrap().join("state.vscdb")));
        let empty_window = storage.join("emptyWindowChatSessions");
        assert!(relevant(
            &empty_window.join("24c442d3-ada5-4d9a-b945-c448f7d78023.json")
        ));
    }

    #[test]
    fn ignores_access_events() {
        let root = Path::new("/sessions");
        let path = root
            .join("24c442d3-ada5-4d9a-b945-c448f7d78023")
            .join("events.jsonl");
        let access = Event::new(EventKind::Access(AccessKind::Any)).add_path(path.clone());
        let modify = Event::new(EventKind::Modify(ModifyKind::Any)).add_path(path);
        let rescan =
            Event::new(EventKind::Create(CreateKind::Any)).set_flag(notify::event::Flag::Rescan);
        assert!(!is_relevant(&access, Some(root)));
        assert!(is_relevant(&modify, Some(root)));
        assert!(is_relevant(&rescan, Some(root)));
    }
}

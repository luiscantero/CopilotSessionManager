//! Line-delimited JSON protocol over stdin/stdout, used by the VS Code extension.
//!
//! Each request is one line `{"id": 1, "method": "search_sessions", "params": {"query": "x"}}`
//! and gets one response line, `{"id": 1, "result": ...}` or `{"id": 1, "error": "..."}`.
//! Methods and parameters match the desktop app's Tauri commands, so the shared frontend in
//! `src/` can call them unchanged. Session changes on disk are reported as
//! `{"event": "sessions-changed"}`. The process exits when stdin is closed.

use super::*;
use serde::Deserialize;
use serde_json::json;
use std::io::{self, Write};

#[derive(Deserialize)]
struct Request {
    #[serde(default)]
    id: Value,
    method: String,
    #[serde(default)]
    params: Value,
}

pub fn run() {
    migrate_legacy_app_dirs();
    let merges_path = app_config_path(project_merges::FILE_NAME);
    let mut index = Index::default();
    if let Some(path) = &merges_path {
        index.merges = project_merges::Merges::load(path);
    }
    spawn_watcher(|| write_line(&json!({ "event": watcher::CHANGED_EVENT })));

    for line in io::stdin().lock().lines() {
        let Ok(line) = line else { break };
        if line.trim().is_empty() {
            continue;
        }
        let response = match serde_json::from_str::<Request>(&line) {
            Ok(request) => match handle(&mut index, merges_path.as_deref(), &request) {
                Ok(result) => json!({ "id": request.id, "result": result }),
                Err(error) => json!({ "id": request.id, "error": error }),
            },
            Err(err) => json!({ "id": null, "error": format!("Invalid request: {err}") }),
        };
        write_line(&response);
    }
}

fn write_line(value: &Value) {
    let line = format!("{value}\n");
    let mut out = io::stdout().lock();
    // The extension closed the pipe: nobody is listening any more.
    if out
        .write_all(line.as_bytes())
        .and_then(|()| out.flush())
        .is_err()
    {
        std::process::exit(0);
    }
}

fn to_value<T: Serialize>(value: T) -> Result<Value, String> {
    serde_json::to_value(value).map_err(|err| err.to_string())
}

fn handle(
    index: &mut Index,
    merges_path: Option<&Path>,
    request: &Request,
) -> Result<Value, String> {
    let param = |name: &str| {
        request
            .params
            .get(name)
            .and_then(Value::as_str)
            .ok_or_else(|| format!("Missing parameter {name}"))
    };
    let merges_path =
        || merges_path.ok_or_else(|| "Unable to locate the config folder".to_string());
    match request.method.as_str() {
        "refresh_sessions" => {
            rebuild(index)?;
            to_value(index.entries.len())
        }
        "search_sessions" => to_value(search(index, param("query")?)),
        "get_session" => to_value(session_detail(index, param("id")?)?),
        "delete_session" => to_value(delete_entry(index, param("id")?)?),
        "list_projects" => to_value(list_project_infos(index)),
        "merge_project" => to_value(merge_projects(
            index,
            merges_path()?,
            param("from")?,
            param("into")?,
            request
                .params
                .get("moveSessions")
                .and_then(Value::as_bool)
                .unwrap_or(false),
        )?),
        "move_merged_project" => {
            to_value(move_merged_project(index, merges_path()?, param("key")?)?)
        }
        "unmerge_project" => to_value(unmerge_project(index, merges_path()?, param("key")?)?),
        other => Err(format!("Unknown method {other}")),
    }
}

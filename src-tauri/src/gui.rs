//! The Tauri desktop app: commands for the frontend in `src/`, window size and file watching.

use super::*;
use std::sync::{Mutex, MutexGuard};
use tauri::{AppHandle, Emitter, Manager, Runtime, State};

#[derive(Default)]
struct AppState {
    index: Arc<Mutex<Index>>,
}

fn lock(index: &Mutex<Index>) -> MutexGuard<'_, Index> {
    index
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
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
    with_index(&state, move |index| session_detail(index, &id)).await
}

#[tauri::command]
async fn delete_session(state: State<'_, AppState>, id: String) -> Result<(), String> {
    with_index(&state, move |index| delete_entry(index, &id)).await
}

fn merges_path<R: Runtime>(app: &AppHandle<R>) -> Result<PathBuf, String> {
    app.path()
        .app_config_dir()
        .map(|dir| dir.join(project_merges::FILE_NAME))
        .map_err(|err| format!("Unable to locate the config folder: {err}"))
}

#[tauri::command]
async fn list_projects(state: State<'_, AppState>) -> Result<Vec<ProjectInfo>, String> {
    with_index(&state, |index| Ok(list_project_infos(index))).await
}

/// Groups the sessions of project `from` under project `into` (both current group keys).
/// Returns the key of the resulting group.
#[tauri::command]
async fn merge_project(
    app: AppHandle,
    state: State<'_, AppState>,
    from: String,
    into: String,
) -> Result<String, String> {
    let path = merges_path(&app)?;
    with_index(&state, move |index| {
        merge_projects(index, &path, &from, &into)
    })
    .await
}

/// Undoes the merge of project `key` (it becomes its own group again).
#[tauri::command]
async fn unmerge_project(
    app: AppHandle,
    state: State<'_, AppState>,
    key: String,
) -> Result<(), String> {
    let path = merges_path(&app)?;
    with_index(&state, move |index| {
        super::unmerge_project(index, &path, &key)
    })
    .await
}

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    tauri::Builder::default()
        .manage(AppState::default())
        .manage(window_state::WindowSizeState::default())
        .setup(|app| {
            if let Ok(path) = merges_path(app.handle()) {
                lock(&app.state::<AppState>().index).merges = load_merges(&path);
            }
            if let Some(window) = app.get_webview_window("main") {
                if let Err(err) = window_state::restore(&window) {
                    eprintln!("Unable to restore window size: {err}");
                    window.show()?;
                }
            }
            let handle = app.handle().clone();
            spawn_watcher(move || {
                if let Err(err) = handle.emit(watcher::CHANGED_EVENT, ()) {
                    eprintln!("Unable to notify session changes: {err}");
                }
            });
            Ok(())
        })
        .on_window_event(window_state::handle_event)
        .invoke_handler(tauri::generate_handler![
            refresh_sessions,
            search_sessions,
            get_session,
            delete_session,
            list_projects,
            merge_project,
            unmerge_project
        ])
        .run(tauri::generate_context!())
        .expect("error while running Session Manager for GitHub Copilot");
}

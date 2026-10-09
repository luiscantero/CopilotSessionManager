use serde::{Deserialize, Serialize};
use std::fs;
use std::path::PathBuf;
use std::sync::Mutex;
use tauri::{
    AppHandle, LogicalSize, Manager, PhysicalPosition, PhysicalSize, Runtime, WebviewWindow,
    Window, WindowEvent,
};

const STATE_FILE: &str = "window-state.json";
const MIN_WIDTH: f64 = 560.0;
const MIN_HEIGHT: f64 = 600.0;

#[derive(Serialize, Deserialize, Clone, Copy, Debug, PartialEq)]
struct SavedWindow {
    width: f64,
    height: f64,
    #[serde(default)]
    maximized: bool,
}

impl SavedWindow {
    fn is_valid(&self) -> bool {
        self.width.is_finite() && self.height.is_finite() && self.width > 0.0 && self.height > 0.0
    }
}

/// Last known non-maximized, non-minimized logical size of the main window.
#[derive(Default)]
pub struct WindowSizeState(Mutex<Option<LogicalSize<f64>>>);

fn state_path<R: Runtime>(app: &AppHandle<R>) -> Option<PathBuf> {
    app.path()
        .app_config_dir()
        .ok()
        .map(|dir| dir.join(STATE_FILE))
}

fn load<R: Runtime>(app: &AppHandle<R>) -> Option<SavedWindow> {
    let text = fs::read_to_string(state_path(app)?).ok()?;
    serde_json::from_str::<SavedWindow>(&text)
        .ok()
        .filter(SavedWindow::is_valid)
}

fn save<R: Runtime>(app: &AppHandle<R>, saved: SavedWindow) {
    let Some(path) = state_path(app) else { return };
    if let Some(dir) = path.parent() {
        let _ = fs::create_dir_all(dir);
    }
    if let Ok(text) = serde_json::to_string_pretty(&saved) {
        let _ = fs::write(path, text);
    }
}

/// Clamps `desired` to `[min, max]`, letting `max` win when the screen is smaller than `min`.
fn clamp_dimension(desired: u32, min: u32, max: u32) -> (u32, u32) {
    let min = min.min(max);
    (desired.clamp(min, max), min)
}

/// Shrinks the window so it fits within the work area of its monitor and centers it there.
fn fit_to_monitor<R: Runtime>(window: &WebviewWindow<R>) -> tauri::Result<()> {
    let monitor = match window.current_monitor()? {
        Some(monitor) => monitor,
        None => match window.primary_monitor()? {
            Some(monitor) => monitor,
            None => return Ok(()),
        },
    };
    let scale = monitor.scale_factor();
    let area = monitor.work_area();
    let inner = window.inner_size()?;
    let outer = window.outer_size()?;
    let frame_w = outer.width.saturating_sub(inner.width);
    let frame_h = outer.height.saturating_sub(inner.height);
    let max_w = area.size.width.saturating_sub(frame_w).max(1);
    let max_h = area.size.height.saturating_sub(frame_h).max(1);

    let (width, min_w) = clamp_dimension(inner.width, (MIN_WIDTH * scale).round() as u32, max_w);
    let (height, min_h) =
        clamp_dimension(inner.height, (MIN_HEIGHT * scale).round() as u32, max_h);

    window.set_min_size(Some(PhysicalSize::new(min_w, min_h)))?;
    if (width, height) != (inner.width, inner.height) {
        window.set_size(PhysicalSize::new(width, height))?;
    }

    let x = area.position.x + (area.size.width.saturating_sub(width + frame_w) / 2) as i32;
    let y = area.position.y + (area.size.height.saturating_sub(height + frame_h) / 2) as i32;
    window.set_position(PhysicalPosition::new(x, y))
}

/// Restores the saved window size, fits it to the current screen and shows the window.
pub fn restore<R: Runtime>(window: &WebviewWindow<R>) -> tauri::Result<()> {
    let saved = load(window.app_handle());
    if let Some(saved) = saved {
        window.set_size(LogicalSize::new(saved.width, saved.height))?;
    }
    fit_to_monitor(window)?;

    let scale = window.scale_factor()?;
    let size = window.inner_size()?.to_logical::<f64>(scale);
    *window.state::<WindowSizeState>().0.lock().unwrap() = Some(size);

    if saved.is_some_and(|saved| saved.maximized) {
        window.maximize()?;
    }
    window.show()?;
    window.set_focus()
}

/// Tracks the window's normal size and persists it when the window closes.
pub fn handle_event<R: Runtime>(window: &Window<R>, event: &WindowEvent) {
    match event {
        WindowEvent::Resized(size) if size.width > 0 && size.height > 0 => {
            let maximized = window.is_maximized().unwrap_or(false);
            let minimized = window.is_minimized().unwrap_or(false);
            if maximized || minimized {
                return;
            }
            if let Ok(scale) = window.scale_factor() {
                *window.state::<WindowSizeState>().0.lock().unwrap() =
                    Some(size.to_logical::<f64>(scale));
            }
        }
        WindowEvent::CloseRequested { .. } => {
            let size = *window.state::<WindowSizeState>().0.lock().unwrap();
            if let Some(size) = size {
                save(
                    window.app_handle(),
                    SavedWindow {
                        width: size.width,
                        height: size.height,
                        maximized: window.is_maximized().unwrap_or(false),
                    },
                );
            }
        }
        _ => {}
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn clamps_to_screen_even_below_minimum() {
        assert_eq!(clamp_dimension(1000, 600, 800), (800, 600));
        assert_eq!(clamp_dimension(700, 600, 800), (700, 600));
        assert_eq!(clamp_dimension(400, 600, 800), (600, 600));
        assert_eq!(clamp_dimension(1000, 600, 500), (500, 500));
    }

    #[test]
    fn rejects_invalid_saved_sizes() {
        let parse = |text: &str| {
            serde_json::from_str::<SavedWindow>(text)
                .ok()
                .filter(SavedWindow::is_valid)
        };
        assert_eq!(
            parse(r#"{"width":800,"height":900}"#),
            Some(SavedWindow {
                width: 800.0,
                height: 900.0,
                maximized: false
            })
        );
        assert_eq!(parse(r#"{"width":0,"height":900}"#), None);
        assert_eq!(parse("not json"), None);
    }
}

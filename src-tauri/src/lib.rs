mod clips;
mod obs;

use obs::{config::Settings, ws::DISCONNECTED, ObsManager};
use serde::Serialize;
use serde_json::json;
use std::{path::PathBuf, sync::Arc};
use tauri::{
    menu::{Menu, MenuItem},
    tray::{MouseButton, MouseButtonState, TrayIconBuilder, TrayIconEvent},
    AppHandle, Emitter, Manager, RunEvent, WindowEvent,
};
use tokio::sync::Mutex;

#[derive(Serialize, Clone, Default)]
pub struct Status {
    /// "starting" | "ready" | "stopped" | "error: …"
    pub obs: String,
    pub buffer: bool,
    pub game: Option<String>,
}

pub struct AppState {
    pub obs: Arc<ObsManager>,
    pub clips_dir: PathBuf,
    pub status: Mutex<Status>,
}

async fn set_status(app: &AppHandle, f: impl FnOnce(&mut Status)) {
    let st = app.state::<AppState>();
    let mut s = st.status.lock().await;
    f(&mut s);
    let _ = app.emit("status", s.clone());
}

fn show_main(app: &AppHandle) {
    if let Some(w) = app.get_webview_window("main") {
        let _ = w.show();
        let _ = w.unminimize();
        let _ = w.set_focus();
    }
}

async fn save_clip_inner(app: &AppHandle) -> Result<(), String> {
    let st = app.state::<AppState>();
    if !st.status.lock().await.buffer {
        return Err("The replay buffer is not running".into());
    }
    st.obs.client.call("SaveReplayBuffer", json!({})).await.map_err(|e| e.to_string())?;
    Ok(())
}

#[tauri::command]
async fn get_status(state: tauri::State<'_, AppState>) -> Result<Status, String> {
    Ok(state.status.lock().await.clone())
}

#[tauri::command]
async fn start_buffer(state: tauri::State<'_, AppState>) -> Result<(), String> {
    state.obs.client.call("StartReplayBuffer", json!({})).await.map_err(|e| e.to_string())?;
    Ok(())
}

#[tauri::command]
async fn stop_buffer(state: tauri::State<'_, AppState>) -> Result<(), String> {
    state.obs.client.call("StopReplayBuffer", json!({})).await.map_err(|e| e.to_string())?;
    Ok(())
}

#[tauri::command]
async fn save_clip(app: AppHandle) -> Result<(), String> {
    save_clip_inner(&app).await
}

#[tauri::command]
fn list_clips(state: tauri::State<'_, AppState>) -> Vec<clips::ClipInfo> {
    clips::list(&state.clips_dir)
}

/// Follow OBS events: buffer on/off, and adopt saved replays into the clips folder.
async fn watch_events(app: AppHandle) {
    let mut rx = app.state::<AppState>().obs.client.subscribe();
    loop {
        let ev = match rx.recv().await {
            Ok(ev) => ev,
            Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => continue,
            Err(_) => break,
        };
        match ev["eventType"].as_str().unwrap_or_default() {
            "ReplayBufferStateChanged" => {
                let active = ev["eventData"]["outputActive"].as_bool().unwrap_or(false);
                set_status(&app, |s| s.buffer = active).await;
            }
            "ReplayBufferSaved" => {
                let Some(src) = ev["eventData"]["savedReplayPath"].as_str() else { continue };
                let st = app.state::<AppState>();
                let game = st.status.lock().await.game.clone();
                match clips::adopt(src.as_ref(), &st.clips_dir, game.as_deref()) {
                    Ok(p) => {
                        let _ = app.emit("clip-saved", p.to_string_lossy());
                    }
                    Err(e) => {
                        let _ = app.emit("error", format!("Could not store clip: {e:#}"));
                    }
                }
            }
            DISCONNECTED => set_status(&app, |s| s.buffer = false).await,
            _ => {}
        }
    }
}

fn dir_from_env(var: &str) -> Option<PathBuf> {
    std::env::var_os(var).filter(|v| !v.is_empty()).map(PathBuf::from)
}

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    let start_hidden = std::env::args().any(|a| a == "--hidden");

    #[allow(unused_mut)]
    let mut builder = tauri::Builder::default();

    #[cfg(windows)]
    {
        use tauri_plugin_global_shortcut::ShortcutState;
        builder = builder.plugin(
            tauri_plugin_global_shortcut::Builder::new()
                .with_handler(|app, _shortcut, ev| {
                    if ev.state() == ShortcutState::Pressed {
                        let app = app.clone();
                        tauri::async_runtime::spawn(async move {
                            if let Err(e) = save_clip_inner(&app).await {
                                let _ = app.emit("error", e);
                            }
                        });
                    }
                })
                .build(),
        );
    }

    let app = builder
        .invoke_handler(tauri::generate_handler![
            get_status, start_buffer, stop_buffer, save_clip, list_clips
        ])
        .setup(move |app| {
            let handle = app.handle().clone();
            let data_dir = match dir_from_env("CLIPPER_DATA_DIR") {
                Some(d) => d,
                None => app.path().app_data_dir()?,
            };
            let clips_dir = match dir_from_env("CLIPPER_CLIPS_DIR") {
                Some(d) => d,
                None => app.path().video_dir()?.join("Clipper"),
            };
            std::fs::create_dir_all(&data_dir)?;
            std::fs::create_dir_all(&clips_dir)?;

            let obs = ObsManager::new(data_dir, Settings::default())?;
            app.manage(AppState { obs: obs.clone(), clips_dir, status: Mutex::default() });

            // Tray
            let open = MenuItem::with_id(app, "open", "Open Clipper", true, None::<&str>)?;
            let save = MenuItem::with_id(app, "save", "Save clip", true, None::<&str>)?;
            let quit = MenuItem::with_id(app, "quit", "Quit", true, None::<&str>)?;
            let menu = Menu::with_items(app, &[&open, &save, &quit])?;
            TrayIconBuilder::with_id("tray")
                .icon(app.default_window_icon().unwrap().clone())
                .tooltip("Clipper")
                .menu(&menu)
                .show_menu_on_left_click(false)
                .on_menu_event(|app, ev| match ev.id().as_ref() {
                    "open" => show_main(app),
                    "save" => {
                        let app = app.clone();
                        tauri::async_runtime::spawn(async move {
                            if let Err(e) = save_clip_inner(&app).await {
                                let _ = app.emit("error", e);
                            }
                        });
                    }
                    "quit" => app.exit(0),
                    _ => {}
                })
                .on_tray_icon_event(|tray, ev| {
                    if let TrayIconEvent::Click {
                        button: MouseButton::Left, button_state: MouseButtonState::Up, ..
                    } = ev
                    {
                        show_main(tray.app_handle());
                    }
                })
                .build(app)?;

            #[cfg(windows)]
            {
                use tauri_plugin_global_shortcut::{Code, GlobalShortcutExt, Shortcut};
                if let Err(e) = app.global_shortcut().register(Shortcut::new(None, Code::F8)) {
                    eprintln!("could not register F8: {e}");
                }
            }

            // OBS supervisor + event follower
            let h = handle.clone();
            tauri::async_runtime::spawn(obs.run(move |state| {
                let h = h.clone();
                tauri::async_runtime::spawn(async move {
                    set_status(&h, |s| s.obs = state).await;
                });
            }));
            tauri::async_runtime::spawn(watch_events(handle.clone()));

            if !start_hidden {
                show_main(&handle);
            }
            Ok(())
        })
        .on_window_event(|window, ev| {
            // Closing the window keeps Clipper running in the tray.
            if let WindowEvent::CloseRequested { api, .. } = ev {
                api.prevent_close();
                let _ = window.hide();
            }
        })
        .build(tauri::generate_context!())
        .expect("error while building Clipper");

    app.run(|app, ev| {
        if let RunEvent::Exit = ev {
            app.state::<AppState>().obs.shutdown();
        }
    });
}

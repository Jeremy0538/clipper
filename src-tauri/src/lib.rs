mod autobuffer;
mod clips;
mod games;
mod hotkey;
mod log;
mod notify;
mod obs;
mod settings;

use obs::{config::Settings, ws::DISCONNECTED, ObsManager};
use serde::Serialize;
use serde_json::json;
use settings::{AudioSettings, UserSettings, MAX_BUFFER_SECS, MIN_BUFFER_SECS};
use std::{
    path::PathBuf,
    sync::{
        atomic::{AtomicBool, AtomicU64, Ordering},
        Arc,
    },
};
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
    /// Bumped every time OBS (re)starts and is ready; inputs must be retargeted.
    pub obs_session: AtomicU64,
    /// The user stopped the buffer by hand during this game: don't auto-restart it.
    pub user_stopped: AtomicBool,
    pub user: Mutex<UserSettings>,
    pub settings_file: PathBuf,
}

pub(crate) async fn set_status(app: &AppHandle, f: impl FnOnce(&mut Status)) {
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

/// Log it, show it in the window and as a toast.
pub(crate) fn report_error(app: &AppHandle, msg: impl Into<String>) {
    let msg = msg.into();
    clog!("ERROR {msg}");
    let _ = app.emit("error", msg.clone());
    notify::toast(app, false, "Clipper problem", msg);
}

pub(crate) async fn save_clip_inner(app: &AppHandle) -> Result<(), String> {
    clog!("save requested");
    let st = app.state::<AppState>();
    let r = async {
        if !st.status.lock().await.buffer {
            return Err("The replay buffer is not running".to_string());
        }
        st.obs.client.call("SaveReplayBuffer", json!({})).await.map_err(|e| e.to_string())?;
        Ok(())
    }
    .await;
    if let Err(e) = &r {
        report_error(app, format!("Could not save clip: {e}"));
    }
    r
}

#[tauri::command]
async fn get_status(state: tauri::State<'_, AppState>) -> Result<Status, String> {
    Ok(state.status.lock().await.clone())
}

#[tauri::command]
async fn start_buffer(state: tauri::State<'_, AppState>) -> Result<(), String> {
    state.user_stopped.store(false, Ordering::SeqCst);
    state.obs.client.call("StartReplayBuffer", json!({})).await.map_err(|e| e.to_string())?;
    Ok(())
}

#[tauri::command]
async fn stop_buffer(state: tauri::State<'_, AppState>) -> Result<(), String> {
    state.user_stopped.store(true, Ordering::SeqCst);
    state.obs.client.call("StopReplayBuffer", json!({})).await.map_err(|e| e.to_string())?;
    Ok(())
}

#[tauri::command]
async fn save_clip(app: AppHandle) -> Result<(), String> {
    save_clip_inner(&app).await
}

#[derive(Serialize)]
struct SettingsView {
    hotkey: hotkey::Hotkey,
    hotkey_label: String,
    buffer_secs: u32,
    max_buffer_secs: u32,
    sound: bool,
    toast: bool,
    audio: AudioSettings,
}

#[tauri::command]
async fn get_settings(state: tauri::State<'_, AppState>) -> Result<SettingsView, String> {
    let u = state.user.lock().await;
    Ok(SettingsView {
        hotkey: u.hotkey.clone(),
        hotkey_label: u.hotkey.label(),
        buffer_secs: u.buffer_secs,
        max_buffer_secs: MAX_BUFFER_SECS,
        sound: u.sound,
        toast: u.toast,
        audio: u.audio.clone(),
    })
}

/// Returns the new key's label.
#[tauri::command]
async fn set_hotkey(state: tauri::State<'_, AppState>, hotkey: hotkey::Hotkey) -> Result<String, String> {
    hotkey::apply(&hotkey)?;
    clog!("clip key set to {}", hotkey.label());
    let mut u = state.user.lock().await;
    u.hotkey = hotkey;
    u.save(&state.settings_file).map_err(|e| e.to_string())?;
    Ok(u.hotkey.label())
}

/// Changing the length restarts a running buffer (OBS reads it on start).
#[tauri::command]
async fn set_buffer(state: tauri::State<'_, AppState>, secs: u32) -> Result<u32, String> {
    let secs = secs.clamp(MIN_BUFFER_SECS, MAX_BUFFER_SECS);
    {
        let mut u = state.user.lock().await;
        u.buffer_secs = secs;
        u.save(&state.settings_file).map_err(|e| e.to_string())?;
    }
    state.obs.settings.lock().unwrap().buffer_secs = secs;
    clog!("buffer length set to {secs} s");
    let obs = &state.obs.client;
    if !obs.is_connected().await {
        return Ok(secs); // applied when OBS starts
    }
    let err = |e: anyhow::Error| e.to_string();
    let was_running = obs.call("GetReplayBufferStatus", json!({})).await.map_err(err)?["outputActive"]
        .as_bool()
        .unwrap_or(false);
    if was_running {
        obs.call("StopReplayBuffer", json!({})).await.map_err(err)?;
        for _ in 0..50 {
            tokio::time::sleep(std::time::Duration::from_millis(100)).await;
            let st = obs.call("GetReplayBufferStatus", json!({})).await.map_err(err)?;
            if st["outputActive"] == false {
                break;
            }
        }
    }
    obs.call("SetProfileParameter", json!({
        "parameterCategory": "AdvOut", "parameterName": "RecRBTime", "parameterValue": secs.to_string(),
    }))
    .await
    .map_err(err)?;
    if was_running {
        obs.call("StartReplayBuffer", json!({})).await.map_err(err)?;
    }
    Ok(secs)
}

/// sound | toast
#[tauri::command]
async fn set_option(state: tauri::State<'_, AppState>, name: String, value: bool) -> Result<(), String> {
    let mut u = state.user.lock().await;
    match name.as_str() {
        "sound" => u.sound = value,
        "toast" => u.toast = value,
        _ => return Err(format!("unknown option {name}")),
    }
    clog!("option {name} = {value}");
    u.save(&state.settings_file).map_err(|e| e.to_string())
}

#[tauri::command]
async fn set_audio(state: tauri::State<'_, AppState>, audio: AudioSettings) -> Result<(), String> {
    {
        let mut u = state.user.lock().await;
        u.audio = audio.clone();
        u.save(&state.settings_file).map_err(|e| e.to_string())?;
    }
    clog!("audio settings: {audio:?}");
    state.obs.settings.lock().unwrap().audio = audio;
    if state.obs.client.is_connected().await {
        state.obs.apply_audio().await.map_err(|e| format!("{e:#}"))?;
        // Make the game detector point Game Audio at the game again.
        state.obs_session.fetch_add(1, Ordering::SeqCst);
    }
    Ok(())
}

#[tauri::command]
async fn list_audio_devices(state: tauri::State<'_, AppState>, output: bool)
    -> Result<Vec<obs::setup::Device>, String>
{
    obs::setup::list_devices(&state.obs.client, output).await.map_err(|e| format!("{e:#}"))
}

#[tauri::command]
fn open_log_folder() -> Result<(), String> {
    let dir = log::path().and_then(|p| p.parent()).ok_or("no log folder")?;
    let opener = if cfg!(windows) { "explorer" } else { "xdg-open" };
    std::process::Command::new(opener).arg(dir).spawn().map_err(|e| e.to_string())?;
    Ok(())
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
                hotkey::set_armed(active);
                let state = ev["eventData"]["outputState"].as_str().unwrap_or("?");
                clog!("buffer {}", state.trim_start_matches("OBS_WEBSOCKET_OUTPUT_").to_lowercase());
                if state == "OBS_WEBSOCKET_OUTPUT_STOPPED" {
                    notify::info(&app, "Buffer stopped", "");
                }
                set_status(&app, |s| s.buffer = active).await;
            }
            "ReplayBufferSaved" => {
                let Some(src) = ev["eventData"]["savedReplayPath"].as_str() else { continue };
                let st = app.state::<AppState>();
                let game = st.status.lock().await.game.clone();
                clog!("OBS wrote {src}");
                match clips::adopt(src.as_ref(), &st.clips_dir, game.as_deref()) {
                    Ok(p) => {
                        clog!("clip saved: {}", p.display());
                        let name = p.file_stem().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
                        notify::toast(&app, true, "Clip saved", name);
                        let _ = app.emit("clip-saved", p.to_string_lossy());
                    }
                    Err(e) => report_error(&app, format!("Could not store clip: {e:#}")),
                }
            }
            DISCONNECTED => {
                clog!("OBS websocket disconnected");
                hotkey::set_armed(false);
                set_status(&app, |s| s.buffer = false).await
            }
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

    let app = tauri::Builder::default()
        .invoke_handler(tauri::generate_handler![
            get_status, start_buffer, stop_buffer, save_clip, list_clips, get_settings, set_hotkey,
            set_buffer, set_option, open_log_folder, set_audio,
            list_audio_devices
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
            log::init(data_dir.join("clipper.log"));
            clog!("data {} | clips {}", data_dir.display(), clips_dir.display());

            let settings_file = data_dir.join("settings.json");
            let user = UserSettings::load(&settings_file);
            hotkey::apply(&user.hotkey)?;
            hotkey::start(handle.clone());
            #[allow(unused_mut)]
            let mut settings = Settings {
                buffer_secs: user.buffer_secs,
                audio: user.audio.clone(),
                ..Settings::default()
            };
            // Canvas = primary monitor (games run there); even sizes for the encoder.
            #[cfg(windows)]
            if let Ok(Some(m)) = app.primary_monitor() {
                settings.width = m.size().width & !1;
                settings.height = m.size().height & !1;
            }
            let obs = ObsManager::new(data_dir, settings)?;
            app.manage(AppState {
                obs: obs.clone(),
                clips_dir,
                status: Mutex::default(),
                obs_session: AtomicU64::new(0),
                user_stopped: AtomicBool::new(false),
                user: Mutex::new(user),
                settings_file,
            });

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

            // OBS supervisor + event follower
            let h = handle.clone();
            tauri::async_runtime::spawn(obs.run(move |state| {
                let h = h.clone();
                // Download progress would spam the log every percent.
                if !state.starts_with("downloading") || state.ends_with(" 0%") || state.ends_with("100%") {
                    clog!("OBS: {state}");
                }
                if state.starts_with("error") {
                    notify::toast(&h, false, "OBS problem", state.clone());
                }
                if state == "ready" {
                    h.state::<AppState>().obs_session.fetch_add(1, Ordering::SeqCst);
                }
                tauri::async_runtime::spawn(async move {
                    set_status(&h, |s| s.obs = state).await;
                });
            }));
            tauri::async_runtime::spawn(watch_events(handle.clone()));
            tauri::async_runtime::spawn(autobuffer::run(handle.clone()));
            notify::setup(&handle)?;

            if !start_hidden {
                show_main(&handle);
            }
            // Development: open a tab directly (screenshots without clicking).
            #[cfg(debug_assertions)]
            if let (Ok(view), Some(w)) = (std::env::var("CLIPPER_DEV_VIEW"), handle.get_webview_window("main")) {
                let js = format!("setTimeout(() => document.querySelector('[data-view={view}]')?.click(), 800)");
                let _ = w.eval(&js);
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

mod autobuffer;
mod autostart;
mod clips;
mod editor;
mod tools;
mod games;
mod gpu;
mod hotkey;
mod installer;
mod log;
mod modes;
mod screen;
mod notify;
mod report;
mod changelog;
mod obs;
mod settings;
mod updater;

/// CREATE_NO_WINDOW: child processes never flash a console window.
#[cfg_attr(not(windows), allow(dead_code))]
pub(crate) const NO_WINDOW: u32 = 0x0800_0000;

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
    /// A normal recording is running.
    pub recording: bool,
    /// Screen mode (monitor capture) instead of game capture.
    pub screen: bool,
}

pub struct AppState {
    pub obs: Arc<ObsManager>,
    /// Where clips live; the user can move it (Settings → Clips folder).
    clips_dir: std::sync::RwLock<PathBuf>,
    /// Clips are being moved to another folder.
    moving: AtomicBool,
    /// Earlier clips folders (and Videos\Clipper) whose clips still show.
    other_clip_dirs: std::sync::RwLock<Vec<PathBuf>>,
    /// Videos\Clipper: used when no other folder is chosen.
    default_clips_dir: PathBuf,
    pub status: Mutex<Status>,
    /// Bumped every time OBS (re)starts and is ready; inputs must be retargeted.
    pub obs_session: AtomicU64,
    /// The user stopped the buffer by hand during this game: don't auto-restart it.
    pub user_stopped: AtomicBool,
    pub user: Mutex<UserSettings>,
    pub settings_file: PathBuf,
    /// The clip open in the editor.
    pub editor: Mutex<Option<editor::ClipInfo>>,
    /// The settings page is open: forward audio levels to it.
    pub meters: AtomicBool,
    pub screen: AtomicBool,
    /// The last game seen: clips saved just after it closed still go to its folder.
    pub last_game: std::sync::Mutex<Option<String>>,
    /// Folder for the recording that is running (decided when it starts).
    pub rec_folder: std::sync::Mutex<Option<String>>,
    /// A setting that needs an OBS restart changed while clipping/recording:
    /// restart as soon as both have stopped.
    pub restart_pending: AtomicBool,
    /// set_buffer is stopping/starting the buffer: the game detector waits.
    pub buffer_changing: AtomicBool,
    /// Bumped by every editor_open: only the newest one sets `editor`.
    editor_seq: AtomicU64,
}

impl AppState {
    /// Where new clips go.
    pub fn clips_dir(&self) -> PathBuf {
        self.clips_dir.read().unwrap().clone()
    }
    /// Every folder whose clips the library shows: the current one first,
    /// then earlier Clipper folders that still exist and don't overlap it.
    pub fn clip_dirs(&self) -> Vec<PathBuf> {
        let current = self.clips_dir();
        let mut seen = vec![canon(&current)];
        let mut out = vec![current];
        let mut others = vec![self.default_clips_dir.clone()];
        others.extend(self.other_clip_dirs.read().unwrap().iter().cloned());
        for d in others {
            if !d.is_dir() || !clips::is_ours(&d) {
                continue;
            }
            let c = canon(&d);
            // Inside each other would list the same clips twice.
            if seen.iter().any(|s| c.starts_with(s) || s.starts_with(&c)) {
                continue;
            }
            seen.push(c);
            out.push(d);
        }
        out
    }
}

pub(crate) async fn set_status(app: &AppHandle, f: impl FnOnce(&mut Status)) {
    let st = app.state::<AppState>();
    let s = {
        let mut s = st.status.lock().await;
        f(&mut s);
        s.clone()
    };
    let _ = app.emit("status", s.clone());
    update_tray(app, &s);
}

/// Green tray icon while the buffer runs, tooltip with the state.
fn update_tray(app: &AppHandle, s: &Status) {
    static ICON_IDLE: &[u8] = include_bytes!("../icons/32x32.png");
    static ICON_REC: &[u8] = include_bytes!("../icons/tray-recording.png");
    type Icon = Option<tauri::image::Image<'static>>;
    static ICONS: std::sync::OnceLock<(Icon, Icon)> = std::sync::OnceLock::new();
    // Last tooltip and icon (true = recording): only touch the tray on a change.
    static LAST: std::sync::Mutex<Option<(String, bool)>> = std::sync::Mutex::new(None);
    let Some(tray) = app.tray_by_id("tray") else { return };
    let tip = match (&s.game, s.buffer, s.recording) {
        (_, _, true) => "Clipper: recording".to_string(),
        (Some(g), true, _) => format!("Clipper: clipping {g}"),
        (None, true, _) => "Clipper: clipping your screen".to_string(),
        _ if s.obs != "ready" => format!("Clipper: OBS {}", s.obs),
        _ => "Clipper: waiting for a game".to_string(),
    };
    let rec = s.buffer || s.recording;
    let mut last = LAST.lock().unwrap();
    if last.as_ref().is_some_and(|(t, r)| *t == tip && *r == rec) {
        return;
    }
    let _ = tray.set_tooltip(Some(&tip));
    if last.as_ref().map(|(_, r)| *r) != Some(rec) {
        let (idle, recording) = ICONS.get_or_init(|| {
            (tauri::image::Image::from_bytes(ICON_IDLE).ok(), tauri::image::Image::from_bytes(ICON_REC).ok())
        });
        if let Some(img) = if rec { recording } else { idle } {
            let _ = tray.set_icon(Some(img.clone()));
        }
    }
    *last = Some((tip, rec));
}

/// Scale the whole page with the window: the layout is made for 1100×720, so a
/// maximised window (e.g. 1920×1080) gets proportionally bigger text and
/// buttons instead of the same small ones with lots of empty space.
fn fit_zoom(app: &AppHandle) {
    let Some(w) = app.get_webview_window("main") else { return };
    let (Ok(size), Ok(scale)) = (w.inner_size(), w.scale_factor()) else { return };
    let (lw, lh) = (size.width as f64 / scale, size.height as f64 / scale);
    if lw < 200.0 || lh < 200.0 {
        return; // minimised
    }
    let z = ((lw / 1100.0).min(lh / 720.0).clamp(1.0, 1.6) * 20.0).round() / 20.0;
    static LAST: std::sync::Mutex<f64> = std::sync::Mutex::new(0.0);
    let mut last = LAST.lock().unwrap();
    if (*last - z).abs() > 0.001 && w.set_zoom(z).is_ok() {
        *last = z;
    }
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
    // A second press within 2 s would save (almost) the same clip twice.
    static LAST: std::sync::Mutex<Option<std::time::Instant>> = std::sync::Mutex::new(None);
    {
        let mut last = LAST.lock().unwrap();
        if last.is_some_and(|t| t.elapsed() < std::time::Duration::from_secs(2)) {
            clog!("save ignored (pressed again within 2 s)");
            return Ok(());
        }
        *last = Some(std::time::Instant::now());
    }
    clog!("save requested");
    let st = app.state::<AppState>();
    let r = async {
        if !st.status.lock().await.buffer {
            return Err("Clipping is off: start a game or press Start clipping".to_string());
        }
        st.obs.client.call("SaveReplayBuffer", json!({})).await.map_err(|e| e.to_string())?;
        Ok(())
    }
    .await;
    if let Err(e) = &r {
        // A failed save must not block the next press for 2 s.
        *LAST.lock().unwrap() = None;
        report_error(app, format!("Could not save clip: {e}"));
    }
    r
}

/// Screenshot of what is captured: the game, or the chosen screen(s).
pub(crate) async fn take_screenshot(app: &AppHandle) -> Result<(), String> {
    let st = app.state::<AppState>();
    let game = st.status.lock().await.game.clone();
    let mut entered = false;
    if game.is_none() && !st.screen.load(Ordering::SeqCst) {
        modes::enter_screen(app).await?;
        entered = true;
        tokio::time::sleep(std::time::Duration::from_millis(600)).await; // let a frame render
    }
    let folder = game.unwrap_or_else(|| "Screenshots".into());
    let dir = st.clips_dir().join(clips::sanitize(&folder));
    let _ = std::fs::create_dir_all(&dir);
    let file = dir.join(format!("{} {}.png", clips::sanitize(&folder), chrono::Local::now().format("%Y-%m-%d %H-%M-%S")));
    let save = || async {
        let scene = st.obs.client.call("GetCurrentProgramScene", json!({})).await?;
        st.obs.client.call("SaveSourceScreenshot", json!({
            "sourceName": scene["currentProgramSceneName"], "imageFormat": "png",
            "imageFilePath": file.to_string_lossy(),
        })).await
    };
    let mut r = save().await;
    if r.is_err() && !dir.is_dir() {
        // The game folder was removed as empty (last clip deleted) in the meantime.
        let _ = std::fs::create_dir_all(&dir);
        r = save().await;
    }
    if entered {
        let _ = modes::leave_screen(app).await;
    }
    r.map_err(|e| e.to_string())?;
    clog!("screenshot saved: {}", file.display());
    notify::toast(app, true, "Screenshot saved", folder);
    let _ = app.emit("clip-saved", file.to_string_lossy());
    Ok(())
}

/// After every new clip: keep the clips folder under the storage limit.
async fn after_new_file(app: &AppHandle) {
    let st = app.state::<AppState>();
    let limit = st.user.lock().await.storage_limit_gb;
    if limit <= 0.0 || st.moving.load(Ordering::SeqCst) {
        return;
    }
    let (dirs, favs) = (st.clip_dirs(), clips::Favorites::new(&st.obs.data_dir).load());
    let n = tauri::async_runtime::spawn_blocking(move || clips::enforce_limit(&dirs, limit, &favs)).await.unwrap_or(0);
    if n > 0 {
        notify::info(app, "Storage limit", format!("Deleted {n} old clip(s)"));
    }
}

#[tauri::command]
async fn screenshot(app: AppHandle) -> Result<(), String> {
    take_screenshot(&app).await
}

// Runs off the main thread: file work (a folder lock held during a big move,
// many clips on a slow drive) must not freeze the window.
#[tauri::command(async)]
fn rename_clip(state: tauri::State<'_, AppState>, path: String, name: String) -> Result<String, String> {
    let p = clips::checked_in(&state.clip_dirs(), &path)?;
    if name.trim().is_empty() {
        return Err("Give the clip a name".into());
    }
    let old_key = clips::fav_key(&p.to_string_lossy());
    let new = clips::rename(&p, &name)?;
    let favs = clips::Favorites::new(&state.obs.data_dir);
    favs.moved(&old_key, &new.to_string_lossy());
    clog!("renamed {} → {}", p.display(), new.display());
    Ok(new.to_string_lossy().into_owned())
}

// Runs off the main thread: file work (a folder lock held during a big move,
// many clips on a slow drive) must not freeze the window.
#[tauri::command(async)]
fn set_favorite(state: tauri::State<'_, AppState>, path: String, on: bool) -> Result<(), String> {
    let p = clips::checked_in(&state.clip_dirs(), &path)?;
    clips::Favorites::new(&state.obs.data_dir).set(&p.to_string_lossy(), on)
}

#[tauri::command]
async fn set_storage_limit(state: tauri::State<'_, AppState>, gb: f64) -> Result<(), String> {
    let mut u = state.user.lock().await;
    u.storage_limit_gb = gb.max(0.0);
    clog!("storage limit: {} GB", u.storage_limit_gb);
    u.save(&state.settings_file).map_err(|e| e.to_string())
}

#[tauri::command]
async fn get_status(state: tauri::State<'_, AppState>) -> Result<Status, String> {
    Ok(state.status.lock().await.clone())
}

#[tauri::command]
async fn start_buffer(app: AppHandle) -> Result<(), String> {
    modes::start_clipping(&app).await.map_err(|e| { clog!("start clipping: {e}"); e })
}

#[tauri::command]
async fn stop_buffer(app: AppHandle) -> Result<(), String> {
    modes::stop_clipping(&app).await
}

#[tauri::command]
async fn toggle_recording(app: AppHandle) -> Result<(), String> {
    modes::toggle_recording(&app).await.map_err(|e| { clog!("recording: {e}"); e })
}

#[tauri::command]
async fn list_monitors(state: tauri::State<'_, AppState>) -> Result<Vec<screen::Monitor>, String> {
    // Once per OBS start: each probe creates and removes a monitor capture in
    // the live OBS, which can cause hitches while a game is being clipped.
    static CACHE: Mutex<Option<(u64, Vec<screen::Monitor>)>> = Mutex::const_new(None);
    let session = state.obs_session.load(Ordering::SeqCst);
    let mut cache = CACHE.lock().await;
    if let Some((s, m)) = cache.as_ref() {
        if *s == session && !m.is_empty() {
            return Ok(m.clone());
        }
    }
    let m = screen::list_monitors(&state.obs.client).await.map_err(|e| format!("{e:#}"))?;
    *cache = Some((session, m.clone()));
    Ok(m)
}

#[tauri::command]
async fn set_screen_monitor(state: tauri::State<'_, AppState>, monitor: String) -> Result<(), String> {
    let mut u = state.user.lock().await;
    u.screen_monitor = monitor;
    clog!("screen monitor: {}", u.screen_monitor);
    u.save(&state.settings_file).map_err(|e| e.to_string())
}

/// New encoder settings need an OBS restart: done right away when nothing is
/// being written, otherwise at the next start.
#[tauri::command]
async fn set_quality(state: tauri::State<'_, AppState>, quality: String) -> Result<bool, String> {
    if !["high", "balanced", "small"].contains(&quality.as_str()) {
        return Err(format!("unknown quality {quality}"));
    }
    {
        let mut u = state.user.lock().await;
        u.quality = quality.clone();
        u.save(&state.settings_file).map_err(|e| e.to_string())?;
    }
    state.obs.settings.lock().unwrap().quality = quality.clone();
    clog!("quality: {quality}");
    let s = state.status.lock().await.clone();
    if s.buffer || s.recording {
        state.restart_pending.store(true, Ordering::SeqCst);
        return Ok(false);
    }
    state.obs.restart();
    Ok(true)
}

#[tauri::command]
async fn save_clip(app: AppHandle) -> Result<(), String> {
    save_clip_inner(&app).await
}

#[derive(Serialize)]
struct SettingsView {
    hotkey: hotkey::Hotkey,
    hotkey_label: String,
    record_hotkey_label: String,
    screenshot_hotkey_label: String,
    storage_limit_gb: f64,
    buffer_secs: u32,
    max_buffer_secs: u32,
    sound: bool,
    toast: bool,
    autostart: bool,
    test_builds: bool,
    theme: String,
    screen_monitor: String,
    quality: String,
    version: String,
    audio: AudioSettings,
    setup_done: bool,
    clips_dir: String,
    /// The clips folder is not the default Videos\Clipper.
    clips_dir_custom: bool,
    /// A Todoist token is stored, so "Report a problem" can send.
    report_ready: bool,
    /// Which chip encodes, e.g. "NVIDIA (NVENC)".
    encoder: String,
    gpu_encoding: bool,
}

/// "What's new" since the version the user saw last (empty on a fresh install
/// or when nothing changed). Marks the current version as seen.
#[tauri::command]
async fn whats_new(state: tauri::State<'_, AppState>) -> Result<Vec<changelog::Entry>, String> {
    let current = env!("CARGO_PKG_VERSION");
    let mut u = state.user.lock().await;
    if u.last_version == current {
        return Ok(vec![]);
    }
    // A fresh install gets the setup guide, not a list of changes.
    let entries = if !u.setup_done {
        vec![]
    } else if u.last_version.is_empty() && u.test_builds {
        // Test builds before 0.4.3 didn't store the version either; a tester
        // already saw the earlier ones, so show just this version.
        changelog::only(current)
    } else {
        changelog::since(&u.last_version, current)
    };
    clog!("what's new: {} → {current}, {} version(s) shown", if u.last_version.is_empty() { "?" } else { &u.last_version }, entries.len());
    u.last_version = current.to_string();
    u.save(&state.settings_file).map_err(|e| e.to_string())?;
    Ok(entries)
}

/// The setup guide is done (or skipped): don't show it on the next start.
#[tauri::command]
async fn finish_setup(state: tauri::State<'_, AppState>) -> Result<(), String> {
    let mut u = state.user.lock().await;
    u.setup_done = true;
    clog!("setup guide finished");
    u.save(&state.settings_file).map_err(|e| e.to_string())
}

#[tauri::command]
async fn get_settings(state: tauri::State<'_, AppState>) -> Result<SettingsView, String> {
    let u = state.user.lock().await;
    Ok(SettingsView {
        hotkey: u.hotkey.clone(),
        hotkey_label: u.hotkey.label(),
        record_hotkey_label: u.record_hotkey.label(),
        screenshot_hotkey_label: u.screenshot_hotkey.label(),
        storage_limit_gb: u.storage_limit_gb,
        buffer_secs: u.buffer_secs,
        max_buffer_secs: MAX_BUFFER_SECS,
        sound: u.sound,
        toast: u.toast,
        autostart: u.autostart,
        test_builds: u.test_builds,
        theme: u.theme.clone(),
        screen_monitor: u.screen_monitor.clone(),
        quality: u.quality.clone(),
        version: env!("CARGO_PKG_VERSION").to_string(),
        audio: u.audio.clone(),
        setup_done: u.setup_done,
        clips_dir: state.clips_dir().to_string_lossy().into_owned(),
        clips_dir_custom: state.clips_dir() != state.default_clips_dir,
        report_ready: report::token(&state.obs.data_dir).is_some(),
        encoder: if state.obs.settings.lock().unwrap().force_cpu { gpu::Vendor::Cpu } else { gpu::detect() }.label().into(),
        gpu_encoding: !state.obs.settings.lock().unwrap().force_cpu && gpu::detect() != gpu::Vendor::Cpu,
    })
}

/// Returns the new key's label.
#[tauri::command]
async fn set_hotkey(state: tauri::State<'_, AppState>, hotkey: hotkey::Hotkey, which: Option<String>)
    -> Result<String, String>
{
    let slot = match which.as_deref() {
        Some("record") => hotkey::Slot::Record,
        Some("screenshot") => hotkey::Slot::Screenshot,
        _ => hotkey::Slot::Clip,
    };
    let mut u = state.user.lock().await;
    let same = |o: &hotkey::Hotkey| o.vk() == hotkey.vk() && o.shift == hotkey.shift && o.ctrl == hotkey.ctrl && o.alt == hotkey.alt;
    let others = [(hotkey::Slot::Clip, &u.hotkey), (hotkey::Slot::Record, &u.record_hotkey), (hotkey::Slot::Screenshot, &u.screenshot_hotkey)];
    if others.iter().any(|(s, k)| *s != slot && same(k)) {
        return Err("That key is already used for another action".into());
    }
    hotkey::apply(slot, &hotkey)?;
    clog!("{slot:?} key set to {}", hotkey.label());
    let label = hotkey.label();
    match slot {
        hotkey::Slot::Record => u.record_hotkey = hotkey,
        hotkey::Slot::Screenshot => u.screenshot_hotkey = hotkey,
        hotkey::Slot::Clip => u.hotkey = hotkey,
    }
    u.save(&state.settings_file).map_err(|e| e.to_string())?;
    Ok(label)
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
    // Keep the game detector from restarting clipping while we change it
    // (a flag of its own: user_stopped may change meanwhile, e.g. a new game).
    state.buffer_changing.store(true, Ordering::SeqCst);
    let r = async {
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
    Ok::<(), String>(())
    }
    .await;
    state.buffer_changing.store(false, Ordering::SeqCst);
    r?;
    Ok(secs)
}

/// sound | toast
#[tauri::command]
async fn set_option(state: tauri::State<'_, AppState>, name: String, value: bool) -> Result<(), String> {
    let mut u = state.user.lock().await;
    match name.as_str() {
        "sound" => u.sound = value,
        "toast" => u.toast = value,
        "test_builds" => u.test_builds = value,
        "autostart" => {
            u.autostart = value;
            autostart::apply(value);
        }
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
    let tracks_changed = {
        let mut s = state.obs.settings.lock().unwrap();
        let changed = obs::setup::track_mask(&s.audio) != obs::setup::track_mask(&audio);
        s.audio = audio;
        changed
    };
    // A different number of tracks needs an OBS restart (when nothing runs).
    if tracks_changed {
        let st = state.status.lock().await.clone();
        if !st.buffer && !st.recording && state.obs.client.is_connected().await {
            state.obs.restart();
            return Ok(());
        }
        // The new track list applies once clipping and recording have stopped
        // (without OBS running, its next start reads the new settings anyway).
        if state.obs.client.is_connected().await {
            state.restart_pending.store(true, Ordering::SeqCst);
        }
    }
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

#[derive(Serialize)]
struct GameEntry {
    name: String,
    enabled: bool,
    /// Added by the user (can be removed again).
    custom: bool,
}

#[tauri::command]
async fn list_running_apps() -> Result<Vec<games::AppRef>, String> {
    tauri::async_runtime::spawn_blocking(games::running_apps).await.map_err(|e| e.to_string())
}

#[tauri::command]
async fn add_custom_game(state: tauri::State<'_, AppState>, app: games::AppRef) -> Result<(), String> {
    let mut u = state.user.lock().await;
    u.custom_games.retain(|g| !g.exe.eq_ignore_ascii_case(&app.exe));
    clog!("clip application added: {} ({})", app.name, app.exe);
    u.custom_games.push(app);
    u.save(&state.settings_file).map_err(|e| e.to_string())
}

#[tauri::command]
async fn remove_custom_game(state: tauri::State<'_, AppState>, name: String) -> Result<(), String> {
    let mut u = state.user.lock().await;
    u.custom_games.retain(|g| g.name != name);
    clog!("clip application removed: {name}");
    u.save(&state.settings_file).map_err(|e| e.to_string())
}

#[tauri::command]
fn set_meters(state: tauri::State<'_, AppState>, on: bool) {
    state.meters.store(on, Ordering::SeqCst);
}

#[tauri::command]
async fn list_games(state: tauri::State<'_, AppState>) -> Result<Vec<GameEntry>, String> {
    let names = tauri::async_runtime::spawn_blocking(games::installed_games)
        .await
        .map_err(|e| e.to_string())?;
    let u = state.user.lock().await;
    let enabled = |n: &str| !u.disabled_games.iter().any(|d| d.eq_ignore_ascii_case(n));
    let mut out: Vec<GameEntry> = u
        .custom_games
        .iter()
        .map(|g| GameEntry { enabled: enabled(&g.name), name: g.name.clone(), custom: true })
        .collect();
    out.extend(names.into_iter().map(|name| GameEntry { enabled: enabled(&name), name, custom: false }));
    Ok(out)
}

#[tauri::command]
async fn set_game_enabled(state: tauri::State<'_, AppState>, name: String, enabled: bool) -> Result<(), String> {
    let mut u = state.user.lock().await;
    u.disabled_games.retain(|d| !d.eq_ignore_ascii_case(&name));
    if !enabled {
        u.disabled_games.push(name.clone());
    }
    clog!("game {name}: {}", if enabled { "on" } else { "off" });
    u.save(&state.settings_file).map_err(|e| e.to_string())
}

#[tauri::command]
async fn set_theme(state: tauri::State<'_, AppState>, theme: String) -> Result<(), String> {
    if !["system", "light", "dark", "oled"].contains(&theme.as_str()) {
        return Err(format!("unknown theme {theme}"));
    }
    let mut u = state.user.lock().await;
    u.theme = theme;
    u.save(&state.settings_file).map_err(|e| e.to_string())
}

#[tauri::command]
async fn check_update(state: tauri::State<'_, AppState>) -> Result<Option<updater::Update>, String> {
    let test = state.user.lock().await.test_builds;
    let r = updater::check(test).await.map_err(|e| format!("{e:#}"));
    clog!("update check: {r:?}");
    r
}

/// Downloads, swaps the exe and quits; the new version starts by itself.
#[tauri::command]
async fn install_update(app: AppHandle, version: String) -> Result<(), String> {
    let update = updater::found(&version).ok_or("Check for updates again first")?;
    clog!("installing update {}", update.version);
    let h = app.clone();
    updater::install(&update, move |p| { let _ = h.emit("update-progress", p); })
        .await
        .map_err(|e| { clog!("ERROR update: {e:#}"); format!("{e:#}") })?;
    app.exit(0);
    Ok(())
}

/// Lines from the UI (errors, editor events) into clipper.log.
#[tauri::command]
fn ui_log(msg: String) {
    clog!("ui: {}", msg.chars().take(500).collect::<String>());
}

/// Windows 11 draws the window border itself (follows the rounded corners), in
/// the theme's accent colour. Returns false where that isn't supported, so the
/// page keeps its own border.
#[tauri::command]
fn set_frame_color(app: AppHandle, color: String) -> bool {
    #[cfg(windows)]
    {
        use windows::Win32::{
            Foundation::{COLORREF, HWND},
            Graphics::Dwm::{DwmSetWindowAttribute, DWMWA_BORDER_COLOR, DWMWA_WINDOW_CORNER_PREFERENCE, DWMWCP_ROUND},
        };
        let hex = color.trim().trim_start_matches('#');
        let Ok(rgb) = u32::from_str_radix(hex, 16) else { return false };
        // COLORREF is 0x00BBGGRR.
        let cref = COLORREF(((rgb & 0xFF) << 16) | (rgb & 0xFF00) | ((rgb >> 16) & 0xFF));
        let Some(w) = app.get_webview_window("main") else { return false };
        let Ok(h) = w.hwnd() else { return false };
        let hwnd = HWND(h.0);
        unsafe {
            let corner = DWMWCP_ROUND;
            let _ = DwmSetWindowAttribute(hwnd, DWMWA_WINDOW_CORNER_PREFERENCE,
                &corner as *const _ as *const std::ffi::c_void, std::mem::size_of_val(&corner) as u32);
            return DwmSetWindowAttribute(hwnd, DWMWA_BORDER_COLOR,
                &cref as *const _ as *const std::ffi::c_void, std::mem::size_of::<COLORREF>() as u32).is_ok();
        }
    }
    #[cfg(not(windows))]
    {
        let _ = (app, color);
        false
    }
}

/// Store (after checking) or remove ("") the Todoist token for problem reports.
#[tauri::command]
async fn set_report_token(state: tauri::State<'_, AppState>, token: String) -> Result<(), String> {
    report::set_token(&state.obs.data_dir, &token).await.map_err(|e| format!("{e:#}"))?;
    clog!("report token {}", if token.trim().is_empty() { "removed" } else { "saved" });
    Ok(())
}

/// Send the user's description with the logs to their Todoist Inbox. Returns the task link.
#[tauri::command]
async fn report_problem(state: tauri::State<'_, AppState>, text: String) -> Result<String, String> {
    clog!("problem report: {}", text.chars().take(300).collect::<String>().replace('\n', " / "));
    let summary = report_summary(&state).await;
    let url = report::send(&state.obs.data_dir, &text, &summary).await.map_err(|e| {
        clog!("problem report failed: {e:#}");
        format!("{e:#}")
    })?;
    clog!("problem report sent");
    Ok(url)
}

/// Everyone without a Todoist token: open a filled-in GitHub issue (personal
/// details removed) in the browser; the user checks it and presses Submit.
#[tauri::command]
async fn report_github(app: AppHandle, text: String) -> Result<(), String> {
    use tauri_plugin_clipboard_manager::ClipboardExt;
    clog!("problem report via GitHub: {}", text.chars().take(300).collect::<String>().replace('\n', " / "));
    let summary = report_summary(&app.state::<AppState>()).await;
    let (url, log) = report::github_issue(&text, &summary);
    app.clipboard().write_text(log).map_err(|e| format!("Could not copy the log: {e}"))?;
    tauri::async_runtime::spawn_blocking(move || report::open_in_browser(&url))
        .await
        .map_err(|e| e.to_string())?
        .map_err(|e| format!("Could not open the browser: {e}"))
}

/// Version, status and settings for a problem report (no secrets).
async fn report_summary(state: &AppState) -> String {
    {
        let u = state.user.lock().await;
        let s = state.status.lock().await;
        let obs = state.obs.settings.lock().unwrap().clone();
        format!(
            "---\nVersion {} · {} · {}\nStatus: OBS {} · clipping {} · recording {} · screen mode {} · game {}\n\
             Quality {} ({}x{}, encoder {}{}) · buffer {} s · screen {}\nAudio: {}\nAdded apps: {}",
            env!("CARGO_PKG_VERSION"),
            std::env::consts::OS,
            chrono::Local::now().format("%Y-%m-%d %H:%M:%S"),
            s.obs, s.buffer, s.recording, s.screen, s.game.as_deref().unwrap_or("none"),
            u.quality, obs.width, obs.height, gpu::detect().label(), if obs.force_cpu { ", forced CPU" } else { "" },
            u.buffer_secs, u.screen_monitor,
            serde_json::to_string(&u.audio).unwrap_or_default(),
            u.custom_games.iter().map(|g| g.name.as_str()).collect::<Vec<_>>().join(", "),
        )
    }
}

#[tauri::command]
fn open_log_folder() -> Result<(), String> {
    let dir = log::path().and_then(|p| p.parent()).ok_or("no log folder")?;
    let opener = if cfg!(windows) { "explorer" } else { "xdg-open" };
    std::process::Command::new(opener).arg(dir).spawn().map_err(|e| e.to_string())?;
    Ok(())
}

// Runs off the main thread: file work (a folder lock held during a big move,
// many clips on a slow drive) must not freeze the window.
#[tauri::command(async)]
fn delete_clip(state: tauri::State<'_, AppState>, path: String) -> Result<(), String> {
    let dirs = state.clip_dirs();
    let p = clips::checked_in(&dirs, &path)?;
    if let Err(e) = clips::trash(&p) {
        clog!("ERROR could not move {} to the recycle bin: {e}", p.display());
        return Err(format!("Could not move it to the recycle bin: {e}"));
    }
    clog!("clip moved to the recycle bin: {}", p.display());
    // Only folders that are Clipper's own: never remove a user's own (empty) folder.
    let ours: Vec<PathBuf> = dirs.into_iter().filter(|d| clips::is_ours(d)).collect();
    clips::remove_empty_folder(&p, &clips::root_keys(&ours));
    Ok(())
}

/// Share = put a file on the clipboard to paste in Discord. With a size limit
/// (MB) the clip is made to fit first, unless it already does. Fitted copies go
/// to `<data>/share` (not the library) and are cleaned up after a day.
/// Returns what was copied: "original" or "fitted".
#[tauri::command]
async fn share_clip(app: AppHandle, path: String, max_mb: Option<f64>) -> Result<String, String> {
    let st = app.state::<AppState>();
    let p = clips::checked_in(&st.clip_dirs(), &path)?;
    let size_mb = std::fs::metadata(&p).map(|m| m.len() as f64 / 1e6).unwrap_or(f64::MAX);
    let is_image = p.extension().is_some_and(|e| e.eq_ignore_ascii_case("png") || e.eq_ignore_ascii_case("jpg"));
    let (file, kind) = match max_mb {
        Some(mb) if size_mb > mb * 0.97 && !is_image => {
            // Same clip (path, size, time) + same limit = the copy made before.
            let data = st.obs.data_dir.clone();
            let out_dir = data.join("share");
            let cached = out_dir.join(share_name(&p, mb));
            if cached.is_file() {
                clog!("share: reusing {}", cached.display());
                let _ = std::fs::File::options().append(true).open(&cached)
                    .and_then(|f| f.set_modified(std::time::SystemTime::now())); // keep it another day
                tauri::async_runtime::spawn_blocking(move || clips::copy_file(&cached))
                    .await
                    .map_err(|e| e.to_string())??;
                return Ok("reused".into());
            }
            let tools = tools_for(&app).await?;
            clog!("share: fitting {} ({size_mb:.1} MB) into {mb} MB", p.display());
            let info = editor::probe(&tools, &p)
                .await
                .map_err(|e| format!("Could not read the clip: {e:#}"))?;
            std::fs::create_dir_all(&out_dir).map_err(|e| e.to_string())?;
            let req = editor::ExportRequest {
                path: info.path.clone(), start: 0.0, end: info.duration, volumes: vec![], target_mb: Some(mb), keep: vec![], fade: 0.0,
            };
            let h = app.clone();
            let out = editor::export(&tools, &out_dir, &info, &req, move |pc| { let _ = h.emit("share-progress", pc); })
                .await
                .map_err(|e| { clog!("ERROR share export: {e:#}"); format!("Could not make it smaller: {e:#}") })?;
            let _ = std::fs::remove_dir_all(data.join("share-cache")); // left by older versions
            std::fs::rename(&out, &cached).map_err(|e| e.to_string())?;
            clog!("share: fitted to {:.1} MB", std::fs::metadata(&cached).map(|m| m.len() as f64 / 1e6).unwrap_or(0.0));
            (cached, "fitted")
        }
        _ => (p, "original"),
    };
    tauri::async_runtime::spawn_blocking(move || clips::copy_file(&file))
        .await
        .map_err(|e| e.to_string())??;
    Ok(kind.into())
}

/// File name of the fitted copy of `clip` for `mb`: readable (it is what Discord
/// shows) plus a short hash of path, size, modified time and limit.
fn share_name(clip: &std::path::Path, mb: f64) -> String {
    use sha2::{Digest, Sha256};
    let md = std::fs::metadata(clip).ok();
    let stamp = md.as_ref().and_then(|m| m.modified().ok())
        .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok()).map(|d| d.as_secs()).unwrap_or(0);
    let key = format!("{}|{}|{stamp}|{mb}", clip.display(), md.map(|m| m.len()).unwrap_or(0));
    let hash: String = Sha256::digest(key.as_bytes()).iter().take(4).map(|b| format!("{b:02x}")).collect();
    let stem = clip.file_stem().map(|s| s.to_string_lossy().into_owned()).unwrap_or_else(|| "clip".into());
    format!("{stem} (Discord {mb} MB) {hash}.mp4")
}

/// Fitted share copies older than a day (the clipboard has long moved on).
fn clean_share_dir(data_dir: &std::path::Path) {
    let Ok(dir) = std::fs::read_dir(data_dir.join("share")) else { return };
    for e in dir.flatten() {
        let old = e.metadata().and_then(|m| m.modified()).ok()
            .and_then(|t| t.elapsed().ok())
            .is_some_and(|age| age > std::time::Duration::from_secs(24 * 3600));
        if old {
            let _ = std::fs::remove_file(e.path());
        }
    }
}

/// Async (off the main thread): checked_in canonicalizes every clips folder,
/// which can block for seconds on a slow or unplugged drive.
#[tauri::command]
async fn reveal_clip(state: tauri::State<'_, AppState>, path: String) -> Result<(), String> {
    let dirs = state.clip_dirs();
    tauri::async_runtime::spawn_blocking(move || clips::reveal(&clips::checked_in(&dirs, &path)?))
        .await
        .map_err(|e| e.to_string())?
}

/// Where OBS writes a clip before Clipper moves it into the clips folder: on
/// the same drive, so the move is instant and big recordings don't fill C:
/// (also for the default Videos\Clipper, which may be on another drive).
fn incoming_dir(clips_dir: &std::path::Path) -> PathBuf {
    clips_dir.join(clips::INCOMING)
}

/// Files OBS finished while Clipper wasn't there to move them (quit, update or
/// a crash during a recording): put them in the library under "Recovered".
/// Only files from before this start, so nothing OBS is writing now.
async fn recover_leftovers(app: AppHandle, started: std::time::SystemTime) {
    let st = app.state::<AppState>();
    let clips_dir = st.clips_dir();
    let mut dirs = vec![st.obs.data_dir.join("incoming"), clips_dir.join(clips::INCOMING)];
    dirs.extend(st.clip_dirs().iter().map(|d| d.join(clips::INCOMING)));
    dirs.sort();
    dirs.dedup();
    let found = tauri::async_runtime::spawn_blocking(move || {
        let mut out = vec![];
        for d in dirs {
            for e in std::fs::read_dir(&d).into_iter().flatten().flatten() {
                let p = e.path();
                let video = p.extension().and_then(|x| x.to_str())
                    .is_some_and(|x| ["mp4", "mkv", "mov"].contains(&x.to_ascii_lowercase().as_str()));
                let old = e.metadata().and_then(|m| m.modified()).is_ok_and(|m| m < started);
                let partial = p.file_stem().is_some_and(|s| s.to_string_lossy().ends_with(".part"));
                if video && old && !partial && p.is_file() {
                    out.push(p);
                }
            }
        }
        out
    }).await.unwrap_or_default();
    for p in found {
        match adopt_blocking(&p.to_string_lossy(), clips_dir.clone(), Some("Recovered".into())).await {
            Ok(dst) => {
                clog!("recovered {} → {}", p.display(), dst.display());
                let _ = app.emit("clip-saved", dst.to_string_lossy());
            }
            Err(e) => clog!("could not recover {}: {e:#}", p.display()),
        }
    }
}

/// Windows folder picker. None when cancelled.
#[tauri::command]
async fn pick_folder(app: AppHandle, start: Option<String>) -> Result<Option<String>, String> {
    use tauri_plugin_dialog::DialogExt;
    let mut d = app.dialog().file().set_title("Choose a folder for your clips");
    if let Some(s) = start.filter(|s| std::path::Path::new(s).is_dir()) {
        d = d.set_directory(s);
    }
    let picked = tauri::async_runtime::spawn_blocking(move || d.blocking_pick_folder()).await.map_err(|e| e.to_string())?;
    Ok(picked.and_then(|p| p.into_path().ok()).map(|p| p.to_string_lossy().into_owned()))
}

#[derive(Serialize)]
struct ClipsDirChange {
    /// The folder now in use.
    dir: String,
    /// Clips still in `old` (not moved, or in use); the library keeps showing them.
    left: usize,
    old: String,
}

/// Use another clips folder ("" = back to Videos\Clipper). A folder that
/// already holds other things gets a "Clipper" folder inside it. New clips go
/// to the new folder right away; with `move_clips`, the existing clips (and
/// their stars) follow.
#[tauri::command]
async fn set_clips_dir(app: AppHandle, path: String, move_clips: bool) -> Result<ClipsDirChange, String> {
    let st = app.state::<AppState>();
    if st.status.lock().await.recording {
        return Err("Stop the recording first".into());
    }
    if st.moving.swap(true, Ordering::SeqCst) {
        return Err("Clips are still being moved; wait until that is done".into());
    }
    let r = change_clips_dir(&app, path.trim(), move_clips).await;
    st.moving.store(false, Ordering::SeqCst);
    let _ = app.emit("clips-moving", false);
    r
}

/// The real path of `p`, also when it doesn't exist yet (then its parent's).
fn canon(p: &std::path::Path) -> PathBuf {
    std::fs::canonicalize(p).unwrap_or_else(|_| match (p.parent(), p.file_name()) {
        (Some(parent), Some(name)) => std::fs::canonicalize(parent).map(|c| c.join(name)).unwrap_or(p.to_path_buf()),
        _ => p.to_path_buf(),
    })
}

async fn change_clips_dir(app: &AppHandle, path: &str, move_clips: bool) -> Result<ClipsDirChange, String> {
    let st = app.state::<AppState>();
    let old = st.clips_dir();
    let new = if path.is_empty() { st.default_clips_dir.clone() } else { clips::target_for(std::path::Path::new(path)) };
    // All checks before anything is created on disk.
    if !new.is_absolute() {
        return Err("Choose a complete folder, like D:\\Clips".into());
    }
    let (canon_old, canon_new) = (canon(&old), canon(&new));
    let same = canon_new == canon_old;
    if !same && (canon_new.starts_with(&canon_old) || canon_old.starts_with(&canon_new)) {
        return Err("Pick a folder that is not inside the current clips folder (or the other way around)".into());
    }
    if canon_new.starts_with(canon(&st.obs.data_dir)) {
        return Err("That is Clipper's own data folder; pick another one".into());
    }
    if move_clips && !same && !clips::is_ours(&old) {
        return Err(format!(
            "Clipper can't move the clips out of {}: it doesn't know that folder as its own. \
             Untick \"Move my clips there too\" and move them yourself.",
            old.display()
        ));
    }
    std::fs::create_dir_all(&new).map_err(|e| format!("Can't use {}: {e}", new.display()))?;
    clips::mark(&new).map_err(|e| format!("Clipper can't write in {}: {e}", new.display()))?;
    app.asset_protocol_scope().allow_directory(&new, true).map_err(|e| e.to_string())?;

    // Switch first, so a clip saved while the old ones move lands in the new folder.
    *st.clips_dir.write().unwrap() = new.clone();
    {
        let mut u = st.user.lock().await;
        u.clips_dir = if new == st.default_clips_dir { String::new() } else { new.to_string_lossy().into_owned() };
        // The old folder's clips stay in the library until they are moved.
        u.old_clips_dirs.retain(|d| canon(std::path::Path::new(d)) != canon_new);
        if !same && clips::is_ours(&old) && old != st.default_clips_dir
            && !u.old_clips_dirs.iter().any(|d| canon(std::path::Path::new(d)) == canon_old)
        {
            u.old_clips_dirs.push(old.to_string_lossy().into_owned());
        }
        *st.other_clip_dirs.write().unwrap() = u.old_clips_dirs.iter().map(PathBuf::from).collect();
        u.save(&st.settings_file).map_err(|e| e.to_string())?;
    }
    let incoming = incoming_dir(&new);
    st.obs.set_incoming(incoming.clone());
    if st.obs.client.is_connected().await {
        if let Err(e) = st.obs.client.call("SetRecordDirectory", json!({ "recordDirectory": incoming.to_string_lossy() })).await {
            clog!("ERROR SetRecordDirectory: {e:#}");
        }
    }

    let (mut moved, mut left) = (0, 0);
    if move_clips && !same {
        (moved, left) = move_clips_now(app, &old, &new).await?;
    } else if !same {
        left = clips::list(&old).len();
    }
    clog!("clips folder {} → {} (moved {moved}, {left} still in the old folder)", old.display(), new.display());
    Ok(ClipsDirChange { dir: new.to_string_lossy().into_owned(), left, old: old.to_string_lossy().into_owned() })
}

async fn move_clips_now(app: &AppHandle, from: &std::path::Path, to: &std::path::Path) -> Result<(usize, usize), String> {
    let st = app.state::<AppState>();
    let _ = app.emit("clips-moving", true);
    let favs = clips::Favorites::new(&st.obs.data_dir);
    let (o, n) = (from.to_path_buf(), to.to_path_buf());
    let r = tauri::async_runtime::spawn_blocking(move || clips::move_all(&o, &n, &favs)).await.map_err(|e| e.to_string());
    let _ = app.emit("clips-moving", false);
    r
}

#[tauri::command]
async fn open_clips_folder(state: tauri::State<'_, AppState>) -> Result<(), String> {
    let opener = if cfg!(windows) { "explorer" } else { "xdg-open" };
    std::process::Command::new(opener).arg(&state.clips_dir()).spawn().map_err(|e| e.to_string())?;
    Ok(())
}

async fn tools_for(app: &AppHandle) -> Result<tools::Tools, String> {
    let st = app.state::<AppState>();
    let h = app.clone();
    tools::ensure(&st.obs.data_dir, move |p| { let _ = h.emit("tools-progress", p); })
        .await
        .map_err(|e| format!("Could not get ffmpeg: {e:#}"))
}

#[tauri::command]
async fn editor_open(app: AppHandle, path: String) -> Result<editor::ClipInfo, String> {
    let st = app.state::<AppState>();
    let seq = st.editor_seq.fetch_add(1, Ordering::SeqCst) + 1;
    let p = clips::checked_in(&st.clip_dirs(), &path)?;
    let tools = tools_for(&app).await?;
    // A fresh folder per opened clip: scrub images still being made for a
    // previous clip can't end up in this one.
    let stamp = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap_or_default().as_millis();
    let _ = std::fs::remove_dir_all(st.obs.data_dir.join("editor-cache"));
    let info = editor::open(&tools, &st.obs.data_dir.join("editor-cache").join(stamp.to_string()), &p)
        .await
        .map_err(|e| format!("Could not open the clip: {e:#}"))?;
    // A slower open of an earlier clip must not replace a newer one.
    let mut ed = st.editor.lock().await;
    if st.editor_seq.load(Ordering::SeqCst) != seq {
        return Err("Another clip was opened meanwhile".into());
    }
    clog!("editor: opened {} ({:.1} s, {} sources)", info.name, info.duration, info.sources.len());
    *ed = Some(info.clone());
    Ok(info)
}

#[tauri::command]
async fn editor_export(app: AppHandle, request: editor::ExportRequest) -> Result<String, String> {
    let st = app.state::<AppState>();
    let info = st.editor.lock().await.clone().ok_or("No clip is open in the editor")?;
    if info.path != request.path {
        return Err("The editor has a different clip open".into());
    }
    let tools = tools_for(&app).await?;
    clog!("editor: export {:.2}-{:.2} parts {:?} fade {} volumes {:?}", request.start, request.end, request.keep, request.fade, request.volumes);
    let h = app.clone();
    // The edited copy goes next to the original (its game folder).
    let folder = std::path::Path::new(&info.path).parent().map(|p| p.to_path_buf()).unwrap_or(st.clips_dir());
    let out = editor::export(&tools, &folder, &info, &request, move |p| { let _ = h.emit("export-progress", p); })
        .await
        .map_err(|e| { clog!("ERROR editor export: {e:#}"); format!("{e:#}") })?;
    clog!("editor: saved {}", out.display());
    let _ = app.emit("clip-saved", out.to_string_lossy());
    after_new_file(&app).await;
    Ok(out.to_string_lossy().into_owned())
}

/// Save the frame at `time` of the open clip as a PNG next to it.
#[tauri::command]
async fn editor_frame(app: AppHandle, time: f64) -> Result<String, String> {
    let st = app.state::<AppState>();
    let info = st.editor.lock().await.clone().ok_or("No clip is open in the editor")?;
    let tools = tools_for(&app).await?;
    let out = editor::save_frame(&tools, &info, time)
        .await
        .map_err(|e| { clog!("ERROR editor frame: {e:#}"); format!("{e:#}") })?;
    clog!("editor: saved frame {}", out.display());
    // A small PNG: no storage-limit run (it could delete the clip being edited).
    let _ = app.emit("clip-saved", out.to_string_lossy());
    Ok(out.to_string_lossy().into_owned())
}

/// Keyframe images for instant scrubbing; made after the editor is open.
#[tauri::command]
async fn editor_thumbs(app: AppHandle) -> Result<Vec<(f64, String)>, String> {
    let st = app.state::<AppState>();
    let info = st.editor.lock().await.clone().ok_or("No clip is open in the editor")?;
    let tools = tools_for(&app).await?;
    let cache = std::path::PathBuf::from(&info.cache);
    let t0 = std::time::Instant::now();
    let th = editor::keyframe_thumbs(&tools, std::path::Path::new(&info.path), &cache, info.start).await;
    clog!("editor: {} scrub images in {:.1} s", th.len(), t0.elapsed().as_secs_f64());
    Ok(th)
}

/// One extracted audio track as raw bytes (decoded by WebAudio in the editor).
#[tauri::command]
async fn editor_track(state: tauri::State<'_, AppState>, path: String, index: usize) -> Result<tauri::ipc::Response, String> {
    let file = {
        let ed = state.editor.lock().await;
        let info = ed.as_ref().ok_or("No clip is open in the editor")?;
        // Another clip was opened meanwhile: its track must not end up in this one.
        if info.path != path {
            return Err("The editor has a different clip open".into());
        }
        info.sources.iter().find(|t| t.index == index).ok_or("No such track")?.file.clone()
    };
    let bytes = tokio::fs::read(&file).await.map_err(|e| e.to_string())?;
    Ok(tauri::ipc::Response::new(bytes))
}

/// Clips in the given order → one video, saved next to the first one.
#[tauri::command]
async fn combine_clips(app: AppHandle, paths: Vec<String>) -> Result<String, String> {
    let st = app.state::<AppState>();
    let ps = paths.iter().map(|p| clips::checked_in(&st.clip_dirs(), p)).collect::<Result<Vec<_>, _>>()?;
    if ps.len() < 2 {
        return Err("Pick at least two clips".into());
    }
    let tools = tools_for(&app).await?;
    let dir = ps[0].parent().map(|p| p.to_path_buf()).unwrap_or(st.clips_dir());
    clog!("combine {} clips", ps.len());
    let h = app.clone();
    let out = editor::combine(&tools, &dir, &ps, move |p| { let _ = h.emit("combine-progress", p); })
        .await
        .map_err(|e| { clog!("ERROR combine: {e:#}"); format!("{e:#}") })?;
    clog!("combined: {}", out.display());
    notify::toast(&app, true, "Clips combined", out.file_stem().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default());
    let _ = app.emit("clip-saved", out.to_string_lossy());
    after_new_file(&app).await;
    Ok(out.to_string_lossy().into_owned())
}

#[tauri::command]
async fn editor_close(state: tauri::State<'_, AppState>) -> Result<(), String> {
    *state.editor.lock().await = None;
    let _ = std::fs::remove_dir_all(state.obs.data_dir.join("editor-cache"));
    Ok(())
}

// Runs off the main thread: file work (a folder lock held during a big move,
// many clips on a slow drive) must not freeze the window.
#[tauri::command(async)]
fn list_clips(state: tauri::State<'_, AppState>) -> Vec<clips::ClipInfo> {
    let favs = clips::Favorites::new(&state.obs.data_dir).load();
    let mut v = clips::list_all(&state.clip_dirs());
    for c in &mut v {
        c.favorite = clips::Favorites::has(&favs, &c.path);
    }
    v
}

/// Follow OBS events: buffer on/off, and adopt saved replays into the clips folder.
async fn watch_events(app: AppHandle) {
    let mut rx = app.state::<AppState>().obs.client.subscribe();
    loop {
        let ev = match rx.recv().await {
            Ok(ev) => ev,
            Err(tokio::sync::broadcast::error::RecvError::Lagged(n)) => {
                clog!("WARN: {n} OBS events were missed");
                continue;
            }
            Err(_) => break,
        };
        match ev["eventType"].as_str().unwrap_or_default() {
            "ReplayBufferStateChanged" => {
                let active = ev["eventData"]["outputActive"].as_bool().unwrap_or(false);
                hotkey::set_armed(active);
                let state = ev["eventData"]["outputState"].as_str().unwrap_or("?");
                clog!("buffer {}", state.trim_start_matches("OBS_WEBSOCKET_OUTPUT_").to_lowercase());
                if state == "OBS_WEBSOCKET_OUTPUT_STARTED" {
                    let key = app.state::<AppState>().user.lock().await.hotkey.label();
                    notify::info(&app, "Clipping is on", format!("Press {key} to save a clip"));
                }
                if state == "OBS_WEBSOCKET_OUTPUT_STOPPED" {
                    notify::info(&app, "Clipping stopped", "");
                }
                set_status(&app, |s| s.buffer = active).await;
                if state == "OBS_WEBSOCKET_OUTPUT_STOPPED" {
                    let app = app.clone();
                    tauri::async_runtime::spawn(async move { restart_if_pending(&app).await });
                }
            }
            "ReplayBufferSaved" => {
                let Some(src) = ev["eventData"]["savedReplayPath"].as_str() else { continue };
                let st = app.state::<AppState>();
                let game = match st.status.lock().await.game.clone() {
                    Some(g) => Some(g),
                    None if st.screen.load(Ordering::SeqCst) => Some("Screen".to_string()),
                    // The game closed a moment ago: still its clip.
                    None => st.last_game.lock().unwrap().clone(),
                };
                clog!("OBS wrote {src}");
                // Moving the file can take a while (another drive): never hold up
                // the event follower with it.
                let (app, src, dir) = (app.clone(), src.to_string(), st.clips_dir());
                tauri::async_runtime::spawn(async move {
                    match adopt_blocking(&src, dir, game).await {
                        Ok(p) => {
                            clog!("clip saved: {}", p.display());
                            let name = p.file_stem().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
                            notify::toast(&app, true, "Clip saved", name);
                            let _ = app.emit("clip-saved", p.to_string_lossy());
                            after_new_file(&app).await;
                        }
                        Err(e) => report_error(&app, format!("Could not store clip: {e:#}")),
                    }
                });
            }
            "RecordStateChanged" => {
                let d = &ev["eventData"];
                let active = d["outputActive"].as_bool().unwrap_or(false);
                let state = d["outputState"].as_str().unwrap_or("?");
                clog!("recording {}", state.trim_start_matches("OBS_WEBSOCKET_OUTPUT_").to_lowercase());
                set_status(&app, |s| s.recording = active).await;
                if state == "OBS_WEBSOCKET_OUTPUT_STARTED" {
                    let st = app.state::<AppState>();
                    let folder = st.status.lock().await.game.clone().unwrap_or_else(|| "Screen recording".into());
                    *st.rec_folder.lock().unwrap() = Some(folder);
                    notify::info(&app, "Recording started", "");
                }
                if state == "OBS_WEBSOCKET_OUTPUT_STOPPED" {
                    let st = app.state::<AppState>();
                    if let Some(src) = d["outputPath"].as_str() {
                        let folder = st.rec_folder.lock().unwrap().take().unwrap_or_else(|| "Screen recording".into());
                        let (app, src, dir) = (app.clone(), src.to_string(), st.clips_dir());
                        tauri::async_runtime::spawn(async move {
                            match adopt_blocking(&src, dir, Some(folder)).await {
                                Ok(p) => {
                                    clog!("recording saved: {}", p.display());
                                    notify::toast(&app, true, "Recording saved", p.file_stem().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default());
                                    let _ = app.emit("clip-saved", p.to_string_lossy());
                                    after_new_file(&app).await;
                                }
                                Err(e) => report_error(&app, format!("Could not store recording: {e:#}")),
                            }
                        });
                    }
                    // Leaving screen mode waits for OBS to go idle (seconds):
                    // never on the event loop.
                    let app = app.clone();
                    tauri::async_runtime::spawn(async move {
                        if !app.state::<AppState>().status.lock().await.buffer {
                            let _ = modes::leave_screen(&app).await;
                        }
                        restart_if_pending(&app).await;
                    });
                }
            }
            "InputVolumeMeters" => {
                // ~20/s from OBS: forward at most 10/s, only while someone looks.
                static LAST: std::sync::Mutex<Option<std::time::Instant>> = std::sync::Mutex::new(None);
                if !app.state::<AppState>().meters.load(Ordering::SeqCst) {
                    continue;
                }
                {
                    let mut l = LAST.lock().unwrap();
                    if l.is_some_and(|t| t.elapsed() < std::time::Duration::from_millis(100)) {
                        continue;
                    }
                    *l = Some(std::time::Instant::now());
                }
                let mut levels = serde_json::Map::new();
                for i in ev["eventData"]["inputs"].as_array().into_iter().flatten() {
                    let Some(name) = i["inputName"].as_str() else { continue };
                    // inputLevelsMul: per channel [magnitude, peak, input peak]; use the peak.
                    let peak = i["inputLevelsMul"].as_array().into_iter().flatten()
                        .filter_map(|c| c[1].as_f64()).fold(0.0f64, f64::max);
                    let db = if peak > 0.0 { 20.0 * peak.log10() } else { -100.0 };
                    levels.insert(name.to_string(), json!((db * 10.0).round() / 10.0));
                }
                let _ = app.emit("meters", levels);
            }
            DISCONNECTED => {
                clog!("OBS websocket disconnected");
                hotkey::set_armed(false);
                app.state::<AppState>().screen.store(false, Ordering::SeqCst);
                set_status(&app, |s| { s.buffer = false; s.recording = false; s.screen = false; }).await
            }
            _ => {}
        }
    }
}

/// Apply a setting that needed an OBS restart, now that nothing records.
async fn restart_if_pending(app: &AppHandle) {
    let st = app.state::<AppState>();
    let s = st.status.lock().await.clone();
    if !s.buffer && !s.recording && st.restart_pending.swap(false, Ordering::SeqCst) {
        clog!("restarting OBS for changed settings");
        st.obs.restart();
    }
}

/// Moving a clip in can be a multi-GB copy (other drive): not on the event loop.
async fn adopt_blocking(src: &str, clips_dir: PathBuf, game: Option<String>) -> anyhow::Result<PathBuf> {
    let src = PathBuf::from(src);
    tauri::async_runtime::spawn_blocking(move || clips::adopt(&src, &clips_dir, game.as_deref())).await?
}

fn dir_from_env(var: &str) -> Option<PathBuf> {
    std::env::var_os(var).filter(|v| !v.is_empty()).map(PathBuf::from)
}

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    // After an in-app update: let the old version (and its OBS) exit first.
    updater::wait_for_previous();
    if let installer::Action::Exit = installer::handle_start() {
        return;
    }
    let start_hidden = std::env::args().any(|a| a == "--hidden");
    let just_installed = std::env::args().any(|a| a == "--installed");

    let app = tauri::Builder::default()
        // A second start (desktop icon while it runs in the tray) opens the
        // running Clipper instead of starting another OBS.
        .plugin(tauri_plugin_single_instance::init(|app, args, _cwd| {
            if args.iter().any(|a| a == "--replace") {
                installer::replace_with_installed(app);
            } else {
                show_main(app)
            }
        }))
        .plugin(tauri_plugin_dialog::init())
        .plugin(tauri_plugin_clipboard_manager::init())
        .invoke_handler(tauri::generate_handler![
            get_status, start_buffer, stop_buffer, save_clip, list_clips, get_settings, set_hotkey,
            set_buffer, set_option, open_log_folder, set_audio,
            list_audio_devices, list_games, set_game_enabled, list_running_apps, add_custom_game, toggle_recording, list_monitors,
            set_screen_monitor, set_quality,
            remove_custom_game, set_meters,
            delete_clip, reveal_clip, open_clips_folder,
            check_update, install_update, set_theme,
            editor_open, editor_export, editor_close, editor_track, editor_thumbs, editor_frame, ui_log, set_frame_color,
            screenshot, rename_clip, set_favorite, set_storage_limit, combine_clips, finish_setup,
            set_report_token, report_problem, report_github, share_clip, pick_folder, set_clips_dir, whats_new
        ])
        .setup(move |app| {
            let handle = app.handle().clone();
            let data_dir = match dir_from_env("CLIPPER_DATA_DIR") {
                Some(d) => d,
                None => app.path().app_data_dir()?,
            };
            std::fs::create_dir_all(&data_dir)?;
            // First, so whatever goes wrong below (an unreadable settings.json) is logged.
            log::init(data_dir.join("clipper.log"));
            let settings_file = data_dir.join("settings.json");
            let user = UserSettings::load(&settings_file);
            let default_clips_dir = match dir_from_env("CLIPPER_CLIPS_DIR") {
                Some(d) => d,
                None => app.path().video_dir()?.join("Clipper"),
            };
            // A chosen folder on a drive that is gone (USB disk unplugged): use
            // the default for now, but keep the choice for the next start.
            let mut clips_dir = default_clips_dir.clone();
            if !user.clips_dir.is_empty() {
                let chosen = PathBuf::from(&user.clips_dir);
                if std::fs::create_dir_all(&chosen).is_ok() {
                    clips_dir = chosen;
                }
            }
            std::fs::create_dir_all(&clips_dir)?;
            // The page may only load files from the clips folder (clips, screenshots)
            // and Clipper's own data folder (editor scrub images, test clip):
            // tauri.conf.json leaves the asset scope empty.
            let scope = app.asset_protocol_scope();
            scope.allow_directory(&clips_dir, true)?;
            scope.allow_directory(&data_dir, true)?;
            // Videos\Clipper is always ours. A chosen folder is marked when it
            // is chosen; one chosen in 0.4.2/0.4.3 (before markers) only when it
            // holds nothing but clips. A folder with other files is never sorted,
            // cleaned by the storage limit or emptied by a move.
            if std::fs::create_dir_all(&default_clips_dir).is_ok() {
                let _ = clips::mark(&default_clips_dir);
            }
            if !clips::is_ours(&clips_dir) {
                if clips::looks_like_clips(&clips_dir) {
                    let _ = clips::mark(&clips_dir);
                    clog!("clips folder {} marked as Clipper's", clips_dir.display());
                } else {
                    clog!("clips folder {} holds other files: storage limit and moving clips are off there", clips_dir.display());
                }
            }
            if clips::is_ours(&clips_dir) {
                clips::sort_into_folders(&clips_dir);
            }
            // Earlier folders: keep the ones that still exist and hold clips.
            // A missing one (USB drive unplugged) stays listed for when it is back.
            let mut user = user;
            let before = user.old_clips_dirs.len();
            user.old_clips_dirs.retain(|d| {
                let p = std::path::Path::new(d);
                !p.exists() || (clips::is_ours(p) && !clips::list(p).is_empty())
            });
            if user.old_clips_dirs.len() != before {
                let _ = user.save(&settings_file);
            }
            for d in &user.old_clips_dirs {
                let _ = scope.allow_directory(d, true);
            }
            let _ = scope.allow_directory(&default_clips_dir, true);
            updater::cleanup();
            let _ = std::fs::remove_dir_all(data_dir.join("editor-cache"));
            clean_share_dir(&data_dir);
            clog!("data {} | clips {}", data_dir.display(), clips_dir.display());
            if !user.clips_dir.is_empty() && clips_dir == default_clips_dir {
                clog!("ERROR chosen clips folder {} is not available; using {}", user.clips_dir, clips_dir.display());
            }
            autostart::apply(user.autostart);
            hotkey::apply(hotkey::Slot::Clip, &user.hotkey)?;
            hotkey::apply(hotkey::Slot::Record, &user.record_hotkey)?;
            hotkey::apply(hotkey::Slot::Screenshot, &user.screenshot_hotkey)?;
            hotkey::start(handle.clone());
            #[allow(unused_mut)]
            let mut settings = Settings {
                buffer_secs: user.buffer_secs,
                audio: user.audio.clone(),
                quality: user.quality.clone(),
                ..Settings::default()
            };
            // Canvas = primary monitor (games run there); even sizes for the encoder.
            #[cfg(windows)]
            if let Ok(Some(m)) = app.primary_monitor() {
                settings.width = m.size().width & !1;
                settings.height = m.size().height & !1;
            }
            let obs = ObsManager::new(data_dir, settings)?;
            obs.set_incoming(incoming_dir(&clips_dir));
            app.manage(AppState {
                obs: obs.clone(),
                clips_dir: std::sync::RwLock::new(clips_dir),
                moving: AtomicBool::new(false),
                other_clip_dirs: std::sync::RwLock::new(user.old_clips_dirs.iter().map(PathBuf::from).collect()),
                default_clips_dir,
                status: Mutex::default(),
                obs_session: AtomicU64::new(0),
                user_stopped: AtomicBool::new(false),
                user: Mutex::new(user),
                settings_file,
                editor: Mutex::new(None),
                meters: AtomicBool::new(false),
                screen: AtomicBool::new(false),
                last_game: std::sync::Mutex::new(None),
                rec_folder: std::sync::Mutex::new(None),
                restart_pending: AtomicBool::new(false),
                buffer_changing: AtomicBool::new(false),
                editor_seq: AtomicU64::new(0),
            });

            // Tray
            let open = MenuItem::with_id(app, "open", "Open Clipper", true, None::<&str>)?;
            let save = MenuItem::with_id(app, "save", "Save clip", true, None::<&str>)?;
            let record = MenuItem::with_id(app, "record", "Start/stop recording", true, None::<&str>)?;
            let quit = MenuItem::with_id(app, "quit", "Quit Clipper", true, None::<&str>)?;
            let menu = Menu::with_items(app, &[&open, &save, &record, &quit])?;
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
                            // save_clip_inner already reports the error itself.
                            let _ = save_clip_inner(&app).await;
                        });
                    }
                    "record" => {
                        let app = app.clone();
                        tauri::async_runtime::spawn(async move {
                            if let Err(e) = modes::toggle_recording(&app).await {
                                report_error(&app, format!("Recording: {e}"));
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
            // One task applies OBS states in the order they were reported
            // (a task per report could land an older state last).
            let (obs_tx, mut obs_rx) = tokio::sync::mpsc::unbounded_channel::<String>();
            let h = handle.clone();
            tauri::async_runtime::spawn(async move {
                while let Some(state) = obs_rx.recv().await {
                    set_status(&h, |s| s.obs = state).await;
                }
            });
            let h = handle.clone();
            tauri::async_runtime::spawn(obs.run(move |state| {
                // Download progress would spam the log every percent.
                if !state.starts_with("downloading") || state.ends_with(" 0%") || state.ends_with("100%") {
                    clog!("OBS: {state}");
                }
                if state.starts_with("error") {
                    notify::toast(&h, false, "OBS problem", state.clone());
                }
                if state == "ready" {
                    let st = h.state::<AppState>();
                    st.obs_session.fetch_add(1, Ordering::SeqCst);
                    st.screen.store(false, Ordering::SeqCst);
                    // This OBS start already read the current settings.
                    st.restart_pending.store(false, Ordering::SeqCst);
                }
                let _ = obs_tx.send(state);
            }));
            tauri::async_runtime::spawn(watch_events(handle.clone()));
            tauri::async_runtime::spawn(recover_leftovers(handle.clone(), std::time::SystemTime::now()));
            tauri::async_runtime::spawn(autobuffer::run(handle.clone()));
            notify::setup(&handle)?;
            if just_installed {
                clog!("installed to {}", std::env::current_exe().map(|p| p.display().to_string()).unwrap_or_default());
                let h = handle.clone();
                tauri::async_runtime::spawn(async move {
                    tokio::time::sleep(std::time::Duration::from_secs(2)).await;
                    notify::info(&h, "Clipper is installed", "You can delete the downloaded file");
                    let _ = h.emit("error", "Clipper is installed: start it from the Start menu or desktop from now on. The downloaded file can be deleted.");
                });
            }

            // Look for an update shortly after start; tell the user, never auto-install.
            let h = handle.clone();
            tauri::async_runtime::spawn(async move {
                tokio::time::sleep(std::time::Duration::from_secs(15)).await;
                let test = h.state::<AppState>().user.lock().await.test_builds;
                match updater::check(test).await {
                    Ok(Some(u)) => {
                        clog!("update available: {}", u.version);
                        // Once per start, also with pop-ups off: easy to miss otherwise.
                        notify::notice(&h, "Update available", format!("Clipper {}: Settings → Updates", u.version));
                        let _ = h.emit("update-available", u);
                    }
                    Ok(None) => clog!("no update"),
                    Err(e) => clog!("update check failed: {e:#}"),
                }
            });

            if !start_hidden {
                show_main(&handle);
            }
            // Development: open a tab directly (screenshots without clicking).
            #[cfg(debug_assertions)]
            if let (Some(js), Some(w)) = (std::env::var("CLIPPER_DEV_JS_FILE").ok().and_then(|f| std::fs::read_to_string(f).ok()), handle.get_webview_window("main")) {
                let _ = w.eval(&format!("setTimeout(async () => {{ try {{ {js} }} catch (e) {{ uiLog('dev js: ' + e); }} }}, 6000)"));
            }
            #[cfg(debug_assertions)]
            if let (Ok(_), Some(w)) = (std::env::var("CLIPPER_DEV_EDIT"), handle.get_webview_window("main")) {
                let then = if std::env::var("CLIPPER_DEV_EDIT").as_deref() == Ok("export") {
                    "ed.start = 1; ed.end = 4; document.querySelector('#ed-tracks input').value = 50; \
                     document.querySelector('#ed-tracks input').oninput(); $('ed-export').click();"
                } else { "" };
                let _ = w.eval(&format!(
                    "setTimeout(async () => {{ await openEditor((await invoke('list_clips'))[0]); {then} }}, 1500)"));
            }
            fit_zoom(&handle);
            #[cfg(debug_assertions)]
            if let (Ok(view), Some(w)) = (std::env::var("CLIPPER_DEV_VIEW"), handle.get_webview_window("main")) {
                let js = format!("setTimeout(() => document.querySelector('[data-view={view}]')?.click(), 800)");
                let _ = w.eval(&js);
            }
            Ok(())
        })
        .on_window_event(|window, ev| {
            if let WindowEvent::Resized(_) = ev {
                if window.label() == "main" {
                    fit_zoom(window.app_handle());
                }
            }
            // Closing the window keeps Clipper running in the tray.
            if let WindowEvent::CloseRequested { api, .. } = ev {
                api.prevent_close();
                let _ = window.hide();
                static TOLD: AtomicBool = AtomicBool::new(false);
                if window.label() == "main" && !TOLD.swap(true, Ordering::SeqCst) {
                    notify::info(window.app_handle(), "Clipper is still running", "Find it in the tray; Quit from its menu");
                }
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

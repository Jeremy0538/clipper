//! Game mode vs screen mode, and recording.
//!
//! Game mode: the "Clipper" scene (game capture), canvas = primary monitor.
//! Screen mode: the "Screen" scene (chosen monitor(s)), canvas = their size.
//! The canvas can only change while nothing is being written, so mode switches
//! wait for clipping/recording to stop first.

use crate::{clog, obs::config::SCENE, screen, set_status, AppState};
use serde_json::json;
use std::{sync::atomic::Ordering, time::Duration};
use tauri::{AppHandle, Manager};

async fn outputs_idle(app: &AppHandle) -> bool {
    let obs = &app.state::<AppState>().obs.client;
    for _ in 0..50 {
        let rb = obs.call("GetReplayBufferStatus", json!({})).await.map(|v| v["outputActive"] == true).unwrap_or(false);
        let rec = obs.call("GetRecordStatus", json!({})).await.map(|v| v["outputActive"] == true).unwrap_or(false);
        if !rb && !rec {
            return true;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    false
}

pub async fn enter_screen(app: &AppHandle) -> Result<(), String> {
    let st = app.state::<AppState>();
    if st.screen.load(Ordering::SeqCst) {
        return Ok(());
    }
    let obs = &st.obs.client;
    let choice = st.user.lock().await.screen_monitor.clone();
    if !outputs_idle(app).await {
        return Err("Stop clipping first".into());
    }
    let audio = screen::audio_inputs(obs).await;
    let audio: Vec<&str> = audio.iter().map(String::as_str).collect();
    // prepare() changes the canvas first: on any failure put it and the game
    // scene back, or game clips would come out at the screen's size.
    let restore = || async {
        let (gw, gh) = {
            let s = st.obs.settings.lock().unwrap();
            (s.width, s.height)
        };
        let _ = screen::set_canvas(obs, gw, gh).await;
        if let Err(e) = st.obs.apply_audio().await {
            clog!("refit game scene: {e:#}");
        }
    };
    let (w, h) = match screen::prepare(obs, &choice, &audio).await {
        Ok(v) => v,
        Err(e) => {
            restore().await;
            return Err(format!("{e:#}"));
        }
    };
    if let Err(e) = screen::use_scene(obs, screen::SCREEN_SCENE).await {
        restore().await;
        return Err(format!("{e:#}"));
    }
    st.screen.store(true, Ordering::SeqCst);
    clog!("screen mode ({choice}, {w}x{h})");
    set_status(app, |s| s.screen = true).await;
    Ok(())
}

pub async fn leave_screen(app: &AppHandle) -> Result<(), String> {
    let st = app.state::<AppState>();
    if !st.screen.load(Ordering::SeqCst) {
        return Ok(());
    }
    let obs = &st.obs.client;
    if !outputs_idle(app).await {
        return Err("Still writing a clip or recording".into());
    }
    let (w, h) = {
        let s = st.obs.settings.lock().unwrap();
        (s.width, s.height)
    };
    screen::set_canvas(obs, w, h).await.map_err(|e| format!("{e:#}"))?;
    // The canvas changes rescaled the game scene too: put it back in place.
    if let Err(e) = st.obs.apply_audio().await {
        clog!("refit game scene: {e:#}");
    }
    // Let the game detector retarget the captures (cheap, and safe after a canvas change).
    st.obs_session.fetch_add(1, Ordering::SeqCst);
    screen::use_scene(obs, SCENE).await.map_err(|e| format!("{e:#}"))?;
    st.screen.store(false, Ordering::SeqCst);
    clog!("game mode");
    set_status(app, |s| s.screen = false).await;
    Ok(())
}

/// "Start clipping" by hand: with a game running that's the game, otherwise the screen.
pub async fn start_clipping(app: &AppHandle) -> Result<(), String> {
    let st = app.state::<AppState>();
    st.user_stopped.store(false, Ordering::SeqCst);
    if st.status.lock().await.game.is_none() {
        enter_screen(app).await?;
    }
    st.obs.client.call("StartReplayBuffer", json!({})).await.map_err(|e| e.to_string())?;
    Ok(())
}

pub async fn stop_clipping(app: &AppHandle) -> Result<(), String> {
    let st = app.state::<AppState>();
    st.user_stopped.store(true, Ordering::SeqCst);
    // Back to game mode even when the stop fails (the buffer had already
    // stopped): otherwise OBS stays in screen mode with a screen-sized canvas.
    let r = st.obs.client.call("StopReplayBuffer", json!({})).await.map_err(|e| e.to_string());
    if !st.status.lock().await.recording {
        let _ = leave_screen(app).await;
    }
    r.map(|_| ())
}

/// Start or stop a normal recording: of the game when one runs, else the screen.
pub async fn toggle_recording(app: &AppHandle) -> Result<(), String> {
    let st = app.state::<AppState>();
    let obs = &st.obs.client;
    if st.status.lock().await.recording {
        obs.call("StopRecord", json!({})).await.map_err(|e| e.to_string())?;
        return Ok(());
    }
    if st.status.lock().await.game.is_none() {
        let clipping = st.status.lock().await.buffer;
        if clipping && !st.screen.load(Ordering::SeqCst) {
            return Err("Stop clipping first".into());
        }
        enter_screen(app).await?;
    }
    obs.call("StartRecord", json!({})).await.map_err(|e| e.to_string())?;
    Ok(())
}

/// A game started: hand OBS back to the game unless a recording is running.
/// Returns false when the game buffer must not start now.
pub async fn game_started(app: &AppHandle) -> bool {
    let st = app.state::<AppState>();
    if !st.screen.load(Ordering::SeqCst) {
        return true;
    }
    if st.status.lock().await.recording {
        return false; // don't cut a screen recording short
    }
    if st.status.lock().await.buffer {
        let _ = st.obs.client.call("StopReplayBuffer", json!({})).await;
    }
    leave_screen(app).await.is_ok()
}

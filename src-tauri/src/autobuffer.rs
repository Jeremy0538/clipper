//! Starts the replay buffer while a game runs, stops it afterwards, and (on
//! Windows) points OBS's game capture + game audio at the game's window.

use crate::{clog, games::Detector, notify, report_error, set_status, AppState};
use serde_json::json;
use std::{sync::atomic::Ordering, time::Duration};
use tauri::{AppHandle, Manager};

const TICK: Duration = Duration::from_secs(2);
/// Consecutive empty scans before a game counts as closed (loading screens,
/// launcher → game handoff).
const GONE_AFTER: u32 = 3;

pub async fn run(app: AppHandle) {
    let mut det = Some(Detector::new());
    let mut current: Option<crate::games::Game> = None;
    let mut misses = 0u32;
    let mut targeted: Option<(u32, u64, String)> = None; // (pid, OBS session, window)
    let mut auto_started = false;

    loop {
        tokio::time::sleep(TICK).await;
        let d = det.take().unwrap();
        let (disabled, custom) = {
            let u = app.state::<AppState>();
            let u = u.user.lock().await;
            (u.disabled_games.clone(), u.custom_games.clone())
        };
        let Ok((d, found)) = tokio::task::spawn_blocking(move || {
            let mut d = d;
            let g = d.detect(&disabled, &custom);
            (d, g)
        })
        .await
        else {
            det = Some(Detector::new());
            continue;
        };
        det = Some(d);

        let st = app.state::<AppState>();
        match found {
            Some(g) => {
                misses = 0;
                if current.as_ref().map(|c| c.pid) != Some(g.pid) {
                    st.user_stopped.store(false, Ordering::SeqCst);
                    clog!("game detected: {} (pid {}, window \"{}\")", g.name, g.pid, g.obs_window);
                    notify::info(&app, "Game detected", g.name.clone());
                    let name = g.name.clone();
                    *st.last_game.lock().unwrap() = Some(name.clone());
                    set_status(&app, |s| s.game = Some(name)).await;
                    current = Some(g);
                } else if let Some(c) = current.as_mut() {
                    // Same game; remember its window once it has one (late window,
                    // or restored from minimized) so OBS can be pointed at it.
                    if !g.obs_window.is_empty() && g.obs_window != c.obs_window {
                        c.obs_window = g.obs_window;
                    }
                }
            }
            None => {
                misses += 1;
                if misses >= GONE_AFTER && current.is_some() {
                    clog!("game closed: {}", current.take().unwrap().name);
                    set_status(&app, |s| s.game = None).await;
                    targeted = None;
                    st.user_stopped.store(false, Ordering::SeqCst);
                    if auto_started && st.status.lock().await.buffer {
                        if let Err(e) = st.obs.client.call("StopReplayBuffer", json!({})).await {
                            report_error(&app, format!("Could not stop the buffer: {e:#}"));
                        }
                    }
                    auto_started = false;
                }
            }
        }

        let Some(game) = &current else { continue };
        let (ready, buffer) = {
            let s = st.status.lock().await;
            (s.obs == "ready", s.buffer)
        };
        if !ready {
            continue;
        }

        let session = st.obs_session.load(Ordering::SeqCst);
        let want = (game.pid, session, game.obs_window.clone());
        // Only retarget when we know a window; never point OBS at "" mid-game.
        if targeted.as_ref() != Some(&want) && !(cfg!(windows) && game.obs_window.is_empty()) {
            if cfg!(windows) {
                let app_audio = st.obs.settings.lock().unwrap().audio.game_mode
                    == crate::settings::GameAudioMode::Game;
                use crate::obs::setup::{GAME_AUDIO, GAME_CAPTURE, WINDOW_CAPTURE};
                // Games are hooked (Game Capture); added apps are captured as a window.
                let (video, other) = if game.custom { (WINDOW_CAPTURE, GAME_CAPTURE) } else { (GAME_CAPTURE, WINDOW_CAPTURE) };
                for (input, on) in [(video, true), (other, false)] {
                    if let Ok(v) = st.obs.client.call("GetSceneItemId",
                        json!({ "sceneName": crate::obs::config::SCENE, "sourceName": input })).await
                    {
                        let _ = st.obs.client.call("SetSceneItemEnabled", json!({
                            "sceneName": crate::obs::config::SCENE, "sceneItemId": v["sceneItemId"], "sceneItemEnabled": on,
                        })).await;
                    }
                }
                let inputs: &[&str] = if app_audio { &[video, GAME_AUDIO] } else { &[video] };
                for &input in inputs {
                    let r = st.obs.client.call("SetInputSettings", json!({
                        "inputName": input,
                        "inputSettings": { "window": game.obs_window },
                        "overlay": true,
                    }))
                    .await;
                    match r {
                        Ok(_) => clog!("{input} → {}", game.obs_window),
                        Err(e) => report_error(&app, format!("Could not target {}: {e:#}", game.name)),
                    }
                }
            }
            targeted = Some(want);
        }

        if !buffer && !st.user_stopped.load(Ordering::SeqCst) {
            if !crate::modes::game_started(&app).await {
                continue; // a screen recording is running: leave it alone
            }
            match st.obs.client.call("StartReplayBuffer", json!({})).await {
                Ok(_) => {
                    auto_started = true;
                    clog!("buffer auto-started for {}", game.name);
                }
                Err(e) => {
                    // Usually the GPU encoder (old driver, unsupported card): retry on the CPU.
                    if st.obs.fall_back_to_cpu() {
                        crate::notify::info(&app, "Using CPU encoding", "The GPU encoder could not start");
                    } else {
                        report_error(&app, format!("Could not start clipping: {e:#}"));
                        // Don't retry (and report) every 2 s; the next game or Start clipping tries again.
                        st.user_stopped.store(true, Ordering::SeqCst);
                    }
                }
            }
        }
    }
}

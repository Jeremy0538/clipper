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
    let mut targeted: Option<(u32, u64)> = None; // (pid, OBS session)
    let mut auto_started = false;

    loop {
        tokio::time::sleep(TICK).await;
        let d = det.take().unwrap();
        let Ok((d, found)) = tokio::task::spawn_blocking(move || {
            let mut d = d;
            let g = d.detect();
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
                    set_status(&app, |s| s.game = Some(name)).await;
                    current = Some(g);
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
        if targeted != Some((game.pid, session)) {
            if cfg!(windows) {
                for input in [crate::obs::setup::GAME_CAPTURE, crate::obs::setup::GAME_AUDIO] {
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
            targeted = Some((game.pid, session));
        }

        if !buffer && !st.user_stopped.load(Ordering::SeqCst) {
            match st.obs.client.call("StartReplayBuffer", json!({})).await {
                Ok(_) => {
                    auto_started = true;
                    clog!("buffer auto-started for {}", game.name);
                }
                Err(e) => report_error(&app, format!("Could not start the buffer: {e:#}")),
            }
        }
    }
}

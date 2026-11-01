//! Screen mode: a second OBS scene that shows one monitor or all of them.
//! Used for plain screen recordings and for a clip buffer without a game.

use crate::obs::{config::SCENE, setup, ws::ObsClient};
use anyhow::{Context, Result};
use serde::Serialize;
use serde_json::json;

pub const SCREEN_SCENE: &str = "Screen";
pub const ALL: &str = "all";
pub const PRIMARY: &str = "primary";

#[derive(Serialize, Clone, Debug)]
pub struct Monitor {
    pub id: String,
    pub name: String,
    pub x: i32,
    pub y: i32,
    pub w: u32,
    pub h: u32,
    pub primary: bool,
}

/// OBS names monitors like "DELL U3419W: 3440x1440 @ 0,0 (Primary Monitor)".
fn parse_geometry(name: &str) -> Option<(i32, i32, u32, u32)> {
    let (size, pos) = name.rsplit_once(" @ ")?;
    let size = size.rsplit(' ').next()?;
    let (w, h) = size.split_once('x')?;
    let pos = pos.split(' ').next()?;
    let (x, y) = pos.split_once(',')?;
    Some((x.trim().parse().ok()?, y.trim().parse().ok()?, w.trim().parse().ok()?, h.trim().parse().ok()?))
}

fn capture_kind() -> &'static str {
    if cfg!(windows) { "monitor_capture" } else { "color_source_v3" }
}

/// Monitors as OBS sees them (a probe input lists them; it is never shown).
pub async fn list_monitors(obs: &ObsClient) -> Result<Vec<Monitor>> {
    if !cfg!(windows) {
        // Development stand-in: two fake screens side by side.
        return Ok(vec![
            Monitor { id: "dev-a".into(), name: "Dev screen A: 1920x1080 @ 0,0 (Primary Monitor)".into(), x: 0, y: 0, w: 1920, h: 1080, primary: true },
            Monitor { id: "dev-b".into(), name: "Dev screen B: 1280x720 @ 1920,0".into(), x: 1920, y: 0, w: 1280, h: 720, primary: false },
        ]);
    }
    let stamp = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap_or_default().as_millis();
    let name = format!("_probe_monitor_{stamp}");
    obs.call("CreateInput", json!({
        "sceneName": SCENE, "inputName": name, "inputKind": "monitor_capture",
        "inputSettings": {}, "sceneItemEnabled": false,
    }))
    .await?;
    let items = obs
        .call("GetInputPropertiesListPropertyItems", json!({ "inputName": name, "propertyName": "monitor_id" }))
        .await;
    let _ = obs.call("RemoveInput", json!({ "inputName": name })).await;
    Ok(items?["propertyItems"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|i| {
            let id = i["itemValue"].as_str()?.to_string();
            let name = i["itemName"].as_str()?.to_string();
            let (x, y, w, h) = parse_geometry(&name)?;
            Some(Monitor { primary: name.contains("Primary"), id, name, x, y, w, h })
        })
        .collect())
}

fn pick<'a>(monitors: &'a [Monitor], choice: &str) -> Vec<&'a Monitor> {
    match choice {
        ALL => monitors.iter().collect(),
        PRIMARY => monitors.iter().filter(|m| m.primary).take(1).collect(),
        id => monitors.iter().filter(|m| m.id == id).collect(),
    }
}

/// Build the Screen scene for `choice` and switch to it. Returns the canvas size
/// it needs; the caller changes the video settings (only allowed while no
/// output runs).
pub async fn prepare(obs: &ObsClient, choice: &str, audio_inputs: &[&str]) -> Result<(u32, u32)> {
    let monitors = list_monitors(obs).await?;
    let mut chosen = pick(&monitors, choice);
    if chosen.is_empty() {
        chosen = monitors.iter().take(1).collect();
    }
    anyhow::ensure!(!chosen.is_empty(), "No monitors found");
    let min_x = chosen.iter().map(|m| m.x).min().unwrap();
    let min_y = chosen.iter().map(|m| m.y).min().unwrap();
    let w = chosen.iter().map(|m| (m.x - min_x) as u32 + m.w).max().unwrap() & !1;
    let h = chosen.iter().map(|m| (m.y - min_y) as u32 + m.h).max().unwrap() & !1;

    // Fresh scene every time: the monitor layout may have changed.
    let scenes = obs.call("GetSceneList", json!({})).await?;
    let exists = scenes["scenes"].as_array().into_iter().flatten().any(|s| s["sceneName"] == SCREEN_SCENE);
    if exists {
        let items = obs.call("GetSceneItemList", json!({ "sceneName": SCREEN_SCENE })).await?;
        for it in items["sceneItems"].as_array().into_iter().flatten() {
            let _ = obs.call("RemoveSceneItem", json!({ "sceneName": SCREEN_SCENE, "sceneItemId": it["sceneItemId"] })).await;
        }
    } else {
        obs.call("CreateScene", json!({ "sceneName": SCREEN_SCENE })).await?;
    }

    // Old monitor inputs from an earlier layout.
    let inputs = obs.call("GetInputList", json!({})).await?;
    for i in inputs["inputs"].as_array().into_iter().flatten() {
        if let Some(n) = i["inputName"].as_str() {
            if n.starts_with("Screen ") {
                let _ = obs.call("RemoveInput", json!({ "inputName": n })).await;
            }
        }
    }
    tokio::time::sleep(std::time::Duration::from_millis(300)).await; // removal is async

    for (k, m) in chosen.iter().enumerate() {
        let name = format!("Screen {}", k + 1);
        let settings = if cfg!(windows) {
            json!({ "monitor_id": m.id, "capture_cursor": true })
        } else {
            json!({ "width": m.w, "height": m.h, "color": if k == 0 { 4281545523u32 } else { 4283215696u32 } })
        };
        let id = obs
            .call("CreateInput", json!({
                "sceneName": SCREEN_SCENE, "inputName": name, "inputKind": capture_kind(),
                "inputSettings": settings, "sceneItemEnabled": true,
            }))
            .await
            .with_context(|| format!("could not capture {}", m.name))?["sceneItemId"]
            .clone();
        obs.call("SetSceneItemTransform", json!({
            "sceneName": SCREEN_SCENE, "sceneItemId": id,
            "sceneItemTransform": { "positionX": m.x - min_x, "positionY": m.y - min_y, "alignment": 5 },
        }))
        .await?;
    }
    // The same audio sources as in the game scene.
    for a in audio_inputs {
        let _ = obs.call("CreateSceneItem", json!({ "sceneName": SCREEN_SCENE, "sourceName": a })).await;
    }
    Ok((w, h))
}

pub async fn set_canvas(obs: &ObsClient, w: u32, h: u32) -> Result<()> {
    let cur = obs.call("GetVideoSettings", json!({})).await?;
    if cur["baseWidth"] == w && cur["baseHeight"] == h {
        return Ok(());
    }
    obs.call("SetVideoSettings", json!({ "baseWidth": w, "baseHeight": h, "outputWidth": w, "outputHeight": h })).await?;
    crate::clog!("canvas {w}x{h}");
    Ok(())
}

pub async fn use_scene(obs: &ObsClient, scene: &str) -> Result<()> {
    obs.call("SetCurrentProgramScene", json!({ "sceneName": scene })).await?;
    Ok(())
}

/// Audio inputs that currently exist (to mirror them into the Screen scene).
pub async fn audio_inputs(obs: &ObsClient) -> Vec<String> {
    let names = [setup::GAME_AUDIO, setup::DISCORD, setup::MIC, setup::APP_INPUTS[0], setup::APP_INPUTS[1]];
    let list = obs.call("GetInputList", json!({})).await.unwrap_or_default();
    list["inputs"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|i| i["inputName"].as_str())
        .filter(|n| names.contains(n))
        .map(String::from)
        .collect()
}

#[cfg(test)]
mod tests {
    #[test]
    fn geometry() {
        assert_eq!(super::parse_geometry("DELL U3419W: 3440x1440 @ 0,0 (Primary Monitor)"), Some((0, 0, 3440, 1440)));
        assert_eq!(super::parse_geometry("Generic PnP Monitor: 1920x1080 @ -1920,180"), Some((-1920, 180, 1920, 1080)));
        assert_eq!(super::parse_geometry("weird"), None);
    }
}

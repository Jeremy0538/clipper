//! Builds the scene over the websocket: one capture + one audio source per track.
//! Track layout in every clip: 1 Mix, 2 Game, 3 Discord, 4 Mic.

use super::{config::SCENE, ws::ObsClient};
use anyhow::Result;
use serde_json::{json, Value};
use std::{collections::HashMap, path::Path};

pub const GAME_CAPTURE: &str = "Game Capture";
pub const GAME_AUDIO: &str = "Game Audio";
pub const DISCORD: &str = "Discord";
pub const MIC: &str = "Mic";

pub struct InputSpec {
    pub name: &'static str,
    pub kind: &'static str,
    pub settings: Value,
    /// Audio tracks 1..=6 this input is mixed into (empty = video only).
    pub tracks: &'static [u8],
    /// Scale to fill the canvas.
    pub fit: bool,
    pub filters: Vec<(&'static str, &'static str, Value)>,
}

fn rnnoise() -> (&'static str, &'static str, Value) {
    ("Noise Suppression", "noise_suppress_filter_v2", json!({ "method": "rnnoise" }))
}

/// The real Windows setup. Game inputs start untargeted; `games` points them at the game.
#[cfg(windows)]
pub fn specs(_dev_dir: &Path) -> Vec<InputSpec> {
    vec![
        InputSpec {
            name: GAME_CAPTURE,
            kind: "game_capture",
            settings: json!({ "capture_mode": "window", "window": "", "priority": 2,
                              "capture_cursor": true, "capture_audio": false }),
            tracks: &[],
            fit: true,
            filters: vec![],
        },
        InputSpec {
            name: GAME_AUDIO,
            kind: "wasapi_process_output_capture",
            settings: json!({ "window": "", "priority": 2 }),
            tracks: &[1, 2],
            fit: false,
            filters: vec![],
        },
        InputSpec {
            name: DISCORD,
            kind: "wasapi_process_output_capture",
            settings: json!({ "window": "Discord:Chrome_WidgetWin_1:Discord.exe", "priority": 2 }),
            tracks: &[1, 3],
            fit: false,
            filters: vec![],
        },
        InputSpec {
            name: MIC,
            kind: "wasapi_input_capture",
            settings: json!({ "device_id": "default" }),
            tracks: &[1, 4],
            fit: false,
            filters: vec![rnnoise()],
        },
    ]
}

/// Linux development stand-ins: looping test files instead of real capture,
/// so nothing on the developer's screen or microphone gets recorded.
#[cfg(not(windows))]
pub fn specs(dev_dir: &Path) -> Vec<InputSpec> {
    let file = |n: &str| dev_dir.join(n).to_string_lossy().into_owned();
    let media = |f: String| json!({ "local_file": f, "is_local_file": true, "looping": true,
                                    "restart_on_activate": false, "close_when_inactive": false });
    vec![
        InputSpec {
            name: GAME_CAPTURE,
            kind: "ffmpeg_source",
            settings: media(file("dev-game.mp4")),
            tracks: &[1, 2],
            fit: true,
            filters: vec![],
        },
        InputSpec {
            name: DISCORD,
            kind: "ffmpeg_source",
            settings: media(file("dev-discord.m4a")),
            tracks: &[1, 3],
            fit: false,
            filters: vec![],
        },
        InputSpec {
            name: MIC,
            kind: "ffmpeg_source",
            settings: media(file("dev-mic.m4a")),
            tracks: &[1, 4],
            fit: false,
            filters: vec![rnnoise()],
        },
    ]
}

/// Generate the Linux stand-in media once (needs ffmpeg).
#[cfg(not(windows))]
pub fn ensure_dev_media(dev_dir: &Path) -> Result<()> {
    use std::process::Command;
    let jobs: [(&str, &[&str]); 3] = [
        ("dev-game.mp4", &["-f", "lavfi", "-i", "testsrc2=s=1920x1080:r=60:d=20",
                           "-f", "lavfi", "-i", "sine=f=330:d=20", "-c:v", "libx264", "-g", "60",
                           "-pix_fmt", "yuv420p", "-c:a", "aac", "-shortest"]),
        ("dev-discord.m4a", &["-f", "lavfi", "-i", "sine=f=550:d=20:beep_factor=4", "-c:a", "aac"]),
        ("dev-mic.m4a", &["-f", "lavfi", "-i", "anoisesrc=d=20:a=0.05", "-c:a", "aac"]),
    ];
    for (name, args) in jobs {
        let out = dev_dir.join(name);
        if out.exists() {
            continue;
        }
        let ok = Command::new("ffmpeg")
            .args(["-loglevel", "error", "-y"])
            .args(args)
            .arg(&out)
            .status()?
            .success();
        anyhow::ensure!(ok, "ffmpeg failed for {name}");
    }
    Ok(())
}

pub async fn ensure_scene(obs: &ObsClient, specs: &[InputSpec], w: u32, h: u32) -> Result<()> {
    let list = obs.call("GetInputList", json!({})).await?;
    let existing: HashMap<String, String> = list["inputs"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|i| Some((i["inputName"].as_str()?.into(), i["inputKind"].as_str()?.into())))
        .collect();

    for spec in specs {
        match existing.get(spec.name) {
            Some(kind) if kind == spec.kind => {
                obs.call("SetInputSettings",
                    json!({ "inputName": spec.name, "inputSettings": spec.settings, "overlay": true }))
                    .await?;
            }
            other => {
                if other.is_some() {
                    obs.call("RemoveInput", json!({ "inputName": spec.name })).await?;
                }
                obs.call("CreateInput", json!({
                    "sceneName": SCENE, "inputName": spec.name, "inputKind": spec.kind,
                    "inputSettings": spec.settings, "sceneItemEnabled": true,
                }))
                .await?;
            }
        }

        if !spec.tracks.is_empty() {
            let tracks: serde_json::Map<String, Value> =
                (1..=6u8).map(|t| (t.to_string(), spec.tracks.contains(&t).into())).collect();
            obs.call("SetInputAudioTracks",
                json!({ "inputName": spec.name, "inputAudioTracks": tracks })).await?;
        }

        if spec.fit {
            let id = obs
                .call("GetSceneItemId", json!({ "sceneName": SCENE, "sourceName": spec.name }))
                .await?["sceneItemId"]
                .clone();
            obs.call("SetSceneItemTransform", json!({
                "sceneName": SCENE, "sceneItemId": id,
                "sceneItemTransform": {
                    "positionX": 0, "positionY": 0, "alignment": 5,
                    "boundsType": "OBS_BOUNDS_SCALE_INNER", "boundsAlignment": 0,
                    "boundsWidth": w, "boundsHeight": h,
                },
            }))
            .await?;
        }

        let have: Vec<String> = obs
            .call("GetSourceFilterList", json!({ "sourceName": spec.name }))
            .await?["filters"]
            .as_array()
            .into_iter()
            .flatten()
            .filter_map(|f| f["filterName"].as_str().map(String::from))
            .collect();
        for (fname, fkind, fsettings) in &spec.filters {
            if !have.iter().any(|n| n == fname) {
                obs.call("CreateSourceFilter", json!({
                    "sourceName": spec.name, "filterName": fname,
                    "filterKind": fkind, "filterSettings": fsettings,
                }))
                .await?;
            }
        }
    }
    Ok(())
}

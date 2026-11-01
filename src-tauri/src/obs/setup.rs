//! Builds the scene over the websocket: one capture + one audio source per track.
//! Track layout in every clip: 1 Mix, 2 Game, 3 Discord, 4 Mic.
//! Re-run whenever the audio settings change; it adds, updates and removes inputs.

use super::{config::SCENE, ws::ObsClient};
use crate::settings::{AudioSettings, GameAudioMode};
use anyhow::Result;
use serde::Serialize;
use serde_json::{json, Value};
use std::{collections::HashMap, path::Path};

pub const GAME_CAPTURE: &str = "Game Capture";
pub const GAME_AUDIO: &str = "Game Audio";
pub const DISCORD: &str = "Discord";
pub const MIC: &str = "Mic";
/// Audio inputs we own: removed when switched off.
const MANAGED_AUDIO: [&str; 3] = [GAME_AUDIO, DISCORD, MIC];
const NOISE_FILTER: &str = "Noise Suppression";

pub struct InputSpec {
    pub name: &'static str,
    pub kind: &'static str,
    pub settings: Value,
    /// Audio tracks 1..=6 this input is mixed into (empty = video only).
    pub tracks: &'static [u8],
    /// Scale to fill the canvas.
    pub fit: bool,
    pub volume: f64,
    pub filters: Vec<(&'static str, &'static str, Value)>,
}

fn rnnoise() -> (&'static str, &'static str, Value) {
    (NOISE_FILTER, "noise_suppress_filter_v2", json!({ "method": "rnnoise" }))
}

pub fn input_device_kind() -> &'static str {
    if cfg!(windows) { "wasapi_input_capture" } else { "pulse_input_capture" }
}
pub fn output_device_kind() -> &'static str {
    if cfg!(windows) { "wasapi_output_capture" } else { "pulse_output_capture" }
}

/// The real Windows setup. Game Audio in "game" mode starts untargeted; the
/// game detector points it (and Game Capture) at the game's window.
#[cfg(windows)]
pub fn specs(_dev_dir: &Path, a: &AudioSettings) -> Vec<InputSpec> {
    let mut v = vec![InputSpec {
        name: GAME_CAPTURE,
        kind: "game_capture",
        settings: json!({ "capture_mode": "window", "window": "", "priority": 2,
                          "capture_cursor": true, "capture_audio": false }),
        tracks: &[],
        fit: true,
        volume: 1.0,
        filters: vec![],
    }];
    match a.game_mode {
        GameAudioMode::Game => v.push(InputSpec {
            name: GAME_AUDIO,
            kind: "wasapi_process_output_capture",
            settings: json!({ "priority": 2 }),
            tracks: &[1, 2],
            fit: false,
            volume: a.game_volume,
            filters: vec![],
        }),
        GameAudioMode::Desktop => v.push(InputSpec {
            name: GAME_AUDIO,
            kind: "wasapi_output_capture",
            settings: json!({ "device_id": a.desktop_device }),
            tracks: &[1, 2],
            fit: false,
            volume: a.game_volume,
            filters: vec![],
        }),
        GameAudioMode::Off => {}
    }
    if a.discord_enabled {
        v.push(InputSpec {
            name: DISCORD,
            kind: "wasapi_process_output_capture",
            settings: json!({ "window": "Discord:Chrome_WidgetWin_1:Discord.exe", "priority": 2 }),
            tracks: &[1, 3],
            fit: false,
            volume: a.discord_volume,
            filters: vec![],
        });
    }
    if a.mic_enabled {
        v.push(InputSpec {
            name: MIC,
            kind: "wasapi_input_capture",
            settings: json!({ "device_id": a.mic_device }),
            tracks: &[1, 4],
            fit: false,
            volume: a.mic_volume,
            filters: if a.mic_noise_suppression { vec![rnnoise()] } else { vec![] },
        });
    }
    v
}

/// Linux development stand-ins: looping test files instead of real capture,
/// so nothing on the developer's screen or microphone gets recorded. Device
/// choices are ignored here; on/off, volume and noise suppression apply.
#[cfg(not(windows))]
pub fn specs(dev_dir: &Path, a: &AudioSettings) -> Vec<InputSpec> {
    let file = |n: &str| dev_dir.join(n).to_string_lossy().into_owned();
    let media = |f: String| json!({ "local_file": f, "is_local_file": true, "looping": true,
                                    "restart_on_activate": false, "close_when_inactive": false });
    let mut v = vec![InputSpec {
        name: GAME_CAPTURE,
        kind: "ffmpeg_source",
        settings: media(file("dev-game.mp4")),
        tracks: if a.game_mode == GameAudioMode::Off { &[] } else { &[1, 2] },
        fit: true,
        volume: a.game_volume,
        filters: vec![],
    }];
    if a.discord_enabled {
        v.push(InputSpec {
            name: DISCORD,
            kind: "ffmpeg_source",
            settings: media(file("dev-discord.m4a")),
            tracks: &[1, 3],
            fit: false,
            volume: a.discord_volume,
            filters: vec![],
        });
    }
    if a.mic_enabled {
        v.push(InputSpec {
            name: MIC,
            kind: "ffmpeg_source",
            settings: media(file("dev-mic.m4a")),
            tracks: &[1, 4],
            fit: false,
            volume: a.mic_volume,
            filters: if a.mic_noise_suppression { vec![rnnoise()] } else { vec![] },
        });
    }
    v
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

    // Switched-off sources go away entirely (their track stays silent).
    for name in MANAGED_AUDIO {
        if existing.contains_key(name) && !specs.iter().any(|s| s.name == name) {
            obs.call("RemoveInput", json!({ "inputName": name })).await?;
        }
    }

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

        // Media stand-ins carry audio even when no track is wanted: set all six.
        let tracks: serde_json::Map<String, Value> =
            (1..=6u8).map(|t| (t.to_string(), spec.tracks.contains(&t).into())).collect();
        if !spec.tracks.is_empty() || spec.kind == "ffmpeg_source" {
            obs.call("SetInputAudioTracks",
                json!({ "inputName": spec.name, "inputAudioTracks": tracks })).await?;
            obs.call("SetInputVolume",
                json!({ "inputName": spec.name, "inputVolumeMul": spec.volume.clamp(0.0, 20.0) })).await?;
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
        if have.iter().any(|n| n == NOISE_FILTER) && !spec.filters.iter().any(|f| f.0 == NOISE_FILTER) {
            obs.call("RemoveSourceFilter",
                json!({ "sourceName": spec.name, "filterName": NOISE_FILTER })).await?;
        }
    }
    Ok(())
}

#[derive(Serialize, Clone)]
pub struct Device {
    pub id: String,
    pub name: String,
}

/// Audio devices as OBS sees them (ids are what OBS stores as `device_id`).
/// Uses a short-lived probe input that is never shown or mixed.
pub async fn list_devices(obs: &ObsClient, output: bool) -> Result<Vec<Device>> {
    // OBS removes inputs asynchronously: a unique name avoids clashing with
    // a probe from a moment ago.
    let stamp = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap_or_default().as_millis();
    let (name, kind) = if output {
        (format!("_probe_output_{stamp}"), output_device_kind())
    } else {
        (format!("_probe_input_{stamp}"), input_device_kind())
    };
    let name = name.as_str();
    obs.call("CreateInput", json!({
        "sceneName": SCENE, "inputName": name, "inputKind": kind,
        "inputSettings": {}, "sceneItemEnabled": false,
    }))
    .await?;
    let _ = obs.call("SetInputMute", json!({ "inputName": name, "inputMuted": true })).await;
    let items = obs
        .call("GetInputPropertiesListPropertyItems", json!({ "inputName": name, "propertyName": "device_id" }))
        .await;
    let _ = obs.call("RemoveInput", json!({ "inputName": name })).await;
    Ok(items?["propertyItems"]
        .as_array()
        .into_iter()
        .flatten()
        .filter(|i| i["itemEnabled"].as_bool().unwrap_or(true))
        .filter_map(|i| Some(Device {
            id: i["itemValue"].as_str()?.to_string(),
            name: i["itemName"].as_str()?.to_string(),
        }))
        .collect())
}

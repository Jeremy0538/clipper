//! Where our private OBS lives and the config files we write before every start,
//! so OBS never shows its first-run wizard and always uses our profile.

use anyhow::Result;
use rand::distr::{Alphanumeric, SampleString};
use std::{
    fs,
    path::{Path, PathBuf},
};

pub const PROFILE: &str = "Clipper";
pub const COLLECTION: &str = "Clipper";
pub const SCENE: &str = "Clipper";
pub const WS_PORT: u16 = 4466; // not the default 4455, so a user's own OBS doesn't clash

#[derive(Clone, Debug)]
pub struct Settings {
    pub buffer_secs: u32,
    pub fps: u32,
    pub width: u32,
    pub height: u32,
    pub bitrate_kbps: u32,
    pub audio: crate::settings::AudioSettings,
    /// "high" | "balanced" | "small"
    pub quality: String,
    /// The GPU encoder failed to start: use x264 on the CPU.
    pub force_cpu: bool,
}

impl Default for Settings {
    fn default() -> Self {
        Self { buffer_secs: 120, fps: 60, width: 1920, height: 1080, bitrate_kbps: 40_000, audio: Default::default(), quality: "balanced".into(), force_cpu: false }
    }
}

#[derive(Clone, Debug)]
pub struct ObsPaths {
    /// The OBS executable.
    pub exe: PathBuf,
    /// Working directory OBS must be started in.
    pub cwd: PathBuf,
    /// The `obs-studio` config directory OBS will read.
    pub config: PathBuf,
    /// Linux only: XDG_CONFIG_HOME pointing at our private config.
    pub xdg_config_home: Option<PathBuf>,
    /// Extra args (Windows: `--portable`).
    pub extra_args: Vec<String>,
}

impl ObsPaths {
    /// Windows: portable OBS unpacked in `<data>/obs` (config under `<data>/obs/config`).
    /// Linux (development): the system `obs` with a private XDG_CONFIG_HOME.
    pub fn resolve(data_dir: &Path) -> Self {
        if cfg!(windows) {
            let root = data_dir.join("obs");
            let bin = root.join("bin").join("64bit");
            Self {
                exe: bin.join("obs64.exe"),
                cwd: bin,
                config: root.join("config").join("obs-studio"),
                xdg_config_home: None,
                extra_args: vec!["--portable".into()],
            }
        } else {
            let xdg = data_dir.join("obs-config");
            Self {
                exe: "obs".into(),
                cwd: data_dir.to_path_buf(),
                config: xdg.join("obs-studio"),
                xdg_config_home: Some(xdg),
                extra_args: vec![],
            }
        }
    }
}

/// Paths in OBS ini files use forward slashes on every platform.
fn ini_path(p: &Path) -> String {
    p.to_string_lossy().replace('\\', "/")
}

/// Same resolution for every level; only the bitrate changes. VBR spends bits
/// where the picture needs them, so "balanced" is about half of "high".
pub fn vendor(s: &Settings) -> crate::gpu::Vendor {
    if s.force_cpu { crate::gpu::Vendor::Cpu } else { crate::gpu::detect() }
}

fn encoder_settings(s: &Settings) -> serde_json::Value {
    use crate::gpu::Vendor;
    let (rc, bitrate, max) = match s.quality.as_str() {
        "high" => ("CBR", s.bitrate_kbps, s.bitrate_kbps),
        "small" => ("VBR", 12_000, 20_000),
        _ => ("VBR", 20_000, 35_000),
    };
    match vendor(s) {
        Vendor::Nvidia => serde_json::json!({
            "rate_control": rc, "bitrate": bitrate, "max_bitrate": max,
            "keyint_sec": 1, "preset2": "p5", "profile": "high", "bf": 2, "lookahead": true,
        }),
        Vendor::Amd => serde_json::json!({
            "rate_control": rc, "bitrate": bitrate, "max_bitrate": max,
            "keyint_sec": 1, "preset": "quality", "profile": "high", "bf": 2,
        }),
        Vendor::Intel => serde_json::json!({
            "rate_control": rc, "bitrate": bitrate, "max_bitrate": max,
            "keyint_sec": 1, "target_usage": "TU4", "profile": "high",
        }),
        // CPU: quality-based so a slower PC doesn't also get huge files.
        Vendor::Cpu => serde_json::json!({
            "rate_control": "CRF",
            "crf": match s.quality.as_str() { "high" => 18, "small" => 26, _ => 22 },
            "keyint_sec": 1, "preset": "veryfast", "profile": "high",
        }),
    }
}

/// Websocket password: generated once, kept in our data dir.
pub fn ws_password(data_dir: &Path) -> Result<String> {
    let f = data_dir.join("obs-ws-password");
    if let Ok(p) = fs::read_to_string(&f) {
        if p.trim().len() >= 16 {
            return Ok(p.trim().to_string());
        }
    }
    let p = Alphanumeric.sample_string(&mut rand::rng(), 24);
    fs::write(&f, &p)?;
    Ok(p)
}

pub fn write_all(paths: &ObsPaths, s: &Settings, incoming: &Path, password: &str) -> Result<()> {
    let c = &paths.config;
    let profile = c.join("basic").join("profiles").join(PROFILE);
    let scenes = c.join("basic").join("scenes");
    let ws = c.join("plugin_config").join("obs-websocket");
    for d in [&profile, &scenes, &ws] {
        fs::create_dir_all(d)?;
    }

    // OBS 31+ keeps UI state in user.ini; FirstRun=true skips the auto-config wizard.
    fs::write(
        c.join("user.ini"),
        format!(
            "[General]\nFirstRun=true\nConfirmOnExit=false\nHotkeyFocusType=NeverDisableHotkeys\n\
             Pre19Defaults=false\nPre21Defaults=false\nPre23Defaults=false\nPre24.1Defaults=false\n\n\
             [BasicWindow]\nSysTrayEnabled=true\nSysTrayWhenStarted=true\nSysTrayMinimizeToTray=true\n\
             WarnBeforeStartingReplayBuffer=false\nWarnBeforeStoppingReplayBuffer=false\nPreviewEnabled=false\n\n\
             [Basic]\nProfile={PROFILE}\nProfileDir={PROFILE}\nSceneCollection={COLLECTION}\n\
             SceneCollectionFile={COLLECTION}.json\nConfigOnNewProfile=false\n"
        ),
    )?;

    fs::write(
        ws.join("config.json"),
        serde_json::to_string_pretty(&serde_json::json!({
            "alerts_enabled": false,
            "auth_required": true,
            "first_load": false,
            "server_enabled": true,
            "server_password": password,
            "server_port": WS_PORT,
        }))?,
    )?;

    // Advanced output mode: replay buffer uses the recording encoder + tracks 1-4.
    fs::write(
        profile.join("basic.ini"),
        format!(
            "[General]\nName={PROFILE}\n\n\
             [Output]\nMode=Advanced\nFilenameFormatting=Clip %CCYY-%MM-%DD %hh-%mm-%ss\nDelayEnable=false\n\n\
             [AdvOut]\nRecType=Standard\nRecFilePath={rec}\nRecFormat2=mp4\nRecEncoder={encoder}\n\
             RecTracks={tracks}\nRecRB=true\nRecRBTime={secs}\nRecRBSize=8192\nRecRBPrefix=Replay\n\
             Track1Name=Mix\nTrack2Name=Game\nTrack3Name=Discord\nTrack4Name=Mic\n\n\
             [Video]\nBaseCX={w}\nBaseCY={h}\nOutputCX={w}\nOutputCY={h}\nFPSType=0\nFPSCommon={fps}\n\n\
             [Audio]\nSampleRate=48000\nChannelSetup=Stereo\n",
            rec = ini_path(incoming),
            tracks = super::setup::track_mask(&s.audio),
            encoder = vendor(s).obs_encoder(),
            secs = s.buffer_secs,
            w = s.width,
            h = s.height,
            fps = s.fps,
        ),
    )?;
    // 1 s keyframes keep trimming/seeking in the editor snappy.
    fs::write(
        profile.join("recordEncoder.json"),
        serde_json::to_string(&encoder_settings(s))?,
    )?;

    write_mono_script(c)?;

    // An empty scene; inputs are (re)created over the websocket on every start.
    let collection = scenes.join(format!("{COLLECTION}.json"));
    if !collection.exists() {
        fs::write(
            &collection,
            serde_json::to_string_pretty(&serde_json::json!({
                "name": COLLECTION,
                "current_scene": SCENE,
                "current_program_scene": SCENE,
                "scene_order": [{ "name": SCENE }],
                "sources": [{
                    "id": "scene", "versioned_id": "scene", "name": SCENE,
                    "settings": { "items": [], "id_counter": 0, "custom_size": false },
                    "enabled": true, "mixers": 0, "volume": 1.0, "muted": false, "flags": 0
                }],
                "transitions": [], "groups": [], "quick_transitions": []
            }))?,
        )?;
    }
    register_script(&collection, &c.join(MONO_SCRIPT))?;
    Ok(())
}

/// OBS script (Lua is built into OBS) that applies the mono setting of the Mic.
pub const MONO_SCRIPT: &str = "clipper-mono.lua";

fn write_mono_script(config: &Path) -> Result<()> {
    let script = format!(
        r#"-- Written by Clipper on every start. Applies "Downmix to mono" to the
-- Mic input, which the websocket cannot set: Clipper puts the wish in the
-- input's settings ("{key}").
obs = obslua
local MONO = 2 -- OBS_SOURCE_FLAG_FORCE_MONO

function script_description() return "Clipper: microphone mono setting" end

local function tick()
  local src = obs.obs_get_source_by_name("{mic}")
  if src == nil then return end
  local s = obs.obs_source_get_settings(src)
  local want = obs.obs_data_get_bool(s, "{key}")
  obs.obs_data_release(s)
  local flags = obs.obs_source_get_flags(src)
  local has = math.floor(flags / MONO) % 2 == 1
  if want ~= has then
    obs.obs_source_set_flags(src, want and flags + MONO or flags - MONO)
    obs.script_log(obs.LOG_INFO, "Mic mono " .. (want and "on" or "off"))
  end
  obs.obs_source_release(src)
end

function script_load(settings) obs.timer_add(tick, 500) end
function script_unload() obs.timer_remove(tick) end
"#,
        key = super::setup::MONO_KEY,
        mic = super::setup::MIC,
    );
    fs::write(config.join(MONO_SCRIPT), script)?;
    Ok(())
}

/// Scripts are loaded per scene collection (OBS's "scripts-tool" module data).
fn register_script(collection: &Path, script: &Path) -> Result<()> {
    let mut doc: serde_json::Value = serde_json::from_str(&fs::read_to_string(collection)?)?;
    let path = ini_path(script);
    if !doc["modules"].is_object() {
        doc["modules"] = serde_json::json!({});
    }
    let list = &mut doc["modules"]["scripts-tool"];
    if !list.is_array() {
        *list = serde_json::json!([]);
    }
    let arr = list.as_array_mut().unwrap();
    if arr.iter().any(|e| e["path"] == path.as_str()) {
        return Ok(());
    }
    arr.retain(|e| !e["path"].as_str().is_some_and(|p| p.ends_with(MONO_SCRIPT)));
    arr.push(serde_json::json!({ "path": path, "settings": {} }));
    fs::write(collection, serde_json::to_string_pretty(&doc)?)?;
    Ok(())
}

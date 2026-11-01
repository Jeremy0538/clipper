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
}

impl Default for Settings {
    fn default() -> Self {
        Self { buffer_secs: 120, fps: 60, width: 1920, height: 1080, bitrate_kbps: 40_000, audio: Default::default() }
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
             [AdvOut]\nRecType=Standard\nRecFilePath={rec}\nRecFormat2=mp4\nRecEncoder=obs_nvenc_h264_tex\n\
             RecTracks=15\nRecRB=true\nRecRBTime={secs}\nRecRBSize=8192\nRecRBPrefix=Replay\n\
             Track1Name=Mix\nTrack2Name=Game\nTrack3Name=Discord\nTrack4Name=Mic\n\n\
             [Video]\nBaseCX={w}\nBaseCY={h}\nOutputCX={w}\nOutputCY={h}\nFPSType=0\nFPSCommon={fps}\n\n\
             [Audio]\nSampleRate=48000\nChannelSetup=Stereo\n",
            rec = ini_path(incoming),
            secs = s.buffer_secs,
            w = s.width,
            h = s.height,
            fps = s.fps,
        ),
    )?;
    // 1 s keyframes keep trimming/seeking in the editor snappy.
    fs::write(
        profile.join("recordEncoder.json"),
        serde_json::to_string(&serde_json::json!({
            "rate_control": "CBR",
            "bitrate": s.bitrate_kbps,
            "keyint_sec": 1,
            "preset2": "p5",
            "profile": "high",
        }))?,
    )?;

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
    Ok(())
}

//! User settings, stored as settings.json in the data folder.

use crate::{games::AppRef, hotkey::Hotkey};
use serde::{Deserialize, Serialize};
use std::path::Path;

pub const MIN_BUFFER_SECS: u32 = 10;
pub const MAX_BUFFER_SECS: u32 = 300;

#[derive(Serialize, Deserialize, Clone, Debug)]
#[serde(default)]
pub struct UserSettings {
    pub hotkey: Hotkey,
    /// Start/stop a normal recording.
    pub record_hotkey: Hotkey,
    /// Save a screenshot of what is being captured.
    pub screenshot_hotkey: Hotkey,
    /// Delete the oldest clips above this size (GB, 0 = off). Starred ones stay.
    pub storage_limit_gb: f64,
    pub buffer_secs: u32,
    /// Chime when a clip is saved.
    pub sound: bool,
    /// Small corner pop-ups (clip saved, clipping on, game detected, errors).
    pub toast: bool,
    pub audio: AudioSettings,
    /// Games the user switched off: no buffer while they run.
    pub disabled_games: Vec<String>,
    /// Apps the user added as clip applications (treated like games).
    pub custom_games: Vec<AppRef>,
    /// Start hidden in the tray when Windows starts.
    pub autostart: bool,
    /// Offer test builds (pre-releases) as updates.
    pub test_builds: bool,
    /// "system" | "light" | "dark" | "oled"
    pub theme: String,
    /// Screen mode: "primary", "all" or an OBS monitor id.
    pub screen_monitor: String,
    /// "high" | "balanced" | "small"
    pub quality: String,
    /// The first-start setup guide was finished (or skipped).
    pub setup_done: bool,
    /// Clips folder chosen by the user ("" = Videos\Clipper).
    pub clips_dir: String,
    /// The version whose "What's new" was shown last.
    pub last_version: String,
    /// An earlier clips folder that still holds clips (not moved, or a move
    /// that could not finish); Settings offers to move them.
    pub leftover_clips_dir: String,
}

#[derive(Serialize, Deserialize, Clone, Copy, Debug, PartialEq, Default)]
#[serde(rename_all = "lowercase")]
pub enum GameAudioMode {
    /// Only the detected game's own sound (application capture).
    #[default]
    Game,
    /// Everything the chosen output device plays.
    Desktop,
    Off,
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
#[serde(default)]
pub struct AudioSettings {
    pub game_mode: GameAudioMode,
    /// Output device for Desktop mode ("default" = Windows default).
    pub desktop_device: String,
    pub game_volume: f64,
    pub discord_enabled: bool,
    pub discord_volume: f64,
    pub mic_enabled: bool,
    /// Input device id ("default" = Windows default recording device).
    pub mic_device: String,
    pub mic_volume: f64,
    pub mic_noise_suppression: bool,
    /// Extra gain on the microphone in dB (0..=30), before a limiter.
    pub mic_boost_db: f64,
    /// Mix the microphone down to mono: for mics that only come out on the
    /// left (or right) side of a clip.
    pub mic_mono: bool,
    /// Up to two extra apps recorded on their own tracks (5 and 6).
    pub apps: Vec<AppRef>,
    /// Volume per entry in `apps` (1.0 = as played).
    pub app_volumes: Vec<f64>,
}

impl Default for AudioSettings {
    fn default() -> Self {
        Self {
            game_mode: GameAudioMode::Game,
            desktop_device: "default".into(),
            game_volume: 1.0,
            discord_enabled: true,
            discord_volume: 1.0,
            mic_enabled: true,
            mic_device: "default".into(),
            mic_volume: 1.0,
            mic_noise_suppression: true,
            mic_boost_db: 0.0,
            mic_mono: false,
            apps: vec![],
            app_volumes: vec![],
        }
    }
}

impl Default for UserSettings {
    fn default() -> Self {
        Self { hotkey: Hotkey::default(), record_hotkey: Hotkey::default_record(), screenshot_hotkey: Hotkey::default_screenshot(), storage_limit_gb: 0.0, buffer_secs: 120, sound: true, toast: true, audio: AudioSettings::default(), disabled_games: vec![], custom_games: vec![], autostart: true, test_builds: false, theme: "system".into(), screen_monitor: "primary".into(), quality: "balanced".into(), setup_done: false, clips_dir: String::new(), last_version: String::new(), leftover_clips_dir: String::new() }
    }
}

impl UserSettings {
    pub fn load(file: &Path) -> Self {
        let mut s: Self = match std::fs::read_to_string(file) {
            Err(_) => Self::default(), // first start
            Ok(t) => match serde_json::from_str(&t) {
                Ok(s) => s,
                Err(e) => {
                    // Keep the damaged file for the log/report instead of losing it on the next save.
                    let _ = std::fs::copy(file, file.with_extension("broken.json"));
                    crate::clog!("ERROR settings.json unreadable ({e}); using defaults, copy kept as settings.broken.json");
                    Self::default()
                }
            },
        };
        if s.hotkey.vk().is_none() {
            s.hotkey = Hotkey::default();
        }
        if s.record_hotkey.vk().is_none() {
            s.record_hotkey = Hotkey::default_record();
        }
        if s.screenshot_hotkey.vk().is_none() {
            s.screenshot_hotkey = Hotkey::default_screenshot();
        }
        s.buffer_secs = s.buffer_secs.clamp(MIN_BUFFER_SECS, MAX_BUFFER_SECS);
        s
    }

    /// Write to a temp file and rename it over the old one, so a crash or power
    /// loss mid-write never leaves a half settings.json.
    pub fn save(&self, file: &Path) -> std::io::Result<()> {
        let tmp = file.with_extension("json.tmp");
        std::fs::write(&tmp, serde_json::to_string_pretty(self)?)?;
        std::fs::rename(&tmp, file)
    }
}

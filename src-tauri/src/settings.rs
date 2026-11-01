//! User settings, stored as settings.json in the data folder.

use crate::hotkey::Hotkey;
use serde::{Deserialize, Serialize};
use std::path::Path;

pub const MIN_BUFFER_SECS: u32 = 10;
pub const MAX_BUFFER_SECS: u32 = 300;

#[derive(Serialize, Deserialize, Clone, Debug)]
#[serde(default)]
pub struct UserSettings {
    pub hotkey: Hotkey,
    pub buffer_secs: u32,
    /// Chime when a clip is saved.
    pub sound: bool,
    /// Small corner pop-ups (clip saved, game detected, errors) — debug aid.
    pub toast: bool,
    pub audio: AudioSettings,
    /// Games the user switched off: no buffer while they run.
    pub disabled_games: Vec<String>,
    /// Start hidden in the tray when Windows starts.
    pub autostart: bool,
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
        }
    }
}

impl Default for UserSettings {
    fn default() -> Self {
        Self { hotkey: Hotkey::default(), buffer_secs: 120, sound: true, toast: true, audio: AudioSettings::default(), disabled_games: vec![], autostart: true }
    }
}

impl UserSettings {
    pub fn load(file: &Path) -> Self {
        let mut s: Self = std::fs::read_to_string(file)
            .ok()
            .and_then(|t| serde_json::from_str(&t).ok())
            .unwrap_or_default();
        if s.hotkey.vk().is_none() {
            s.hotkey = Hotkey::default();
        }
        s.buffer_secs = s.buffer_secs.clamp(MIN_BUFFER_SECS, MAX_BUFFER_SECS);
        s
    }

    pub fn save(&self, file: &Path) -> std::io::Result<()> {
        std::fs::write(file, serde_json::to_string_pretty(self)?)
    }
}

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
}

impl Default for UserSettings {
    fn default() -> Self {
        Self { hotkey: Hotkey::default(), buffer_secs: 120, sound: true, toast: true }
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

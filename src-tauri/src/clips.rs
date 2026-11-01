//! The clips folder: moving fresh replays in with a readable name, listing.

use anyhow::Result;
use serde::Serialize;
use std::{
    fs,
    path::{Path, PathBuf},
    time::UNIX_EPOCH,
};

#[derive(Serialize, Clone)]
pub struct ClipInfo {
    pub path: String,
    pub name: String,
    pub size: u64,
    /// Seconds since epoch.
    pub modified: u64,
}

/// Characters Windows forbids in file names.
fn sanitize(s: &str) -> String {
    let cleaned: String = s
        .chars()
        .filter(|c| !"™®©".contains(*c))
        .map(|c| if "<>:\"/\\|?*".contains(c) || c.is_control() { ' ' } else { c })
        .collect();
    let trimmed = cleaned.split_whitespace().collect::<Vec<_>>().join(" ");
    if trimmed.is_empty() { "Clip".into() } else { trimmed }
}

/// Move a replay from OBS's incoming folder into the clips folder as
/// "<Game> YYYY-MM-DD HH-MM-SS.mp4".
pub fn adopt(src: &Path, clips_dir: &Path, game: Option<&str>) -> Result<PathBuf> {
    fs::create_dir_all(clips_dir)?;
    let stamp = chrono::Local::now().format("%Y-%m-%d %H-%M-%S");
    let base = format!("{} {stamp}", sanitize(game.unwrap_or("Clip")));
    let ext = src.extension().and_then(|e| e.to_str()).unwrap_or("mp4");
    let mut dst = clips_dir.join(format!("{base}.{ext}"));
    let mut n = 2;
    while dst.exists() {
        dst = clips_dir.join(format!("{base} ({n}).{ext}"));
        n += 1;
    }
    if fs::rename(src, &dst).is_err() {
        // Different drive: copy, then remove.
        fs::copy(src, &dst)?;
        fs::remove_file(src)?;
    }
    Ok(dst)
}

pub fn list(clips_dir: &Path) -> Vec<ClipInfo> {
    let mut out: Vec<ClipInfo> = fs::read_dir(clips_dir)
        .into_iter()
        .flatten()
        .flatten()
        .filter_map(|e| {
            let p = e.path();
            let ext = p.extension()?.to_str()?.to_ascii_lowercase();
            if !matches!(ext.as_str(), "mp4" | "mkv" | "mov") {
                return None;
            }
            let md = e.metadata().ok()?;
            Some(ClipInfo {
                name: p.file_stem()?.to_string_lossy().into_owned(),
                path: p.to_string_lossy().into_owned(),
                size: md.len(),
                modified: md.modified().ok()?.duration_since(UNIX_EPOCH).ok()?.as_secs(),
            })
        })
        .collect();
    out.sort_by(|a, b| b.modified.cmp(&a.modified));
    out
}

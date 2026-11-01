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

/// A clip path from the UI, only if it really is a file inside the clips folder.
pub fn checked(clips_dir: &Path, path: &str) -> Result<PathBuf, String> {
    let dir = fs::canonicalize(clips_dir).map_err(|e| e.to_string())?;
    let p = fs::canonicalize(path).map_err(|_| "That clip no longer exists".to_string())?;
    if p.parent() != Some(dir.as_path()) || !p.is_file() {
        return Err("Not a clip in the clips folder".into());
    }
    Ok(p)
}

pub fn trash(p: &Path) -> Result<(), String> {
    trash::delete(p).map_err(|e| e.to_string())
}

/// Put the file itself on the clipboard (paste into Discord / Explorer).
pub fn copy_file(p: &Path) -> Result<(), String> {
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        const NO_WINDOW: u32 = 0x0800_0000;
        let lit = p.to_string_lossy().replace('\'', "''");
        let ok = std::process::Command::new("powershell")
            .args(["-NoProfile", "-NonInteractive", "-Command", &format!("Set-Clipboard -LiteralPath '{lit}'")])
            .creation_flags(NO_WINDOW)
            .status()
            .map_err(|e| e.to_string())?
            .success();
        if ok { Ok(()) } else { Err("Copy failed".into()) }
    }
    #[cfg(not(windows))]
    {
        let uri = format!("file://{}", p.display());
        let ok = std::process::Command::new("wl-copy")
            .args(["--type", "text/uri-list", &uri])
            .status()
            .map_err(|e| e.to_string())?
            .success();
        if ok { Ok(()) } else { Err("Copy failed".into()) }
    }
}

pub fn reveal(p: &Path) -> Result<(), String> {
    #[cfg(windows)]
    {
        std::process::Command::new("explorer")
            .arg(format!("/select,{}", p.display()))
            .spawn()
            .map_err(|e| e.to_string())?;
    }
    #[cfg(not(windows))]
    {
        std::process::Command::new("xdg-open")
            .arg(p.parent().unwrap_or(p))
            .spawn()
            .map_err(|e| e.to_string())?;
    }
    Ok(())
}

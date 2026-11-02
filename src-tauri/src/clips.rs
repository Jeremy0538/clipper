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
    /// Game folder the clip lives in ("" = directly in the clips folder).
    pub folder: String,
    /// "video" or "image" (screenshot).
    pub kind: String,
    /// Starred: kept by the storage limit.
    pub favorite: bool,
}

/// Characters Windows forbids in file names.
pub fn sanitize(s: &str) -> String {
    let cleaned: String = s
        .chars()
        .filter(|c| !"™®©".contains(*c))
        .map(|c| if "<>:\"/\\|?*".contains(c) || c.is_control() { ' ' } else { c })
        .collect();
    let trimmed = cleaned.split_whitespace().collect::<Vec<_>>().join(" ");
    if trimmed.is_empty() { "Clip".into() } else { trimmed }
}

/// Move a replay from OBS's incoming folder into the game's own folder:
/// "<clips>/<Game>/<Game> YYYY-MM-DD HH-MM-SS.mp4".
pub fn adopt(src: &Path, clips_dir: &Path, game: Option<&str>) -> Result<PathBuf> {
    let name = sanitize(game.unwrap_or("Clip"));
    let clips_dir = &clips_dir.join(&name);
    fs::create_dir_all(clips_dir)?;
    let stamp = chrono::Local::now().format("%Y-%m-%d %H-%M-%S");
    let base = format!("{name} {stamp}");
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

fn videos_in(dir: &Path, folder: &str) -> Vec<ClipInfo> {
    fs::read_dir(dir)
        .into_iter()
        .flatten()
        .flatten()
        .filter_map(|e| {
            let p = e.path();
            let ext = p.extension()?.to_str()?.to_ascii_lowercase();
            let kind = match ext.as_str() {
                "mp4" | "mkv" | "mov" if !p.to_string_lossy().ends_with(".part.mp4") => "video",
                "png" | "jpg" => "image",
                _ => return None,
            };
            let md = e.metadata().ok()?;
            Some(ClipInfo {
                name: p.file_stem()?.to_string_lossy().into_owned(),
                path: p.to_string_lossy().into_owned(),
                size: md.len(),
                modified: md.modified().ok()?.duration_since(UNIX_EPOCH).ok()?.as_secs(),
                folder: folder.to_string(),
                kind: kind.to_string(),
                favorite: false,
            })
        })
        .collect()
}

/// All clips: directly in the clips folder and one level of game folders.
pub fn list(clips_dir: &Path) -> Vec<ClipInfo> {
    let mut out = videos_in(clips_dir, "");
    for e in fs::read_dir(clips_dir).into_iter().flatten().flatten() {
        let p = e.path();
        if p.is_dir() {
            let folder = e.file_name().to_string_lossy().into_owned();
            if !folder.starts_with('.') {
                out.extend(videos_in(&p, &folder));
            }
        }
    }
    out.sort_by(|a, b| b.modified.cmp(&a.modified));
    out
}

/// One-time tidy-up: clips saved before game folders existed move into the
/// folder of their game ("<Game> 2026-11-01 14-17-58.mp4" → "<Game>/").
pub fn sort_into_folders(clips_dir: &Path) {
    for c in videos_in(clips_dir, "").into_iter().filter(|c| c.kind == "video") {
        let game = crate::editor::base_name(&c.name);
        let dir = clips_dir.join(sanitize(&game));
        let src = PathBuf::from(&c.path);
        let Some(fname) = src.file_name() else { continue };
        let dst = dir.join(fname);
        if dst.exists() || fs::create_dir_all(&dir).is_err() {
            continue;
        }
        if fs::rename(&src, &dst).is_ok() {
            crate::clog!("moved {} into {}", c.name, dir.display());
        }
    }
}

/// Windows `canonicalize` returns "\\\\?\\C:\\..." paths, while the clip list uses
/// "C:\\...". Everything that stores or compares clip paths uses the plain form.
fn plain(p: &str) -> String {
    match p.strip_prefix(r"\\?\") {
        Some(rest) if !rest.starts_with("UNC\\") => rest.to_string(),
        _ => p.to_string(),
    }
}

/// A clip path from the UI, only if it really is a file inside the clips folder.
pub fn checked(clips_dir: &Path, path: &str) -> Result<PathBuf, String> {
    let dir = fs::canonicalize(clips_dir).map_err(|e| e.to_string())?;
    let p = fs::canonicalize(path).map_err(|_| "That clip no longer exists".to_string())?;
    let inside = p.parent() == Some(dir.as_path()) || p.parent().and_then(|d| d.parent()) == Some(dir.as_path());
    if !inside || !p.is_file() {
        return Err("Not a clip in the clips folder".into());
    }
    Ok(PathBuf::from(plain(&p.to_string_lossy())))
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
        // explorer parses its own command line: /select,"C:\path with spaces.mp4"
        // must not be wrapped in quotes as a whole (that opens Documents).
        use std::os::windows::process::CommandExt;
        std::process::Command::new("explorer")
            .raw_arg(format!("/select,\"{}\"", p.display()))
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

/// Rename a clip in place (same folder, same extension). Returns the new path.
pub fn rename(p: &Path, new_name: &str) -> Result<PathBuf, String> {
    let name = sanitize(new_name);
    let ext = p.extension().and_then(|e| e.to_str()).unwrap_or("mp4");
    let dst = p.with_file_name(format!("{name}.{ext}"));
    if dst == p {
        return Ok(dst);
    }
    let only_case = dst.to_string_lossy().to_lowercase() == p.to_string_lossy().to_lowercase();
    if dst.exists() && !only_case {
        return Err("A clip with that name already exists".into());
    }
    fs::rename(p, &dst).map_err(|e| e.to_string())?;
    Ok(dst)
}

/// Starred clips, stored as paths in <data>/favorites.json.
pub struct Favorites(PathBuf);

impl Favorites {
    pub fn new(data_dir: &Path) -> Self {
        Self(data_dir.join("favorites.json"))
    }
    pub fn load(&self) -> Vec<String> {
        let v: Vec<String> = fs::read_to_string(&self.0).ok().and_then(|s| serde_json::from_str(&s).ok()).unwrap_or_default();
        // Stars saved by 0.2.3-0.3.4 have the "\\\\?\\" form and never matched.
        v.iter().map(|p| plain(p)).collect()
    }
    fn save(&self, v: &[String]) {
        let _ = fs::write(&self.0, serde_json::to_string(v).unwrap_or_default());
    }
    pub fn set(&self, path: &str, on: bool) {
        let mut v = self.load();
        v.retain(|p| p != path);
        if on {
            v.push(path.to_string());
        }
        self.save(&v);
    }
    pub fn moved(&self, from: &str, to: &str) {
        let mut v = self.load();
        if let Some(p) = v.iter_mut().find(|p| *p == from) {
            *p = to.to_string();
            self.save(&v);
        }
    }
}

/// Storage limit: delete the oldest clips (never starred ones) until the
/// clips folder is under `limit_gb`. Returns how many were deleted.
pub fn enforce_limit(clips_dir: &Path, limit_gb: f64, favorites: &[String]) -> usize {
    if limit_gb <= 0.0 {
        return 0;
    }
    let limit = (limit_gb * 1e9) as u64;
    let mut all = list(clips_dir);
    let mut total: u64 = all.iter().map(|c| c.size).sum();
    all.sort_by_key(|c| c.modified); // oldest first
    let mut n = 0;
    for c in all {
        if total <= limit {
            break;
        }
        if favorites.contains(&c.path) {
            continue;
        }
        if fs::remove_file(&c.path).is_ok() {
            total = total.saturating_sub(c.size);
            n += 1;
            crate::clog!("storage limit: deleted {}", c.path);
        }
    }
    n
}

#[cfg(test)]
mod plain_tests {
    #[test]
    fn strips_verbatim_prefix() {
        assert_eq!(super::plain(r"\\?\C:\Users\a\Videos\Clipper\x.mp4"), r"C:\Users\a\Videos\Clipper\x.mp4");
        assert_eq!(super::plain(r"C:\x.mp4"), r"C:\x.mp4");
        assert_eq!(super::plain(r"\\?\UNC\server\x.mp4"), r"\\?\UNC\server\x.mp4");
        assert_eq!(super::plain("/home/a/x.mp4"), "/home/a/x.mp4");
    }
}

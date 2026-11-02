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
    // Across drives (a buffer started before the clips folder moved) this is
    // copy + delete, without leaving half or double files.
    move_file(src, &dst)?;
    Ok(dst)
}

/// OBS's folder for fresh replays when the clips folder is a chosen one (same
/// drive). Folders starting with "." are not listed as games.
pub const INCOMING: &str = ".incoming";

/// Marks a folder as Clipper's own clips folder. Moving clips out of a folder
/// and the storage limit only touch folders that have it, so a folder full of
/// other files (a whole drive, the user's Videos) is never cleaned or emptied.
pub const MARKER: &str = ".clipper-clips";

pub fn is_ours(dir: &Path) -> bool {
    dir.join(MARKER).is_file()
}

pub fn mark(dir: &Path) -> std::io::Result<()> {
    let f = dir.join(MARKER);
    if f.is_file() {
        return Ok(());
    }
    fs::write(&f, "This folder is used by Clipper for clips. Don't delete this file.\r\n")?;
    #[cfg(windows)]
    {
        use std::os::windows::ffi::OsStrExt;
        let wide: Vec<u16> = f.as_os_str().encode_wide().chain(Some(0)).collect();
        unsafe {
            let _ = windows::Win32::Storage::FileSystem::SetFileAttributesW(
                windows::core::PCWSTR(wide.as_ptr()),
                windows::Win32::Storage::FileSystem::FILE_ATTRIBUTE_HIDDEN,
            );
        }
    }
    Ok(())
}

fn is_media(p: &Path) -> bool {
    p.extension().and_then(|e| e.to_str()).is_some_and(|e| {
        ["mp4", "mkv", "mov", "png", "jpg"].contains(&e.to_ascii_lowercase().as_str())
    })
}

/// Only what Clipper puts in a clips folder: clips and screenshots, directly
/// or one folder deep, plus its own "." folders. Used to mark a folder that
/// was chosen before markers existed (0.4.2/0.4.3).
pub fn looks_like_clips(dir: &Path) -> bool {
    let ok_file = |p: &Path| is_media(p) || p.file_name().is_some_and(|n| n == "desktop.ini");
    fs::read_dir(dir).is_ok_and(|r| {
        r.flatten().all(|e| {
            let p = e.path();
            if e.file_name().to_string_lossy().starts_with('.') {
                return true;
            }
            if p.is_dir() {
                fs::read_dir(&p).is_ok_and(|r| r.flatten().all(|f| f.path().is_file() && ok_file(&f.path())))
            } else {
                ok_file(&p)
            }
        })
    })
}

/// Nothing in it yet (Windows' own desktop.ini doesn't count).
pub fn is_empty(dir: &Path) -> bool {
    fs::read_dir(dir).map(|mut r| r.all(|e| e.is_ok_and(|e| e.file_name() == "desktop.ini"))).unwrap_or(true)
}

/// The folder clips really go into when the user picks `picked`: the folder
/// itself when it is empty or already Clipper's, else a "Clipper" folder in it
/// (picking D:\ gives D:\Clipper, not the whole drive).
pub fn target_for(picked: &Path) -> PathBuf {
    if !picked.exists() || is_ours(picked) || is_empty(picked) {
        picked.to_path_buf()
    } else {
        picked.join("Clipper")
    }
}

/// Move one file; across drives that is copy + delete.
fn move_file(src: &Path, dst: &Path) -> std::io::Result<()> {
    if fs::rename(src, dst).is_ok() {
        return Ok(());
    }
    if let Err(e) = fs::copy(src, dst) {
        let _ = fs::remove_file(dst); // no half copy (disk full, file in use)
        return Err(e);
    }
    fs::remove_file(src).inspect_err(|_| {
        let _ = fs::remove_file(dst); // keep one copy, the original
    })
}

/// Move everything in the clips folder (files and game folders, not ".…"
/// folders) into `to`, keeping stars. Existing names get " (2)". Returns
/// (moved, failed).
pub fn move_all(from: &Path, to: &Path, favs: &Favorites) -> (usize, usize) {
    let (mut moved, mut failed) = (0, 0);
    let mut renamed: Vec<(String, PathBuf)> = vec![];
    if !is_ours(from) {
        crate::clog!("not moving clips out of {}: not a Clipper folder", from.display());
        return (0, 0);
    }
    let mut one = |src: &Path, dir: &Path| {
        let _ = fs::create_dir_all(dir);
        let Some(name) = src.file_name() else { return };
        let mut dst = dir.join(name);
        let (stem, ext) = (
            src.file_stem().map(|s| s.to_string_lossy().into_owned()).unwrap_or_default(),
            src.extension().map(|e| format!(".{}", e.to_string_lossy())).unwrap_or_default(),
        );
        let mut n = 2;
        while dst.exists() {
            dst = dir.join(format!("{stem} ({n}){ext}"));
            n += 1;
        }
        let key = fav_key(&src.to_string_lossy());
        match move_file(src, &dst) {
            Ok(()) => {
                renamed.push((key, dst));
                moved += 1;
            }
            Err(e) => {
                crate::clog!("ERROR could not move {}: {e}", src.display());
                failed += 1;
            }
        }
    };
    for e in fs::read_dir(from).into_iter().flatten().flatten() {
        let p = e.path();
        let name = e.file_name().to_string_lossy().into_owned();
        if p.is_dir() {
            if name.starts_with('.') {
                continue;
            }
            for f in fs::read_dir(&p).into_iter().flatten().flatten() {
                if f.path().is_file() {
                    one(&f.path(), &to.join(&name));
                }
            }
            let _ = fs::remove_dir(&p); // only when empty
        } else if p.is_file() && name != "desktop.ini" && name != MARKER {
            one(&p, to);
        }
    }
    favs.moved_many(&renamed); // one read and one write, not one per clip
    (moved, failed)
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

/// One spelling per file for comparing clip paths: the real location (through
/// junctions/links), plain form, and on Windows case-insensitive. Starred clips
/// are stored and looked up by this key, so a clips folder behind a junction or
/// a differently cased path still finds them. Falls back to the plain path for
/// a file that no longer exists.
pub fn fav_key(p: &str) -> String {
    let k = fs::canonicalize(p).map(|c| plain(&c.to_string_lossy())).unwrap_or_else(|_| plain(p));
    if cfg!(windows) { k.to_lowercase() } else { k }
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
        // Keys (see fav_key); older files hold paths in other spellings.
        v.iter().map(|p| fav_key(p)).collect()
    }
    fn save(&self, v: &[String]) {
        let _ = fs::write(&self.0, serde_json::to_string(v).unwrap_or_default());
    }
    pub fn set(&self, path: &str, on: bool) {
        let key = fav_key(path);
        let mut v = self.load();
        v.retain(|p| *p != key);
        if on {
            v.push(key);
        }
        self.save(&v);
    }
    /// After a rename: `from_key` is fav_key of the old path, taken before the rename.
    pub fn moved(&self, from_key: &str, to: &str) {
        let mut v = self.load();
        if let Some(p) = v.iter_mut().find(|p| *p == from_key) {
            *p = fav_key(to);
            self.save(&v);
        }
    }
    /// After moving many files: (fav_key of the old path, new path).
    pub fn moved_many(&self, moves: &[(String, PathBuf)]) {
        let mut v = self.load();
        let mut changed = false;
        for (from_key, to) in moves {
            if let Some(p) = v.iter_mut().find(|p| *p == from_key) {
                *p = fav_key(&to.to_string_lossy());
                changed = true;
            }
        }
        if changed {
            self.save(&v);
        }
    }
    /// Is this clip (path as listed) starred? `favs` comes from load().
    pub fn has(favs: &[String], path: &str) -> bool {
        favs.contains(&fav_key(path))
    }
}

/// Storage limit: delete the oldest clips (never starred ones) until the
/// clips folder is under `limit_gb`. Returns how many were deleted.
pub fn enforce_limit(clips_dir: &Path, limit_gb: f64, favorites: &[String]) -> usize {
    if limit_gb <= 0.0 || !is_ours(clips_dir) {
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
        if Favorites::has(favorites, &c.path) {
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

#[cfg(test)]
mod fav_tests {
    #[cfg(unix)]
    #[test]
    fn star_found_through_a_link() {
        let base = std::env::temp_dir().join(format!("clipper-fav-{}", std::process::id()));
        let real = base.join("real");
        std::fs::create_dir_all(&real).unwrap();
        std::fs::write(real.join("a.mp4"), b"x").unwrap();
        let link = base.join("link");
        std::os::unix::fs::symlink(&real, &link).unwrap();
        let favs = super::Favorites::new(&base);
        // Starred through the real path (what checked() returns)...
        favs.set(&real.join("a.mp4").to_string_lossy(), true);
        // ...found through the link path (what the clip list shows).
        let loaded = favs.load();
        assert!(super::Favorites::has(&loaded, &link.join("a.mp4").to_string_lossy()));
        // And the storage limit keeps it (in a folder that is Clipper's).
        super::mark(&real).unwrap();
        std::fs::write(real.join("b.mp4"), b"y").unwrap();
        assert_eq!(super::enforce_limit(&link, 0.000_000_000_1, &loaded), 1, "the unstarred one goes");
        assert!(!real.join("b.mp4").exists());
        assert!(real.join("a.mp4").exists());
        assert_eq!(super::target_for(&base), base.join("Clipper"), "a folder with other things in it");
        assert!(super::looks_like_clips(&real));
        std::fs::write(base.join("notes.txt"), b"x").unwrap();
        assert!(!super::looks_like_clips(&base));
        assert_eq!(super::target_for(&base.join("new")), base.join("new"));
        std::fs::remove_dir_all(base).unwrap();
    }
}

#[cfg(test)]
mod move_tests {
    #[test]
    fn moves_clips_and_stars() {
        let base = std::env::temp_dir().join(format!("clipper-move-{}", std::process::id()));
        let (from, to, data) = (base.join("a"), base.join("b"), base.join("data"));
        for d in [from.join("Game"), from.join(".incoming"), to.join("Game"), data.clone()] {
            std::fs::create_dir_all(d).unwrap();
        }
        std::fs::write(from.join("Game/x.mp4"), "1").unwrap();
        std::fs::write(to.join("Game/x.mp4"), "old").unwrap();
        std::fs::write(from.join("y.png"), "2").unwrap();
        std::fs::write(from.join(".incoming/busy.mp4"), "3").unwrap();
        let favs = super::Favorites::new(&data);
        favs.set(&from.join("Game/x.mp4").to_string_lossy(), true);
        assert_eq!(super::move_all(&from, &to, &favs), (0, 0), "not marked: not ours");
        super::mark(&from).unwrap();
        assert_eq!(super::move_all(&from, &to, &favs), (2, 0));
        assert_eq!(std::fs::read_to_string(to.join("Game/x (2).mp4")).unwrap(), "1");
        assert!(to.join("y.png").exists() && !from.join("Game").exists());
        assert!(from.join(".incoming/busy.mp4").exists());
        assert!(super::Favorites::has(&favs.load(), &to.join("Game/x (2).mp4").to_string_lossy()));
        assert_eq!(super::target_for(&base), base.join("Clipper"), "a folder with other things in it");
        assert_eq!(super::target_for(&base.join("new")), base.join("new"));
        std::fs::remove_dir_all(base).unwrap();
    }
}

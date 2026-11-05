//! ffmpeg/ffprobe for the editor. Windows: a pinned gyan.dev build (verified by
//! SHA-256) unpacked in <data>/ffmpeg on first use. Linux: the system ones.

use anyhow::Result;
use std::path::{Path, PathBuf};

#[derive(Clone, Debug)]
pub struct Tools {
    pub ffmpeg: PathBuf,
    pub ffprobe: PathBuf,
}

#[cfg(windows)]
const VERSION: &str = "9.0.2";
#[cfg(windows)]
const URL: &str = "https://github.com/GyanD/codexffmpeg/releases/download/9.0.2/ffmpeg-9.0.2-essentials_build.zip";
#[cfg(windows)]
const SHA256: &str = "60f467265b1e312373dbcd92200c2618a74850f98d3d078e94296bb3fa2047ba";

#[cfg_attr(not(windows), allow(dead_code))]
static INSTALL: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

/// A command that never flashes a console window on Windows.
pub fn command(exe: &Path) -> tokio::process::Command {
    #[allow(unused_mut)]
    let mut c = tokio::process::Command::new(exe);
    #[cfg(windows)]
    c.creation_flags(crate::NO_WINDOW);
    c.kill_on_drop(true);
    c
}

#[cfg(not(windows))]
pub async fn ensure(_data_dir: &Path, _progress: impl Fn(u32)) -> Result<Tools> {
    Ok(Tools { ffmpeg: "ffmpeg".into(), ffprobe: "ffprobe".into() })
}

#[cfg(windows)]
pub async fn ensure(data_dir: &Path, progress: impl Fn(u32)) -> Result<Tools> {
    use std::fs;
    let _guard = INSTALL.lock().await;
    let root = data_dir.join("ffmpeg");
    let tools = Tools { ffmpeg: root.join("bin").join("ffmpeg.exe"), ffprobe: root.join("bin").join("ffprobe.exe") };
    let marker = root.join("clipper-ffmpeg-version");
    if fs::read_to_string(&marker).map(|v| v.trim() == VERSION).unwrap_or(false)
        && tools.ffmpeg.exists() && tools.ffprobe.exists()
    {
        return Ok(tools);
    }
    crate::clog!("downloading ffmpeg {VERSION}");
    // The zip wraps everything in "ffmpeg-<ver>-essentials_build/".
    let find = |tmp: &Path| -> Option<PathBuf> {
        fs::read_dir(tmp).ok()?.flatten().map(|e| e.path()).find(|p| p.join("bin").join("ffmpeg.exe").exists())
    };
    crate::obs::download::install_zip(URL, SHA256, &root, None, find, &progress).await?;
    fs::write(&marker, VERSION)?;
    crate::clog!("ffmpeg {VERSION} installed");
    Ok(tools)
}


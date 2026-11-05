#![cfg_attr(not(windows), allow(dead_code))]
//! First-run install of the portable OBS that Clipper drives (Windows).
//! A pinned official release, verified by SHA-256 before it is unpacked.

use anyhow::{bail, Context, Result};
use futures_util::StreamExt;
use sha2::{Digest, Sha256};
use std::{
    fs,
    io::Write,
    path::{Path, PathBuf},
};

pub const OBS_VERSION: &str = "32.2.2";
const URL: &str =
    "https://github.com/obsproject/obs-studio/releases/download/32.2.2/OBS-Studio-32.2.2-Windows-x64.zip";
const SHA256: &str = "4d6e40e3ab155f56b30de517380566a206d74b63cdf5ad49aa596924768f97e1";
const MARKER: &str = "clipper-obs-version";

/// `root` = the portable OBS folder (contains bin/, data/, config/).
pub fn installed(root: &Path) -> bool {
    fs::read_to_string(root.join(MARKER)).map(|v| v.trim() == OBS_VERSION).unwrap_or(false)
        && root.join("bin").join("64bit").join("obs64.exe").exists()
}

/// Download, verify and unpack OBS into `root`, keeping an existing `config/`.
/// `progress` gets whole percentages while downloading.
pub async fn install(root: &Path, progress: impl Fn(u32)) -> Result<()> {
    let r = install_zip(URL, SHA256, root, Some("config"), find_obs_root, &progress).await?;
    fs::write(r.join(MARKER), OBS_VERSION)?;
    Ok(())
}

/// The shared installer for OBS and ffmpeg: download `url` (checked against
/// `sha256`) next to `root`, unpack it into `<root>.new`, find the program's
/// folder in it with `find`, and replace `root` with it, keeping `keep` (a
/// sub-folder of the old install, e.g. OBS's config). Returns `root`.
pub(crate) async fn install_zip(
    url: &str,
    sha256: &str,
    root: &Path,
    keep: Option<&str>,
    find: fn(&Path) -> Option<PathBuf>,
    progress: &impl Fn(u32),
) -> Result<PathBuf> {
    let parent = root.parent().context("install folder has no parent")?;
    let name = root.file_name().context("install folder has no name")?.to_string_lossy().into_owned();
    fs::create_dir_all(parent)?;
    let zip_path = parent.join(format!("{name}-download.zip"));
    download(url, sha256, &zip_path, progress).await?;

    let tmp = parent.join(format!("{name}.new"));
    let _ = fs::remove_dir_all(&tmp);
    let (z, t) = (zip_path.clone(), tmp.clone());
    tokio::task::spawn_blocking(move || unzip(&z, &t)).await??;
    let _ = fs::remove_file(&zip_path);

    // The zip may or may not wrap everything in one top-level folder.
    let unpacked = find(&tmp).context("the program was not found in the download")?;

    // Clear the old install first, all but `keep`: if a file is still in use
    // this fails here, while the kept folder (OBS: profile, scene, websocket
    // password) is still in place for the next attempt.
    if root.exists() {
        for e in fs::read_dir(root)?.flatten() {
            if keep.is_some_and(|k| e.file_name() == k) {
                continue;
            }
            let p = e.path();
            if p.is_dir() { fs::remove_dir_all(&p)? } else { fs::remove_file(&p)? }
        }
    }
    if let Some(k) = keep {
        let old = root.join(k);
        if old.is_dir() {
            let _ = fs::remove_dir_all(unpacked.join(k));
            fs::rename(&old, unpacked.join(k))?;
        }
    }
    if root.exists() {
        fs::remove_dir_all(root)?;
    }
    fs::rename(&unpacked, root)?;
    let _ = fs::remove_dir_all(&tmp);
    Ok(root.to_path_buf())
}

/// Stream `url` to `dest`, verifying the SHA-256 (hex) on the way.
pub(crate) async fn download(url: &str, sha256: &str, dest: &Path, progress: &impl Fn(u32)) -> Result<()> {
    // Timeouts so a stalled connection fails (and is retried) instead of hanging
    // forever; read_timeout is per read, so big downloads on slow lines still work.
    let client = reqwest::Client::builder()
        .user_agent(concat!("Clipper/", env!("CARGO_PKG_VERSION")))
        .connect_timeout(std::time::Duration::from_secs(20))
        .read_timeout(std::time::Duration::from_secs(30))
        .build()?;
    let resp = client.get(url).send().await?.error_for_status()?;
    let total = resp.content_length().unwrap_or(0);
    let part = dest.with_extension("part");
    let mut file = fs::File::create(&part)?;
    let mut hasher = Sha256::new();
    let (mut got, mut last) = (0u64, u32::MAX);
    let mut stream = resp.bytes_stream();
    while let Some(chunk) = stream.next().await {
        let chunk = chunk?;
        hasher.update(&chunk);
        file.write_all(&chunk)?;
        got += chunk.len() as u64;
        if total > 0 {
            let pct = (got * 100 / total) as u32;
            if pct != last {
                last = pct;
                progress(pct);
            }
        }
    }
    file.flush()?;
    drop(file);
    let hash = format!("{:x}", hasher.finalize());
    if hash != sha256 {
        let _ = fs::remove_file(&part);
        bail!("download is corrupt (checksum mismatch), will retry");
    }
    fs::rename(&part, dest)?;
    Ok(())
}

pub(crate) fn unzip(zip_path: &Path, dest: &Path) -> Result<()> {
    let mut archive = zip::ZipArchive::new(fs::File::open(zip_path)?)?;
    for i in 0..archive.len() {
        let mut entry = archive.by_index(i)?;
        // enclosed_name() rejects absolute paths and "..".
        let Some(rel) = entry.enclosed_name() else { continue };
        let out = dest.join(rel);
        if entry.is_dir() {
            fs::create_dir_all(&out)?;
        } else {
            if let Some(p) = out.parent() {
                fs::create_dir_all(p)?;
            }
            std::io::copy(&mut entry, &mut fs::File::create(&out)?)?;
        }
    }
    Ok(())
}

fn find_obs_root(dir: &Path) -> Option<PathBuf> {
    let has = |d: &Path| d.join("bin").join("64bit").join("obs64.exe").exists();
    if has(dir) {
        return Some(dir.to_path_buf());
    }
    fs::read_dir(dir).ok()?.flatten().map(|e| e.path()).find(|p| p.is_dir() && has(p))
}

#[cfg(test)]
mod tests {
    /// Real download (~190 MB). Run by hand: cargo test obs_download -- --ignored --nocapture
    #[tokio::test]
    #[ignore]
    async fn obs_download() {
        let dir = std::env::temp_dir().join("clipper-obs-download-test");
        let _ = std::fs::remove_dir_all(&dir);
        let root = dir.join("obs");
        std::fs::create_dir_all(root.join("config/obs-studio")).unwrap();
        std::fs::write(root.join("config/obs-studio/keep.txt"), "x").unwrap();
        super::install(&root, |p| if p % 20 == 0 { println!("{p}%") }).await.unwrap();
        assert!(super::installed(&root));
        assert!(root.join("config/obs-studio/keep.txt").exists(), "config kept");
        println!("ok: {}", root.display());
    }
}

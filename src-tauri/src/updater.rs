//! In-app updates from the GitHub releases of the Clipper repo.
//! A release tagged vX.Y.Z with a `clipper.exe` asset is an update when X.Y.Z is
//! newer than this build. The download is checked against GitHub's SHA-256
//! digest, then swapped in next to the running exe and Clipper restarts.

use anyhow::{bail, Context, Result};
use serde::{Deserialize, Serialize};
use std::path::PathBuf;

const REPO: &str = "Jeremy0538/clipper";
const ASSET: &str = "clipper.exe";

#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct Update {
    pub version: String,
    pub notes: String,
    pub url: String,
    pub sha256: String,
}

pub fn current() -> semver::Version {
    semver::Version::parse(env!("CARGO_PKG_VERSION")).expect("crate version is semver")
}

fn client() -> Result<reqwest::Client> {
    Ok(reqwest::Client::builder()
        .user_agent(concat!("Clipper/", env!("CARGO_PKG_VERSION")))
        .build()?)
}

/// Newest release newer than this build, if any.
pub async fn check(include_test_builds: bool) -> Result<Option<Update>> {
    // Stable users: GitHub's "latest" release, however many test builds came
    // after it. Testers: the newest releases of any kind.
    let url = if include_test_builds {
        format!("https://api.github.com/repos/{REPO}/releases?per_page=50")
    } else {
        format!("https://api.github.com/repos/{REPO}/releases/latest")
    };
    let resp = client()?.get(url).header("Accept", "application/vnd.github+json").send().await?;
    if resp.status() == reqwest::StatusCode::NOT_FOUND {
        if !include_test_builds {
            return Ok(None); // no stable release yet
        }
        bail!("Update server not reachable (the release page is not public)");
    }
    let body: serde_json::Value = serde_json::from_slice(&resp.error_for_status()?.bytes().await?)?;
    let releases = if body.is_array() { body } else { serde_json::Value::Array(vec![body]) };
    let mine = current();
    let mut best: Option<(semver::Version, Update)> = None;
    for r in releases.as_array().into_iter().flatten() {
        if r["draft"].as_bool() == Some(true)
            || (!include_test_builds && r["prerelease"].as_bool() == Some(true))
        {
            continue;
        }
        let Some(v) = r["tag_name"].as_str().and_then(|t| semver::Version::parse(t.trim_start_matches('v')).ok())
        else { continue };
        if v <= mine || best.as_ref().is_some_and(|(b, _)| &v <= b) {
            continue;
        }
        let Some(asset) = r["assets"].as_array().into_iter().flatten().find(|a| a["name"] == ASSET) else { continue };
        let (Some(url), Some(digest)) = (asset["browser_download_url"].as_str(), asset["digest"].as_str()) else { continue };
        let Some(sha) = digest.strip_prefix("sha256:") else { continue };
        best = Some((v.clone(), Update {
            version: v.to_string(),
            notes: r["body"].as_str().unwrap_or_default().to_string(),
            url: url.to_string(),
            sha256: sha.to_lowercase(),
        }));
    }
    let found = best.map(|(_, u)| u);
    *FOUND.lock().unwrap() = found.clone();
    Ok(found)
}

/// The update the last check found, straight from GitHub. Installing only ever
/// uses this one, never a URL or hash handed in by the page.
static FOUND: std::sync::Mutex<Option<Update>> = std::sync::Mutex::new(None);

pub fn found(version: &str) -> Option<Update> {
    FOUND.lock().unwrap().clone().filter(|u| u.version == version)
}

fn exe_paths() -> Result<(PathBuf, PathBuf, PathBuf)> {
    let exe = std::env::current_exe()?;
    Ok((exe.with_extension("exe.new"), exe.with_extension("exe.old"), exe))
}

/// Remove the previous version left behind by an update.
pub fn cleanup() {
    if let Ok((new, old, _)) = exe_paths() {
        let _ = std::fs::remove_file(old);
        let _ = std::fs::remove_file(new);
    }
}

/// Download + verify, swap the exe, start the new one (it waits for us to exit).
/// `progress` gets whole percentages.
pub async fn install(u: &Update, progress: impl Fn(u32)) -> Result<()> {
    if !cfg!(windows) {
        bail!("Updating is only supported on Windows");
    }
    let (new, old, exe) = exe_paths()?;
    crate::obs::download::download(&u.url, &u.sha256, &new, &progress)
        .await
        .context("could not download the update")?;
    // A running exe can be renamed on Windows, not overwritten.
    let _ = std::fs::remove_file(&old);
    std::fs::rename(&exe, &old)?;
    if let Err(e) = std::fs::rename(&new, &exe) {
        let _ = std::fs::rename(&old, &exe);
        return Err(e.into());
    }
    let mut cmd = std::process::Command::new(&exe);
    cmd.args(["--wait-pid", &std::process::id().to_string()]);
    if std::env::args().any(|a| a == "--hidden") {
        cmd.arg("--hidden");
    }
    cmd.spawn()?;
    Ok(())
}

/// Started by an update: wait until the old instance (and its OBS) are gone.
pub fn wait_for_previous() {
    let args: Vec<String> = std::env::args().collect();
    let Some(pid) = args.iter().position(|a| a == "--wait-pid").and_then(|i| args.get(i + 1))
        .and_then(|p| p.parse::<u32>().ok())
    else { return };
    let pid = sysinfo::Pid::from_u32(pid);
    let mut sys = sysinfo::System::new();
    for _ in 0..100 {
        sys.refresh_processes(sysinfo::ProcessesToUpdate::Some(&[pid]), true);
        if sys.process(pid).is_none() {
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(200));
    }
    // Give its OBS a moment to release the websocket port.
    std::thread::sleep(std::time::Duration::from_secs(2));
}

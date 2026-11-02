//! "Report a problem": sends the user's description plus the end of clipper.log
//! and the newest OBS log to the owner's Todoist Inbox, so a bug report always
//! comes with its log.
//!
//! The repo is public, so no key may be built in: the user pastes their own
//! Todoist API token once. It is kept in `<data>/todoist-token` (not in
//! settings.json, so it never reaches the UI or the log).

use anyhow::{bail, Context, Result};
use serde_json::{json, Value};
use std::path::{Path, PathBuf};

const API: &str = "https://api.todoist.com/api/v1";
/// Todoist limits: description 16 383 chars, comment 15 000 chars.
const DESCRIPTION_MAX: usize = 15_000;
const COMMENT_MAX: usize = 14_000;

fn token_file(data_dir: &Path) -> PathBuf {
    data_dir.join("todoist-token")
}

pub fn token(data_dir: &Path) -> Option<String> {
    let t = std::fs::read_to_string(token_file(data_dir)).ok()?;
    let t = t.trim().to_string();
    (!t.is_empty()).then_some(t)
}

fn client() -> Result<reqwest::Client> {
    Ok(reqwest::Client::builder()
        .user_agent(concat!("Clipper/", env!("CARGO_PKG_VERSION")))
        .timeout(std::time::Duration::from_secs(30))
        .build()?)
}

async fn post(token: &str, path: &str, body: Value) -> Result<Value> {
    let r = client()?
        .post(format!("{API}/{path}"))
        .bearer_auth(token)
        .header(reqwest::header::CONTENT_TYPE, "application/json")
        .body(body.to_string())
        .send()
        .await
        .context("could not reach Todoist")?;
    let code = r.status();
    let text = r.text().await.unwrap_or_default();
    if code == reqwest::StatusCode::UNAUTHORIZED || code == reqwest::StatusCode::FORBIDDEN {
        bail!("Todoist did not accept the token. Paste it again in Settings → Troubleshooting.");
    }
    if !code.is_success() {
        bail!("Todoist answered {code}: {}", text.chars().take(200).collect::<String>());
    }
    Ok(serde_json::from_str(&text).unwrap_or(Value::Null))
}

/// Check the token with a cheap call, then store it ("" removes it).
pub async fn set_token(data_dir: &Path, token: &str) -> Result<()> {
    let token = token.trim();
    if token.is_empty() {
        let _ = std::fs::remove_file(token_file(data_dir));
        return Ok(());
    }
    let r = client()?
        .get(format!("{API}/projects?limit=1"))
        .bearer_auth(token)
        .send()
        .await
        .context("could not reach Todoist")?;
    if !r.status().is_success() {
        bail!("Todoist did not accept this token ({}). Copy it again from Todoist → Settings → Integrations → Developer.", r.status());
    }
    std::fs::write(token_file(data_dir), token)?;
    Ok(())
}

/// The last `max` characters of a file, starting at a whole line.
fn tail(path: &Path, max: usize) -> Option<String> {
    let bytes = std::fs::read(path).ok()?;
    let text = String::from_utf8_lossy(&bytes);
    if text.len() <= max {
        return Some(text.into_owned());
    }
    let mut start = text.len() - max;
    while !text.is_char_boundary(start) {
        start += 1;
    }
    let cut = &text[start..];
    Some(cut.split_once('\n').map(|(_, rest)| rest).unwrap_or(cut).to_string())
}

/// Newest OBS log (OBS writes one file per start).
fn newest_obs_log(data_dir: &Path) -> Option<PathBuf> {
    let dir = crate::obs::config::ObsPaths::resolve(data_dir).config.join("logs");
    std::fs::read_dir(dir)
        .ok()?
        .flatten()
        .filter(|e| e.path().extension().is_some_and(|x| x == "txt"))
        .max_by_key(|e| e.metadata().and_then(|m| m.modified()).ok())
        .map(|e| e.path())
}

fn block(title: &str, body: &str) -> String {
    format!("**{title}**\n```\n{}\n```", body.trim_end())
}

/// Create the task and attach the logs as comments. Returns the task URL.
pub async fn send(data_dir: &Path, text: &str, summary: &str) -> Result<String> {
    let Some(token) = token(data_dir) else {
        bail!("Add your Todoist token first (Settings → Troubleshooting).");
    };
    let text = text.trim();
    let first_line = text.lines().next().unwrap_or("").chars().take(120).collect::<String>();
    let title = if first_line.is_empty() { "problem report".to_string() } else { first_line };

    let description: String = format!("{text}\n\n{summary}").chars().take(DESCRIPTION_MAX).collect();
    let task = post(&token, "tasks", json!({
        "content": format!("Clipper {}: {title}", env!("CARGO_PKG_VERSION")),
        "description": description,
    }))
    .await?;
    let id = task["id"].as_str().context("Todoist did not return a task id")?.to_string();

    // Logs go in comments: each has its own size limit, so both fit whole-ish.
    let mut comments = vec![];
    if let Some(p) = crate::log::path() {
        match tail(p, COMMENT_MAX - 200) {
            Some(t) => comments.push(block("clipper.log (end)", &t)),
            None => comments.push("clipper.log could not be read.".into()),
        }
    }
    match newest_obs_log(data_dir).and_then(|p| Some((tail(&p, COMMENT_MAX - 300)?, p))) {
        Some((t, p)) => {
            let name = p.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
            comments.push(block(&format!("OBS log {name} (end)"), &t));
        }
        None => comments.push("No OBS log found.".into()),
    }
    for c in comments {
        // A failed comment shouldn't lose the report itself.
        if let Err(e) = post(&token, "comments", json!({ "task_id": id, "content": c })).await {
            crate::clog!("report: comment failed: {e:#}");
        }
    }
    Ok(task["url"].as_str().map(String::from).unwrap_or_else(|| format!("https://app.todoist.com/app/task/{id}")))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tail_starts_at_a_line() {
        let dir = std::env::temp_dir().join(format!("clipper-tail-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let f = dir.join("x.log");
        std::fs::write(&f, "first line\nsecond line\nthird é line\n").unwrap();
        assert_eq!(tail(&f, 1000).unwrap(), "first line\nsecond line\nthird é line\n");
        assert_eq!(tail(&f, 20).unwrap(), "third é line\n");
        std::fs::remove_dir_all(dir).unwrap();
    }
}

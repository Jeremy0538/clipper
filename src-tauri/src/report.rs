//! "Report a problem".
//! - Everyone: a GitHub issue on the Clipper repo, opened in the browser with the
//!   description and a status summary filled in; the end of clipper.log goes
//!   on the clipboard to paste in. Issues
//!   are public, so personal details are removed first (`redact`).
//! - The owner: with a Todoist API token set, the report (with the full log
//!   tails) goes to their Todoist Inbox instead. The repo is public, so no key
//!   is built in: the token is kept in `<data>/todoist-token` (not in
//!   settings.json, so it never reaches the UI or the log).

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

const ISSUES: &str = "https://github.com/Jeremy0538/clipper/issues/new";
/// Windows hands long URLs to the browser cut off (about 2 KB), and a URL cut
/// in the middle of a %-code made GitHub answer with an error page. The log
/// is therefore not put in the URL but on the clipboard.
const URL_MAX: usize = 1_900;
/// GitHub's limit for an issue body is 65 536 characters.
const LOG_MAX: usize = 50_000;

/// Remove what must not end up in a public issue: the Windows user name in
/// paths, window titles (Discord servers and DMs, browser tabs…) and IP
/// addresses. What stays is enough to debug: exe names, classes, times, errors.
pub fn redact(text: &str) -> String {
    use regex::Regex;
    use std::sync::OnceLock;
    static RES: OnceLock<Vec<(Regex, &'static str)>> = OnceLock::new();
    let res = RES.get_or_init(|| {
        vec![
            // C:\Users\name\…, C:/Users/name/…, /home/name/…
            (Regex::new(r#"(?i)([A-Z]:[\\/]+Users[\\/]+)[^\\/\s"]+"#).unwrap(), "${1}<user>"),
            (Regex::new(r#"(/home/)[^/\s"]+"#).unwrap(), "${1}<user>"),
            // OBS window strings "title:class:exe" (OBS writes ":" in a title as "#3A")
            (Regex::new(r#"[^\s":→][^":→\n]*:([A-Za-z0-9_.\- ]+):([^:\s"]+\.(?i:exe))"#).unwrap(), "<window>:${1}:${2}"),
            // OBS log "\ttitle: …" lines
            (Regex::new(r"(?m)^(\S*\s*title: ).*$").unwrap(), "${1}<window>"),
            // IPv4 addresses (OBS logs the LAN address)
            (Regex::new(r"\b\d{1,3}\.\d{1,3}\.\d{1,3}\.\d{1,3}\b").unwrap(), "<ip>"),
        ]
    });
    let mut out = text.to_string();
    for (re, with) in res {
        out = re.replace_all(&out, *with).into_owned();
    }
    out
}

fn url_encode(s: &str) -> String {
    let mut out = String::with_capacity(s.len() * 3);
    for b in s.bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => out.push(b as char),
            _ => out.push_str(&format!("%{b:02X}")),
        }
    }
    out
}

/// The last error lines of a log (newest last), each kept short: they go in
/// the issue itself, because the full log on the clipboard is often not pasted.
fn error_lines(log: &str, max_lines: usize) -> String {
    let errs: Vec<String> = log
        .lines()
        .filter(|l| l.contains("ERROR"))
        .map(|l| if l.chars().count() > 240 { format!("{}…", l.chars().take(240).collect::<String>()) } else { l.to_string() })
        .collect();
    errs[errs.len().saturating_sub(max_lines)..].join("\n")
}

/// The "new issue" URL (description, recent errors and status, redacted,
/// short enough for every browser) and the end of clipper.log (redacted) to
/// paste into it.
pub fn github_issue(text: &str, summary: &str) -> (String, String) {
    let log = crate::log::path().and_then(|p| tail(p, LOG_MAX)).map(|t| redact(&t));
    github_issue_with(text, summary, log)
}

fn github_issue_with(text: &str, summary: &str, log: Option<String>) -> (String, String) {
    let text = redact(text.trim());
    let errors = log.as_deref().map(|l| error_lines(l, 8)).unwrap_or_default();
    let first = text.lines().next().unwrap_or("").chars().take(80).collect::<String>();
    let title = url_encode(&format!("{}: {}", env!("CARGO_PKG_VERSION"), if first.is_empty() { "problem report" } else { &first }));
    let summary = redact(summary);
    let paste = "**End of clipper.log**\nClipper copied it to your clipboard: click on this line and press Ctrl+V.\n";
    // Never more than fits anyway (each char is at least 3 URL bytes when
    // encoded, often more), then shorten in big steps.
    let (mut text_len, mut summary_len) = (text.chars().count().min(URL_MAX / 2), summary.chars().count().min(URL_MAX / 2));
    // Errors are cut from the oldest end: the newest one matters most.
    let mut errors_len = errors.chars().count().min(URL_MAX / 2);
    let url = loop {
        let cut = |s: &str, n: usize| -> String {
            if s.chars().count() <= n { s.to_string() } else { format!("{}…", s.chars().take(n).collect::<String>()) }
        };
        let errs = if errors_len == 0 {
            String::new()
        } else {
            let n = errors.chars().count();
            let kept: String = errors.chars().skip(n - errors_len.min(n)).collect();
            format!("**Recent errors**\n```\n{}{kept}\n```\n\n", if errors_len < n { "…" } else { "" })
        };
        let body = format!("**What happened**\n{}\n\n{errs}**Details**\n```\n{}\n```\n\n{paste}", cut(&text, text_len), cut(&summary, summary_len));
        let url = format!("{ISSUES}?title={title}&body={}", url_encode(&body));
        if url.len() <= URL_MAX || (text_len == 0 && summary_len == 0 && errors_len == 0) {
            break url;
        }
        // Shorten the status first, then the errors, then the description.
        if summary_len > 0 {
            summary_len = (summary_len * 4 / 5).saturating_sub(20)
        } else if errors_len > 0 {
            errors_len = (errors_len * 4 / 5).saturating_sub(20)
        } else {
            text_len = (text_len * 4 / 5).saturating_sub(10)
        }
    };
    let log = log.unwrap_or_else(|| "(clipper.log could not be read)".into());
    let clip = format!("```\n{}\n```\n(names, paths and window titles removed)\n", log.trim_end());
    (url, clip)
}

/// Open a URL in the default browser (no shell, so "&" in the URL is safe).
pub fn open_in_browser(url: &str) -> std::io::Result<()> {
    // ShellExecute may hand the URL to COM shell extensions: it wants its own
    // thread with COM set up (STA), not an async worker.
    #[cfg(windows)]
    {
        let url = url.to_string();
        let code = std::thread::spawn(move || unsafe {
            use windows::core::{w, HSTRING};
            use windows::Win32::System::Com::{CoInitializeEx, CoUninitialize, COINIT_APARTMENTTHREADED, COINIT_DISABLE_OLE1DDE};
            use windows::Win32::UI::{Shell::ShellExecuteW, WindowsAndMessaging::SW_SHOWNORMAL};
            let com = CoInitializeEx(None, COINIT_APARTMENTTHREADED | COINIT_DISABLE_OLE1DDE);
            let r = ShellExecuteW(None, w!("open"), &HSTRING::from(url), None, None, SW_SHOWNORMAL);
            if com.is_ok() {
                CoUninitialize();
            }
            r.0 as isize
        })
        .join()
        .unwrap_or(0);
        // Values above 32 mean success.
        if code <= 32 {
            return Err(std::io::Error::other(format!("ShellExecute failed ({code})")));
        }
    }
    #[cfg(not(windows))]
    std::process::Command::new("xdg-open").arg(url).spawn()?;
    Ok(())
}

#[cfg(test)]
mod tests {
    #[test]
    fn redacts_personal_details() {
        let log = "2026-11-02 12:14:00.127 game detected: Discord (pid 18452, window \"〔🪴〕Het Grote Wiet Schuurtje | Alwyn's funzige hoekje - Discord:Chrome_WidgetWin_1:Discord.exe\")\n\
                   2026-11-02 12:45:11.299 Window Capture → @Alwyn - Discord:Chrome_WidgetWin_1:Discord.exe\n\
                   2026-11-02 12:13:56 data C:\\Users\\jerkl\\AppData\\Roaming\\x | clips C:\\Users\\jerkl\\Videos\\Clipper\n\
                   OBS wrote C:/Users/jerkl/AppData/Roaming/x/incoming/Replay.mp4\n\
                   12:13:58.981: \ttitle: 〔🪴〕Het Grote Wiet Schuurtje\n\
                   Server started successfully on port 4466. Possible connect address: 192.168.1.168\n\
                   ERROR Could not target Discord: SetInputSettings failed (code 600)";
        let r = super::redact(log);
        for gone in ["Wiet", "Alwyn", "jerkl", "192.168"] {
            assert!(!r.contains(gone), "{gone} still in:\n{r}");
        }
        for kept in ["Chrome_WidgetWin_1:Discord.exe", "pid 18452", "code 600", "port 4466", "Users\\<user>\\Videos", "Window Capture → <window>"] {
            assert!(r.contains(kept), "{kept} missing in:\n{r}");
        }
    }

    #[test]
    fn issue_url_fits() {
        for (text, summary) in [("x".repeat(20_000), "Version 1".to_string()), ("é".repeat(3000), "s".repeat(5000)), ("short".into(), "Version 1".into())] {
            let (u, _) = super::github_issue(&text, &summary);
            assert!(u.len() <= super::URL_MAX, "{}", u.len());
            assert!(u.starts_with("https://github.com/Jeremy0538/clipper/issues/new?title="));
        }
        let (u, _) = super::github_issue("short", "Version 1");
        assert!(u.contains("short") && u.contains("Version%201"));
    }

    #[test]
    fn issue_has_recent_errors() {
        let log = "a ok\nb ERROR old one\nc fine\nd ERROR could not move x to the recycle bin\n".to_string();
        let (u, _) = super::github_issue_with("it won't delete", "Version 1", Some(log));
        assert!(u.contains(&super::url_encode("Recent errors")));
        assert!(u.contains(&super::url_encode("could not move x to the recycle bin")));
        assert!(!u.contains(&super::url_encode("a ok")));
        // A huge error list still fits, keeping the newest error.
        let many = (0..200).map(|i| format!("{i} ERROR {}", "y".repeat(200))).collect::<Vec<_>>().join("\n") + "\nZ ERROR newest";
        let (u, _) = super::github_issue_with("x", &"s".repeat(3000), Some(many));
        assert!(u.len() <= super::URL_MAX, "{}", u.len());
        assert!(u.contains(&super::url_encode("newest")), "newest error dropped");
    }

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

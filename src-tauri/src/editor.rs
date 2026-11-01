//! Clip editor backend: inspect a clip, split out its audio tracks for the
//! preview, and export a trimmed / re-mixed copy.
//!
//! Track layout: a:0 = Mix (what players play), a:1.. = sources (Game, Discord,
//! Mic). Export rebuilds Mix from the sources at the chosen volumes and keeps the
//! sources themselves untouched, so an edited clip can be edited again. Silent
//! sources (Discord without a call) are left out.

use crate::tools::{command, Tools};
use anyhow::{bail, Context, Result};
use serde::{Deserialize, Serialize};
use std::{
    path::{Path, PathBuf},
    process::Stdio,
};
use tokio::io::{AsyncBufReadExt, BufReader};

const LAYOUT: [&str; 4] = ["Mix", "Game", "Discord", "Mic"];
/// A source this quiet (dB peak) counts as silent.
const SILENT_DB: f64 = -80.0;

#[derive(Serialize, Clone, Debug)]
pub struct Track {
    /// Audio stream number (a:N).
    pub index: usize,
    pub name: String,
    /// Extracted copy for the preview player.
    pub file: String,
    /// Part of the rebuilt Mix. False for Discord/apps when a "Desktop" track
    /// (everything you heard) already contains them.
    pub in_mix: bool,
}

#[derive(Serialize, Clone, Debug)]
pub struct ClipInfo {
    pub path: String,
    pub name: String,
    pub duration: f64,
    pub width: u32,
    pub height: u32,
    /// Sources with their own volume (for a single-track clip: that one track).
    pub sources: Vec<Track>,
    /// Small keyframe images for instant scrubbing: (time in s, file).
    pub thumbs: Vec<(f64, String)>,
    /// Container start time (keyframe times are relative to it).
    pub start: f64,
}

/// One small image per keyframe (≈ every second). Only keyframes are decoded,
/// so this takes a few seconds even for a 5-minute 1440p clip.
pub async fn keyframe_thumbs(tools: &Tools, path: &Path, cache: &Path, start: f64) -> Vec<(f64, String)> {
    let mut c = command(&tools.ffmpeg);
    c.args(["-hide_banner", "-v", "info", "-hwaccel", "auto", "-skip_frame", "nokey", "-i"])
        .arg(path)
        .args(["-map", "0:v:0", "-an", "-vf", "scale=640:-2,showinfo", "-fps_mode", "passthrough", "-q:v", "7"])
        .arg(cache.join("k%05d.jpg"));
    let Ok(out) = c.stdin(Stdio::null()).output().await else { return vec![] };
    let log = String::from_utf8_lossy(&out.stderr);
    log.lines()
        .filter(|l| l.contains("Parsed_showinfo") && l.contains(" n:"))
        .filter_map(|l| l.split("pts_time:").nth(1)?.split_whitespace().next()?.parse::<f64>().ok())
        .enumerate()
        .map(|(i, t)| ((t - start).max(0.0), cache.join(format!("k{:05}.jpg", i + 1)).to_string_lossy().into_owned()))
        .filter(|(_, f)| Path::new(f).exists())
        .collect()
}

#[derive(Deserialize, Debug)]
pub struct ExportRequest {
    pub path: String,
    pub start: f64,
    pub end: f64,
    /// Volume per source, same order as `ClipInfo::sources` (1.0 = unchanged).
    pub volumes: Vec<f64>,
}

async fn run(mut c: tokio::process::Command) -> Result<std::process::Output> {
    let out = c.stdin(Stdio::null()).output().await?;
    if !out.status.success() {
        let err = String::from_utf8_lossy(&out.stderr);
        bail!("{}", err.lines().rev().take(3).collect::<Vec<_>>().join(" | "));
    }
    Ok(out)
}

fn track_name(pos: usize, tags: &serde_json::Value, total: usize) -> String {
    // OBS writes the track name as "name"; our own exports as title/handler_name.
    for key in ["name", "title", "handler_name"] {
        if let Some(t) = tags[key].as_str() {
            let t = t.trim();
            if !t.is_empty() && !t.eq_ignore_ascii_case("SoundHandler") {
                return t.to_string();
            }
        }
    }
    if total == 4 || total == 3 && pos < 3 { LAYOUT.get(pos).copied().unwrap_or("Audio").into() }
    else { format!("Track {}", pos + 1) }
}

pub async fn open(tools: &Tools, cache: &Path, path: &Path) -> Result<ClipInfo> {
    let mut c = command(&tools.ffprobe);
    c.args(["-v", "error", "-show_entries",
            "format=duration,start_time:stream=index,codec_type,width,height:stream_tags=name,title,handler_name",
            "-of", "json"]).arg(path);
    let probe: serde_json::Value = serde_json::from_slice(&run(c).await?.stdout)?;
    let duration: f64 = probe["format"]["duration"].as_str().and_then(|d| d.parse().ok()).context("no duration")?;
    let streams = probe["streams"].as_array().cloned().unwrap_or_default();
    let video = streams.iter().find(|s| s["codec_type"] == "video").context("no video stream")?;
    let audio: Vec<&serde_json::Value> = streams.iter().filter(|s| s["codec_type"] == "audio").collect();

    // With a Mix + sources layout, the sources are what you edit.
    let picks: Vec<usize> = if audio.len() >= 2 { (1..audio.len()).collect() } else { (0..audio.len()).collect() };

    let _ = std::fs::remove_dir_all(cache);
    std::fs::create_dir_all(cache)?;
    let mut sources = vec![];
    if !picks.is_empty() {
        let mut c = command(&tools.ffmpeg);
        c.args(["-v", "error", "-y", "-i"]).arg(path);
        for &i in &picks {
            let f = cache.join(format!("track{i}.m4a"));
            c.args(["-map", &format!("0:a:{i}"), "-c", "copy", "-vn"]).arg(&f);
            sources.push(Track {
                index: i,
                name: track_name(i, &audio[i]["tags"], audio.len()),
                file: f.to_string_lossy().into_owned(),
                in_mix: true,
            });
        }
        run(c).await.context("could not read the audio tracks")?;
    }
    let start: f64 = probe["format"]["start_time"].as_str().and_then(|d| d.parse().ok()).unwrap_or(0.0);
    Ok(ClipInfo {
        path: path.to_string_lossy().into_owned(),
        name: path.file_stem().map(|s| s.to_string_lossy().into_owned()).unwrap_or_default(),
        duration,
        width: video["width"].as_u64().unwrap_or(0) as u32,
        height: video["height"].as_u64().unwrap_or(0) as u32,
        sources,
        thumbs: vec![],
        start,
    })
}

/// Peak level of one audio stream over the export range.
async fn peak_db(tools: &Tools, path: &Path, index: usize, start: f64, end: f64) -> Result<f64> {
    let mut c = command(&tools.ffmpeg);
    c.args(["-hide_banner", "-nostats", "-ss", &format!("{start:.3}"), "-to", &format!("{end:.3}"), "-i"])
        .arg(path)
        .args(["-map", &format!("0:a:{index}"), "-af", "volumedetect", "-f", "null", "-"]);
    let out = run(c).await?;
    let err = String::from_utf8_lossy(&out.stderr);
    Ok(err
        .lines()
        .find_map(|l| l.split("max_volume:").nth(1))
        .and_then(|v| v.trim().trim_end_matches("dB").trim().parse().ok())
        .unwrap_or(f64::NEG_INFINITY))
}

/// "<Game> 2026-11-01 14-17-58" → "<Game>"; also strips an earlier " (edited)".
pub fn base_name(stem: &str) -> String {
    let mut s = stem.trim().to_string();
    let b = s.as_bytes();
    if b.len() >= 20 {
        let tail = &s[s.len() - 19..];
        let shape_ok = tail.chars().enumerate().all(|(i, ch)| match i {
            4 | 7 | 13 | 16 => ch == '-',
            10 => ch == ' ',
            _ => ch.is_ascii_digit(),
        });
        if shape_ok {
            s.truncate(s.len() - 19);
        }
    }
    let s = s.trim().trim_end_matches("(edited)").trim().to_string();
    if s.is_empty() { "Clip".into() } else { s }
}

pub async fn export(
    tools: &Tools,
    clips_dir: &Path,
    info: &ClipInfo,
    req: &ExportRequest,
    progress: impl Fn(u32),
) -> Result<PathBuf> {
    let src = Path::new(&req.path);
    let start = req.start.clamp(0.0, info.duration);
    let end = req.end.clamp(start + 0.2, info.duration);
    let trimmed = start > 0.05 || end < info.duration - 0.05;

    // Which sources carry sound in the range?
    let mut keep: Vec<(&Track, f64)> = vec![];
    for (t, &v) in info.sources.iter().zip(req.volumes.iter().chain(std::iter::repeat(&1.0))) {
        let peak = peak_db(tools, src, t.index, start, end).await.unwrap_or(0.0);
        if peak > SILENT_DB {
            keep.push((t, v));
        } else {
            crate::clog!("export: leaving out silent track {} ({peak:.0} dB)", t.name);
        }
    }
    if keep.is_empty() {
        keep = info.sources.iter().zip(req.volumes.iter().copied().chain(std::iter::repeat(1.0))).collect();
    }

    // Sources whose sound already sits in "Desktop" stay out of the new Mix.
    let mut mixed: Vec<(&Track, f64)> = keep.iter().filter(|(t, _)| t.in_mix).cloned().collect();
    if mixed.is_empty() {
        mixed = keep.clone();
    }

    let stamp = chrono::Local::now().format("%Y-%m-%d %H-%M-%S");
    let mut out = clips_dir.join(format!("{} (edited) {stamp}.mp4", base_name(&info.name)));
    let mut n = 2;
    while out.exists() {
        out = clips_dir.join(format!("{} (edited) {stamp} ({n}).mp4", base_name(&info.name)));
        n += 1;
    }
    let part = out.with_extension("part.mp4");

    let attempt = |encoder: &'static str| {
        let mut c = command(&tools.ffmpeg);
        c.args(["-hide_banner", "-v", "error", "-y", "-progress", "pipe:1", "-nostats"]);
        if trimmed {
            // GPU decoding for the part that gets re-encoded (falls back to CPU by itself).
            c.args(["-hwaccel", "auto", "-ss", &format!("{start:.3}"), "-to", &format!("{end:.3}")]);
        }
        c.arg("-i").arg(src);
        let mut graph = String::new();
        if !mixed.is_empty() {
            for (k, (t, v)) in mixed.iter().enumerate() {
                graph += &format!("[0:a:{}]volume={:.3}[s{k}];", t.index, v.clamp(0.0, 20.0));
            }
            if mixed.len() == 1 {
                graph += "[s0]anull[mix]";
            } else {
                for k in 0..mixed.len() {
                    graph += &format!("[s{k}]");
                }
                graph += &format!("amix=inputs={}:normalize=0:dropout_transition=0[mix]", mixed.len());
            }
            c.args(["-filter_complex", &graph]);
        }
        c.args(["-map", "0:v:0"]);
        if !mixed.is_empty() {
            c.args(["-map", "[mix]"]);
            if keep.len() > 1 || info.sources.len() > 1 {
                for (t, _) in &keep {
                    c.args(["-map", &format!("0:a:{}", t.index)]);
                }
            }
        }
        match (trimmed, encoder) {
            (false, _) => { c.args(["-c:v", "copy"]); }
            (true, "h264_nvenc") => { c.args(["-c:v", "h264_nvenc", "-preset", "p5", "-rc", "vbr", "-cq", "19", "-b:v", "0", "-g", "60"]); }
            (true, "h264_amf") => { c.args(["-c:v", "h264_amf", "-quality", "quality", "-rc", "cqp", "-qp_i", "19", "-qp_p", "21", "-qp_b", "23", "-g", "60"]); }
            (true, "h264_qsv") => { c.args(["-c:v", "h264_qsv", "-preset", "medium", "-global_quality", "21", "-g", "60"]); }
            (true, _) => { c.args(["-c:v", "libx264", "-preset", "veryfast", "-crf", "18", "-g", "60"]); }
        }
        c.args(["-pix_fmt", "yuv420p"]);
        if !keep.is_empty() {
            c.args(["-c:a:0", "aac", "-b:a:0", "192k", "-metadata:s:a:0", "title=Mix", "-metadata:s:a:0", "handler_name=Mix"]);
            let sources_mapped = keep.len() > 1 || info.sources.len() > 1;
            if sources_mapped {
                for (k, (t, _)) in keep.iter().enumerate() {
                    let o = k + 1;
                    if trimmed {
                        c.args([format!("-c:a:{o}"), "aac".into(), format!("-b:a:{o}"), "160k".into()]);
                    } else {
                        c.args([format!("-c:a:{o}"), "copy".into()]);
                    }
                    c.args([format!("-metadata:s:a:{o}"), format!("title={}", t.name),
                            format!("-metadata:s:a:{o}"), format!("handler_name={}", t.name)]);
                }
            }
        }
        c.args(["-movflags", "+faststart", "-f", "mp4"]).arg(&part);
        c
    };

    let length = (end - start).max(0.1);
    let mut last_err = None;
    let gpu = crate::gpu::detect().ffmpeg_encoder();
    let encoders: Vec<&'static str> = if gpu == "libx264" { vec!["libx264"] } else { vec![gpu, "libx264"] };
    for encoder in encoders {
        let mut child = attempt(encoder).stdin(Stdio::null()).stdout(Stdio::piped()).stderr(Stdio::piped()).spawn()?;
        let stdout = child.stdout.take().unwrap();
        let mut lines = BufReader::new(stdout).lines();
        while let Some(l) = lines.next_line().await? {
            if let Some(us) = l.strip_prefix("out_time_us=").and_then(|v| v.parse::<f64>().ok()) {
                progress(((us / 1e6 / length) * 100.0).clamp(0.0, 99.0) as u32);
            }
        }
        let output = child.wait_with_output().await?;
        if output.status.success() {
            std::fs::rename(&part, &out)?;
            progress(100);
            return Ok(out);
        }
        let err = String::from_utf8_lossy(&output.stderr).lines().rev().take(3).collect::<Vec<_>>().join(" | ");
        crate::clog!("export with {encoder} failed: {err}");
        let _ = std::fs::remove_file(&part);
        last_err = Some(err);
        if !trimmed {
            break; // no video encoding involved: another encoder won't help
        }
    }
    bail!("export failed: {}", last_err.unwrap_or_default())
}

#[cfg(test)]
mod tests {
    use super::base_name;
    #[test]
    fn names() {
        assert_eq!(base_name("Rocket League 2026-11-01 14-17-58"), "Rocket League");
        assert_eq!(base_name("Rocket League (edited) 2026-11-01 14-17-58"), "Rocket League");
        assert_eq!(base_name("2026-11-01 14-17-58"), "2026-11-01 14-17-58");
        assert_eq!(base_name("My clip"), "My clip");
    }
}

#[cfg(test)]
mod real_tests {
    /// cargo test editor_roundtrip -- --ignored --nocapture  (CLIPPER_TEST_CLIP, CLIPPER_TEST_OUT)
    #[tokio::test]
    #[ignore]
    async fn editor_roundtrip() {
        let clip = std::path::PathBuf::from(std::env::var("CLIPPER_TEST_CLIP").unwrap());
        let out = std::path::PathBuf::from(std::env::var("CLIPPER_TEST_OUT").unwrap());
        let tools = crate::tools::ensure(&out, |_| {}).await.unwrap();
        let info = super::open(&tools, &out.join("cache"), &clip).await.unwrap();
        println!("{info:#?}");
        for (s, e) in [(1.0, 4.0), (0.0, info.duration)] {
            let req = super::ExportRequest { path: info.path.clone(), start: s, end: e, volumes: vec![0.5, 2.0, 1.0] };
            let p = super::export(&tools, &out, &info, &req, |_| {}).await.unwrap();
            println!("exported {}", p.display());
        }
    }
}

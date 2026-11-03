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
    /// Frames per second (for frame-by-frame stepping).
    pub fps: f64,
    /// Sources with their own volume (for a single-track clip: that one track).
    pub sources: Vec<Track>,
    /// Small keyframe images for instant scrubbing: (time in s, file).
    pub thumbs: Vec<(f64, String)>,
    /// Container start time (keyframe times are relative to it).
    pub start: f64,
    /// This clip's own temp folder (tracks + scrub images).
    pub cache: String,
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
    /// Make the file fit in this many MB (e.g. Discord's upload limit):
    /// one Mix track, bitrate (and if needed resolution) chosen to fit.
    #[serde(default)]
    pub target_mb: Option<f64>,
    /// Parts to keep (absolute times, in order). Empty = `start..end`. More
    /// than one = the parts in between were cut out.
    #[serde(default)]
    pub keep: Vec<(f64, f64)>,
    /// Fade in and out (video and Mix), seconds. 0 = none.
    #[serde(default)]
    pub fade: f64,
}

/// The parts to keep: clamped to `start..end`, sorted, touching parts merged,
/// slivers under 0.1 s dropped.
pub fn keep_ranges(req: &ExportRequest, duration: f64) -> Vec<(f64, f64)> {
    let start = req.start.clamp(0.0, duration);
    let end = req.end.clamp((start + 0.2).min(duration), duration);
    let mut r: Vec<(f64, f64)> = if req.keep.is_empty() { vec![(start, end)] } else { req.keep.clone() };
    r = r.into_iter()
        .map(|(a, b)| (a.max(start), b.min(end)))
        .filter(|(a, b)| a.is_finite() && b.is_finite() && b - a >= 0.1)
        .collect();
    r.sort_by(|x, y| x.0.total_cmp(&y.0));
    let mut out: Vec<(f64, f64)> = vec![];
    for (a, b) in r {
        match out.last_mut() {
            Some(last) if a <= last.1 + 0.02 => last.1 = last.1.max(b),
            _ => out.push((a, b)),
        }
    }
    out
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
            "format=duration,start_time:stream=index,codec_type,width,height,avg_frame_rate:stream_tags=name,title,handler_name",
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
        fps: video["avg_frame_rate"].as_str().and_then(parse_rate).unwrap_or(60.0),
        sources,
        thumbs: vec![],
        start,
        cache: cache.to_string_lossy().into_owned(),
    })
}

/// "60000/1001" → 59.94.
fn parse_rate(r: &str) -> Option<f64> {
    let (n, d) = r.split_once('/').unwrap_or((r, "1"));
    let v = n.parse::<f64>().ok()? / d.parse::<f64>().ok()?;
    (v.is_finite() && v > 1.0 && v < 1000.0).then_some(v)
}

/// Save the frame at `time` as a PNG next to the clip ("<Game> (frame) <stamp>.png").
pub async fn save_frame(tools: &Tools, info: &ClipInfo, time: f64) -> Result<PathBuf> {
    let src = Path::new(&info.path);
    let dir = src.parent().context("clip has no folder")?;
    let stamp = chrono::Local::now().format("%Y-%m-%d %H-%M-%S");
    let mut out = dir.join(format!("{} (frame) {stamp}.png", base_name(&info.name)));
    let mut n = 2;
    while out.exists() {
        out = dir.join(format!("{} (frame) {stamp} ({n}).png", base_name(&info.name)));
        n += 1;
    }
    let part = out.with_extension("part.png");
    let mut c = command(&tools.ffmpeg);
    c.args(["-hide_banner", "-v", "error", "-y", "-ss", &format!("{:.3}", time.clamp(0.0, info.duration))])
        .arg("-i").arg(src)
        .args(["-map", "0:v:0", "-frames:v", "1", "-update", "1", "-f", "image2", "-c:v", "png"])
        .arg(&part);
    run(c).await.context("could not save the frame")?;
    std::fs::rename(&part, &out)?;
    Ok(out)
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
    // The last 19 characters (not bytes: "é" is two bytes and must not be cut).
    let cut = s.char_indices().rev().nth(18).map(|(i, _)| i);
    if let Some(cut) = cut.filter(|&c| c > 0) {
        let tail = &s[cut..];
        let shape_ok = tail.chars().enumerate().all(|(i, ch)| match i {
            4 | 7 | 13 | 16 => ch == '-',
            10 => ch == ' ',
            _ => ch.is_ascii_digit(),
        });
        if shape_ok {
            s.truncate(cut);
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
    let ranges = keep_ranges(req, info.duration);
    anyhow::ensure!(!ranges.is_empty(), "Nothing left to save: every part was cut out");
    let (start, end) = (ranges[0].0, ranges[ranges.len() - 1].1);
    let cut = ranges.len() > 1;
    let trimmed = start > 0.05 || end < info.duration - 0.05;
    let length = ranges.iter().map(|(a, b)| b - a).sum::<f64>().max(0.1);
    let fade = if req.fade > 0.0 { req.fade.min(length / 3.0) } else { 0.0 };
    // Cuts and fades go through the filter graph on the CPU.
    let video_graph = cut || fade > 0.0;
    // Fit-to-size: total bits / seconds, minus the audio, with 7 % headroom for the container.
    // Capped at the "High" recording bitrate: a big limit (Nitro, 1 GB) must not
    // make the file larger than the original.
    if let Some(mb) = req.target_mb {
        // Below ~250 kb/s of video the clip can't be made to fit in a watchable way.
        let max_secs = (mb * 8000.0 * 0.93 / (250.0 + 128.0)).floor();
        if length > max_secs {
            bail!("This clip is too long for {mb} MB. Trim it to {max_secs:.0} seconds or less in the editor first.");
        }
    }
    let fit_kbps = req.target_mb.map(|mb| ((mb * 8000.0 * 0.93 / length) - 128.0).clamp(250.0, 40_000.0) as u32);
    let reencode = trimmed || cut || fade > 0.0 || fit_kbps.is_some();
    // Video bitrate of the original (file size minus ~160 kb/s per audio track).
    // A trimmed copy may not use more per second than that, or it ends up
    // bigger than the clip it was cut from.
    let src_kbps: u32 = {
        let bytes = std::fs::metadata(src).map(|m| m.len()).unwrap_or(0) as f64;
        let total = bytes * 8.0 / 1000.0 / info.duration.max(0.1);
        (total - 160.0 * (info.sources.len() + 1) as f64).clamp(1500.0, 60_000.0) as u32
    };
    // Average 85 % of the original, never above it; a 1 s buffer keeps short
    // clips from bursting past the original's size.
    let (sb, smax, sbuf) = (format!("{}k", src_kbps * 85 / 100), format!("{src_kbps}k"), format!("{src_kbps}k"));
    let max_h: Option<u32> = fit_kbps
        .and_then(|k| if k < 2500 { Some(720) } else if k < 6000 { Some(1080) } else if k < 12_000 { Some(1440) } else { None })
        .filter(|&h| h < info.height);

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
    let tag = if req.target_mb.is_some() { "(Discord)" } else { "(edited)" };
    let mut out = clips_dir.join(format!("{} {tag} {stamp}.mp4", base_name(&info.name)));
    let mut n = 2;
    while out.exists() {
        out = clips_dir.join(format!("{} {tag} {stamp} ({n}).mp4", base_name(&info.name)));
        n += 1;
    }
    let part = out.with_extension("part.mp4");

    // `on_gpu`: NVIDIA fit-to-size keeps every frame on the graphics card (CUDA
    // decode → scale_cuda → NVENC). Copying 3440×1440 frames to the CPU and back
    // made a 60 s clip take ~50 s.
    let attempt = |encoder: &'static str, on_gpu: bool| {
        let mut c = command(&tools.ffmpeg);
        c.args(["-hide_banner", "-v", "error", "-y", "-progress", "pipe:1", "-nostats"]);
        if on_gpu {
            c.args(["-hwaccel", "cuda", "-hwaccel_output_format", "cuda"]);
        } else if reencode {
            // GPU decoding for what gets re-encoded (falls back to CPU by itself).
            c.args(["-hwaccel", "auto"]);
        }
        if trimmed {
            c.args(["-ss", &format!("{start:.3}"), "-to", &format!("{end:.3}")]);
        }
        c.arg("-i").arg(src);
        let sources_mapped = fit_kbps.is_none() && (keep.len() > 1 || info.sources.len() > 1);
        let in_mix = |i: usize| mixed.iter().any(|(t, _)| t.index == i);
        let mapped = |i: usize| sources_mapped && keep.iter().any(|(t, _)| t.index == i);
        let mut graph = String::new();
        // Label for each audio track's sound, ready to use once (for the Mix)
        // and once more (as its own track) where needed.
        let mix_in: Vec<String>;
        let out_audio: Vec<String>;
        let video_label: String;
        if cut {
            // After -ss the input starts at 0: times are relative to `start`.
            let used: Vec<usize> = keep.iter().map(|(t, _)| t.index).filter(|&i| in_mix(i) || mapped(i)).collect();
            for (k, (a, b)) in ranges.iter().enumerate() {
                let (a, b) = (a - start, b - start);
                graph += &format!("[0:v:0]trim=start={a:.3}:end={b:.3},setpts=PTS-STARTPTS[v{k}];");
                for &i in &used {
                    graph += &format!("[0:a:{i}]atrim=start={a:.3}:end={b:.3},asetpts=PTS-STARTPTS[a{i}_{k}];");
                }
            }
            for k in 0..ranges.len() {
                graph += &format!("[v{k}]");
                for &i in &used {
                    graph += &format!("[a{i}_{k}]");
                }
            }
            graph += &format!("concat=n={}:v=1:a={}[vc]", ranges.len(), used.len());
            for &i in &used {
                graph += &format!("[c{i}]");
            }
            graph += ";";
            for &i in &used {
                if in_mix(i) && mapped(i) {
                    graph += &format!("[c{i}]asplit=2[m{i}][o{i}];");
                }
            }
            // Mix inputs in `mixed` order (that is the volume order), own tracks in `keep` order.
            let both = |i: usize| in_mix(i) && mapped(i);
            mix_in = mixed.iter().map(|(t, _)| format!("[{}{}]", if both(t.index) { 'm' } else { 'c' }, t.index)).collect();
            out_audio = keep.iter().filter(|(t, _)| mapped(t.index))
                .map(|(t, _)| format!("[{}{}]", if both(t.index) { 'o' } else { 'c' }, t.index)).collect();
            video_label = "[vc]".into();
        } else {
            mix_in = mixed.iter().map(|(t, _)| format!("[0:a:{}]", t.index)).collect();
            out_audio = keep.iter().filter(|(t, _)| mapped(t.index)).map(|(t, _)| format!("0:a:{}", t.index)).collect();
            video_label = "[0:v:0]".into();
        }
        if !mixed.is_empty() {
            for (k, ((_, v), l)) in mixed.iter().zip(&mix_in).enumerate() {
                graph += &format!("{l}volume={:.3}[s{k}];", v.clamp(0.0, 20.0));
            }
            let fades = if fade > 0.0 {
                format!(",afade=t=in:st=0:d={fade:.3},afade=t=out:st={:.3}:d={fade:.3}", length - fade)
            } else { String::new() };
            if mixed.len() == 1 {
                graph += &format!("[s0]anull{fades}[mix]");
            } else {
                for k in 0..mixed.len() {
                    graph += &format!("[s{k}]");
                }
                graph += &format!("amix=inputs={}:normalize=0:dropout_transition=0{fades}[mix]", mixed.len());
            }
        }
        let mut vf: Vec<String> = vec![];
        if fade > 0.0 {
            vf.push(format!("fade=t=in:st=0:d={fade:.3},fade=t=out:st={:.3}:d={fade:.3}", length - fade));
        }
        if let Some(h) = max_h {
            vf.push(if on_gpu { format!("scale_cuda=-2:{h}") } else { format!("scale=-2:{h}") });
        }
        let video_map = if video_graph {
            if !graph.is_empty() && !graph.ends_with(';') {
                graph += ";";
            }
            if vf.is_empty() {
                graph += &format!("{video_label}null[vout]");
            } else {
                graph += &format!("{video_label}{}[vout]", vf.join(","));
            }
            "[vout]".to_string()
        } else {
            "0:v:0".to_string()
        };
        let graph = graph.trim_end_matches(';').to_string();
        if !graph.is_empty() {
            c.args(["-filter_complex", &graph]);
        }
        c.args(["-map", &video_map]);
        if !mixed.is_empty() {
            c.args(["-map", "[mix]"]);
            for l in &out_audio {
                c.args(["-map", l]);
            }
        }
        if !video_graph && !vf.is_empty() {
            c.args(["-vf", &vf.join(",")]);
        }
        if let Some(k) = fit_kbps {
            let (b, buf) = (format!("{k}k"), format!("{}k", k * 2));
            match encoder {
                "h264_nvenc" => c.args(["-c:v", "h264_nvenc", "-preset", "p4", "-rc", "vbr", "-b:v", &b, "-maxrate", &b, "-bufsize", &buf, "-g", "120"]),
                "h264_amf" => c.args(["-c:v", "h264_amf", "-quality", "quality", "-rc", "vbr_peak", "-b:v", &b, "-maxrate", &b, "-bufsize", &buf, "-g", "120"]),
                "h264_qsv" => c.args(["-c:v", "h264_qsv", "-preset", "medium", "-b:v", &b, "-maxrate", &b, "-bufsize", &buf, "-g", "120"]),
                _ => c.args(["-c:v", "libx264", "-preset", "veryfast", "-b:v", &b, "-maxrate", &b, "-bufsize", &buf, "-g", "120"]),
            };
        } else {
        match (reencode, encoder) {
            (false, _) => { c.args(["-c:v", "copy"]); }
            (true, "h264_nvenc") => { c.args(["-c:v", "h264_nvenc", "-preset", "p5", "-rc", "vbr", "-b:v", &sb, "-maxrate", &smax, "-bufsize", &sbuf, "-g", "60"]); }
            (true, "h264_amf") => { c.args(["-c:v", "h264_amf", "-quality", "quality", "-rc", "vbr_peak", "-b:v", &sb, "-maxrate", &smax, "-bufsize", &sbuf, "-g", "60"]); }
            (true, "h264_qsv") => { c.args(["-c:v", "h264_qsv", "-preset", "medium", "-b:v", &sb, "-maxrate", &smax, "-bufsize", &sbuf, "-g", "60"]); }
            (true, _) => { c.args(["-c:v", "libx264", "-preset", "veryfast", "-b:v", &sb, "-maxrate", &smax, "-bufsize", &sbuf, "-g", "60"]); }
        }
        }
        if !on_gpu {
            c.args(["-pix_fmt", "yuv420p"]);
        }
        if !keep.is_empty() {
            let ab = if fit_kbps.is_some() { "128k" } else { "160k" }; // same as OBS
            c.args(["-c:a:0", "aac", "-b:a:0", ab, "-metadata:s:a:0", "title=Mix", "-metadata:s:a:0", "handler_name=Mix"]);
            if sources_mapped {
                for (k, (t, _)) in keep.iter().filter(|(t, _)| mapped(t.index)).enumerate() {
                    let o = k + 1;
                    if reencode {
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

    let mut last_err = None;
    let gpu = crate::gpu::detect().ffmpeg_encoder();
    let mut encoders: Vec<(&'static str, bool)> = vec![];
    if gpu == "h264_nvenc" && fit_kbps.is_some() && !video_graph {
        encoders.push((gpu, true));
    }
    if gpu != "libx264" {
        encoders.push((gpu, false));
    }
    encoders.push(("libx264", false));
    for (encoder, on_gpu) in encoders {
        let t0 = std::time::Instant::now();
        let mut child = attempt(encoder, on_gpu).stdin(Stdio::null()).stdout(Stdio::piped()).stderr(Stdio::piped()).spawn()?;
        let stdout = child.stdout.take().unwrap();
        let mut lines = BufReader::new(stdout).lines();
        while let Some(l) = lines.next_line().await? {
            if let Some(us) = l.strip_prefix("out_time_us=").and_then(|v| v.parse::<f64>().ok()) {
                progress(((us / 1e6 / length) * 100.0).clamp(0.0, 99.0) as u32);
            }
        }
        let output = child.wait_with_output().await?;
        if output.status.success() {
            crate::clog!("export: {encoder}{} {:.1} s for {length:.1} s of video", if on_gpu { " (all on GPU)" } else { "" }, t0.elapsed().as_secs_f64());
            std::fs::rename(&part, &out)?;
            progress(100);
            return Ok(out);
        }
        let err = String::from_utf8_lossy(&output.stderr).lines().rev().take(3).collect::<Vec<_>>().join(" | ");
        crate::clog!("export with {encoder}{} failed: {err}", if on_gpu { " (all on GPU)" } else { "" });
        let _ = std::fs::remove_file(&part);
        last_err = Some(err);
        if !reencode {
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
            let req = super::ExportRequest { path: info.path.clone(), start: s, end: e, volumes: vec![0.5, 2.0, 1.0], target_mb: None, keep: vec![], fade: 0.0 };
            let p = super::export(&tools, &out, &info, &req, |_| {}).await.unwrap();
            println!("exported {}", p.display());
        }
    }
}

#[cfg(test)]
mod fit_tests {
    /// cargo test editor_fit -- --ignored --nocapture  (CLIPPER_TEST_CLIP, CLIPPER_TEST_OUT)
    #[tokio::test]
    #[ignore]
    async fn editor_fit() {
        let clip = std::path::PathBuf::from(std::env::var("CLIPPER_TEST_CLIP").unwrap());
        let out = std::path::PathBuf::from(std::env::var("CLIPPER_TEST_OUT").unwrap());
        let tools = crate::tools::ensure(&out, |_| {}).await.unwrap();
        let info = super::open(&tools, &out.join("cache"), &clip).await.unwrap();
        let req = super::ExportRequest { path: info.path.clone(), start: 0.0, end: info.duration, volumes: vec![], target_mb: Some(10.0), keep: vec![], fade: 0.0 };
        let p = super::export(&tools, &out, &info, &req, |_| {}).await.unwrap();
        println!("fit {} -> {:.2} MB", p.display(), std::fs::metadata(&p).unwrap().len() as f64 / 1e6);
    }
}

/// Put clips after each other in one video (in the given order). Every part is
/// scaled to the first clip's size; only the Mix track is kept (clips can have
/// different source tracks).
pub async fn combine(tools: &Tools, out_dir: &Path, paths: &[PathBuf], progress: impl Fn(u32)) -> Result<PathBuf> {
    anyhow::ensure!(paths.len() >= 2, "Pick at least two clips");
    let mut lens = vec![];
    let (mut w, mut h) = (0u32, 0u32);
    for (i, p) in paths.iter().enumerate() {
        let mut c = command(&tools.ffprobe);
        c.args(["-v", "error", "-show_entries", "format=duration:stream=codec_type,width,height", "-of", "json"]).arg(p);
        let v: serde_json::Value = serde_json::from_slice(&run(c).await?.stdout)?;
        let d: f64 = v["format"]["duration"].as_str().and_then(|d| d.parse().ok()).context("no duration")?;
        let streams = v["streams"].as_array().cloned().unwrap_or_default();
        anyhow::ensure!(streams.iter().any(|s| s["codec_type"] == "audio"),
            "{} has no sound track", p.file_name().unwrap_or_default().to_string_lossy());
        if i == 0 {
            let vs = streams.iter().find(|s| s["codec_type"] == "video").context("no video")?;
            w = vs["width"].as_u64().unwrap_or(1920) as u32 & !1;
            h = vs["height"].as_u64().unwrap_or(1080) as u32 & !1;
        }
        lens.push(d);
    }
    let total: f64 = lens.iter().sum();
    let first = paths[0].file_stem().unwrap_or_default().to_string_lossy().into_owned();
    let stamp = chrono::Local::now().format("%Y-%m-%d %H-%M-%S");
    let out = out_dir.join(format!("{} (combined) {stamp}.mp4", base_name(&first)));
    let part = out.with_extension("part.mp4");

    let mut graph = String::new();
    for i in 0..paths.len() {
        graph += &format!(
            "[{i}:v:0]scale={w}:{h}:force_original_aspect_ratio=decrease,pad={w}:{h}:(ow-iw)/2:(oh-ih)/2,setsar=1,fps=60,format=yuv420p[v{i}];\
             [{i}:a:0]aformat=sample_rates=48000:channel_layouts=stereo[a{i}];"
        );
    }
    for i in 0..paths.len() {
        graph += &format!("[v{i}][a{i}]");
    }
    graph += &format!("concat=n={}:v=1:a=1[v][a]", paths.len());

    let gpu = crate::gpu::detect().ffmpeg_encoder();
    let encoders: Vec<&'static str> = if gpu == "libx264" { vec!["libx264"] } else { vec![gpu, "libx264"] };
    let mut last_err = String::new();
    for encoder in encoders {
        let mut c = command(&tools.ffmpeg);
        c.args(["-hide_banner", "-v", "error", "-y", "-progress", "pipe:1", "-nostats"]);
        for p in paths {
            c.args(["-hwaccel", "auto", "-i"]).arg(p);
        }
        c.args(["-filter_complex", &graph, "-map", "[v]", "-map", "[a]"]);
        match encoder {
            "h264_nvenc" => c.args(["-c:v", "h264_nvenc", "-preset", "p5", "-rc", "vbr", "-cq", "19", "-b:v", "0", "-g", "60"]),
            "h264_amf" => c.args(["-c:v", "h264_amf", "-quality", "quality", "-rc", "cqp", "-qp_i", "19", "-qp_p", "21", "-qp_b", "23", "-g", "60"]),
            "h264_qsv" => c.args(["-c:v", "h264_qsv", "-preset", "medium", "-global_quality", "21", "-g", "60"]),
            _ => c.args(["-c:v", "libx264", "-preset", "veryfast", "-crf", "18", "-g", "60"]),
        };
        c.args(["-c:a", "aac", "-b:a", "192k", "-metadata:s:a:0", "title=Mix", "-metadata:s:a:0", "handler_name=Mix",
                "-movflags", "+faststart", "-f", "mp4"]).arg(&part);
        let mut child = c.stdin(Stdio::null()).stdout(Stdio::piped()).stderr(Stdio::piped()).spawn()?;
        let mut lines = BufReader::new(child.stdout.take().unwrap()).lines();
        while let Some(l) = lines.next_line().await? {
            if let Some(us) = l.strip_prefix("out_time_us=").and_then(|v| v.parse::<f64>().ok()) {
                progress(((us / 1e6 / total) * 100.0).clamp(0.0, 99.0) as u32);
            }
        }
        let output = child.wait_with_output().await?;
        if output.status.success() {
            std::fs::rename(&part, &out)?;
            progress(100);
            return Ok(out);
        }
        last_err = String::from_utf8_lossy(&output.stderr).lines().rev().take(3).collect::<Vec<_>>().join(" | ");
        crate::clog!("combine with {encoder} failed: {last_err}");
        let _ = std::fs::remove_file(&part);
    }
    bail!("combining failed: {last_err}")
}

#[cfg(test)]
mod combine_tests {
    /// cargo test editor_combine -- --ignored --nocapture  (CLIPPER_TEST_CLIPS = a|b|c, CLIPPER_TEST_OUT)
    #[tokio::test]
    #[ignore]
    async fn editor_combine() {
        let clips: Vec<std::path::PathBuf> = std::env::var("CLIPPER_TEST_CLIPS").unwrap().split('|').map(Into::into).collect();
        let out = std::path::PathBuf::from(std::env::var("CLIPPER_TEST_OUT").unwrap());
        let tools = crate::tools::ensure(&out, |_| {}).await.unwrap();
        let p = super::combine(&tools, &out, &clips, |_| {}).await.unwrap();
        println!("combined {}", p.display());
    }
}

#[cfg(test)]
mod base_name_tests {
    #[test]
    fn non_ascii_names_do_not_panic() {
        assert_eq!(super::base_name("Rocket League 2026-11-01 14-17-58"), "Rocket League");
        assert_eq!(super::base_name("Résumé van de finale é"), "Résumé van de finale é");
        assert_eq!(super::base_name("Ölçek ü 2026-11-01 14-17-58"), "Ölçek ü");
        assert_eq!(super::base_name("ééééééééééééééééééééé"), "ééééééééééééééééééééé");
        assert_eq!(super::base_name("Goal (edited)"), "Goal");
    }
}

#[cfg(test)]
mod keep_tests {
    use super::{keep_ranges, parse_rate, ExportRequest};
    fn req(start: f64, end: f64, keep: Vec<(f64, f64)>) -> ExportRequest {
        ExportRequest { path: String::new(), start, end, volumes: vec![], target_mb: None, keep, fade: 0.0 }
    }
    #[test]
    fn ranges() {
        assert_eq!(keep_ranges(&req(1.0, 9.0, vec![]), 10.0), vec![(1.0, 9.0)]);
        // clamped to the trim, sorted, touching parts merged, slivers dropped
        assert_eq!(keep_ranges(&req(1.0, 9.0, vec![(5.0, 12.0), (0.0, 3.0), (3.0, 4.0), (4.5, 4.55)]), 10.0),
                   vec![(1.0, 4.0), (5.0, 9.0)]);
        assert!(keep_ranges(&req(0.0, 10.0, vec![(20.0, 30.0)]), 10.0).is_empty());
    }
    #[test]
    fn rates() {
        assert_eq!(parse_rate("60/1"), Some(60.0));
        assert!((parse_rate("60000/1001").unwrap() - 59.94).abs() < 0.01);
        assert_eq!(parse_rate("0/0"), None);
    }
}

#[cfg(test)]
mod cut_tests {
    /// cargo test editor_cut -- --ignored --nocapture  (CLIPPER_TEST_CLIP, CLIPPER_TEST_OUT)
    #[tokio::test]
    #[ignore]
    async fn editor_cut() {
        let clip = std::path::PathBuf::from(std::env::var("CLIPPER_TEST_CLIP").unwrap());
        let out = std::path::PathBuf::from(std::env::var("CLIPPER_TEST_OUT").unwrap());
        let tools = crate::tools::ensure(&out, |_| {}).await.unwrap();
        let info = super::open(&tools, &out.join("cache"), &clip).await.unwrap();
        let d = info.duration;
        let req = super::ExportRequest {
            path: info.path.clone(), start: 0.5, end: d - 0.5, volumes: vec![1.0, 0.5, 2.0], target_mb: None,
            keep: vec![(0.5, d * 0.3), (d * 0.5, d * 0.7), (d * 0.8, d - 0.5)], fade: 0.5,
        };
        let p = super::export(&tools, &out, &info, &req, |_| {}).await.unwrap();
        println!("cut {} (expected {:.2} s)", p.display(), d * 0.3 - 0.5 + d * 0.2 + d * 0.2 - 0.5);
        let f = super::save_frame(&tools, &info, d / 2.0).await.unwrap();
        println!("frame {}", f.display());
    }
}

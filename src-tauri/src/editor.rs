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
    sync::atomic::{AtomicBool, Ordering},
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

/// Open a clip in the editor: probe it and extract the source tracks for the preview.
pub async fn open(tools: &Tools, cache: &Path, path: &Path) -> Result<ClipInfo> {
    probe_into(tools, Some(cache), path).await
}

/// Only read what the clip contains (no files written): enough for an export.
pub async fn probe(tools: &Tools, path: &Path) -> Result<ClipInfo> {
    probe_into(tools, None, path).await
}

async fn probe_into(tools: &Tools, cache: Option<&Path>, path: &Path) -> Result<ClipInfo> {
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

    let mut sources = vec![];
    if let Some(cache) = cache {
        let _ = std::fs::remove_dir_all(cache);
        std::fs::create_dir_all(cache)?;
    }
    if cache.is_none() {
        for &i in &picks {
            sources.push(Track { index: i, name: track_name(i, &audio[i]["tags"], audio.len()), file: String::new() });
        }
    } else if let Some(cache) = cache.filter(|_| !picks.is_empty()) {
        let mut c = command(&tools.ffmpeg);
        c.args(["-v", "error", "-y", "-i"]).arg(path);
        for &i in &picks {
            let f = cache.join(format!("track{i}.m4a"));
            c.args(["-map", &format!("0:a:{i}"), "-c", "copy", "-vn"]).arg(&f);
            sources.push(Track {
                index: i,
                name: track_name(i, &audio[i]["tags"], audio.len()),
                file: f.to_string_lossy().into_owned(),
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
        cache: cache.map(|c| c.to_string_lossy().into_owned()).unwrap_or_default(),
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
    let out = crate::clips::free_name(dir, &format!("{} (frame) {stamp}", base_name(&info.name)), Some("png"));
    let part = out.with_extension("part.png");
    let mut c = command(&tools.ffmpeg);
    c.args(["-hide_banner", "-v", "error", "-y", "-ss", &format!("{:.3}", time.clamp(0.0, (info.duration - 1.0 / info.fps.max(1.0)).max(0.0)))])
        .arg("-i").arg(src)
        .args(["-map", "0:v:0", "-frames:v", "1", "-update", "1", "-f", "image2", "-c:v", "png"])
        .arg(&part);
    let done = match run(c).await.context("could not save the frame") {
        Ok(_) if part.exists() => std::fs::rename(&part, &out).context("could not save the frame"),
        Ok(_) => Err(anyhow::anyhow!("no picture at this point of the clip")),
        Err(e) => Err(e),
    };
    if let Err(e) = done {
        let _ = std::fs::remove_file(&part);
        return Err(e);
    }
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

/// Most cut parts the all-GPU path takes on: each part is its own CUDA decoder.
const MAX_GPU_PARTS: usize = 4;
/// Set when the all-GPU path failed once; later saves go straight to the CPU path.
static GPU_PATH_FAILED: AtomicBool = AtomicBool::new(false);

/// Pixel format of the first video stream.
async fn pix_fmt(tools: &Tools, src: &Path) -> Option<String> {
    let mut c = command(&tools.ffprobe);
    c.args(["-v", "error", "-select_streams", "v:0", "-show_entries", "stream=pix_fmt", "-of", "default=nw=1:nk=1"]).arg(src);
    let out = run(c).await.ok()?;
    Some(String::from_utf8_lossy(&out.stdout).trim().to_string()).filter(|s| !s.is_empty())
}

/// How the video bitrate is chosen: a target (export) or a constant quality (combine).
enum Rate<'a> {
    Vbr { b: &'a str, max: &'a str, buf: &'a str, gop: &'a str },
    Quality,
}

/// ffmpeg video codec arguments for one encoder. The one place encoder settings live.
fn video_args(encoder: &str, rate: Rate) -> Vec<String> {
    let mut a: Vec<&str> = match encoder {
        "h264_nvenc" => vec!["-c:v", "h264_nvenc", "-preset", "p4", "-rc", "vbr"],
        "h264_amf" => vec!["-c:v", "h264_amf", "-quality", "quality"],
        "h264_qsv" => vec!["-c:v", "h264_qsv", "-preset", "medium"],
        _ => vec!["-c:v", "libx264", "-preset", "veryfast"],
    };
    let gop = match rate {
        Rate::Vbr { b, max, buf, gop } => {
            if encoder == "h264_amf" {
                a.extend(["-rc", "vbr_peak"]);
            }
            a.extend(["-b:v", b, "-maxrate", max, "-bufsize", buf]);
            gop
        }
        Rate::Quality => {
            a.extend(match encoder {
                "h264_nvenc" => &["-cq", "19", "-b:v", "0"][..],
                "h264_amf" => &["-rc", "cqp", "-qp_i", "19", "-qp_p", "21", "-qp_b", "23"][..],
                "h264_qsv" => &["-global_quality", "21"][..],
                _ => &["-crf", "18"][..],
            });
            "60"
        }
    };
    a.extend(["-g", gop]);
    a.into_iter().map(String::from).collect()
}

/// Runs an ffmpeg command that writes `-progress pipe:1`, reporting progress
/// against `total` seconds. stderr is read at the same time: a full stderr pipe
/// (~4 KB on Windows, e.g. many decode errors) would otherwise stall ffmpeg.
/// `None` = success, `Some(last error lines)` = failed.
async fn run_with_progress(mut c: tokio::process::Command, total: f64, progress: &impl Fn(u32)) -> Result<Option<String>> {
    let mut child = c.stdin(Stdio::null()).stdout(Stdio::piped()).stderr(Stdio::piped()).spawn()?;
    let stderr = child.stderr.take().context("no stderr")?;
    let errors = tokio::spawn(async move {
        let mut tail = std::collections::VecDeque::new();
        let mut lines = BufReader::new(stderr).lines();
        while let Ok(Some(l)) = lines.next_line().await {
            if tail.len() == 3 {
                tail.pop_front();
            }
            tail.push_back(l);
        }
        tail.into_iter().rev().collect::<Vec<_>>().join(" | ")
    });
    let mut lines = BufReader::new(child.stdout.take().context("no stdout")?).lines();
    while let Some(l) = lines.next_line().await? {
        if let Some(us) = l.strip_prefix("out_time_us=").and_then(|v| v.parse::<f64>().ok()) {
            progress(((us / 1e6 / total.max(0.1)) * 100.0).clamp(0.0, 99.0) as u32);
        }
    }
    let status = child.wait().await?;
    let err = errors.await.unwrap_or_default();
    Ok(if status.success() { None } else { Some(err) })
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
    // Cuts and fades go through the filter graph (on the GPU or the CPU, see `on_gpu`).
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
    // Only the kept parts count: sound in a cut-out part doesn't make a track
    // worth keeping. All tracks are measured at the same time.
    let peaks = futures_util::future::join_all(info.sources.iter().map(|t| {
        let ranges = &ranges;
        async move {
            let mut peak = f64::NEG_INFINITY;
            for &(a, b) in ranges {
                peak = peak.max(peak_db(tools, src, t.index, a, b).await.unwrap_or(0.0));
                if peak > SILENT_DB {
                    break;
                }
            }
            peak
        }
    }))
    .await;
    let mut keep: Vec<(&Track, f64)> = vec![];
    for ((t, &v), peak) in info.sources.iter().zip(req.volumes.iter().chain(std::iter::repeat(&1.0))).zip(peaks) {
        if peak > SILENT_DB {
            keep.push((t, v));
        } else {
            crate::clog!("export: leaving out silent track {} ({peak:.0} dB)", t.name);
        }
    }
    if keep.is_empty() {
        keep = info.sources.iter().zip(req.volumes.iter().copied().chain(std::iter::repeat(1.0))).collect();
    }

    // Every kept source goes into the new Mix.
    let mixed = &keep;

    let stamp = chrono::Local::now().format("%Y-%m-%d %H-%M-%S");
    let tag = if req.target_mb.is_some() { "(Discord)" } else { "(edited)" };
    let out = crate::clips::free_name(clips_dir, &format!("{} {tag} {stamp}", base_name(&info.name)), Some("mp4"));
    let part = out.with_extension("part.mp4");

    // `on_gpu`: on NVIDIA, a re-encode without a fade keeps every frame on the
    // graphics card (CUDA decode → concat/scale_cuda → NVENC). Copying 3440×1440
    // frames to the CPU and back made a 60 s clip take ~50 s.
    // Mix + every kept source as its own track (so the clip can be edited again).
    let sources_mapped = fit_kbps.is_none() && (keep.len() > 1 || info.sources.len() > 1);
    let attempt = |encoder: &'static str, on_gpu: bool| {
        let mut c = command(&tools.ffmpeg);
        c.args(["-hide_banner", "-v", "error", "-y", "-progress", "pipe:1", "-nostats"]);
        let hw = |c: &mut tokio::process::Command| {
            if on_gpu {
                c.args(["-hwaccel", "cuda", "-hwaccel_output_format", "cuda"]);
            } else if reencode {
                // GPU decoding for what gets re-encoded (falls back to CPU by itself).
                c.args(["-hwaccel", "auto"]);
            }
        };
        if cut {
            // One input per kept part: ffmpeg only decodes what is kept.
            for (a, b) in &ranges {
                hw(&mut c);
                c.args(["-ss", &format!("{a:.3}"), "-to", &format!("{b:.3}"), "-i"]).arg(src);
            }
        } else {
            hw(&mut c);
            if trimmed {
                c.args(["-ss", &format!("{start:.3}"), "-to", &format!("{end:.3}")]);
            }
            c.arg("-i").arg(src);
        }
        // Filter chains, joined with ';'. A filter output can be used only once;
        // an input stream ([0:a:1]) as often as needed.
        let mut graph: Vec<String> = vec![];
        let mut mix_label = std::collections::HashMap::new();
        let mut own_label = std::collections::HashMap::new();
        let video_label: String;
        if cut {
            let used: Vec<usize> = keep.iter().map(|(t, _)| t.index).collect();
            let mut chain = String::new();
            for k in 0..ranges.len() {
                chain += &format!("[{k}:v:0]");
                for &i in &used {
                    chain += &format!("[{k}:a:{i}]");
                }
            }
            chain += &format!("concat=n={}:v=1:a={}[vc]", ranges.len(), used.len());
            for &i in &used {
                chain += &format!("[c{i}]");
            }
            graph.push(chain);
            for &i in &used {
                // In the Mix, and (when sources are kept) as its own track too.
                if sources_mapped {
                    graph.push(format!("[c{i}]asplit=2[m{i}][o{i}]"));
                    mix_label.insert(i, format!("[m{i}]"));
                    own_label.insert(i, format!("[o{i}]"));
                } else {
                    mix_label.insert(i, format!("[c{i}]"));
                }
            }
            video_label = "[vc]".into();
        } else {
            for (t, _) in &keep {
                mix_label.insert(t.index, format!("[0:a:{}]", t.index));
                own_label.insert(t.index, format!("[0:a:{}]", t.index));
            }
            video_label = "[0:v:0]".into();
        }
        let afade = format!("afade=t=in:st=0:d={fade:.3},afade=t=out:st={:.3}:d={fade:.3}", length - fade);
        if !mixed.is_empty() {
            for (k, (t, v)) in mixed.iter().enumerate() {
                graph.push(format!("{}volume={:.3}[s{k}]", mix_label[&t.index], v.clamp(0.0, 20.0)));
            }
            let fades = if fade > 0.0 { format!(",{afade}") } else { String::new() };
            let ins: String = (0..mixed.len()).map(|k| format!("[s{k}]")).collect();
            graph.push(if mixed.len() == 1 {
                format!("[s0]anull{fades}[mix]")
            } else {
                format!("{ins}amix=inputs={}:normalize=0:dropout_transition=0{fades}[mix]", mixed.len())
            });
        }
        // The sources' own tracks: faded like the Mix, or straight from the input.
        let mut own_maps: Vec<String> = vec![];
        if sources_mapped {
            for (k, (t, _)) in keep.iter().enumerate() {
                let l = &own_label[&t.index];
                if fade > 0.0 {
                    graph.push(format!("{l}{afade}[f{k}]"));
                    own_maps.push(format!("[f{k}]"));
                } else if cut {
                    own_maps.push(l.clone());
                } else {
                    own_maps.push(format!("0:a:{}", t.index));
                }
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
            let f = if vf.is_empty() { "null".to_string() } else { vf.join(",") };
            graph.push(format!("{video_label}{f}[vout]"));
            "[vout]".to_string()
        } else {
            "0:v:0".to_string()
        };
        if !graph.is_empty() {
            c.args(["-filter_complex", &graph.join(";")]);
        }
        c.args(["-map", &video_map]);
        if !mixed.is_empty() {
            c.args(["-map", "[mix]"]);
            for l in &own_maps {
                c.args(["-map", l]);
            }
        }
        if !video_graph && !vf.is_empty() {
            c.args(["-vf", &vf.join(",")]);
        }
        match (fit_kbps, reencode) {
            (Some(k), _) => {
                let (b, buf) = (format!("{k}k"), format!("{}k", k * 2));
                c.args(video_args(encoder, Rate::Vbr { b: &b, max: &b, buf: &buf, gop: "120" }));
            }
            (None, false) => {
                c.args(["-c:v", "copy"]);
            }
            (None, true) => {
                c.args(video_args(encoder, Rate::Vbr { b: &sb, max: &smax, buf: &sbuf, gop: "60" }));
            }
        }
        if !on_gpu {
            c.args(["-pix_fmt", "yuv420p"]);
        }
        if !keep.is_empty() {
            let ab = if fit_kbps.is_some() { "128k" } else { "160k" }; // same as OBS
            c.args(["-c:a:0", "aac", "-b:a:0", ab, "-metadata:s:a:0", "title=Mix", "-metadata:s:a:0", "handler_name=Mix"]);
            if sources_mapped {
                for (k, (t, _)) in keep.iter().enumerate() {
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
    // Everything on the graphics card whenever the video is re-encoded, except
    // fades (no CUDA fade filter: those go through the CPU path below). Only for
    // plain 8-bit 4:2:0 sources: NVDEC can't decode 4:4:4 H.264, and without
    // the CPU path's `-pix_fmt yuv420p` a 10-bit source would stay 10-bit.
    // Few cut parts only (one decoder each), and never again this session once
    // it has failed, so a failing GPU path doesn't double every save.
    if gpu == "h264_nvenc"
        && reencode
        && fade == 0.0
        && ranges.len() <= MAX_GPU_PARTS
        && !GPU_PATH_FAILED.load(Ordering::Relaxed)
        && matches!(pix_fmt(tools, src).await.as_deref(), Some("yuv420p" | "yuvj420p" | "nv12"))
    {
        encoders.push((gpu, true));
    }
    if gpu != "libx264" {
        encoders.push((gpu, false));
    }
    encoders.push(("libx264", false));
    for (encoder, on_gpu) in encoders {
        let t0 = std::time::Instant::now();
        let Some(err) = run_with_progress(attempt(encoder, on_gpu), length, &progress).await? else {
            crate::clog!("export: {encoder}{} {:.1} s for {length:.1} s of video", if on_gpu { " (all on GPU)" } else { "" }, t0.elapsed().as_secs_f64());
            std::fs::rename(&part, &out)?;
            progress(100);
            return Ok(out);
        };
        crate::clog!("export with {encoder}{} failed: {err}", if on_gpu { " (all on GPU)" } else { "" });
        if on_gpu {
            GPU_PATH_FAILED.store(true, Ordering::Relaxed);
        }
        let _ = std::fs::remove_file(&part);
        last_err = Some(err);
        if !reencode {
            break; // no video encoding involved: another encoder won't help
        }
    }
    bail!("export failed: {}", last_err.unwrap_or_default())
}

/// Shared setup for the ignored real-ffmpeg tests: CLIPPER_TEST_CLIP and
/// CLIPPER_TEST_OUT; logs to <out>/clipper.log from the start.
#[cfg(test)]
async fn test_env() -> (Tools, PathBuf, ClipInfo) {
    let clip = PathBuf::from(std::env::var("CLIPPER_TEST_CLIP").unwrap());
    let out = PathBuf::from(std::env::var("CLIPPER_TEST_OUT").unwrap());
    crate::log::init(out.join("clipper.log"));
    let tools = crate::tools::ensure(&out, |_| {}).await.unwrap();
    let info = open(&tools, &out.join("cache"), &clip).await.unwrap();
    (tools, out, info)
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
        let (tools, out, info) = super::test_env().await;
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
        let (tools, out, info) = super::test_env().await;
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
    let out = crate::clips::free_name(out_dir, &format!("{} (combined) {stamp}", base_name(&first)), Some("mp4"));
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
        c.args(video_args(encoder, Rate::Quality));
        c.args(["-c:a", "aac", "-b:a", "192k", "-metadata:s:a:0", "title=Mix", "-metadata:s:a:0", "handler_name=Mix",
                "-movflags", "+faststart", "-f", "mp4"]).arg(&part);
        let Some(err) = run_with_progress(c, total, &progress).await? else {
            std::fs::rename(&part, &out)?;
            progress(100);
            return Ok(out);
        };
        last_err = err;
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
        let (tools, out, info) = super::test_env().await;
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

#[cfg(test)]
mod perf_tests {
    /// cargo test editor_perf -- --ignored --nocapture  (CLIPPER_TEST_CLIP, CLIPPER_TEST_OUT)
    #[tokio::test]
    #[ignore]
    async fn editor_perf() {
        let (tools, out, info) = super::test_env().await;
        let d = info.duration;
        assert!(d > 40.0, "editor_perf needs a test clip longer than 40 s (the cut case would be a plain trim)");
        let cases = [
            ("trim", vec![], 0.0),
            ("cut", vec![(5.0, 20.0), (30.0, d - 5.0)], 0.0),
            ("trim+fade", vec![], 0.5),
        ];
        for (name, keep, fade) in cases {
            let req = super::ExportRequest {
                path: info.path.clone(), start: 5.0, end: d - 5.0, volumes: vec![], target_mb: None, keep, fade,
            };
            let t0 = std::time::Instant::now();
            let p = super::export(&tools, &out, &info, &req, |_| {}).await.unwrap();
            println!("{name}: {:.1} s -> {}", t0.elapsed().as_secs_f64(), p.display());
        }
    }
}

// Clip editor: trim (timeline handles, I/O), per-track volume preview via
// WebAudio (tracks decoded from bytes the app hands over, so gain > 100% works),
// export through the app (ffmpeg). Loaded after main.js and uses its helpers.
const ed = { info: null, start: 0, end: 0, tracks: [], ctx: null, playingFrom: null, busy: false, opening: 0 };
const edv = $("ed-video");
// While dragging, show the nearest keyframe image instantly and only seek the
// real video when the mouse is released (seeking a big clip is slow).
function thumbAt(t) {
  const th = ed.info?.thumbs || [];
  if (!th.length) return null;
  let lo = 0, hi = th.length - 1;
  while (lo < hi) { const mid = (lo + hi + 1) >> 1; if (th[mid][0] <= t) lo = mid; else hi = mid - 1; }
  return th[lo][1];
}
function preview(t) {
  ed.shownTime = t;
  const f = thumbAt(t);
  if (!f) return seekTo(t);   // no images: fall back to seeking
  $("ed-scrub").src = convertFileSrc(f);
  $("ed-scrub").classList.add("on");
}
function endPreview() {
  if (ed.shownTime == null) return;
  const t = ed.shownTime;
  edv.addEventListener("seeked", () => $("ed-scrub").classList.remove("on"), { once: true });
  seekTo(t);
}

// Seeking a big clip is slow: never queue seeks. While one runs, remember only
// the latest wanted time and jump there when it finishes.
let seekWanted = null;
function seekTo(t) {
  ed.shownTime = t;
  if (edv.seeking) { seekWanted = t; return; }
  edv.currentTime = t;
}
edv.addEventListener("seeked", () => {
  if (seekWanted !== null) { const t = seekWanted; seekWanted = null; edv.currentTime = t; }
  else ed.shownTime = null;
});
const fmtT = (s) => `${Math.floor(s / 60)}:${(s % 60).toFixed(1).padStart(4, "0")}`;

function showView(name) {
  document.querySelectorAll(".tab").forEach((t) => t.classList.toggle("active", t.dataset.view === name));
  document.querySelectorAll(".view").forEach((v) => v.classList.toggle("hidden", v.id !== "view-" + name));
}

async function openEditor(c) {
  // Each open gets a number; an older open that is still loading stops as soon
  // as it sees a newer one, so tracks of two clips never get mixed.
  const gen = ++ed.opening;
  const stale = () => gen !== ed.opening;
  showView("editor");
  $("ed-title").textContent = c.name;
  $("ed-status").textContent = "Opening…";
  $("ed-tracks").replaceChildren();
  $("ed-export").disabled = true;
  try {
    const info = await invoke("editor_open", { path: c.path });
    if (stale()) return;
    stopAudio();
    ed.info = info; ed.start = 0; ed.end = info.duration;
    $("ed-scrub").classList.remove("on");
    edv.src = convertFileSrc(info.path);
    ed.ctx = ed.ctx || new AudioContext();
    ed.tracks = [];
    const tracks = [];
    for (const s of info.sources) {
      const bytes = await invoke("editor_track", { path: info.path, index: s.index });
      if (stale()) return;
      const buf = await ed.ctx.decodeAudioData(bytes);
      if (stale()) return;
      const gain = ed.ctx.createGain();
      gain.connect(ed.ctx.destination);
      tracks.push({ s, buf, gain, node: null });
    }
    ed.tracks = tracks;
    renderTracks();
    renderTimeline();
    const forPath = info.path;
    invoke("editor_thumbs").then((th) => {
      if (ed.info?.path !== forPath) return;
      ed.info.thumbs = th;
      for (const [, f] of th.slice(0, 400)) { new Image().src = convertFileSrc(f); }   // warm the cache
    }).catch((e) => uiLog("scrub images: " + e));
    uiLog(`editor ready: ${ed.tracks.length} tracks decoded (${ed.tracks.map((t) => `${t.s.name} ${t.buf.duration.toFixed(1)}s`).join(", ")})`);
    $("ed-status").textContent = "";
    $("ed-export").disabled = false;
  } catch (err) {
    if (stale()) return;   // a newer clip took over; its own open reports its errors
    $("ed-status").textContent = String(err); uiLog("editor open failed: " + err);
  }
}

function renderTracks() {
  $("ed-tracks").replaceChildren(...ed.tracks.flatMap((t) => {
    const name = document.createElement("span");
    name.textContent = t.s.name;
    const r = document.createElement("input");
    r.type = "range"; r.min = 0; r.max = 500; r.step = 5; r.value = 100;   // 100% = as recorded
    if (!t.s.in_mix) {   // already part of "Desktop": would be heard twice
      r.disabled = true; r.value = 0; t.gain.gain.value = 0;
      name.title = "Already part of the Desktop track";
      name.textContent += " (in Desktop)";
    }
    const out = document.createElement("output");
    out.textContent = "100%";
    r.oninput = () => { t.gain.gain.value = r.value / 100; out.textContent = r.value + "%"; };
    return [name, r, out];
  }));
}

// --- audio follows the (muted) video ---
function stopAudio() {
  for (const t of ed.tracks) { try { t.node?.stop(); } catch {} t.node = null; }
  ed.playingFrom = null;
}
function startAudio() {
  stopAudio();
  if (!ed.ctx) return;
  ed.ctx.resume();
  const at = edv.currentTime;
  for (const t of ed.tracks) {
    const n = ed.ctx.createBufferSource();
    n.buffer = t.buf;
    n.connect(t.gain);
    n.start(0, Math.min(at, t.buf.duration));
    t.node = n;
  }
  ed.playingFrom = { video: at, ctx: ed.ctx.currentTime };
}
edv.onplay = () => { $("ed-play").textContent = "Pause"; startAudio(); };
edv.onpause = () => { $("ed-play").textContent = "Play"; stopAudio(); };
edv.addEventListener("play", () => $("ed-scrub").classList.remove("on"));
edv.onseeked = () => { if (!edv.paused) startAudio(); renderTimeline(); };
setInterval(() => {   // drift correction
  if (edv.paused || !ed.playingFrom) return;
  const audioPos = ed.playingFrom.video + (ed.ctx.currentTime - ed.playingFrom.ctx);
  if (Math.abs(audioPos - edv.currentTime) > 0.12) startAudio();
}, 500);

function tick() {
  if (ed.info && !$("view-editor").classList.contains("hidden")) {
    if (!edv.paused && edv.currentTime >= ed.end) { edv.pause(); edv.currentTime = ed.start; }
    renderTimeline();
  }
  requestAnimationFrame(tick);
}
requestAnimationFrame(tick);

function renderTimeline() {
  if (!ed.info) return;
  const d = ed.info.duration, pct = (x) => (100 * x / d) + "%";
  $("ed-range").style.left = pct(ed.start);
  $("ed-range").style.width = pct(ed.end - ed.start);
  $("ed-h-start").style.left = pct(ed.start);
  $("ed-h-end").style.left = pct(ed.end);
  const now = ed.shownTime ?? edv.currentTime ?? 0;   // follow the mouse while seeking
  $("ed-playhead").style.left = pct(now);
  $("ed-time").textContent = `${fmtT(now)} / ${fmtT(d)}`;
  $("ed-length").textContent = "Length " + fmtT(ed.end - ed.start);
}

function timeAt(ev) {
  const r = $("ed-timeline").getBoundingClientRect();
  return Math.max(0, Math.min(1, (ev.clientX - r.left) / r.width)) * ed.info.duration;
}
let dragging = null;
$("ed-timeline").onpointerdown = (ev) => {
  if (!ed.info) return;
  dragging = ev.target.id === "ed-h-start" ? "start" : ev.target.id === "ed-h-end" ? "end" : "seek";
  $("ed-timeline").setPointerCapture(ev.pointerId);
  $("ed-timeline").onpointermove(ev);
};
$("ed-timeline").onpointermove = (ev) => {
  if (!dragging) return;
  const t = timeAt(ev);
  if (dragging === "start") { ed.start = Math.min(t, ed.end - 0.5); preview(ed.start); }
  else if (dragging === "end") { ed.end = Math.max(t, ed.start + 0.5); preview(ed.end); }
  else preview(Math.max(ed.start, Math.min(t, ed.end)));
  renderTimeline();
};
$("ed-timeline").onpointerup = () => { if (dragging) endPreview(); dragging = null; };
$("ed-timeline").onpointercancel = $("ed-timeline").onpointerup;

const togglePlay = () => {
  if (!ed.info) return;
  if (edv.paused) {
    if (edv.currentTime < ed.start || edv.currentTime >= ed.end - 0.05) edv.currentTime = ed.start;
    edv.play();
  } else edv.pause();
};
const setStart = () => { ed.start = Math.min(edv.currentTime, ed.end - 0.5); renderTimeline(); };
const setEnd = () => { ed.end = Math.max(edv.currentTime, ed.start + 0.5); renderTimeline(); };
$("ed-play").onclick = togglePlay;
$("ed-set-start").onclick = setStart;
$("ed-set-end").onclick = setEnd;
$("ed-reset").onclick = () => { if (ed.info) { ed.start = 0; ed.end = ed.info.duration; renderTimeline(); } };

window.addEventListener("keydown", (e) => {
  if ($("view-editor").classList.contains("hidden") || listening || e.target.tagName === "INPUT" || !ed.info) return;
  if (e.code === "Space") { e.preventDefault(); togglePlay(); }
  else if (e.code === "KeyI") setStart();
  else if (e.code === "KeyO") setEnd();
  else if (e.code === "ArrowLeft") seekTo(Math.max(0, (ed.shownTime ?? edv.currentTime) - 1));
  else if (e.code === "ArrowRight") seekTo(Math.min(ed.info.duration, (ed.shownTime ?? edv.currentTime) + 1));
});

async function closeEditor() {
  ed.opening++;   // a clip that is still loading stops
  edv.pause(); stopAudio();
  edv.removeAttribute("src"); edv.load();
  ed.info = null; ed.tracks = [];
  invoke("editor_close").catch(() => {});
  showView("clips");
}
$("ed-back").onclick = closeEditor;

$("ed-export").onclick = async () => {
  if (!ed.info || ed.busy) return;
  ed.busy = true;
  $("ed-export").disabled = true;
  edv.pause();
  $("ed-status").textContent = "Saving…";
  try {
    const out = await invoke("editor_export", { request: {
      path: ed.info.path, start: ed.start, end: ed.end,
      volumes: ed.tracks.map((t) => t.gain.gain.value),
    } });
    await closeEditor();
    say("Saved: " + out);
  } catch (err) { $("ed-status").textContent = "Save failed: " + err; }
  finally { ed.busy = false; $("ed-export").disabled = !ed.info; }
};
listen("export-progress", (e) => { if (ed.busy) $("ed-status").textContent = `Saving… ${e.payload}%`; });
listen("tools-progress", (e) => { $("ed-status").textContent = `Downloading ffmpeg (first time only)… ${e.payload}%`; });

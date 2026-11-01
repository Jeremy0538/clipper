const { invoke, convertFileSrc } = window.__TAURI__.core;
const { listen } = window.__TAURI__.event;
const $ = (id) => document.getElementById(id);
let status = { obs: "", buffer: false, game: null };
let settings = null;

function say(t) { $("msg").textContent = t; }
const uiLog = (m) => invoke("ui_log", { msg: String(m) }).catch(() => {});
window.addEventListener("error", (e) => uiLog(`error: ${e.message} @ ${e.filename}:${e.lineno}`));
window.addEventListener("unhandledrejection", (e) => uiLog(`unhandled: ${e.reason}`));

// ---------- tabs ----------
for (const tab of document.querySelectorAll(".tab")) {
  tab.onclick = () => {
    if (ed.info) closeEditor();
    document.querySelectorAll(".tab").forEach((t) => t.classList.toggle("active", t === tab));
    document.querySelectorAll(".view").forEach((v) => v.classList.toggle("hidden", v.id !== "view-" + tab.dataset.view));
    if (tab.dataset.view === "settings") { loadDevices(); loadGames(); loadMonitors(); loadCameras(); }
    meters(tab.dataset.view === "settings");
  };
}

// ---------- status ----------
function render() {
  const ready = status.obs === "ready";
  const err = (status.obs || "").startsWith("error");
  $("obs").textContent = "OBS: " + (err ? "error" : status.obs || "…");
  $("obs").title = "OBS: " + (status.obs || "…");
  $("obs").classList.toggle("on", ready);
  $("obs").classList.toggle("bad", err);
  if (err) say("OBS problem: " + status.obs.replace(/^error:\s*/, ""));
  $("game").title = status.game || "No game";
  $("game").textContent = status.game || "No game";
  $("game").classList.toggle("on", !!status.game);
  $("buffer").textContent = "Clipping: " + (status.buffer ? "on" : "off");
  $("buffer").classList.toggle("on", status.buffer);
  $("toggle").textContent = status.buffer ? "Stop clipping" : "Start clipping";
  $("toggle").disabled = !ready;
  $("record").textContent = status.recording ? "■ Stop recording" : "● Record";
  $("record").classList.toggle("rec", !!status.recording);
  $("record").disabled = !ready;
  $("save").disabled = !status.buffer;
  if (status.screen && !status.game) { $("game").textContent = "Screen"; $("game").classList.add("on"); }
  if (ready && !devicesLoaded) loadDevices();
}

$("toggle").onclick = () => invoke(status.buffer ? "stop_buffer" : "start_buffer").catch(say);
$("record").onclick = () => invoke("toggle_recording").catch(say);
$("save").onclick = () => invoke("save_clip").catch(say);

// ---------- clips ----------
const fmtDur = (s) => `${Math.floor(s / 60)}:${String(Math.floor(s % 60)).padStart(2, "0")}`;

// Thumbnails only load when the card scrolls into view (big clips are heavy).
const lazy = new IntersectionObserver((entries) => {
  for (const e of entries) {
    if (!e.isIntersecting) continue;
    const v = e.target;
    v.src = v.dataset.src + "#t=1";
    lazy.unobserve(v);
  }
}, { rootMargin: "200px" });

function openPlayer(c) {
  $("player-title").textContent = c.name;
  $("player-video").src = convertFileSrc(c.path);
  $("player").classList.remove("hidden");
  $("player-video").play().catch(() => {});
}
function closePlayer() {
  $("player-video").pause();
  $("player-video").removeAttribute("src");
  $("player-video").load();
  $("player").classList.add("hidden");
}
$("player-close").onclick = closePlayer;
$("player").onclick = (e) => { if (e.target.id === "player") closePlayer(); };
window.addEventListener("keydown", (e) => { if (e.key === "Escape" && !listening) closePlayer(); });
$("open-folder").onclick = () => invoke("open_clips_folder").catch(say);

// ---- select clips to combine (order = click order) ----
let selecting = false, selected = [];
function syncSelection() {
  document.body.classList.toggle("selecting", selecting);
  $("sel-mode").textContent = selecting ? "Done" : "Select";
  for (const id of ["sel-info", "sel-clear"]) $(id).classList.toggle("hidden", !selecting);
  $("sel-combine").classList.toggle("hidden", !selecting || selected.length < 2);
  $("sel-info").textContent = selected.length ? `${selected.length} selected` : "Click clips in the order you want";
  for (const el of document.querySelectorAll(".clip")) {
    const i = selected.indexOf(el.dataset.path);
    el.classList.toggle("selected", i >= 0);
    el.querySelector(".order")?.remove();
    if (i >= 0) {
      const b = document.createElement("span"); b.className = "order"; b.textContent = i + 1;
      el.querySelector(".thumb").append(b);
    }
  }
}
$("sel-mode").onclick = () => { selecting = !selecting; if (!selecting) selected = []; syncSelection(); };
$("sel-clear").onclick = () => { selected = []; syncSelection(); };
$("sel-combine").onclick = async () => {
  $("sel-combine").disabled = true;
  say("Combining… (first time: downloading ffmpeg)");
  try {
    await invoke("combine_clips", { paths: selected });
    selected = []; selecting = false; syncSelection();
    say("Combined into a new clip.");
    loadClips();
  } catch (err) { say("Combine failed: " + err); }
  finally { $("sel-combine").disabled = false; }
};
listen("combine-progress", (e) => say(`Combining… ${e.payload}%`));

function clipCard(c) {
  const el = document.createElement("div");
  el.className = "clip";
  const isImg = c.kind === "image";
  el.innerHTML = `<div class="thumb">${isImg ? "<img alt=''>" : "<video muted preload='metadata'></video><span class='dur'></span>"}</div>
    <div class="meta"><div><div class="name" title="Double-click to rename"></div><div class="sub"></div></div>
      <button class="star" title="Star: never deleted by the storage limit">★</button></div>
    <div class="actions"><button data-a="copy" title="Copy the file, then paste it in Discord">Copy</button>
      ${isImg ? "" : "<button data-a='edit'>Edit</button>"}<button data-a="reveal">Show in folder</button><span class="spacer"></span>
      <button data-a="delete" class="danger">Delete</button></div>`;
  if (isImg) {
    el.querySelector("img").src = convertFileSrc(c.path);
  } else {
    const v = el.querySelector("video");
    v.dataset.src = convertFileSrc(c.path);
    v.onloadedmetadata = () => { el.querySelector(".dur").textContent = fmtDur(v.duration); };
    lazy.observe(v);
  }
  const nameEl = el.querySelector(".name");
  nameEl.textContent = c.name;
  nameEl.ondblclick = () => {
    const inp = document.createElement("input");
    inp.className = "rename"; inp.value = c.name;
    nameEl.replaceWith(inp); inp.focus(); inp.select();
    let done = false;
    const finish = async (save) => {
      if (done) return; done = true;
      if (save && inp.value.trim() && inp.value.trim() !== c.name) {
        try { await invoke("rename_clip", { path: c.path, name: inp.value.trim() }); say("Renamed."); }
        catch (err) { say(err); }
      }
      loadClips();
    };
    inp.onkeydown = (e) => { if (e.key === "Enter") finish(true); if (e.key === "Escape") finish(false); e.stopPropagation(); };
    inp.onblur = () => finish(true);
  };
  const star = el.querySelector(".star");
  star.classList.toggle("on", !!c.favorite);
  star.onclick = () => {
    c.favorite = !c.favorite;
    star.classList.toggle("on", c.favorite);
    invoke("set_favorite", { path: c.path, on: c.favorite }).catch(say);
  };
  el.querySelector(".sub").textContent =
    `${new Date(c.modified * 1000).toLocaleString()} · ${(c.size / 1e6).toFixed(c.size < 1e7 ? 1 : 0)} MB`;
  el.dataset.path = c.path;
  el.querySelector(".thumb").onclick = () => {
    if (selecting) {
      if (isImg) return say("Screenshots can't be combined.");
      const i = selected.indexOf(c.path);
      if (i >= 0) selected.splice(i, 1); else selected.push(c.path);
      return syncSelection();
    }
    isImg ? invoke("reveal_clip", { path: c.path }).catch(say) : openPlayer(c);
  };
  el.querySelector('[data-a="copy"]').onclick = () =>
    invoke("copy_clip", { path: c.path }).then(() => say("Copied: paste it in Discord with Ctrl+V."), say);
  el.querySelector('[data-a="edit"]')?.addEventListener("click", () => openEditor(c));
  el.querySelector('[data-a="reveal"]').onclick = () => invoke("reveal_clip", { path: c.path }).catch(say);
  const del = el.querySelector('[data-a="delete"]');
  del.onclick = async () => {
    if (!del.classList.contains("armed")) {   // first click arms, second deletes
      del.classList.add("armed");
      del.textContent = "Sure?";
      setTimeout(() => { del.classList.remove("armed"); del.textContent = "Delete"; }, 3000);
      return;
    }
    try {
      await invoke("delete_clip", { path: c.path });
      say("Moved to the recycle bin.");
      loadClips();
    } catch (err) { say(err); }
  };
  return el;
}

let folderFilter = "";
async function loadClips() {
  const all = await invoke("list_clips");
  const folders = [...new Set(all.map((c) => c.folder).filter(Boolean))].sort((a, b) => a.localeCompare(b));
  if (folderFilter && !folders.includes(folderFilter)) folderFilter = "";
  $("folders").replaceChildren(...["", ...folders].map((f) => {
    const b = document.createElement("button");
    b.textContent = f || "All";
    b.classList.toggle("active", f === folderFilter);
    b.onclick = () => { folderFilter = f; loadClips(); };
    return b;
  }));
  $("folders").classList.toggle("hidden", folders.length < 2);
  const clips = folderFilter ? all.filter((c) => c.folder === folderFilter) : all;
  $("clips").replaceChildren(...clips.map(clipCard));
  selected = selected.filter((p) => all.some((c) => c.path === p));
  syncSelection();
  const gb = clips.reduce((n, c) => n + c.size, 0) / 1e9;
  $("clip-count").textContent = clips.length ? `${clips.length} clips · ${gb.toFixed(1)} GB`
    : "No clips yet: start a game and press " + (settings?.hotkey_label || "the clip key");
}

// ---------- settings: recording ----------
const MODIFIERS = ["ShiftLeft", "ShiftRight", "ControlLeft", "ControlRight", "AltLeft", "AltRight", "MetaLeft", "MetaRight"];
let listening = false;

async function loadSettings() {
  settings = await invoke("get_settings");
  $("hotkey").textContent = settings.hotkey_label;
  $("record-hotkey").textContent = settings.record_hotkey_label;
  $("screenshot-hotkey").textContent = settings.screenshot_hotkey_label;
  const sl = $("storage-limit");
  if (![...sl.options].some((o) => +o.value === settings.storage_limit_gb)) sl.add(new Option(settings.storage_limit_gb + " GB", settings.storage_limit_gb));
  sl.value = String(settings.storage_limit_gb);
  $("quality").value = settings.quality;
  loadMonitors();
  const sel = $("buffer-len");
  if (![...sel.options].some((o) => +o.value === settings.buffer_secs)) {
    sel.add(new Option(`${settings.buffer_secs} seconds`, settings.buffer_secs));
  }
  sel.value = String(settings.buffer_secs);
  $("opt-sound").checked = settings.sound;
  $("opt-toast").checked = settings.toast;
  $("opt-autostart").checked = settings.autostart;
  $("opt-test_builds").checked = settings.test_builds;
  $("version").textContent = settings.version;
  applyTheme(settings.theme);
  showAudio(settings.audio);
}

for (const b of document.querySelectorAll(".keybtn")) {
  b.onclick = () => {
    listening = b;
    b.classList.add("listening");
    b.textContent = "Press keys… (Esc cancels)";
  };
}
window.addEventListener("keydown", async (e) => {
  if (!listening) return;
  e.preventDefault();
  if (MODIFIERS.includes(e.code)) return; // wait for the real key
  const btn = listening;
  listening = false;
  btn.classList.remove("listening");
  if (e.code === "Escape") return loadSettings();
  try {
    btn.textContent = await invoke("set_hotkey", {
      hotkey: { code: e.code, shift: e.shiftKey, ctrl: e.ctrlKey, alt: e.altKey }, which: btn.dataset.which,
    });
    say("Key saved.");
  } catch (err) { say(err); loadSettings(); }
});

$("buffer-len").onchange = async (e) => {
  try {
    await invoke("set_buffer", { secs: +e.target.value });
    say(status.buffer ? "Clip length changed (clipping restarted)." : "Clip length saved.");
  } catch (err) { say(err); }
};
for (const name of ["sound", "toast", "autostart", "test_builds"]) {
  $("opt-" + name).onchange = (e) => invoke("set_option", { name, value: e.target.checked }).catch(say);
}
$("logs").onclick = () => invoke("open_log_folder").catch(say);

// ---------- window frame ----------
const win = window.__TAURI__.window.getCurrentWindow();
$("win-min").onclick = () => win.minimize();
$("win-max").onclick = () => win.toggleMaximize();
$("win-close").onclick = () => win.close();   // the app hides to the tray

// ---------- appearance ----------
function applyTheme(t) {
  document.documentElement.dataset.theme = t;
  $("theme").value = t;
  try { localStorage.setItem("theme", t); } catch {}
  syncFrame();
}
// The window border: drawn by Windows 11 (rounded) when possible, else by the page.
async function syncFrame() {
  const accent = getComputedStyle(document.documentElement).getPropertyValue("--accent").trim();
  const native = await invoke("set_frame_color", { color: accent }).catch(() => false);
  document.documentElement.classList.toggle("native-frame", !!native);
}
matchMedia("(prefers-color-scheme: dark)").addEventListener("change", syncFrame);
try { const t = localStorage.getItem("theme"); if (t) document.documentElement.dataset.theme = t; } catch {}
$("theme").onchange = (e) => { applyTheme(e.target.value); invoke("set_theme", { theme: e.target.value }).catch(say); };

// ---------- updates ----------
let pendingUpdate = null;
function showUpdate(u) {
  pendingUpdate = u;
  $("update-text").textContent = u ? `Version ${u.version} is available` : "You have the latest version";
  $("install-update").classList.toggle("hidden", !u);
  document.querySelector('[data-view="settings"]').textContent = u ? "Settings •" : "Settings";
}
$("check-update").onclick = async () => {
  $("update-text").textContent = "Checking…";
  try { showUpdate(await invoke("check_update")); } catch (err) { $("update-text").textContent = String(err); }
};
$("install-update").onclick = async () => {
  if (!pendingUpdate) return;
  $("install-update").disabled = $("check-update").disabled = true;
  $("update-text").textContent = "Downloading…";
  try { await invoke("install_update", { update: pendingUpdate }); }
  catch (err) {
    $("update-text").textContent = "Update failed: " + err;
    $("install-update").disabled = $("check-update").disabled = false;
  }
};
listen("update-available", (e) => showUpdate(e.payload));
listen("update-progress", (e) => { $("update-text").textContent = `Downloading… ${e.payload}%`; });

// ---------- settings: games ----------
let games = [];
async function loadGames() {
  try { games = await invoke("list_games"); renderGames(); } catch (err) { say(err); }
}
function renderGames() {
  const f = $("game-filter").value.trim().toLowerCase();
  $("games").replaceChildren(...games.filter((g) => g.name.toLowerCase().includes(f)).map((g) => {
    const l = document.createElement("label");
    const cb = document.createElement("input");
    cb.type = "checkbox";
    cb.checked = g.enabled;
    cb.onchange = () => {
      g.enabled = cb.checked;
      invoke("set_game_enabled", { name: g.name, enabled: cb.checked }).catch(say);
    };
    l.append(cb, document.createTextNode(g.name));
    if (g.custom) {
      const tag = document.createElement("span"); tag.className = "custom-tag"; tag.textContent = "added by you";
      const rm = document.createElement("button"); rm.className = "remove"; rm.textContent = "Remove";
      rm.onclick = (ev) => { ev.preventDefault(); invoke("remove_custom_game", { name: g.name }).then(loadGames, say); };
      l.append(tag, rm);
    }
    return l;
  }));
}
$("game-filter").oninput = renderGames;

// ---------- settings: audio ----------
const VOLUMES = { "a-game-volume": "game_volume", "a-discord-volume": "discord_volume", "a-mic-volume": "mic_volume" };
let devicesLoaded = false;

// ---------- quality / screen ----------
$("quality").onchange = async (e) => {
  try {
    const restarted = await invoke("set_quality", { quality: e.target.value });
    say(restarted ? "Quality saved. OBS restarted to use it." : "Quality saved. It's used from the next clipping session.");
  } catch (err) { say(err); }
};
async function loadMonitors() {
  const sel = $("screen-monitor");
  const want = settings?.screen_monitor || "primary";
  try {
    const mons = status.obs === "ready" ? await invoke("list_monitors") : [];
    sel.replaceChildren(new Option("Main screen", "primary"), new Option("All screens", "all"),
      ...mons.map((m) => new Option(`${m.name.split(":")[0]} (${m.w}×${m.h})`, m.id)));
  } catch (err) { say(err); }
  if (![...sel.options].some((o) => o.value === want)) sel.add(new Option(want, want));
  sel.value = want;
}
$("storage-limit").onchange = (e) => invoke("set_storage_limit", { gb: +e.target.value })
  .then(() => say(+e.target.value ? "Storage limit saved." : "No storage limit."), say);
$("screen-monitor").onchange = (e) => invoke("set_screen_monitor", { monitor: e.target.value }).then(() => say("Screen saved."), say);

// ---------- app picker ----------
function pickApp(title) {
  return new Promise(async (resolve) => {
    $("picker-title").textContent = title;
    $("picker-list").textContent = "Loading…";
    $("picker").classList.remove("hidden");
    const done = (v) => { $("picker").classList.add("hidden"); resolve(v); };
    $("picker-close").onclick = () => done(null);
    try {
      const apps = await invoke("list_running_apps");
      $("picker-list").replaceChildren(...apps.map((a) => {
        const b = document.createElement("button");
        b.textContent = a.name;
        const s = document.createElement("small"); s.textContent = a.exe; b.append(s);
        b.onclick = () => done(a);
        return b;
      }));
      if (!apps.length) $("picker-list").textContent = "No apps with a window found.";
    } catch (err) { $("picker-list").textContent = String(err); }
  });
}
$("add-game").onclick = async () => {
  const a = await pickApp("Add a clip application");
  if (!a) return;
  try { await invoke("add_custom_game", { app: a }); say(`${a.name} will now start clipping like a game.`); loadGames(); }
  catch (err) { say(err); }
};

// ---------- webcam ----------
async function loadCameras() {
  if (!settings) return;
  const w = settings.webcam, sel = $("cam-device");
  $("cam-enabled").checked = w.enabled;
  $("cam-position").value = w.position;
  $("cam-size").value = w.size;
  $("cam-size").nextElementSibling.textContent = w.size + "%";
  try {
    const cams = status.obs === "ready" ? await invoke("list_cameras") : [];
    sel.replaceChildren(...cams.map((c) => new Option(c.name, c.id)));
    if (!cams.length) sel.replaceChildren(new Option("No camera found", ""));
  } catch (err) { say(err); }
  if (w.device && ![...sel.options].some((o) => o.value === w.device)) sel.add(new Option(w.device, w.device));
  if (w.device) sel.value = w.device;
  syncCam();
}
function syncCam() {
  for (const id of ["cam-device", "cam-position", "cam-size"]) $(id).disabled = !$("cam-enabled").checked;
}
let camTimer;
function saveCam() {
  settings.webcam = {
    enabled: $("cam-enabled").checked, device: $("cam-device").value,
    position: $("cam-position").value, size: +$("cam-size").value,
  };
  syncCam();
  clearTimeout(camTimer);
  camTimer = setTimeout(() => invoke("set_webcam", { webcam: settings.webcam }).then(() => say("Webcam saved."), say), 250);
}
for (const id of ["cam-enabled", "cam-device", "cam-position"]) $(id).onchange = saveCam;
$("cam-size").oninput = (e) => { e.target.nextElementSibling.textContent = e.target.value + "%"; saveCam(); };

// ---------- level meters ----------
listen("meters", (e) => {
  for (const m of document.querySelectorAll(".meter")) {
    const names = m.dataset.inputs.split(",");
    const db = Math.max(-100, ...names.map((n) => e.payload[n] ?? -100));
    const pct = Math.max(0, Math.min(100, (db + 60) / 60 * 100));   // -60 dB .. 0 dB
    const bar = m.querySelector("i");
    bar.style.width = pct + "%";
    bar.style.setProperty("--w", pct > 0 ? (100 / pct * 100) + "%" : "100%");
  }
});
const meters = (on) => invoke("set_meters", { on }).catch(() => {});

function renderAudioApps(a) {
  $("audio-apps").replaceChildren(...a.apps.map((app, i) => {
    const row = document.createElement("div");
    row.className = "row";
    const name = document.createElement("span"); name.textContent = app.name;
    const r = document.createElement("input");
    r.type = "range"; r.min = 0; r.max = 300; r.step = 5; r.value = Math.round((a.app_volumes[i] ?? 1) * 100);
    const out = document.createElement("output"); out.textContent = r.value + "%";
    r.oninput = () => { out.textContent = r.value + "%"; settings.audio.app_volumes[i] = r.value / 100; saveAudio(); };
    const meter = document.createElement("div"); meter.className = "meter"; meter.dataset.inputs = `App ${i + 1}`;
    meter.append(document.createElement("i"));
    meter.style.maxWidth = "120px";
    const rm = document.createElement("button"); rm.textContent = "Remove"; rm.className = "small";
    rm.onclick = () => { settings.audio.apps.splice(i, 1); settings.audio.app_volumes.splice(i, 1); saveAudio(); renderAudioApps(settings.audio); };
    row.append(name, r, out, meter, rm);
    return row;
  }));
  $("add-audio-app").disabled = a.apps.length >= 2;
}
$("add-audio-app").onclick = async () => {
  const app = await pickApp("Record the sound of…");
  if (!app) return;
  settings.audio.apps.push(app);
  settings.audio.app_volumes.push(1);
  renderAudioApps(settings.audio);
  saveAudio();
};

function showAudio(a) {
  $("a-game-mode").value = a.game_mode;
  $("a-discord-enabled").checked = a.discord_enabled;
  $("a-mic-enabled").checked = a.mic_enabled;
  $("a-mic-noise").checked = a.mic_noise_suppression;
  $("a-mic-boost").value = a.mic_boost_db;
  $("a-mic-boost").nextElementSibling.textContent = "+" + a.mic_boost_db + " dB";
  renderAudioApps(a);
  for (const [id, key] of Object.entries(VOLUMES)) {
    $(id).value = Math.round(a[key] * 100);
    $(id).nextElementSibling.textContent = Math.round(a[key] * 100) + "%";
  }
  selectDevice("a-mic-device", a.mic_device);
  selectDevice("a-desktop-device", a.desktop_device);
  syncAudioRows();
}

function syncAudioRows() {
  $("row-desktop").classList.toggle("hidden", $("a-game-mode").value !== "desktop");
  $("a-game-volume").disabled = $("a-game-mode").value === "off";
  $("a-discord-volume").disabled = !$("a-discord-enabled").checked;
  for (const id of ["a-mic-device", "a-mic-volume", "a-mic-noise", "a-mic-boost"]) $(id).disabled = !$("a-mic-enabled").checked;
}

function selectDevice(id, value) {
  const sel = $(id);
  if (![...sel.options].some((o) => o.value === value)) {
    sel.add(new Option(value === "default" ? "Windows default" : value, value));
  }
  sel.value = value;
}

function fillDevices(id, devices, current) {
  const sel = $(id);
  sel.replaceChildren(...devices.map((d) => new Option(d.name, d.id)));
  selectDevice(id, current);
}

async function loadDevices() {
  if (status.obs !== "ready" || !settings) return;
  try {
    const [inputs, outputs] = [await invoke("list_audio_devices", { output: false }),
                               await invoke("list_audio_devices", { output: true })];
    fillDevices("a-mic-device", inputs, settings.audio.mic_device);
    fillDevices("a-desktop-device", outputs, settings.audio.desktop_device);
    devicesLoaded = true;
  } catch (err) { say("Could not list audio devices: " + err); }
}
$("refresh-devices").onclick = loadDevices;

let audioTimer;
function saveAudio() {
  const a = {
    game_mode: $("a-game-mode").value,
    desktop_device: $("a-desktop-device").value || "default",
    game_volume: $("a-game-volume").value / 100,
    discord_enabled: $("a-discord-enabled").checked,
    discord_volume: $("a-discord-volume").value / 100,
    mic_enabled: $("a-mic-enabled").checked,
    mic_device: $("a-mic-device").value || "default",
    mic_volume: $("a-mic-volume").value / 100,
    mic_noise_suppression: $("a-mic-noise").checked,
    mic_boost_db: +$("a-mic-boost").value,
    apps: settings.audio.apps || [],
    app_volumes: settings.audio.app_volumes || [],
  };
  settings.audio = a;
  syncAudioRows();
  clearTimeout(audioTimer);
  // Sliders fire continuously: apply once they settle.
  audioTimer = setTimeout(() => invoke("set_audio", { audio: a }).then(() => say("Audio settings saved."), say), 250);
}
for (const id of ["a-game-mode", "a-desktop-device", "a-discord-enabled", "a-mic-enabled", "a-mic-device", "a-mic-noise"]) {
  $(id).onchange = saveAudio;
}
$("a-mic-boost").oninput = (e) => { e.target.nextElementSibling.textContent = "+" + e.target.value + " dB"; saveAudio(); };
for (const id of Object.keys(VOLUMES)) {
  $(id).oninput = (e) => { e.target.nextElementSibling.textContent = e.target.value + "%"; saveAudio(); };
}

// ---------- events ----------
listen("status", (e) => { status = e.payload; render(); });
listen("clip-saved", (e) => { say("Saved: " + e.payload); loadClips(); });
listen("error", (e) => say(e.payload));

loadSettings().then(() => invoke("get_status")).then((s) => { status = s; render(); });
loadClips();

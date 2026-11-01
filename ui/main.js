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
    if (tab.dataset.view === "settings") { loadDevices(); loadGames(); }
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
  $("buffer").textContent = "Buffer: " + (status.buffer ? "recording" : "off");
  $("buffer").classList.toggle("on", status.buffer);
  $("toggle").textContent = status.buffer ? "Stop buffer" : "Start buffer";
  $("toggle").disabled = !ready;
  $("save").disabled = !status.buffer;
  if (ready && !devicesLoaded) loadDevices();
}

$("toggle").onclick = () => invoke(status.buffer ? "stop_buffer" : "start_buffer").catch(say);
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

function clipCard(c) {
  const el = document.createElement("div");
  el.className = "clip";
  el.innerHTML = `<div class="thumb"><video muted preload="metadata"></video><span class="dur"></span></div>
    <div class="meta"><div class="name"></div><div class="sub"></div></div>
    <div class="actions"><button data-a="copy" title="Copy the file, then paste it in Discord">Copy</button>
      <button data-a="edit">Edit</button><button data-a="reveal">Show in folder</button><span class="spacer"></span>
      <button data-a="delete" class="danger">Delete</button></div>`;
  const v = el.querySelector("video");
  v.dataset.src = convertFileSrc(c.path);
  v.onloadedmetadata = () => { el.querySelector(".dur").textContent = fmtDur(v.duration); };
  lazy.observe(v);
  el.querySelector(".name").textContent = c.name;
  el.querySelector(".sub").textContent =
    `${new Date(c.modified * 1000).toLocaleString()} · ${(c.size / 1e6).toFixed(0)} MB`;
  el.querySelector(".thumb").onclick = () => openPlayer(c);
  el.querySelector('[data-a="copy"]').onclick = () =>
    invoke("copy_clip", { path: c.path }).then(() => say("Copied: paste it in Discord with Ctrl+V."), say);
  el.querySelector('[data-a="edit"]').onclick = () => openEditor(c);
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

async function loadClips() {
  const clips = await invoke("list_clips");
  $("clips").replaceChildren(...clips.map(clipCard));
  const gb = clips.reduce((n, c) => n + c.size, 0) / 1e9;
  $("clip-count").textContent = clips.length ? `${clips.length} clips · ${gb.toFixed(1)} GB` : "No clips yet";
}

// ---------- settings: recording ----------
const MODIFIERS = ["ShiftLeft", "ShiftRight", "ControlLeft", "ControlRight", "AltLeft", "AltRight", "MetaLeft", "MetaRight"];
let listening = false;

async function loadSettings() {
  settings = await invoke("get_settings");
  $("hotkey").textContent = settings.hotkey_label;
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

$("hotkey").onclick = () => {
  listening = true;
  $("hotkey").classList.add("listening");
  $("hotkey").textContent = "Press keys… (Esc cancels)";
};
window.addEventListener("keydown", async (e) => {
  if (!listening) return;
  e.preventDefault();
  if (MODIFIERS.includes(e.code)) return; // wait for the real key
  listening = false;
  $("hotkey").classList.remove("listening");
  if (e.code === "Escape") return loadSettings();
  try {
    $("hotkey").textContent = await invoke("set_hotkey", {
      hotkey: { code: e.code, shift: e.shiftKey, ctrl: e.ctrlKey, alt: e.altKey },
    });
    say("Clip key saved.");
  } catch (err) { say(err); loadSettings(); }
});

$("buffer-len").onchange = async (e) => {
  try {
    await invoke("set_buffer", { secs: +e.target.value });
    say(status.buffer ? "Buffer length changed (the buffer restarted)." : "Buffer length saved.");
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
}
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
    return l;
  }));
}
$("game-filter").oninput = renderGames;

// ---------- settings: audio ----------
const VOLUMES = { "a-game-volume": "game_volume", "a-discord-volume": "discord_volume", "a-mic-volume": "mic_volume" };
let devicesLoaded = false;

function showAudio(a) {
  $("a-game-mode").value = a.game_mode;
  $("a-discord-enabled").checked = a.discord_enabled;
  $("a-mic-enabled").checked = a.mic_enabled;
  $("a-mic-noise").checked = a.mic_noise_suppression;
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
  for (const id of ["a-mic-device", "a-mic-volume", "a-mic-noise"]) $(id).disabled = !$("a-mic-enabled").checked;
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
for (const id of Object.keys(VOLUMES)) {
  $(id).oninput = (e) => { e.target.nextElementSibling.textContent = e.target.value + "%"; saveAudio(); };
}

// ---------- events ----------
listen("status", (e) => { status = e.payload; render(); });
listen("clip-saved", (e) => { say("Saved: " + e.payload); loadClips(); });
listen("error", (e) => say(e.payload));

loadSettings().then(() => invoke("get_status")).then((s) => { status = s; render(); });
loadClips();

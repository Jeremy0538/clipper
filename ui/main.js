const { invoke, convertFileSrc } = window.__TAURI__.core;
const { listen } = window.__TAURI__.event;
const $ = (id) => document.getElementById(id);
let status = { obs: "", buffer: false, game: null };
let settings = null;

function say(t) { $("msg").textContent = t; }

// ---------- tabs ----------
for (const tab of document.querySelectorAll(".tab")) {
  tab.onclick = () => {
    document.querySelectorAll(".tab").forEach((t) => t.classList.toggle("active", t === tab));
    document.querySelectorAll(".view").forEach((v) => v.classList.toggle("hidden", v.id !== "view-" + tab.dataset.view));
    if (tab.dataset.view === "settings") loadDevices();
  };
}

// ---------- status ----------
function render() {
  const ready = status.obs === "ready";
  $("obs").textContent = "OBS: " + (status.obs || "…");
  $("obs").classList.toggle("on", ready);
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
async function loadClips() {
  const clips = await invoke("list_clips");
  $("clips").replaceChildren(...clips.map((c) => {
    const el = document.createElement("div");
    el.className = "clip";
    const v = document.createElement("video");
    v.src = convertFileSrc(c.path);
    v.preload = "metadata";
    v.controls = true;
    const meta = document.createElement("div");
    meta.className = "meta";
    meta.innerHTML = `<div class="name"></div><div class="sub"></div>`;
    meta.querySelector(".name").textContent = c.name;
    meta.querySelector(".sub").textContent =
      `${new Date(c.modified * 1000).toLocaleString()} · ${(c.size / 1e6).toFixed(1)} MB`;
    el.append(v, meta);
    return el;
  }));
  if (!clips.length) say("No clips yet.");
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
for (const name of ["sound", "toast"]) {
  $("opt-" + name).onchange = (e) => invoke("set_option", { name, value: e.target.checked }).catch(say);
}
$("logs").onclick = () => invoke("open_log_folder").catch(say);

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

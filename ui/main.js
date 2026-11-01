const { invoke, convertFileSrc } = window.__TAURI__.core;
const { listen } = window.__TAURI__.event;
const $ = (id) => document.getElementById(id);
let status = { obs: "", buffer: false };

function say(t) { $("msg").textContent = t; }

function render() {
  const ready = status.obs === "ready";
  $("obs").textContent = "OBS: " + (status.obs || "…");
  $("obs").classList.toggle("on", ready);
  $("buffer").textContent = "Buffer: " + (status.buffer ? "recording" : "off");
  $("buffer").classList.toggle("on", status.buffer);
  $("toggle").textContent = status.buffer ? "Stop buffer" : "Start buffer";
  $("toggle").disabled = !ready;
  $("save").disabled = !status.buffer;
}

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

$("toggle").onclick = () =>
  invoke(status.buffer ? "stop_buffer" : "start_buffer").catch((e) => say(e));
$("save").onclick = () => invoke("save_clip").catch((e) => say(e));

listen("status", (e) => { status = e.payload; render(); });
listen("clip-saved", (e) => { say("Saved: " + e.payload); loadClips(); });
listen("error", (e) => say(e.payload));

// ---- settings ----
const MODIFIERS = ["ShiftLeft", "ShiftRight", "ControlLeft", "ControlRight", "AltLeft", "AltRight", "MetaLeft", "MetaRight"];
let listening = false;

async function loadSettings() {
  const s = await invoke("get_settings");
  $("hotkey").textContent = s.hotkey_label;
  const sel = $("buffer-len");
  if (![...sel.options].some((o) => +o.value === s.buffer_secs)) {
    sel.add(new Option(`${s.buffer_secs} s`, s.buffer_secs));
  }
  sel.value = String(s.buffer_secs);
  $("opt-sound").checked = s.sound;
  $("opt-toast").checked = s.toast;
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
loadSettings();

invoke("get_status").then((s) => { status = s; render(); });
loadClips();

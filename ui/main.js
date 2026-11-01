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

invoke("get_status").then((s) => { status = s; render(); });
loadClips();

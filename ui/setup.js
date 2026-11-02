// First-start setup guide: keys, game sound, microphone, quality, extras.
// Every choice is saved right away through the same commands Settings uses,
// so skipping halfway keeps what was already chosen.
const su = { step: 0, steps: [...document.querySelectorAll("#view-setup .step")], busy: false };

function setupOpen() {
  su.step = 0;
  document.body.classList.add("in-setup");
  document.querySelectorAll(".view").forEach((v) => v.classList.toggle("hidden", v.id !== "view-setup"));
  $("setup-steps").replaceChildren(...su.steps.map((s) => {
    const li = document.createElement("li"); li.textContent = s.dataset.title; return li;
  }));
  meters(true);
  suFill();
  suShow();
}

async function setupClose() {
  meters(false);
  document.body.classList.remove("in-setup");
  await invoke("finish_setup").catch(say);
  await loadSettings();
  document.querySelector('.tab[data-view="clips"]').click();
}

function suShow() {
  $("su-test-video").pause();
  su.steps.forEach((s, i) => s.classList.toggle("hidden", i !== su.step));
  [...$("setup-steps").children].forEach((li, i) => {
    li.classList.toggle("done", i < su.step);
    li.classList.toggle("now", i === su.step);
  });
  const last = su.step === su.steps.length - 1;
  $("su-back").classList.toggle("hidden", su.step === 0);
  $("su-next").textContent = last ? "Start using Clipper" : "Next";
  $("su-skip").classList.toggle("hidden", last);
  suReady();
  $("view-setup").scrollTop = 0;
}

$("su-next").onclick = () => (su.step === su.steps.length - 1 ? setupClose() : (su.step++, suShow()));
$("su-back").onclick = () => { su.step = Math.max(0, su.step - 1); suShow(); };
$("su-skip").onclick = setupClose;
$("run-setup").onclick = setupOpen;

// ---------- fill from the current settings ----------
function radio(name, value) {
  for (const r of document.querySelectorAll(`input[name="${name}"]`)) r.checked = r.value === value;
}
function suFill() {
  const a = settings.audio;
  syncKeyLabels();
  suKeys();
  const b = $("su-buffer");
  if (![...b.options].some((o) => +o.value === settings.buffer_secs)) b.add(new Option(`${settings.buffer_secs} seconds`, settings.buffer_secs));
  b.value = String(settings.buffer_secs);
  radio("su-gm", a.game_mode);
  $("su-discord").checked = a.discord_enabled;
  $("su-mic").checked = a.mic_enabled;
  $("su-noise").checked = a.mic_noise_suppression;
  radio("su-q", settings.quality);
  $("su-storage").value = String(settings.storage_limit_gb);
  $("su-autostart").checked = settings.autostart;
  $("su-sound").checked = settings.sound;
  $("su-toast").checked = settings.toast;
  const enc = $("su-encoder");
  enc.classList.toggle("warn", !settings.gpu_encoding);
  enc.textContent = settings.gpu_encoding
    ? `Clips are encoded by your graphics card: ${settings.encoder}. Your games keep their FPS.`
    : "No supported graphics card found: clips are encoded by the processor. Pick Balanced or Small to keep games smooth.";
  suSync();
}
function suKeys() {
  $("su-k-clip").textContent = settings.hotkey_label;
  $("su-k-rec").textContent = settings.record_hotkey_label;
  $("su-k-shot").textContent = settings.screenshot_hotkey_label;
}
document.addEventListener("keys-changed", suKeys);

function suSync() {
  const gm = document.querySelector('input[name="su-gm"]:checked')?.value;
  $("su-row-desktop").classList.toggle("hidden", gm !== "desktop");
  document.querySelector(".su-mic-on").classList.toggle("hidden", !$("su-mic").checked);
}

// Device lists need OBS; until it's ready, say so instead of showing empty lists.
let suLoaded = false;
async function suReady() {
  const ready = status.obs === "ready";
  const o = status.obs || "";
  $("su-obs").textContent = ready ? "✓ The recorder is ready."
    : o.startsWith("downloading") ? `Downloading the recorder… ${o.replace(/\D+/g, "")}%`
    : o.startsWith("error") ? "The recorder had a problem; Clipper keeps retrying. " + o.replace(/^error:\s*/, "")
    : "Getting the recorder ready…";
  $("su-find-mic").disabled = $("su-level-mic").disabled = !ready || su.busy;
  if (!ready) {
    for (const id of ["su-mic-device", "su-desktop-device", "su-screen"]) {
      const sel = $(id);
      if (!sel.options.length) sel.replaceChildren(new Option("Waiting for the recorder…", ""));
    }
    return;
  }
  if (suLoaded) return;
  suLoaded = true;
  try {
    const [ins, outs] = [await invoke("list_audio_devices", { output: false }), await invoke("list_audio_devices", { output: true })];
    suDevices("su-mic-device", ins, settings.audio.mic_device);
    suDevices("su-desktop-device", outs, settings.audio.desktop_device);
    const mons = await invoke("list_monitors");
    $("su-screen").replaceChildren(new Option("Main screen", "primary"), new Option("All screens together", "all"),
      ...mons.map((m) => new Option(`${m.name.split(":")[0]} (${m.w}×${m.h})`, m.id)));
    $("su-screen").value = settings.screen_monitor;
    if (!$("su-screen").value) $("su-screen").value = "primary";
  } catch (err) { suLoaded = false; say("Could not list devices: " + err); }
}
function suDevices(id, list, current) {
  const sel = $(id);
  sel.replaceChildren(...list.map((d) => new Option(d.id === "default" ? "Windows default" : d.name, d.id)));
  if (![...sel.options].some((o) => o.value === current)) sel.add(new Option(current, current));
  sel.value = current;
}
listen("status", () => { if (!$("view-setup").classList.contains("hidden")) suReady(); });

// ---------- saving ----------
async function suAudio(changes) {
  Object.assign(settings.audio, changes);
  showAudio(settings.audio);   // keep the Settings page in step
  await invoke("set_audio", { audio: settings.audio });
}
const suSave = (p) => p.then(() => say("Saved."), say);

$("su-buffer").onchange = (e) => suSave(invoke("set_buffer", { secs: +e.target.value }).then((s) => { settings.buffer_secs = s; }));
for (const r of document.querySelectorAll('input[name="su-gm"]')) {
  r.onchange = () => { suSync(); suSave(suAudio({ game_mode: r.value })); };
}
$("su-desktop-device").onchange = (e) => suSave(suAudio({ desktop_device: e.target.value || "default" }));
$("su-discord").onchange = (e) => suSave(suAudio({ discord_enabled: e.target.checked }));
$("su-mic").onchange = (e) => { suSync(); suSave(suAudio({ mic_enabled: e.target.checked })); };
$("su-mic-device").onchange = (e) => suSave(suAudio({ mic_device: e.target.value || "default" }));
$("su-noise").onchange = (e) => suSave(suAudio({ mic_noise_suppression: e.target.checked }));
for (const r of document.querySelectorAll('input[name="su-q"]')) {
  r.onchange = () => suSave(invoke("set_quality", { quality: r.value }).then(() => { settings.quality = r.value; $("quality").value = r.value; }));
}
$("su-storage").onchange = (e) => suSave(invoke("set_storage_limit", { gb: +e.target.value }).then(() => { settings.storage_limit_gb = +e.target.value; }));
$("su-screen").onchange = (e) => suSave(invoke("set_screen_monitor", { monitor: e.target.value }).then(() => { settings.screen_monitor = e.target.value; }));
for (const name of ["autostart", "sound", "toast"]) {
  $("su-" + name).onchange = (e) => suSave(invoke("set_option", { name, value: e.target.checked }).then(() => { settings[name] = e.target.checked; }));
}

// ---------- microphone helpers ----------
// Loudest Mic level (dB) seen during `ms`, from the 10/s meter events.
function listenMic(ms) {
  return new Promise(async (resolve) => {
    let peak = -100;
    const un = await listen("meters", (e) => { peak = Math.max(peak, e.payload["Mic"] ?? -100); });
    setTimeout(() => { un(); resolve(peak); }, ms);
  });
}
function micMsg(t) { $("su-mic-msg").classList.remove("hidden"); $("su-mic-msg").textContent = t; }
function busy(on) { su.busy = on; suReady(); $("su-mic-device").disabled = on; }

// Try every microphone for a moment while the user talks; keep the loudest.
$("su-find-mic").onclick = async () => {
  const devs = [...$("su-mic-device").options].map((o) => ({ id: o.value, name: o.text })).filter((d) => d.id && d.id !== "default");
  if (!devs.length) return micMsg("No microphones found. Is one plugged in?");
  busy(true);
  const results = [], before = settings.audio.mic_device;
  try {
    for (const [i, d] of devs.entries()) {
      micMsg(`Keep talking… listening to ${d.name} (${i + 1} of ${devs.length})`);
      await suAudio({ mic_device: d.id, mic_enabled: true });
      await new Promise((r) => setTimeout(r, 900));   // let the new input start
      results.push({ ...d, peak: await listenMic(2500) });
    }
    results.sort((a, b) => b.peak - a.peak);
    const best = results[0];
    if (best.peak < -55) {
      await suAudio({ mic_device: before });
      $("su-mic-device").value = before;
      micMsg("Clipper heard nothing on any microphone. Is it muted, or did you talk during the test? Try again.");
    } else {
      await suAudio({ mic_device: best.id });
      $("su-mic-device").value = best.id;
      micMsg(`Found it: ${best.name}. Now press "Set my volume" so your voice isn't too quiet.`);
    }
  } catch (err) { micMsg("Something went wrong: " + err); }
  busy(false);
};

// Measure the voice and set the boost so it peaks around -12 dB (yellow).
$("su-level-mic").onclick = async () => {
  busy(true);
  try {
    micMsg("Talk normally for 5 seconds, like you would while playing…");
    const peak = await listenMic(5000);
    if (peak < -55) {
      micMsg("Clipper heard nothing. Check the device above (or use \"Find my microphone\") and try again.");
    } else {
      const old = settings.audio.mic_boost_db || 0;
      let boost = Math.max(0, Math.min(30, Math.round(old + (-12 - peak))));
      if (Math.abs(boost - old) <= 2) boost = old;   // close enough: don't keep nudging
      await suAudio({ mic_boost_db: boost });
      micMsg(boost === old ? "Your volume is good as it is."
        : boost >= 30 && peak < -24 ? "Boosted as much as possible (+30 dB). Your mic is very quiet: raise it in Windows sound settings too."
        : `Done: your voice is boosted by ${boost} dB. Talk again to see the bar reach yellow.`);
    }
  } catch (err) { micMsg("Something went wrong: " + err); }
  busy(false);
};

// ---------- test clip ----------
// Clip ~10 s of whatever runs now (the game, else the screen) and play it back here.
function testMsg(t) { $("su-test-msg").classList.remove("hidden"); $("su-test-msg").textContent = t; }
const sleep = (ms) => new Promise((r) => setTimeout(r, ms));

// The path of the next saved clip, or null after `ms`.
function nextClip(ms) {
  return new Promise(async (resolve) => {
    let done = false;
    const un = await listen("clip-saved", (e) => { if (!done) { done = true; un(); resolve(e.payload); } });
    setTimeout(() => { if (!done) { done = true; un(); resolve(null); } }, ms);
  });
}

$("su-test").onclick = async () => {
  if (status.obs !== "ready") return testMsg("The recorder isn't ready yet. Wait a moment and try again.");
  if (status.recording) return testMsg("A recording is running. Stop it first.");
  const btn = $("su-test");
  btn.disabled = true;
  $("su-test-video").pause();
  $("su-test-video").classList.add("hidden");
  $("su-test-after").classList.add("hidden");
  const startedHere = !status.buffer;
  uiLog(`setup test clip: start (game: ${status.game || "none"}, buffer was ${status.buffer ? "on" : "off"})`);
  try {
    if (startedHere) {
      testMsg("Starting…");
      await invoke("start_buffer");
    }
    for (let s = 10; s > 0; s--) {
      testMsg(`Recording ${status.game ? status.game : "your screen"}… talk and make some sound. ${s}`);
      await sleep(1000);
    }
    testMsg("Saving…");
    const saved = nextClip(20000);
    await invoke("save_clip");
    const path = await saved;
    if (!path) throw "the clip was not saved in time. Use \"Report a problem\" in Settings so the log can be checked.";
    uiLog(`setup test clip: saved ${path}`);
    const v = $("su-test-video");
    v.src = convertFileSrc(path);
    v.classList.remove("hidden");
    v.play().catch(() => {});
    $("su-test-after").classList.remove("hidden");
    testMsg("Here it is. Do you see the picture and hear the game, Discord and your voice?");
  } catch (err) {
    uiLog(`setup test clip failed: ${err}`);
    testMsg("The test clip failed: " + err);
  } finally {
    if (startedHere) await invoke("stop_buffer").catch(() => {});
    btn.disabled = false;
  }
};

// ---------- open on the first start ----------
const suWait = setInterval(() => {
  if (!settings) return;
  clearInterval(suWait);
  if (!settings.setup_done) setupOpen();
}, 100);

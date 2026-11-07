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
    if (tab.dataset.view === "settings") { loadDevices(); loadGames(); loadMonitors(); }
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
  // Once per OBS start (each call creates probe inputs in OBS); "Refresh" retries by hand.
  if (ready && settings && !devicesLoaded && !devicesTried) { devicesTried = true; loadDevices(); }
  if (!ready) devicesTried = false;
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

// ---- select clips: combine (order = oldest first) or delete ----
let selecting = false, selected = [];
const clipTime = new Map(); // path -> recorded (s): edits keep the original's time
const byAge = (a, b) => (clipTime.get(a) ?? 0) - (clipTime.get(b) ?? 0) || a.localeCompare(b);
const isImage = (path) => /\.(png|jpe?g)$/i.test(path);
function disarmDelete() {
  const d = $("sel-delete");
  d.classList.remove("armed");
  d.textContent = selected.length ? `Delete ${selected.length}` : "Delete";
}
function syncSelection() {
  selected.sort(byAge);
  document.body.classList.toggle("selecting", selecting);
  $("sel-mode").textContent = selecting ? "Done" : "Select";
  for (const id of ["sel-info", "sel-clear", "sel-all"]) $(id).classList.toggle("hidden", !selecting);
  $("sel-combine").classList.toggle("hidden", !selecting || selected.length < 2 || selected.some(isImage));
  $("sel-delete").classList.toggle("hidden", !selecting || !selected.length);
  disarmDelete();
  $("sel-info").textContent = selected.length ? `${selected.length} selected` : "Click clips to select them";
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
// All clips shown right now (respects the folder / Starred filter).
$("sel-all").onclick = () => {
  for (const el of document.querySelectorAll("#clips .clip")) {
    if (!selected.includes(el.dataset.path)) selected.push(el.dataset.path);
  }
  syncSelection();
};
// Windows can't move a file to the recycle bin while a <video> still has it
// open: let go of it everywhere (player, editor, thumbnails) first.
function releaseMedia(path) {
  const url = convertFileSrc(path);
  for (const v of document.querySelectorAll("video")) {
    const src = v.getAttribute("src") || "";
    if (src && src.split("#")[0] === url) {
      v.pause();
      v.removeAttribute("src");
      v.load();
    }
  }
}
let delTimer;
$("sel-delete").onclick = async () => {
  const d = $("sel-delete");
  if (!d.classList.contains("armed")) {   // first click arms, second deletes
    d.classList.add("armed");
    d.textContent = `Sure? Delete ${selected.length}`;
    clearTimeout(delTimer);
    delTimer = setTimeout(disarmDelete, 3000);
    return;
  }
  clearTimeout(delTimer);
  d.disabled = true;
  let ok = 0;
  const failed = [];
  for (const path of [...selected]) {
    releaseMedia(path);
    try { await invoke("delete_clip", { path }); ok++; }
    catch (err) { failed.push(err); }
  }
  d.disabled = false;
  selected = []; selecting = false; syncSelection();
  say(failed.length ? `Moved ${ok} to the recycle bin; ${failed.length} failed: ${failed[0]}` : `Moved ${ok} to the recycle bin.`);
  loadClips();
};
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
    <div class="actions"><button data-a="share" class="primary" title="Copy it to paste in Discord (Ctrl+V)">Share ▾</button>
      ${isImg ? "" : "<button data-a='edit'>Edit</button>"}<button data-a="reveal" title="Show in folder">Folder</button><span class="spacer"></span>
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
      inp.replaceWith(nameEl); // the card may be reused when nothing changed
      loadClips();
    };
    inp.onkeydown = (e) => { if (e.key === "Enter") finish(true); if (e.key === "Escape") finish(false); e.stopPropagation(); };
    inp.onblur = () => finish(true);
  };
  const star = el.querySelector(".star");
  star.classList.toggle("on", !!c.favorite);
  star.onclick = () => {
    const on = !c.favorite;
    star.classList.toggle("on", on);
    invoke("set_favorite", { path: c.path, on })
      .then(() => { c.favorite = on; say(on ? "Starred: the storage limit never deletes it." : "Star removed."); loadClips(); },
        (err) => { star.classList.toggle("on", !on); say(err); }); // not stored: don't show it as starred
  };
  const when = (t) => new Date(t * 1000).toLocaleString();
  const sub = el.querySelector(".sub");
  sub.textContent = (c.edited ? `Edited ${when(c.modified)}` : when(c.modified))
    + ` · ${(c.size / 1e6).toFixed(c.size < 1e7 ? 1 : 0)} MB`;
  sub.title = c.edited ? `Recorded ${when(c.recorded)}\nEdited ${when(c.modified)}` : `Recorded ${when(c.modified)}`;
  el.dataset.path = c.path;
  el.querySelector(".thumb").onclick = () => {
    if (selecting) {
      const i = selected.indexOf(c.path);
      if (i >= 0) selected.splice(i, 1); else selected.push(c.path);
      return syncSelection();
    }
    isImg ? invoke("reveal_clip", { path: c.path }).catch(say) : openPlayer(c);
  };
  el.querySelector('[data-a="share"]').onclick = (e) => { e.stopPropagation(); openShare(e.currentTarget, c, isImg); };
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
    releaseMedia(c.path);
    try {
      await invoke("delete_clip", { path: c.path });
      say("Moved to the recycle bin.");
      loadClips();
    } catch (err) { say(err); }
  };
  return el;
}

// ---------- share ----------
// Share = copy to the clipboard, ready to paste in Discord (no upload). Video
// clips can first be made to fit Discord's upload limit for your account.
const SHARE = [
  { mb: 20, label: "Discord", sub: "free account · 20 MB" },
  { mb: 50, label: "Discord Nitro Basic", sub: "50 MB" },
  { mb: 1000, label: "Discord Nitro", sub: "1 GB" },
  { mb: null, label: "Original file", sub: "full quality" },
];
let shareBusy = false;
function closeShare() { document.querySelector(".share-menu")?.remove(); }
document.addEventListener("click", (e) => { if (!e.target.closest(".share-menu")) closeShare(); });
window.addEventListener("keydown", (e) => { if (e.key === "Escape") closeShare(); });

function openShare(btn, c, isImg) {
  const open = document.querySelector(".share-menu");
  closeShare();
  if (open?.dataset.path === c.path) return;   // second click closes it
  if (isImg) return shareClip(btn, c, null);
  let last = null;
  try { last = localStorage.getItem("share-mb"); } catch {}
  const menu = document.createElement("div");
  menu.className = "share-menu";
  menu.dataset.path = c.path;
  const head = document.createElement("div");
  head.className = "share-head";
  head.textContent = "Copy for…";
  menu.append(head, ...SHARE.map((o) => {
    const b = document.createElement("button");
    const fits = o.mb === null || c.size <= o.mb * 1e6 * 0.97;
    b.innerHTML = `<b></b><small></small>`;
    b.querySelector("b").textContent = o.label + (String(o.mb) === last ? "  ✓" : "");
    b.querySelector("small").textContent = o.sub + (o.mb && !fits ? " · made smaller" : "");
    b.onclick = (e) => { e.stopPropagation(); closeShare(); shareClip(btn, c, o.mb); };
    return b;
  }));
  document.body.append(menu);
  const r = btn.getBoundingClientRect(), m = menu.getBoundingClientRect();
  const top = r.bottom + 4 + m.height > innerHeight ? r.top - 4 - m.height : r.bottom + 4;
  menu.style.top = Math.max(4, top) + "px";
  menu.style.left = Math.max(4, Math.min(r.left, innerWidth - m.width - 4)) + "px";
}

async function shareClip(btn, c, mb) {
  if (shareBusy) return say("Still preparing the previous clip…");
  shareBusy = true;
  try { localStorage.setItem("share-mb", String(mb)); } catch {}
  const text = btn.textContent;
  btn.disabled = true;
  const needsFit = mb !== null && c.size > mb * 1e6 * 0.97;
  btn.textContent = needsFit ? "Preparing…" : "Copying…";
  if (needsFit) say("Getting it ready for Discord… you can keep using Clipper.");
  const un = await listen("share-progress", (e) => { btn.textContent = `Preparing ${e.payload}%`; });
  try {
    const how = await invoke("share_clip", { path: c.path, maxMb: mb });
    say(how === "reused" ? "Copied (already prepared earlier)! Paste it in Discord with Ctrl+V." : "Copied! Paste it in Discord with Ctrl+V.");
    btn.textContent = "Copied ✓";
    setTimeout(() => { btn.textContent = text; }, 2500);
  } catch (err) {
    say(err);
    btn.textContent = text;
  }
  un();
  btn.disabled = false;
  shareBusy = false;
}

let folderFilter = "";
const STARRED = "\u0000starred";   // filter value for "only starred clips"
// Cards are reused while their clip is unchanged: rebuilding every card (and
// its <video> thumbnail) on each star, rename or saved clip was slow with many clips.
const cardCache = new Map();
const cardKey = (c) => [c.path, c.name, c.modified, c.recorded, c.edited, c.size, !!c.favorite, c.folder].join("\u0001");
// One load at a time; calls during a load ask for one more run afterwards, so
// an older answer can never be drawn over a newer one.
let clipsLoading = null, clipsAgain = false;
function loadClips() {
  if (clipsLoading) { clipsAgain = true; return clipsLoading; }
  clipsLoading = loadClipsNow().catch((e) => say(e)).finally(() => {
    clipsLoading = null;
    if (clipsAgain) { clipsAgain = false; loadClips(); }
  });
  return clipsLoading;
}
async function loadClipsNow() {
  const all = await invoke("list_clips");
  clipTime.clear();
  for (const c of all) clipTime.set(c.path, c.recorded ?? c.modified);
  const folders = [...new Set(all.map((c) => c.folder).filter(Boolean))].sort((a, b) => a.localeCompare(b));
  const anyStar = all.some((c) => c.favorite);
  if (folderFilter === STARRED ? !anyStar : folderFilter && !folders.includes(folderFilter)) folderFilter = "";
  const chips = ["", ...(anyStar ? [STARRED] : []), ...folders];
  $("folders").replaceChildren(...chips.map((f) => {
    const b = document.createElement("button");
    b.textContent = f === STARRED ? "★ Starred" : f || "All";
    b.classList.toggle("active", f === folderFilter);
    b.onclick = () => { folderFilter = f; loadClips(); };
    return b;
  }));
  $("folders").classList.toggle("hidden", chips.length < 3 && !anyStar);
  const clips = folderFilter === STARRED ? all.filter((c) => c.favorite)
    : folderFilter ? all.filter((c) => c.folder === folderFilter) : all;
  const fresh = new Map();
  const cards = clips.map((c) => {
    const k = cardKey(c);
    const el = cardCache.get(k) || clipCard(c);
    fresh.set(k, el);
    return el;
  });
  cardCache.clear();
  for (const [k, el] of fresh) cardCache.set(k, el);
  $("clips").replaceChildren(...cards);
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
  syncKeyLabels();
  reportSync();
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
  syncClipsDir();
}

// ---------- clips folder (Settings and the setup guide) ----------
function syncClipsDir() {
  for (const p of ["", "su-"]) {
    $(p + "clips-dir").textContent = settings.clips_dir;
    $(p + "clips-dir-default").classList.toggle("hidden", !settings.clips_dir_custom);
  }
}
async function refreshClipsDir() {
  const s = await invoke("get_settings").catch(() => null);
  if (s) for (const k of ["clips_dir", "clips_dir_custom"]) settings[k] = s[k];
  syncClipsDir();
}
const CLIP_DIR_BUTTONS = ["clips-dir-change", "clips-dir-default", "su-clips-dir-change", "su-clips-dir-default"];
async function changeClipsFolder(reset, move) {
  try {
    const path = reset ? "" : await invoke("pick_folder", { start: settings.clips_dir });
    if (path === null) return;
    CLIP_DIR_BUTTONS.forEach((id) => ($(id).disabled = true));
    say("Changing the clips folder…");
    const r = await invoke("set_clips_dir", { path, moveClips: move });
    say(r.left ? `New clips now go to ${r.dir}. ${r.left} clip(s) stay in ${r.old} and still show in Clipper.`
      : "Clips folder: " + r.dir);
  } catch (err) {
    say(String(err));
  } finally {
    CLIP_DIR_BUTTONS.forEach((id) => ($(id).disabled = false));
    await refreshClipsDir();
    loadClips();
  }
}
// While clips move (can take minutes across drives), the library is in flux:
// hide it behind a message instead of showing half-moved clips.
listen("clips-moving", (e) => {
  document.body.classList.toggle("moving-clips", !!e.payload);
  if (e.payload) say("Moving your clips… this can take a while. New clips already go to the new folder.");
  else loadClips();
});
for (const p of ["", "su-"]) {
  $(p + "clips-dir-change").onclick = () => changeClipsFolder(false, $(p + "clips-move").checked);
  $(p + "clips-dir-default").onclick = () => changeClipsFolder(true, $(p + "clips-move").checked);
}

function syncKeyLabels() {
  const labels = { clip: settings.hotkey_label, record: settings.record_hotkey_label, screenshot: settings.screenshot_hotkey_label };
  for (const b of document.querySelectorAll(".keybtn")) b.textContent = labels[b.dataset.which];
}
let listenTimer;
function stopListening() {
  if (!listening) return;
  listening.classList.remove("listening");
  listening = false;
  clearTimeout(listenTimer);
  syncKeyLabels();
}
for (const b of document.querySelectorAll(".keybtn")) {
  b.onclick = () => {
    stopListening();
    listening = b;
    b.classList.add("listening");
    b.textContent = "Press keys… (Esc cancels)";
    // Never keep waiting: a key typed much later (or elsewhere) must not become the hotkey.
    listenTimer = setTimeout(stopListening, 10000);
  };
}
window.addEventListener("blur", stopListening);
document.addEventListener("pointerdown", (e) => { if (listening && e.target !== listening) stopListening(); }, true);
window.addEventListener("keydown", async (e) => {
  if (!listening) return;
  e.preventDefault();
  if (MODIFIERS.includes(e.code)) return; // wait for the real key
  const btn = listening;
  listening = false;
  clearTimeout(listenTimer);
  btn.classList.remove("listening");
  if (e.code === "Escape") return loadSettings();
  try {
    const label = await invoke("set_hotkey", {
      hotkey: { code: e.code, shift: e.shiftKey, ctrl: e.ctrlKey, alt: e.altKey }, which: btn.dataset.which,
    });
    settings[{ clip: "hotkey_label", record: "record_hotkey_label", screenshot: "screenshot_hotkey_label" }[btn.dataset.which]] = label;
    syncKeyLabels();
    document.dispatchEvent(new Event("keys-changed"));
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

// ---------- report a problem ----------
function reportSync() {
  $("report-token").placeholder = settings?.report_ready ? "Token saved (empty + Save removes it)" : "Paste your API token";
  $("report-send").textContent = settings?.report_ready ? "Send to Todoist" : "Report on GitHub";
}
$("report-token-save").onclick = async () => {
  const btn = $("report-token-save");
  btn.disabled = true;
  try {
    await invoke("set_report_token", { token: $("report-token").value });
    settings.report_ready = !!$("report-token").value.trim();
    $("report-token").value = "";
    reportSync();
    say(settings.report_ready ? "Todoist token saved." : "Todoist token removed.");
  } catch (err) { say(err); }
  btn.disabled = false;
};
$("report-send").onclick = async () => {
  const text = $("report-text").value.trim();
  if (!text) return say("Describe the problem first.");
  const btn = $("report-send");
  btn.disabled = true;
  try {
    if (settings.report_ready) {
      $("report-status").textContent = "Sending…";
      await invoke("report_problem", { text });
      $("report-status").textContent = "Sent to your Todoist Inbox, with the logs.";
    } else {
      await invoke("report_github", { text });
      $("report-status").textContent = "Opened in your browser. The log is on your clipboard: paste it (Ctrl+V) in the report, then press Create.";
    }
    $("report-text").value = "";
  } catch (err) { $("report-status").textContent = "Not sent: " + err; }
  btn.disabled = false;
};

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
  try { await invoke("install_update", { version: pendingUpdate.version }); }
  catch (err) {
    $("update-text").textContent = "Update failed: " + err;
    $("install-update").disabled = $("check-update").disabled = false;
  }
};
listen("update-available", (e) => {
  showUpdate(e.payload);
  say(`Clipper ${e.payload.version} is available: Settings → Updates → Install.`);
});
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
let devicesLoaded = false, devicesTried = false, devicesLoading = false;

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
  $("a-mic-mono").checked = !!a.mic_mono;
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
  for (const id of ["a-mic-device", "a-mic-volume", "a-mic-noise", "a-mic-mono", "a-mic-boost"]) $(id).disabled = !$("a-mic-enabled").checked;
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
  if (status.obs !== "ready" || !settings || devicesLoading) return;
  devicesLoading = true;
  try {
    const [inputs, outputs] = [await invoke("list_audio_devices", { output: false }),
                               await invoke("list_audio_devices", { output: true })];
    fillDevices("a-mic-device", inputs, settings.audio.mic_device);
    fillDevices("a-desktop-device", outputs, settings.audio.desktop_device);
    devicesLoaded = true;
  } catch (err) { say("Could not list audio devices: " + err); }
  finally { devicesLoading = false; }
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
    mic_mono: $("a-mic-mono").checked,
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
for (const id of ["a-game-mode", "a-desktop-device", "a-discord-enabled", "a-mic-enabled", "a-mic-device", "a-mic-noise", "a-mic-mono"]) {
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

// ---------- what's new after an update ----------
function inline(t) {
  const span = document.createElement("span");
  t.split("**").forEach((part, i) => span.append(i % 2 ? Object.assign(document.createElement("b"), { textContent: part }) : part));
  return span;
}
async function showWhatsNew() {
  const entries = await invoke("whats_new").catch(() => []);
  if (!entries.length) return;
  $("whatsnew-list").replaceChildren(...entries.flatMap((e) => {
    const h = document.createElement("h3");
    h.textContent = "Version " + e.version;
    const ul = document.createElement("ul");
    ul.append(...e.items.map((t) => { const li = document.createElement("li"); li.append(inline(t)); return li; }));
    return [h, ul];
  }));
  $("whatsnew").classList.remove("hidden");
}
$("whatsnew-close").onclick = () => $("whatsnew").classList.add("hidden");

loadSettings().then(() => invoke("get_status")).then((s) => { status = s; render(); }).then(showWhatsNew);
loadClips();

# Clipper — notes for coding agents

Clipper is a Medal-style clip recorder for **Windows**. It keeps the last minutes of a game in a
replay buffer and saves a clip on a key press. A hidden, portable OBS does the recording and is
controlled over obs-websocket. Clipper has its own library, editor, updater and installer.
**All UI text is in English.** The owner talks to you in Dutch. Answer in Dutch, but keep code,
UI text and commit messages in English.

## How we work (read first)

- **The Windows desktop is the test machine. Development happens elsewhere** (laptop / cloud agent).
  You cannot run the Windows build yourself. Every change reaches the tester as a **GitHub
  pre-release**, and the in-app updater installs it (Settings → Updates, "Include test builds" on).
- Loop: change code → check that it compiles for Windows → bump the version and publish a
  pre-release → the owner updates in the app and tests → the owner reports back with symptoms and
  the log.
- **Bug reports come with logs:** Settings → Troubleshooting → "Report a problem". The owner has a
  Todoist token set, so their reports become a task in their Todoist Inbox (title `Clipper X.Y.Z: …`,
  status/settings summary in the description, the end of clipper.log and of the newest OBS log as
  comments). Read those comments first. Everyone else gets a pre-filled **GitHub issue** (redacted:
  user name, window titles, IPs; `report::redact`), so also check `gh issue list`.
- Logs on Windows: `%APPDATA%\nl.jeremyklootwijk.clipper\clipper.log`
  (Settings → Troubleshooting → Open log folder). OBS logs are in `obs\config\obs-studio\logs\`
  in the same folder. Ask for the relevant lines when a bug report is vague.
- Bug reports live in the owner's **Todoist Inbox** (in Dutch). If you have Todoist, read the
  whole Inbox for new reports, not only the ones you already know. Do not add test checklists to
  Todoist after a release (owner, 2026-11-07); the release notes list what to test.
- Keep a release small enough to test in one sitting, and list the testable points in the
  release notes.
- Never put secrets in this repo. It is **public**, because the updater downloads from public
  releases.

## Build, check, release

Requirements: Rust stable, Node (for `npx tauri`), `cargo-xwin` (`cargo install cargo-xwin`;
it downloads the MSVC CRT/SDK on first use), `gh` logged in to the repo.

```sh
npm install
cd src-tauri
cargo xwin check --target x86_64-pc-windows-msvc   # Windows compile check (plain `cargo check` on Linux skips Windows code)
cargo test                                         # unit tests (Linux)
cd ..
scripts/release.sh 0.3.1 "What changed, in plain English for the tester"   # build exe, bump, commit, push main+test, gh pre-release
```

- **CI:** `.github/workflows/ci.yml` runs `cargo test` + the Tauri release build (`npx tauri build
  --no-bundle`, same as release.sh) on a real Windows runner for every push to `main` and every PR
  (docs-only commits skipped); the exe is a 7-day workflow artifact.
  Check it with `gh run list` / `gh run view --log-failed` after pushing.
- `release.sh` bumps `Cargo.toml` and `tauri.conf.json`, builds
  `src-tauri/target/x86_64-pc-windows-msvc/release/clipper.exe`, commits "Release X.Y.Z", moves
  branch `test` to HEAD, pushes `main` and `test`, then runs
  `gh release create vX.Y.Z clipper.exe --prerelease`. `X.Y.0` versions are published as the latest (stable) release.
- **What's new:** every user-facing change gets a short line in `CHANGELOG.md` under `## X.Y.Z`
  (written for players, in English) in the same commit. The exe embeds it and shows the entries
  since the last seen version after an update (`changelog.rs`, `whats_new`); a unit test fails
  when the current version has no entry.
- **Version rule (owner, 2026-11-06): `X.Y.0` is a stable release, every `X.Y.N` with N > 0 is a
  test build (pre-release).** `release.sh` picks stable/pre-release from the version itself (no
  `--stable` flag any more) and `promote.sh` only accepts `X.Y.0`. Fixes go out as the next patch
  number (test build); a new stable is the next minor `X.(Y+1).0`.
- **Stable vs test builds:** new installs have "Include test builds" off (since 0.4.0) and only get
  GitHub's *latest* (non-pre-)release; the owner has it on. Ship test builds (`X.Y.N`) first; once the owner
  is happy, release the next `X.Y.0` as stable. `scripts/promote.sh X.Y.0` turns an `X.Y.0` that was
  published as a pre-release into the latest release (notes = CHANGELOG entries since the previous stable).
- The updater accepts any release tagged `vX.Y.Z` that is newer than the running version and has
  an asset named exactly `clipper.exe`. It checks the sha256 digest from the GitHub API.
- No NSIS/MSI: the exe installs itself (`installer.rs`).

## Architecture

Tauri v2 (Rust) + plain HTML/JS in `ui/` (no bundler, `withGlobalTauri`).

| File | What |
|---|---|
| `src-tauri/src/lib.rs` | App state, all `#[tauri::command]`s, tray, OBS event follower (`watch_events`), startup |
| `obs/mod.rs` | `ObsManager`: writes config, spawns OBS, connects, scene setup with retry, supervisor restart, `shutdown()`, Windows job object (OBS dies with Clipper), removes OBS's tray icon |
| `obs/config.rs` | Writes OBS user.ini/global.ini, profile basic.ini (encoder, tracks, replay buffer), websocket config (port 4466, random password in `<data>/obs-ws-password`) |
| `obs/setup.rs` | Builds inputs/scene over websocket each start: Game Capture/Window Capture, Game Audio, Discord, Mic (+RNNoise, gain, limiter, mono), apps. On Linux: looping test files instead. Mono is a source flag the websocket can't set: `obs/config.rs` installs a Lua script (`clipper-mono.lua`) that reads `clipper_mono` from the Mic's settings |
| `obs/download.rs` | First-run download of pinned portable OBS (sha256-checked) into `<data>/obs` |
| `obs/ws.rs` | Minimal obs-websocket v5 client |
| `games.rs` | Steam library scan + Minecraft + user-added apps; picks the game window → OBS window string |
| `autobuffer.rs` | 2 s tick: starts/stops the replay buffer when a game starts/stops |
| `hotkey.rs` | Two listeners that never block the key: raw input (WM_INPUT, normally active) and a low-level keyboard hook (takes over only when raw input receives no keys; checked every 5 s). Only one acts at a time, so no dedup is needed; the log says which (`hook`/`raw`). Slots: Clip (only armed while clipping), Record, Screenshot. Defaults F8/F9/F10 |
| `modes.rs`, `screen.rs` | Recording + screen mode (monitor capture, canvas resize, back to the game scene) |
| `clips.rs` | Library: per-game folders in `Videos\Clipper` or a folder the user picked (`settings.clips_dir`); OBS always writes into `<clips>/.incoming` (same drive; since 0.4.9 also for the default folder), and `recover_leftovers` (lib.rs) adopts files left there or in the old `<data>/incoming` into "Recovered" at start. Only folders with the `.clipper-clips` marker are sorted, size-limited or moved out of; the library also lists earlier Clipper folders (`old_clips_dirs`, Videos\Clipper), rename, favorites, storage limit |
| `editor.rs`, `tools.rs` | Trim, split/cut out parts (`keep` ranges → trim/atrim + concat in one graph), fade, save frame, volumes, export/combine/"Fit for Discord" via ffmpeg (pinned download into `<data>/ffmpeg`) |
| `gpu.rs` | DXGI vendor → OBS + ffmpeg encoder (NVENC/AMF/QSV/x264), CPU fallback |
| `updater.rs`, `installer.rs`, `autostart.rs` | GitHub-release updater, self-install to `%LOCALAPPDATA%\Programs\Clipper` + uninstall, HKCU Run |
| `settings.rs` | `settings.json` (serde defaults, so new fields are safe) |
| `report.rs` | "Report a problem" → Todoist API v1 with the user's own token (`<data>/todoist-token`, never in the repo); otherwise a short pre-filled GitHub issue URL (Windows cuts long URLs) with the redacted log on the clipboard |
| `ui/main.js` | Clips, Settings, status. `ui/editor.js` editor. `ui/setup.js` first-start setup guide (shown while `setup_done` is false), ending with a 10 s test clip |

Logo: `src-tauri/icons/logo.svg` (32 px and up), `logo-16.svg` (16/24 px: play only), `tray-recording.svg`;
the PNGs and icon.ico are rendered from them with rsvg-convert + magick. `ui/logo.svg` and the
copy inline in `ui/toast.html` must stay in step. Toasts use the app's theme colours.

Audio tracks: 1 Mix, 2 Game, 3 Discord, 4 Mic, 5/6 extra apps. OBS writes track names as the
stream tag `name`.

## Gotchas learned the hard way

- OBS's websocket opens **before** OBS is ready (error 207 "not ready"). Scene setup retries for up
  to 60 s, then restarts OBS. Two OBS processes game-capturing the same game give **black clips**.
  Leftover OBS processes from our own exe are killed before each start.
- `SetVideoSettings` makes OBS rescale every scene item: set the canvas **before** placing items.
  `baseWidth` is limited to 4096, so "All screens" is scaled down. Check layouts with
  `GetSceneItemList` transforms.
- `SetProfileParameter` for RecTracks does **not** apply live: track-mask changes need an OBS
  restart. The buffer length (`AdvOut/RecRBTime`) does work live after stop/start of the buffer.
- Game Capture can't hook normal apps (Chrome/Discord come out black). Added apps use Window Capture
  (WGC, method 2).
- Settings that need an OBS restart (quality, track mask) while clipping/recording set `restart_pending`; `watch_events` restarts OBS once both outputs have stopped.
- The obs-websocket event channel is shared with ~20/s volume meters: never block `watch_events` (file moves run in spawned tasks).
- `ensure_scene` never sends `"window": ""` to an existing capture input (it would unhook the running game).
- `RegisterHotKey` swallows the key. That's why we use a listen-only `WH_KEYBOARD_LL` hook.
- OBS only starts hidden when its tray is enabled (`hideWindowOnStart` in OBSBasic.cpp). Clipper
  therefore deletes OBS's Qt tray icon (`QTrayIconMessageWindowClass`, uID 0) instead of disabling
  the tray.
- Explorer `/select,` needs a raw arg: `/select,"path"`.
- OBS volume meters are post-filter (boost/limiter included). RNNoise turns white-noise test input
  into silence.
- On Linux the app runs for development with fake inputs: `CLIPPER_DATA_DIR=$PWD/dev-data/data
  CLIPPER_CLIPS_DIR=$PWD/dev-data/clips` (**absolute** paths: OBS runs in another cwd and a relative
  XDG_CONFIG_HOME makes it miss the websocket config → "could not connect to OBS"), plus system `obs` with its own `XDG_CONFIG_HOME`. Debug-build
  hooks: `CLIPPER_DEV_JS_FILE=<file>` runs JS in the page after 6 s (use `invoke()` and
  `uiLog()`, whose output goes to clipper.log), `CLIPPER_DEV_VIEW=settings`, `CLIPPER_DEV_EDIT=1`.
  `node scripts/obsctl.mjs <data dir> <Request> [json]` drives the private OBS.
- The page is zoomed with the window (`fit_zoom` in lib.rs, 1100×720 = 1.0, max 1.6), so the CSS is
  written for the default window size.
- Security (0.4.0): strict CSP in tauri.conf.json (no inline scripts/styles: `toast.html` uses
  `toast.css`/`toast.js`), and the asset protocol scope is empty in the config; `run()` allows only the
  clips folder and the data folder at runtime. A new place the page loads files from must be added there.
- On Linux, WebKitGTK here can't play the H.264 clips (MediaError 4) even with the old open config;
  that is the dev machine, not Clipper. Images load fine.
- UI screenshots on Linux: WebKitGTK windows can't be grabbed, so render a copy of `ui/` in headless
  Chromium with a mocked `window.__TAURI__` (`--virtual-time-budget=3000`).

## Product decisions (don't undo without asking)

- No upload feature: "Share" on a clip copies it to the clipboard to paste in Discord, optionally
  made to fit Discord's limit first (free 20 MB, Nitro Basic 50 MB, Nitro 1 GB; checked 2026-11-02).
  Fitted copies go to `<data>/share`, not the library. The editor no longer has a size option.
- Clipping a real game always wins over an added app (Discord etc.): added apps only when no game runs.
- OBS stays invisible. The user should only ever see Clipper.
- The chime on save is permanent. Corner pop-ups are a normal user option ("Pop-up notifications", 2026-11-02), no longer a debug aid.
- Default volumes are 100%. Discord and the extra apps are always in the Mix.
- Clipping starts automatically only for Steam games, Minecraft and apps the user added.
- Not wanted for now: vertical export, automatic highlights.
- Screen content stays in three folders on purpose (2026-11-03): "Screen" (clips), "Screen recording", "Screenshots". One folder would be unclear; copying goes through Clipper itself.
- No webcam overlay (removed in 0.3.1): OBS held the camera, so Discord couldn't use it. Setup
  still removes a leftover "Webcam" input from older installs.

## Open items (2026-11-02)

- **0.4.1 was the first stable release** (2026-11-02); **0.5.0 is the current stable** (2026-11-06). New installs only get stable releases; keep
  shipping test builds and promote one once the owner has tested it.

- 0.3.0 (setup guide, F8/F9/F10, OBS tray icon removal) needs a Windows test. It is unconfirmed
  whether removing OBS's tray icon works.
- The owner saw "OBS running without Clipper" at least once. It's unclear whether that was the OBS
  tray icon or a real leftover process after Quit. Ask, and check the log.
- Unconfirmed from 0.2.x: screen recording (main/all screens), recording an added app, combine clips.
- "Could not target Discord" (code 600, no `Window Capture`): the Window Capture input was never
  created since 0.2.2, so added apps never recorded. Fixed in 0.3.2, needs a Windows test.

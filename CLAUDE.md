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
- Logs on Windows: `%APPDATA%\nl.jeremyklootwijk.clipper\clipper.log`
  (Settings → Troubleshooting → Open log folder). OBS logs are in `obs\config\obs-studio\logs\`
  in the same folder. Ask for the relevant lines when a bug report is vague.
- Bug reports and test checklists live in the owner's **Todoist Inbox** (in Dutch). If you have
  Todoist, read the whole Inbox for new reports, not only the ones you already know. After a
  release, add a short Dutch test checklist (a parent task with numbered subtasks) for what changed.
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

- `release.sh` bumps `Cargo.toml` and `tauri.conf.json`, builds
  `src-tauri/target/x86_64-pc-windows-msvc/release/clipper.exe`, commits "Release X.Y.Z", moves
  branch `test` to HEAD, pushes `main` and `test`, then runs
  `gh release create vX.Y.Z clipper.exe --prerelease`. Add `--stable` to make it the latest release.
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
| `obs/setup.rs` | Builds inputs/scene over websocket each start: Game Capture/Window Capture, Game Audio, Discord, Mic (+RNNoise, gain, limiter), apps, webcam. On Linux: looping test files instead |
| `obs/download.rs` | First-run download of pinned portable OBS (sha256-checked) into `<data>/obs` |
| `obs/ws.rs` | Minimal obs-websocket v5 client |
| `games.rs` | Steam library scan + Minecraft + user-added apps; picks the game window → OBS window string |
| `autobuffer.rs` | 2 s tick: starts/stops the replay buffer when a game starts/stops |
| `hotkey.rs` | Low-level keyboard hook that only listens (key still reaches the game). Slots: Clip (only armed while clipping), Record, Screenshot. Defaults F8/F9/F10 |
| `modes.rs`, `screen.rs` | Recording + screen mode (monitor capture, canvas resize, back to the game scene) |
| `clips.rs` | Library: per-game folders in `Videos\Clipper`, rename, favorites, storage limit |
| `editor.rs`, `tools.rs` | Trim/volumes/export/combine/"Fit for Discord" via ffmpeg (pinned download into `<data>/ffmpeg`) |
| `gpu.rs` | DXGI vendor → OBS + ffmpeg encoder (NVENC/AMF/QSV/x264), CPU fallback |
| `updater.rs`, `installer.rs`, `autostart.rs` | GitHub-release updater, self-install to `%LOCALAPPDATA%\Programs\Clipper` + uninstall, HKCU Run |
| `settings.rs` | `settings.json` (serde defaults, so new fields are safe) |
| `ui/main.js` | Clips, Settings, status. `ui/editor.js` editor. `ui/setup.js` first-start setup guide (shown while `setup_done` is false) |

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
- `RegisterHotKey` swallows the key. That's why we use a listen-only `WH_KEYBOARD_LL` hook.
- OBS only starts hidden when its tray is enabled (`hideWindowOnStart` in OBSBasic.cpp). Clipper
  therefore deletes OBS's Qt tray icon (`QTrayIconMessageWindowClass`, uID 0) instead of disabling
  the tray.
- Explorer `/select,` needs a raw arg: `/select,"path"`.
- OBS volume meters are post-filter (boost/limiter included). RNNoise turns white-noise test input
  into silence.
- On Linux the app runs for development with fake inputs: `CLIPPER_DATA_DIR=dev-data/data
  CLIPPER_CLIPS_DIR=dev-data/clips`, plus system `obs` with its own `XDG_CONFIG_HOME`. Debug-build
  hooks: `CLIPPER_DEV_JS_FILE=<file>` runs JS in the page after 6 s (use `invoke()` and
  `uiLog()`, whose output goes to clipper.log), `CLIPPER_DEV_VIEW=settings`, `CLIPPER_DEV_EDIT=1`.
  `node scripts/obsctl.mjs <data dir> <Request> [json]` drives the private OBS.
- UI screenshots on Linux: WebKitGTK windows can't be grabbed, so render a copy of `ui/` in headless
  Chromium with a mocked `window.__TAURI__` (`--virtual-time-budget=3000`).

## Product decisions (don't undo without asking)

- No sharing/upload feature: clips go to Discord by Copy → paste.
- OBS stays invisible. The user should only ever see Clipper.
- The chime on save is permanent. Corner pop-ups are a debug option.
- Default volumes are 100%. Discord and the extra apps are always in the Mix.
- Clipping starts automatically only for Steam games, Minecraft and apps the user added.
- Not wanted for now: vertical export, automatic highlights.

## Open items (2026-11-02)

- 0.3.0 (setup guide, F8/F9/F10, OBS tray icon removal) needs a Windows test. It is unconfirmed
  whether removing OBS's tray icon works.
- The owner saw "OBS running without Clipper" at least once. It's unclear whether that was the OBS
  tray icon or a real leftover process after Quit. Ask, and check the log.
- Unconfirmed from 0.2.x: screen recording (main/all screens), recording an added app, webcam
  overlay, combine clips.

# Clipper

Clipper keeps the last minutes of your game ready. Press one key and that moment is saved as a clip,
with the game, Discord and your microphone on separate audio tracks.

Windows 10/11, 64-bit.

## Install

1. Download `clipper.exe` from the [latest release](https://github.com/Jeremy0538/clipper/releases/latest).
2. Run it. Clipper installs itself for your user (no admin rights) and opens a short setup guide.

Windows may show "Windows protected your PC" because the file is not code-signed. Click
**More info → Run anyway**.

On the first start Clipper downloads the recording engine (a private copy of OBS Studio, about
150 MB). The editor downloads FFmpeg the first time you use it. Both downloads are checked against
a fixed SHA-256 hash.

## Use

- **Start a game.** Clipping starts by itself for Steam games, Minecraft and apps you add under
  Settings → Games. The status bar turns green.
- **F8** saves the last minutes as a clip, **F9** starts or stops a full recording, **F10** takes a
  screenshot. The keys still reach your game; change them in Settings.
- Without a game, Start clipping and Record capture your screen.
- **Clips**: play, star (starred clips are never removed by the storage limit), rename, combine,
  and **Share ▾** to copy a clip for Discord (20 MB, Nitro Basic 50 MB, Nitro 1 GB or the original),
  then paste it in Discord with Ctrl+V.
- **Edit**: trim and change the volume of Game, Discord and Mic separately.

Clips are saved in `Videos\Clipper`, one folder per game.

## Updates

Settings → Updates checks this repository's releases. "Include test builds" also offers
pre-releases.

## Uninstall

Windows Settings → Apps → Clipper → Uninstall. Your clips stay in `Videos\Clipper`.

## Building

See [CLAUDE.md](CLAUDE.md) (Tauri v2 + Rust, cross-compiled from Linux with `cargo-xwin`).

## License

Clipper is MIT licensed, see [LICENSE](LICENSE). It downloads and runs
[OBS Studio](https://obsproject.com) (GPL-2.0) and [FFmpeg](https://ffmpeg.org) (GPL build by
[gyan.dev](https://www.gyan.dev/ffmpeg/builds/)) as separate programs; they are not part of
Clipper and keep their own licenses.

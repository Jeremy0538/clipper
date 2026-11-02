# What's new in Clipper

Shown in the app after an update ("What's new"), and used as the notes of a
stable release (`scripts/promote.sh`). Short, for players: what changed for
them, not how. Newest version on top; one `## X.Y.Z` heading per version.

## 0.4.7
- Deleting the last clip of a game also removes its now-empty folder.
- Deleting works while the clip is open in the player or editor.
- "Report a problem" now includes recent errors in the report itself.

## 0.4.5
- Clips in an earlier clips folder (not moved, or saved while your drive was unplugged) keep showing in Clipper.
- More reliable: one key press never triggers an action twice, and a damaged OBS file no longer stops recording.

## 0.4.4
- Picking a clips folder that already holds other files (or a whole drive) makes a **Clipper** folder inside it, so your other files are never touched.
- New clips go to the new folder right away, also while your old clips are still moving.

## 0.4.3
- After an update, Clipper shows what's new (like this).

## 0.4.2
- The clip key reacts to a short tap again, also while a heavy game runs.
- New **Mono** option for your microphone: for mics you only hear on one side.
- Keep your clips on another drive: Settings → Recording → Clips folder (your clips can move along).
- Clipping Discord itself no longer puts every voice in the clip twice.
- **Report a problem** works again: GitHub opens without an error and the log is on your clipboard.

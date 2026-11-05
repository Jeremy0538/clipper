# What's new in Clipper

Shown in the app after an update ("What's new"), and used as the notes of a
stable release (`scripts/promote.sh`). Short, for players: what changed for
them, not how. Newest version on top; one `## X.Y.Z` heading per version.

## 0.4.11
- When an update is available, Clipper tells you once at start, also with pop-up notifications turned off.
- Saving in the editor can no longer hang on a clip with damaged frames, and a failing fast NVIDIA save no longer makes later saves slower. Clips recorded in 10-bit or 4:4:4 come out playable in Discord again.
- Clipper no longer hangs on "starting" or "downloading" when OBS or the internet doesn't answer: it tries again.
- Fixed: after OBS failed to start, quitting or changing quality could close another program. A leftover OBS no longer blocks installing OBS (and no longer wipes its settings).
- Fixed: stopping clipping or switching between a game's launcher and the game no longer turns auto-clipping off or repeats "Game detected". Changing the clip length while a game starts no longer keeps clipping off.
- Fixed: starring two clips quickly could lose one star, and a star that failed to save still showed as starred.
- The clip list is faster with many clips (no reload of every preview after a star or rename), and opening Settings while clipping no longer makes OBS re-check your screens each time.
- Stopping a screen recording no longer holds up other events for a few seconds, and Clipper doesn't get stuck in screen mode when switching back fails.

## 0.4.10
- Saving in the editor is faster on NVIDIA cards (trimming, cutting and combining clips).

## 0.4.9
- Editor: a **Delete the original** option next to Save. The original goes to the recycle bin and its star moves to the new clip. Clipper remembers your choice.
- Editor: a part you removed can always be restored, even when every part is removed. Shortcuts keep working after you pick a preview speed, and holding a key no longer repeats Split or Save frame.
- Editor: saving a clip with parts cut out is much faster on long clips, and the fade now applies to every audio track.
- The storage limit never deletes a clip you just made (before, with starred clips over the limit, new clips disappeared right away).
- A quality change or a new audio app now applies as soon as clipping stops, instead of after some later restart.
- Recordings that were still being finished when Clipper quit, updated or crashed now show up under **Recovered**.
- Clips are written straight onto the drive of your clips folder (no detour via C:), so saving is instant and big recordings can't fill C:.
- Changing audio settings mid-game no longer gives a few seconds of black or silent video in your clips, and switching "Game sound" no longer loses the game audio.
- Running a newer clipper.exe while Clipper is open now really switches to the new version.
- A Minecraft server running in the background no longer starts clipping.
- Combining clips twice in the same second no longer overwrites the first result, and a key button stops waiting for a key after 10 seconds or when you click elsewhere.

## 0.4.8
- Editor: **Split** (S) cuts the clip at the playhead, **Remove part** (Delete) cuts that part out (press again to bring it back). Removed parts are skipped while playing and left out when you save.
- Editor: Undo/Redo (Ctrl+Z / Ctrl+Y), step one frame with , and . , jump with Home/End, Shift+←/→ for 5 s.
- Editor: preview at 0.25×–2× speed, "Fade in/out" when saving, and **Save frame** (P) saves the current picture next to the clip.

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

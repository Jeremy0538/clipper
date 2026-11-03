#!/usr/bin/env bash
# Make a tested pre-release the stable release everyone gets:
#   scripts/promote.sh 0.4.3
# Its notes become the CHANGELOG.md entries since the previous stable
# release (the same text the app shows under "What's new").
set -euo pipefail
cd "$(dirname "$0")/.."
ver="${1:?usage: scripts/promote.sh X.Y.Z}"
# The newest stable release *before* this one (so re-running promote for a
# version that is already stable doesn't end up with empty notes).
prev="$(gh release list --exclude-pre-releases --limit 100 --json tagName -q '.[].tagName' | python3 -c '
import sys
ver = tuple(map(int, sys.argv[1].split(".")))
vs = [tuple(map(int, t.strip().lstrip("v").split("."))) for t in sys.stdin if t.strip()]
older = [v for v in vs if v < ver]
print(".".join(map(str, max(older))) if older else "")
' "$ver")"
notes="$(python3 - "$prev" "$ver" <<'PY'
import re, sys
prev, ver = (tuple(map(int, v.split("."))) if v else (0, 0, 0) for v in sys.argv[1:3])
out, keep = [], False
for line in open("CHANGELOG.md", encoding="utf-8"):
    m = re.match(r"## (\d+)\.(\d+)\.(\d+)\s*$", line)
    if m:
        v = tuple(map(int, m.groups()))
        keep = prev < v <= ver
        if keep:
            out.append(f"### {'.'.join(map(str, v))}\n")
        continue
    if keep and line.startswith("- "):
        out.append(line)
print("## What's new\n\n" + "".join(out).strip() + "\n\n**Install or update:** download `clipper.exe` below, or update inside the app (Settings → Updates).")
PY
)"
echo "$notes"
gh release edit "v$ver" --prerelease=false --latest --notes "$notes"
gh release view "v$ver" --json url,isPrerelease

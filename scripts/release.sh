#!/usr/bin/env bash
# Build the Windows exe and publish it as a release on the test branch.
# usage: scripts/release.sh 0.5.1 "What changed"
# Version rule: X.Y.0 is a stable release (everyone gets it), anything with a
# patch number after it (X.Y.1, X.Y.2 …) is a test build (pre-release).
# The in-app updater offers any release tagged vX.Y.Z newer than the running build
# (pre-releases only when "Include test builds" is on).
set -euo pipefail
# rustup (Windows target) may live in ~/.cargo instead of the system Rust.
[ -f "$HOME/.cargo/env" ] && . "$HOME/.cargo/env"
cd "$(dirname "$0")/.."
ver=${1:?version}; notes=${2:?notes}
[[ $ver =~ ^[0-9]+\.[0-9]+\.[0-9]+$ ]] || { echo "version must be X.Y.Z" >&2; exit 1; }
[[ -z ${3:-} ]] || { echo "no flags: X.Y.0 is stable, X.Y.N (N>0) a test build" >&2; exit 1; }
if [[ $ver == *.0 ]]; then flag=--latest; else flag=--prerelease; fi
# Every release needs its "What's new" entry (the app shows it after updating).
grep -qx "## $ver" CHANGELOG.md || { echo "CHANGELOG.md has no '## $ver' entry" >&2; exit 1; }
sed -i "0,/^version = \".*\"/s//version = \"$ver\"/" src-tauri/Cargo.toml
sed -i "s/\"version\": \"[^\"]*\"/\"version\": \"$ver\"/" src-tauri/tauri.conf.json
# Unit tests first (one of them checks the CHANGELOG entry for this version).
(cd src-tauri && cargo test -q) || { git checkout -- src-tauri/Cargo.toml src-tauri/tauri.conf.json src-tauri/Cargo.lock; exit 1; }
log=src-tauri/target/release-build.log
if ! npx tauri build --runner cargo-xwin --target x86_64-pc-windows-msvc --no-bundle >"$log" 2>&1; then
  grep -E "^error" "$log" | head -20 || tail -30 "$log"
  git checkout -- src-tauri/Cargo.toml src-tauri/tauri.conf.json src-tauri/Cargo.lock
  exit 1
fi
grep "Built application" "$log" || true
exe=src-tauri/target/x86_64-pc-windows-msvc/release/clipper.exe
git add -A
git commit -qm "Release $ver" || true
git branch -f test HEAD
git push -q origin main test
gh release create "v$ver" "$exe" --target test $flag --title "Clipper $ver" --notes "$notes"

#!/usr/bin/env bash
# Build the Windows exe and publish it as a (pre-)release on the test branch.
# usage: scripts/release.sh 0.1.7 "What changed" [--stable]
# The in-app updater offers any release tagged vX.Y.Z newer than the running build
# (pre-releases only when "Include test builds" is on).
set -euo pipefail
cd "$(dirname "$0")/.."
ver=${1:?version}; notes=${2:?notes}; flag=--prerelease
[[ ${3:-} == --stable ]] && flag=--latest
sed -i "0,/^version = \".*\"/s//version = \"$ver\"/" src-tauri/Cargo.toml
sed -i "s/\"version\": \"[^\"]*\"/\"version\": \"$ver\"/" src-tauri/tauri.conf.json
npx tauri build --runner cargo-xwin --target x86_64-pc-windows-msvc --no-bundle 2>&1 | grep -E "^error|Built application"
exe=src-tauri/target/x86_64-pc-windows-msvc/release/clipper.exe
git add -A
git commit -qm "Release $ver" || true
git branch -f test HEAD
git push -q origin main test
gh release create "v$ver" "$exe" --target test $flag --title "Clipper $ver" --notes "$notes"

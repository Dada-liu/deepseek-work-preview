#!/usr/bin/env bash
# Build DeepSeek Work and publish the dmg as a GitHub Release.
#
# Prerequisites: gh CLI authenticated (`gh auth login`), cargo env available.
# Usage: bash scripts/release.sh [patch|minor|major]   (default: publish current version)
set -euo pipefail

ROOT="$(cd "$(dirname "$0")/.." && pwd)"
cd "$ROOT"

# cargo is typically not on PATH in fresh shells.
[ -f "$HOME/.cargo/env" ] && . "$HOME/.cargo/env"

# Optional version bump.
if [ $# -ge 1 ]; then
  case "$1" in
    patch|minor|major) ;;
    *) echo "usage: bash scripts/release.sh [patch|minor|major]" >&2; exit 1 ;;
  esac
  NEW="$(node -e "
    const v = require('./src-tauri/tauri.conf.json').version.split('.').map(Number);
    const part = '$1';
    if (part === 'patch') v[2]++;
    else if (part === 'minor') { v[1]++; v[2] = 0; }
    else { v[0]++; v[1] = 0; v[2] = 0; }
    console.log(v.join('.'));
  ")"
  node -e "
    const fs = require('fs');
    for (const f of ['src-tauri/tauri.conf.json', 'src-tauri/Cargo.toml']) {
      const s = fs.readFileSync(f, 'utf8');
      const out = s.replace(/version =? \"[0-9.]+\"/, (m) => m.replace(/[0-9.]+/, '$NEW'));
      fs.writeFileSync(f, out);
    }
  "
  # Cargo.toml also carries the version in Cargo.lock; refresh it.
  (cd src-tauri && cargo check -q)
  echo "==> Version bumped to $NEW"
fi

VERSION="$(node -p "require('./src-tauri/tauri.conf.json').version")"
TAG="v$VERSION"
DMG="src-tauri/target/release/bundle/dmg/DeepSeek Work_${VERSION}_aarch64.dmg"

echo "==> Preparing embedded runtime"
bash scripts/prepare-runtime.sh

echo "==> Building (pnpm tauri build)"
pnpm tauri build

echo "==> Copying artifacts to dist-desktop/"
rm -rf dist-desktop
mkdir -p dist-desktop
cp -R "src-tauri/target/release/bundle/macos/DeepSeek Work.app" dist-desktop/
cp "$DMG" dist-desktop/

if gh release view "$TAG" >/dev/null 2>&1; then
  echo "==> Release $TAG already exists, uploading with --clobber"
  gh release upload "$TAG" "$DMG" --clobber
else
  echo "==> Creating release $TAG"
  gh release create "$TAG" "$DMG" \
    --title "DeepSeek Work $TAG" \
    --generate-notes
fi

echo "==> Done: $(gh release view "$TAG" --json url -q .url)"

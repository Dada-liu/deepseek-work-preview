#!/usr/bin/env bash
# Prepare a self-contained runtime (Node.js + DSH dependency tree) under
# src-tauri/runtime/, which gets bundled into the .app via tauri.conf.json
# bundle.resources. Run this before `pnpm tauri build`.
set -euo pipefail

ROOT="$(cd "$(dirname "$0")/.." && pwd)"
RUNTIME="$ROOT/src-tauri/runtime"
DSH_SRC="$ROOT/node_modules/@deepseek-ai/dsh"
# Preinstalled plugin: the dsh-market plugin market (npm package "dshmarket").
# Bundled into the runtime and seeded into the web profile on first launch
# (see seed_dsh_market in src-tauri/src/lib.rs).
DSH_MARKET_VERSION="1.18.0"

echo "==> Cleaning $RUNTIME"
rm -rf "$RUNTIME"
mkdir -p "$RUNTIME/node_modules/@deepseek-ai"

# 1. Standalone Node.js binary (official builds are single self-contained
#    files). On Windows (Git Bash / MSYS on CI runners) it is node.exe.
#    dsh imports node:zlib zstd helpers and node:module type stripping, both
#    missing from older Node (e.g. 22.12) — feature-detect instead of trusting
#    whatever `node` is first on PATH, falling back to nvm-installed versions.
case "$(uname -s)" in
  MINGW*|MSYS*|CYGWIN*) NODE_BIN="node.exe" ;;
  *) NODE_BIN="node" ;;
esac
node_ok() {
  [ -n "$1" ] && "$1" -e "if (typeof require('node:zlib').createZstdDecompress !== 'function' || typeof require('node:module').stripTypeScriptTypes !== 'function') process.exit(1)" >/dev/null 2>&1
}
NODE_SRC="$(command -v "$NODE_BIN" || command -v node || true)"
if ! node_ok "$NODE_SRC"; then
  NODE_SRC=""
  for candidate in $(ls -d "$HOME"/.nvm/versions/node/*/bin/"$NODE_BIN" 2>/dev/null | sort -Vr); do
    if node_ok "$candidate"; then NODE_SRC="$candidate"; break; fi
  done
fi
if [ -z "$NODE_SRC" ]; then
  echo "error: no suitable Node.js found — dsh needs the node:zlib zstd and node:module type-stripping APIs (Node 22.18+/24); install one or put it on PATH" >&2
  exit 1
fi
echo "==> Copying Node.js from $NODE_SRC"
cp -L "$NODE_SRC" "$RUNTIME/$NODE_BIN"
chmod +x "$RUNTIME/$NODE_BIN" 2>/dev/null || true
"$RUNTIME/$NODE_BIN" --version

# 2. Standalone install of DSH in a staging dir. pnpm's default layout is
#    symlink-based and cannot simply be copied, so we do a fresh hoisted
#    install (flat node_modules, real files) and copy the result.
# Read the version with a relative require: on Windows (Git Bash on CI) the
# POSIX path in $DSH_SRC would not be translated for native node.exe.
DSH_VERSION="$(cd "$ROOT" && node -p "require('./node_modules/@deepseek-ai/dsh/package.json').version")" 2>/dev/null || {
  echo "error: $DSH_SRC missing, run pnpm install first" >&2
  exit 1
}
echo "==> Installing @deepseek-ai/dsh@$DSH_VERSION + dshmarket@$DSH_MARKET_VERSION into staging dir (hoisted)"
STAGE="$(mktemp -d)"
trap 'rm -rf "$STAGE"' EXIT
# The staging install runs without a lockfile, so ^-ranged @deepseek-ai/*
# sub-packages float to newer rc builds than the tested set in the project's
# pnpm-lock.yaml (e.g. rc.8 needs Node APIs our bundled Node lacks). Pin
# every @deepseek-ai/* package to its locked version via pnpm overrides.
# Overrides go into the staging pnpm-workspace.yaml: pnpm 11 no longer reads
# the package.json "pnpm" field. node.exe on Windows (Git Bash on CI) cannot
# open POSIX paths, so read the lockfile relative to $ROOT and hand node a
# native staging path.
STAGE_NATIVE="$STAGE"
case "$(uname -s)" in
  MINGW*|MSYS*|CYGWIN*) STAGE_NATIVE="$(cygpath -w "$STAGE")" ;;
esac
cat > "$STAGE/package.json" <<EOF
{"name":"dsh-runtime-stage","private":true,"dependencies":{"@deepseek-ai/dsh":"$DSH_VERSION","dshmarket":"$DSH_MARKET_VERSION"}}
EOF
# Reuse the project's build-approval / release-age settings.
cp "$ROOT/pnpm-workspace.yaml" "$STAGE/pnpm-workspace.yaml"
(cd "$ROOT" && STAGE_NATIVE="$STAGE_NATIVE" node -e "
  const fs = require('fs');
  const lock = fs.readFileSync('pnpm-lock.yaml', 'utf8');
  const seen = new Map();
  for (const m of lock.matchAll(/'(@deepseek-ai\/[^'@]+)@([0-9][^'(]*)'/g))
    seen.set(m[1], m[2]);
  let out = '\noverrides:\n';
  for (const [k, v] of seen) out += '  ' + JSON.stringify(k) + ': ' + JSON.stringify(v) + '\n';
  fs.appendFileSync(process.env.STAGE_NATIVE + '/pnpm-workspace.yaml', out);
")
pnpm -C "$STAGE" install --prod --node-linker=hoisted --no-lockfile

echo "==> Copying runtime tree (dereferencing any remaining symlinks)"
cp -RL "$STAGE/node_modules/." "$RUNTIME/node_modules/"

# dshmarket is not a dependency of the published dsh package, so dsh's
# boot-time module fallback ($DSH_HOME/profiles/node_modules symlink farm,
# a BFS over dsh's dependency closure) would skip it and the cordis loader
# could not resolve the plugin module from the profile directory. Register
# it as a runtime dependency of the bundled dsh copy so the farm links it.
echo "==> Registering dshmarket in the bundled dsh dependency closure"
# Same Windows caveat as above: run node from $ROOT with a relative path,
# native node.exe cannot open the POSIX $RUNTIME path under Git Bash.
(cd "$ROOT" && node -e "
  const fs = require('fs');
  const f = 'src-tauri/runtime/node_modules/@deepseek-ai/dsh/package.json';
  const pkg = JSON.parse(fs.readFileSync(f, 'utf8'));
  pkg.dependencies = { ...pkg.dependencies, dshmarket: '$DSH_MARKET_VERSION' };
  fs.writeFileSync(f, JSON.stringify(pkg, null, 2) + '\n');
")
[ -f "$RUNTIME/node_modules/dshmarket/cordis.patch.yml" ] || {
  echo "error: dshmarket bundle patch missing from the runtime tree" >&2
  exit 1
}

# The marker doubles as a cache-invalidation key in the desktop app
# (ensure_runtime in src-tauri/src/lib.rs re-extracts when it changes), so it
# must cover everything baked into the runtime — including the preinstalled
# plugin set, not just the dsh version.
echo "$DSH_VERSION+dshmarket-$DSH_MARKET_VERSION" > "$RUNTIME/dsh-version"
echo "==> DSH version: $DSH_VERSION"
du -sh "$RUNTIME"

# 3. Smoke test: the copied runtime must boot standalone, with the web
#    profile seeded to load the preinstalled dshmarket bundle (same shape
#    that seed_dsh_market in src-tauri/src/lib.rs writes on first launch).
echo "==> Smoke-testing runtime"
LOG="$(mktemp)"
SMOKE_HOME="$(mktemp -d)"
mkdir -p "$SMOKE_HOME/profiles/web"
cat > "$SMOKE_HOME/profiles/web/package.json" <<EOF
{"name":"dsh-profile-web","private":true,"dependencies":{},"dsh":{"profile":{"bundles":["@deepseek-ai/dsh-base","@deepseek-ai/dsh-web-app","dshmarket"]}}}
EOF
DSH_HOME="$SMOKE_HOME" "$RUNTIME/$NODE_BIN" "$RUNTIME/node_modules/@deepseek-ai/dsh/lib/bin.js" web --port 0 > "$LOG" 2>&1 &
PID=$!
sleep 12
URL="$(grep -m1 -o 'http://[^ ]*' <<< "$(grep -m1 'dsh web: http' "$LOG" || true)" || true)"
# The preinstalled market plugin must actually have come up — probe its
# status endpoint while the server is still alive.
STATUS=""
if [ -n "$URL" ]; then
  echo "==> OK: $(grep -m1 'dsh web: http' "$LOG")"
  STATUS="$(curl -fs --max-time 5 "$URL/dsh-market/status" 2>/dev/null || true)"
fi
kill -9 "$PID" 2>/dev/null || true
if [ -z "$URL" ]; then
  echo "error: runtime smoke test failed, output was:" >&2
  cat "$LOG" >&2
  rm -f "$LOG"
  rm -rf "$SMOKE_HOME"
  exit 1
fi
if [ -n "$STATUS" ]; then
  echo "==> OK: dsh-market responded at /dsh-market/status"
else
  echo "error: dshmarket did not respond at $URL/dsh-market/status; boot log was:" >&2
  cat "$LOG" >&2
  rm -f "$LOG"
  rm -rf "$SMOKE_HOME"
  exit 1
fi
rm -f "$LOG"
rm -rf "$SMOKE_HOME"
echo "==> Runtime prepared at $RUNTIME"

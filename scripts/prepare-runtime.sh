#!/usr/bin/env bash
# Prepare a self-contained runtime (Node.js + DSH dependency tree) under
# src-tauri/runtime/, which gets bundled into the .app via tauri.conf.json
# bundle.resources. Run this before `pnpm tauri build`.
set -euo pipefail

ROOT="$(cd "$(dirname "$0")/.." && pwd)"
RUNTIME="$ROOT/src-tauri/runtime"
DSH_SRC="$ROOT/node_modules/@deepseek-ai/dsh"

echo "==> Cleaning $RUNTIME"
rm -rf "$RUNTIME"
mkdir -p "$RUNTIME/node_modules/@deepseek-ai"

# 1. Standalone Node.js binary (official builds are single self-contained
#    files). On Windows (Git Bash / MSYS on CI runners) it is node.exe.
case "$(uname -s)" in
  MINGW*|MSYS*|CYGWIN*) NODE_BIN="node.exe" ;;
  *) NODE_BIN="node" ;;
esac
NODE_SRC="$(command -v "$NODE_BIN" || command -v node || true)"
if [ -z "$NODE_SRC" ]; then
  echo "error: node not found in PATH" >&2
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
echo "==> Installing @deepseek-ai/dsh@$DSH_VERSION into staging dir (hoisted)"
STAGE="$(mktemp -d)"
trap 'rm -rf "$STAGE"' EXIT
cat > "$STAGE/package.json" <<EOF
{"name":"dsh-runtime-stage","private":true,"dependencies":{"@deepseek-ai/dsh":"$DSH_VERSION"}}
EOF
# Reuse the project's build-approval / release-age settings.
cp "$ROOT/pnpm-workspace.yaml" "$STAGE/pnpm-workspace.yaml"
pnpm -C "$STAGE" install --prod --node-linker=hoisted --no-lockfile

echo "==> Copying runtime tree (dereferencing any remaining symlinks)"
cp -RL "$STAGE/node_modules/." "$RUNTIME/node_modules/"

echo "$DSH_VERSION" > "$RUNTIME/dsh-version"
echo "==> DSH version: $DSH_VERSION"
du -sh "$RUNTIME"

# 3. Smoke test: the copied runtime must boot standalone.
echo "==> Smoke-testing runtime"
LOG="$(mktemp)"
"$RUNTIME/$NODE_BIN" "$RUNTIME/node_modules/@deepseek-ai/dsh/lib/bin.js" web --port 0 > "$LOG" 2>&1 &
PID=$!
sleep 12
kill -9 "$PID" 2>/dev/null || true
if grep -q "dsh web: http" "$LOG"; then
  echo "==> OK: $(grep -m1 'dsh web: http' "$LOG")"
else
  echo "error: runtime smoke test failed, output was:" >&2
  cat "$LOG" >&2
  rm -f "$LOG"
  exit 1
fi
rm -f "$LOG"
echo "==> Runtime prepared at $RUNTIME"

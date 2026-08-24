**English** · [中文](./README.md)

---

<div align="center">

# DeepSeek Work
<img src="./public/favicon.svg" style="width: 40px; height: 40px;"/>

A DeepSeek desktop Agent built on **Tauri v2**.

The desktop app acts as a **desktop shell for the official DSH Web UI**: on startup it launches a local `dsh web` subprocess and, once ready, navigates the Tauri window to the official web interface.

The installer is **fully self-contained**: it bundles the Node.js runtime and the complete `@deepseek-ai/dsh` dependency tree, so users need no pre-installed Node.js / pnpm or any project dependencies.

Adds a plugin market; see the plugin catalog at https://github.com/hotpot-labs/awesome-dsh-industry-plugins;

Project site and downloads: https://www.hotpotliuyu.com/ds-work/

<img src="./public/ds_work_website.png" style="width: 80%;"/>

</div>


## Tech Stack

- Tauri v2
- React 18 (startup loading page only)
- Vite 7
- TypeScript 5.8
- `@deepseek-ai/dsh` `0.1.0-rc.6` (bundled)

## Development

```bash
# Install dependencies
pnpm install

# Prepare the bundled runtime (generates src-tauri/runtime/; run on first setup or when the dsh version changes)
bash scripts/prepare-runtime.sh

# Development mode (hot reload + Tauri window; debug builds use src-tauri/runtime/ directly)
pnpm tauri dev

# Frontend dev server only
pnpm dev
```

## Building the Installer

```bash
# First prepare the bundled runtime (~450 MB, packed into the installer)
bash scripts/prepare-runtime.sh

pnpm tauri build
```

`prepare-runtime.sh` installs `@deepseek-ai/dsh` fresh in a temp directory using pnpm hoisted mode (producing a flat, symlink-free dependency tree), copies it together with the single-file system Node.js binary into `src-tauri/runtime/`, and smoke-tests that `dsh web` can start on its own.

Build artifacts:

- `src-tauri/target/release/bundle/macos/DeepSeek Work.app` (~458 MB)
- `src-tauri/target/release/bundle/dmg/DeepSeek Work_0.1.0_aarch64.dmg` (~99 MB)

## Releasing (GitHub Releases)

The installer is large (dmg ~99 MB), so it is not committed and is distributed exclusively via GitHub Releases. Two options:

**CI release (recommended)**: `.github/workflows/release.yml` builds macOS (`macos-latest`, producing a dmg) and Windows (`windows-latest`, producing an NSIS installer exe) in parallel when a tag is pushed, and uploads both to the same Release:

```bash
git tag v0.1.0 && git push origin v0.1.0
```

**Local release**: `scripts/release.sh` performs the same flow locally and uses `gh` to create the Release (requires `brew install gh && gh auth login` first):

```bash
bash scripts/release.sh          # publish at the current version
bash scripts/release.sh patch    # bump the patch version (syncs tauri.conf.json and Cargo.toml) then publish
```

Both options also copy the artifacts to `dist-desktop/` (gitignored).

## Features

- **Fully self-contained**: bundles Node.js v24 and all DSH dependencies, no environment pre-installation required
- **Automatic first-launch extraction**: copies the runtime to `~/Library/Application Support/com.deepseek-harness.desktop/runtime` (APFS clone completes in seconds; re-extracts automatically on DSH version upgrades)
- Startup loading page is a spinner + status text ("Preparing runtime…" → "Starting DSH service…"); on error it shows the message and a "Retry" button
- Automatically launches the `dsh web` subprocess once the runtime is ready (`--port 0` auto-assigns the port)
- Parses the `dsh web: http://127.0.0.1:<port>` ready signal from stdout
- The window automatically navigates to the official DSH Web UI
- **Error logging**: startup/runtime errors (runtime extraction failure, DSH launch failure, DSH subprocess stderr output, abnormal DSH exit) are appended to `~/deepseek-work-preview/deepseek-work.log` (directory auto-created, with timestamps)
- Tray resident: the app keeps running when the window is closed; click the tray icon to restore
- Tray menu: "Show window / Restart DSH / Quit"
- IPC commands: `get_dsh_status`, `start_dsh_service`, `stop_dsh_service`, `restart_dsh_service`
- **Plugin market allowlist filtering**: the market catalog is filtered by a remote `plugins.json` allowlist (audit index); only plugins with `verdict` `whitelist` are shown, and while the allowlist is unreachable (offline / malformed JSON) the market shows nothing (fail-closed)

## Application Logic

### Startup Flow

1. Tauri `setup` stage: builds the tray menu (show window / restart DSH / quit), registers the "hide on close" behavior, and runs the startup sequence in an async task; the frontend `App.tsx` also triggers the same flow via `invoke('start_dsh_service')` — both paths are idempotent and the lock in `AppState` guarantees it runs only once
2. `ensure_runtime`: checks whether the `runtime/dsh-version` marker in the app data dir matches the bundled version; on mismatch (first launch or upgrade) copies `.app/Contents/Resources/runtime/` to the writable app data dir (preferring APFS `cp -Rc` clone, falling back to a recursive copy), then restores the node executable bit and strips the quarantine attribute
3. `spawn_dsh_web`: uses the extracted node to launch `<runtime>/node_modules/@deepseek-ai/dsh/lib/bin.js web --port 0`; a background thread keeps reading stdout and, once the `dsh web: http://127.0.0.1:<port>` ready signal is parsed, `navigate()`s the Tauri window to the official Web UI and emits `dsh-ready` to the frontend
4. On `dsh-ready` the frontend also redirects via `window.location.href` and polls `get_dsh_status` every 500ms as a fallback — in dev mode vite's first compile is slow and the page may miss the backend event and navigation; the two self-healing paths guarantee it eventually reaches the Web UI

### Plugin Market Allowlist Filtering

1. **Preinstall the market plugin**: `seed_dsh_market` writes `dshmarket` into the web profile's `dsh.profile.bundles` before starting DSH (only on a fresh profile or an untouched default manifest)
2. **Inject the filter script**: `on_page_load` injects `market_filter_script` on every page load (`PageLoadEvent::Finished`); the script guards with `window.__dswMarketFilter` to run only once
3. **Intercept the catalog request**: the market UI loads its catalog via `fetch("/dsh-market/registry")`; the script wraps `window.fetch`, intercepts that request, filters `data.registry.plugins` down to allowed plugins, and returns a new `Response`
4. **Allowlist source**: the allowlist is not bundled; it is fetched once per page load from `plugins.json` in the GitHub repo `hotpot-labs/awesome-dsh-industry-plugins` (raw URL first, falling back to the jsDelivr mirror `cdn.jsdelivr.net` when it is unreachable, each source with a timeout). That file is an audit index — each `plugins[]` entry is keyed by `name` ("owner/repo") with a `verdict` of whitelist / greylist / blacklist / pending; only entries with `verdict === 'whitelist'` and a non-empty string `name` are added to the allowed set
5. **Matching rule**: market registry entries join on `owner + "/" + name` and are compared against the allowed set
6. **Fail-closed**: the allowed set starts empty and an empty set hides everything, so while the allowlist is unreachable (offline, JSON parse failure) the market shows nothing
7. **Presentation-layer only**: the market package and the DSH server (including its install endpoint) are untouched; the filter only affects the frontend display

### Loading Screen

- No progress bar: a spinner + one line of status text, driven by the backend `boot-status` string events ("First launch, preparing runtime…", "Starting DSH service…", etc.)
- When the page detects no Tauri runtime (e.g. opening the vite dev address in a normal browser), it shows a hint instead of a bare error
- On error it receives the `dsh-error` event, shows the message and a "Retry" button, which calls `restart_dsh_service` (re-runs `ensure_runtime` + launches the subprocess)

## Requirements

None. The installer is self-contained with Node.js and the DSH runtime:

- **macOS**: first launch extracts the runtime to `~/Library/Application Support/com.deepseek-harness.desktop/runtime` (~450 MB). The app is not Apple-notarized; the first open after installing the dmg from the browser may warn ""DeepSeek Work" is damaged and cannot be opened" — this is Gatekeeper's quarantine attribute; run `xattr -cr /Applications/DeepSeek\ Work.app` once in the terminal to open it normally
- **Windows**: NSIS installer (`DeepSeek Work_<version>_x64-setup.exe`); first launch extracts the runtime to `%APPDATA%/com.deepseek-harness.desktop/runtime`; the app is unsigned, so choose "Run anyway" on the SmartScreen prompt. If launch reports "Access is denied (os error 5)", it is usually antivirus blocking the bundled node.exe — add the install directory or `%APPDATA%/com.deepseek-harness.desktop` to the antivirus allowlist

```bash
open dist-desktop/DeepSeek\ Work.app
```

## Project Structure

```
.
├── src/                    React startup loading page
│   ├── App.tsx             spinner status page and DSH event listening
│   ├── App.css             loading page styles
│   └── main.tsx            entry
├── src-tauri/              Tauri / Rust backend
│   ├── src/lib.rs          runtime extraction, DSH subprocess management, tray, IPC
│   ├── runtime/            prepare-runtime.sh output (gitignored, packed into .app)
│   ├── tauri.conf.json     Tauri config (bundle.resources references runtime/)
│   └── Cargo.toml          Rust dependencies
├── scripts/
│   ├── prepare-runtime.sh  generates the self-contained runtime (Node + DSH dependency tree)
│   └── release.sh          local one-click build and GitHub Release publish
├── .github/workflows/
│   └── release.yml         tag-triggered CI build and Release publish
├── dist-desktop/           built installers
├── implement-plan.md       implementation spec reference document
└── README.md               this file
```

## License

This project is released under the [MIT](LICENSE) license.

This project is built on [DeepSeek Harness](https://github.com/deepseek-ai/deepseek-harness) (`@deepseek-ai/dsh`, MIT, Copyright (c) 2026 DeepSeek): the installer bundles its full runtime, the desktop window shows the official DSH Web UI, and the app icon reuses DSH's whale logo.

See [THIRD_PARTY_NOTICES.md](THIRD_PARTY_NOTICES.md) for third-party components and their licenses.

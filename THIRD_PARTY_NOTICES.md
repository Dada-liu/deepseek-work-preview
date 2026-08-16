# Third-Party Notices

DeepSeek Work is licensed under [MIT](LICENSE). It bundles and distributes the third-party software listed below. Each project remains under its own license; nothing in this file changes those terms.

## Bundled runtime (shipped inside the installer)

### DeepSeek Harness (`@deepseek-ai/dsh`)

The installer embeds the complete `@deepseek-ai/dsh` runtime (CLI, Web UI, and its full npm dependency tree), which is loaded as a local subprocess at runtime. The DeepSeek Work window navigates to the official DSH Web UI served by that subprocess. The application icon reuses the DeepSeek whale logo shipped with DSH.

- Upstream: [github.com/deepseek-ai/deepseek-harness](https://github.com/deepseek-ai/deepseek-harness)
- License: MIT — Copyright (c) 2026 DeepSeek
- DSH's own third-party disclosures (including its vendored Cordis framework libraries and runtime npm dependencies) are documented upstream in [THIRD_PARTY_NOTICES.md](https://github.com/deepseek-ai/deepseek-harness/blob/master/THIRD_PARTY_NOTICES.md).
- Every bundled package retains its own `LICENSE` file inside the distributed `node_modules` tree; the exact pinned dependency closure is recorded in this repository's [`pnpm-lock.yaml`](pnpm-lock.yaml).

### Node.js

The installer embeds an official Node.js binary (v24) used to run the DSH subprocess.

- Upstream: [github.com/nodejs/node](https://github.com/nodejs/node)
- License: [MIT](https://github.com/nodejs/node/blob/main/LICENSE) — Copyright Node.js contributors

## Direct dependencies of the desktop shell

| Package | License |
| --- | --- |
| [Tauri](https://github.com/tauri-apps/tauri) (`tauri`, `@tauri-apps/api`, `@tauri-apps/cli`) | MIT / Apache-2.0 |
| [React](https://github.com/facebook/react) / [react-dom](https://github.com/facebook/react) | MIT |
| [Vite](https://github.com/vitejs/vite) / [@vitejs/plugin-react](https://github.com/vitejs/vite-plugin-react) | MIT |
| [TypeScript](https://github.com/microsoft/TypeScript) | Apache-2.0 |
| [serde](https://github.com/serde-rs/serde) / [serde_json](https://github.com/serde-rs/json) | MIT / Apache-2.0 |
| [chrono](https://github.com/chronotope/chrono) | MIT / Apache-2.0 |

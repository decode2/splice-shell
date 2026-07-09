# Splice Shell Development

This document explains the minimum setup for local development.

## Quick path

1. Install Node.js 22 or newer.
2. Install Rust through `rustup`.
3. Install frontend dependencies with `npm install`.
4. Start the desktop app with `npm run tauri -- dev`.

## Required tools

| Tool | Required for | Notes |
|------|--------------|-------|
| Node.js | React, Vite, Tauri CLI | The current scaffold was verified with Node `v25.9.0` and npm `11.12.1`. |
| Rust | Tauri backend and Rust crates | Not installed in the current assistant environment at scaffold time. |
| WebView2 | Tauri runtime on Windows | Usually present on modern Windows systems. |

## Commands

```powershell
npm install
npm run dev
npm run tauri -- dev
npm run typecheck
npm test
```

Rust checks, once Rust is installed:

```powershell
cargo test --workspace
cargo clippy --workspace --all-targets -- -D warnings
cargo check --workspace
```

## Current status

Terminal hosting is implemented. Splice Shell launches a real shell through Windows ConPTY, renders output with xterm.js, sends keyboard input, and handles resize. Clipboard image extraction and adapter-routed paste are in place on top of that foundation.

See [`docs/mvp.md`](mvp.md) for the full list of what the MVP supports and what is intentionally out of scope.

## Releases

Versioning is automated with release-please and SemVer. See [`docs/releases.md`](releases.md) for the release flow, signing setup, and auto-updater behaviour.

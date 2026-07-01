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
cargo test
cargo check --workspace
```

## Current limitation

The scaffold intentionally does not implement ConPTY yet.

That is the next milestone because terminal hosting is the foundation. Image paste comes after we can launch, render, write to, and resize a real terminal process.

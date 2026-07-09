# Splice Shell

Splice Shell is a Windows-first terminal for developers who work inside interactive AI CLIs and need image paste to feel native.

## Why Splice Shell

Splice is shaped by two goals:

- **Stay lightweight.** The UI runs on [Tauri](https://tauri.app), using the operating system's built-in WebView (WebView2 on Windows) instead of bundling a full Chromium + Node runtime the way Electron-based terminals do, and the core is written in Rust. That keeps the binary small and the baseline memory footprint low — the terminal should stay out of your machine's way, not compete with it.
- **Make image paste native to AI-CLI work.** Splice is for developers who live inside interactive AI CLIs. Capture a screenshot (`Win+Shift+S`) and paste it straight into the terminal; Splice hands it to the active CLI adapter so the AI can work with the image — no saving a file and pasting its path by hand.

## Current status

This repository contains a working Windows MVP build.

The current MVP can:

1. Launch a live terminal session through Windows ConPTY.
2. Render terminal output with xterm.js.
3. Send keyboard input and resize events to the PTY.
4. Let users start Codex, Claude, or any other CLI from the shell itself.
5. Extract clipboard images, persist them to a controlled temp path, and route a file reference through the active CLI adapter.
6. Show the detected paste target and selected adapter before and after paste.

## Stack

| Layer | Technology |
|-------|------------|
| Native core | Rust |
| Terminal backend | Windows ConPTY |
| Desktop shell | Tauri |
| UI | TypeScript + React |
| Terminal renderer | xterm.js |

## Workspace layout

```txt
apps/desktop/              # Tauri + React desktop app
crates/splice-core/        # Shared domain types and adapter interfaces
crates/splice-pty/         # Terminal process hosting boundary
crates/splice-clipboard/   # Clipboard image pipeline boundary
docs/                      # Product and architecture docs
```

## Development

See [`docs/development.md`](docs/development.md).

## Build

```powershell
npm run tauri -- build
```

Windows installers are produced under:

```txt
target/release/bundle/
```

## Releases & auto-updates

Splice Shell uses [release-please](https://github.com/googleapis/release-please) and [SemVer](https://semver.org) to automate versioning. Merging a `feat:` or `fix:` commit to `master` opens a release PR automatically. Merging that PR builds and publishes the signed `.msi` installer to GitHub Releases. Installed copies check for updates silently on startup.

See [`docs/releases.md`](docs/releases.md) for the full release flow and signing setup.

## Resource safety

Splice Shell takes active measures to avoid freezing the host machine: all Tauri backend commands are async, PTY output is throttled at 16 ms, clipboard temp files are swept on startup/shutdown/session close, and PTY process trees are terminated cleanly using Windows Job objects with a process-tree-walk fallback.

See [`docs/resource-safety.md`](docs/resource-safety.md) for details and test coverage.

## License

Splice Shell is licensed under the [Apache License 2.0](LICENSE).

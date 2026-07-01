# Splice Shell

Splice Shell is a Windows-first terminal for developers who work inside interactive AI CLIs and need image paste to feel native.

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

## License

Splice Shell is licensed under the [Apache License 2.0](LICENSE).

# Contributing to Splice Shell

Thanks for your interest in Splice Shell. This document explains how to set up
the project locally, the checks your change is expected to pass, and how to
propose a change.

By participating in this project you agree to abide by our
[Code of Conduct](CODE_OF_CONDUCT.md).

## Scope first

Splice Shell is deliberately focused. Before opening a large change, read
[`docs/vision.md`](docs/vision.md) and [`docs/mvp.md`](docs/mvp.md) to check it
fits the product direction. The terminal must remain a real terminal first;
image paste is an enhancement layered on top, never a reason to compromise
process lifecycle, input, rendering, or resize behavior.

Small fixes (bugs, docs, tests) are always welcome without prior discussion.
For anything larger, open an issue first so we can agree on the approach before
you invest time.

## Prerequisites

| Tool | Required for | Notes |
|------|--------------|-------|
| Node.js `>=22` | React, Vite, Tauri CLI | The frontend workspace targets Node 22+. |
| Rust (via `rustup`) | Tauri backend and Rust crates | Install the stable toolchain. |
| WebView2 | Tauri runtime on Windows | Usually present on modern Windows systems. |

Splice Shell is Windows-first. The native core (`splice-pty`, `splice-clipboard`)
targets Windows, so the Rust crates are built and tested on Windows. The
frontend workspace is platform-agnostic.

## Setup

```powershell
npm install
```

This installs the frontend workspace dependencies. Rust dependencies are fetched
by Cargo on first build.

## Running locally

```powershell
npm run tauri -- dev
```

See [`docs/development.md`](docs/development.md) for the full development guide.

## Checks before you open a pull request

Your change should pass the same checks CI runs. Run them locally first.

Frontend:

```powershell
npm run typecheck
npm test
npm run build
```

Rust:

```powershell
cargo fmt --all -- --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
```

- `cargo fmt` keeps formatting consistent — run `cargo fmt --all` to fix.
- `cargo clippy` must be clean (warnings are treated as errors in CI).
- Add or update tests for behavior you change. Both crates and the frontend
  have test suites; keep them green.

## Commit and pull request guidelines

- Use clear, conventional commit messages (`fix:`, `feat:`, `docs:`,
  `chore:`, `test:`, `refactor:`).
- Keep pull requests focused. One logical change per PR is easier to review.
- Describe **what** changed and **why**. Link the issue it addresses when there
  is one.
- Make sure CI is green before requesting review.

## Architecture boundaries

Splice Shell is organized around explicit boundaries — see
[`docs/architecture.md`](docs/architecture.md):

- `crates/splice-pty` owns the Windows ConPTY lifecycle and I/O bridge.
- `crates/splice-clipboard` owns clipboard image detection and persistence.
- `crates/splice-core` owns shared domain types and the adapter interfaces.
- `apps/desktop` is the Tauri + React shell.

AI CLI behavior lives behind adapters and must not leak into the terminal host.
When adding an adapter, keep it small, isolated, and testable, and prefer a
conservative fallback over pretending a CLI supports richer input than it does.

## License

By contributing, you agree that your contributions will be licensed under the
[Apache License 2.0](LICENSE), the same license that covers the project.

# Splice Shell Architecture

Splice Shell is a Windows-first desktop terminal built around Rust, Windows ConPTY, Tauri, React, and xterm.js.

## Quick path

1. Prove terminal hosting with ConPTY.
2. Render and control the PTY through the desktop app.
3. Add clipboard image detection.
4. Route image paste through small AI CLI adapters.

## Stack decision

| Layer | Decision |
|-------|----------|
| Native core | Rust |
| Terminal backend | Windows ConPTY |
| Desktop shell | Tauri |
| UI | TypeScript + React |
| Terminal renderer | xterm.js as first candidate |
| Testing | `cargo test`, Vitest, later E2E harness |

## Architectural principle

The terminal must remain a real terminal first.

Image paste is an enhancement layered on top of terminal hosting, not a reason to compromise process lifecycle, keyboard input, output rendering, or resize behavior.

## Initial layers

```txt
apps/desktop
  Tauri + React UI
  terminal view
  paste event handling

crates/splice-core
  shared domain types
  command routing
  adapter interfaces

crates/splice-pty
  Windows ConPTY lifecycle
  process input/output bridge
  output flow control (credit-based backpressure)
  terminal resize handling

crates/splice-clipboard
  Windows clipboard image detection
  image extraction
  temporary asset persistence
```

## Runtime flow

```txt
User presses Ctrl+V
  -> UI intercepts paste intent
  -> clipboard layer checks for image data
  -> image is persisted to a controlled temporary location
  -> active CLI adapter formats a usable reference
  -> PTY layer writes the adapted input into the running process
```

## PTY output flow

Terminal output is the highest-volume path in the app, so it is flow-controlled
end to end. Without a bound, a high-output process (a build log, `yes`, a verbose
AI CLI) can outrun the render pipeline and grow memory without limit — worse when
the window is minimized, because WebView2 suspends `requestAnimationFrame` and
the accumulated output lands as one main-thread-freezing write on refocus.

```txt
ConPTY child
  -> reader thread reads output in chunks
  -> bounded channel (parks the reader when full)
  -> flusher coalesces and emits, but only while it has credit
  -> Tauri event -> xterm.write()
  -> frontend acks consumed bytes back to the backend
  -> credit is replenished, the flusher resumes
```

The flusher holds a per-session **credit window** (`crates/splice-pty/src/flow.rs`).
When credit is exhausted it stops draining the channel; the channel fills, the
reader parks, the ConPTY pipe fills, and the child finally blocks on write — real
backpressure, the same as a physical terminal. The frontend keeps draining and
acking even when hidden (a timer fallback covers a suspended `requestAnimationFrame`),
so a minimized window slows the child rather than buffering without bound. A
session that stays out of credit past a timeout is surfaced as a stalled state in
the UI rather than hanging silently. The credit window and the frontend ack
threshold are a cross-language contract; a compile-time assertion in `flow.rs`
keeps the window strictly above the mirrored threshold so the two cannot drift
into a permanent stall.

## Adapter boundary

AI CLI behavior must not leak into the terminal host.

Each adapter owns:

- detection rules for a supported CLI
- how an image reference should be represented
- whether the adapter can paste directly or needs a fallback

The initial adapter strategy is conservative: write a local file reference into the terminal input stream. Splice Shell should not pretend a CLI supports richer attachment semantics until that behavior is verified for that CLI.

The terminal core owns:

- PTY lifecycle
- clipboard extraction
- session state
- routing a paste event to the current adapter

## Key risks

| Risk | Why it matters | Mitigation |
|------|----------------|------------|
| ConPTY complexity | Terminal behavior is subtle: input, output, resize, encoding, and lifecycle all interact. | Prove a minimal PTY loop before product polish. |
| Adapter fragility | AI CLIs can change accepted input formats. | Keep adapters small, isolated, and testable. |
| Clipboard ambiguity | Clipboard data can include text, files, bitmap data, HTML, or multiple formats. | Define deterministic priority rules. |

## First technical milestone

Splice Shell has proven:

- [x] It can launch a real shell through ConPTY.
- [x] It can render output.
- [x] It can send keyboard input.
- [x] It can resize the terminal.
- [x] It can extract clipboard images and persist them safely.
- [x] It can route image paste through adapter boundaries.

## Process model

Splice Shell starts a normal shell through ConPTY. Users launch AI CLIs from that shell:

- `codex`
- `claude`
- any other command available in the shell environment

The PTY backend owns process lifecycle and adapter routing. The frontend does not act as a CLI launcher.

# Splice Shell MVP

The MVP proves one thing: image paste into an interactive AI CLI can feel native on Windows.

## Quick path

1. Build a real terminal host.
2. Add clipboard image extraction.
3. Add one AI CLI adapter.
4. Package the smallest usable developer workflow.

## Scope

### 1. Desktop shell

- Tauri desktop app.
- React UI.
- Single terminal pane.
- Basic app window lifecycle.

### 2. Terminal host

- Launch a configured shell or command.
- Use Windows ConPTY.
- Render terminal output.
- Send keyboard input.
- Support basic resize.

### 3. Clipboard image pipeline

- Detect `Ctrl+V`.
- Check whether the clipboard contains image data.
- Extract the image.
- Save it to a controlled temporary location.
- Return a local reference for adapter use.

### 4. First AI CLI adapter

- Detect one supported AI CLI.
- Convert the pasted image into the best supported prompt input.
- Fall back safely when the active process is unsupported.

Initial adapters use a conservative file-reference strategy:

```txt
Image file: "<local path>"
```

This is intentionally modest. The adapter boundary lets Splice Shell improve each CLI integration later without leaking CLI-specific behavior into the terminal host.

### 5. Documentation

- Explain what Splice Shell is.
- Explain what the MVP supports.
- Explain what it intentionally does not support yet.
- Provide local development instructions.

## Acceptance criteria

The MVP is successful when a user can:

- [x] Open Splice Shell on Windows.
- [x] Start an interactive AI CLI from the shell when it is available on `PATH`.
- [x] Copy an image to the clipboard.
- [x] Press `Ctrl+V` inside Splice Shell.
- [x] See the image reference inserted or otherwise made available to the AI CLI.

## Current MVP behavior

Splice Shell starts a live ConPTY shell session and renders it through xterm.js. Users launch Codex, Claude, or another CLI from the shell itself. Clipboard image paste is routed through the detected active process tree, choosing the first supported adapter candidate before falling back conservatively.

The initial adapter output is intentionally simple:

```txt
Image file: "<local path>"
```

The UI displays the detected paste target and selected adapter so users can verify routing before relying on a paste.

## Out of scope

- Multiple panes.
- Tabs.
- Cross-platform support.
- Plugin marketplace.
- Cloud features.
- Built-in model provider integration.
- Full terminal customization system.

## Build order

1. Project scaffold.
2. ConPTY proof of life.
3. Terminal rendering.
4. Keyboard input.
5. Resize.
6. Clipboard image detection.
7. Temporary image persistence.
8. Adapter interface.
9. First AI CLI adapter.
10. MVP packaging and README.

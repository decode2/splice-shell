# Security Policy

## Reporting a vulnerability

If you believe you have found a security vulnerability in Splice Shell, please
report it privately. **Do not open a public issue for security problems.**

Preferred channel: use GitHub's private vulnerability reporting on this
repository — open the **Security** tab and choose **Report a vulnerability**.
This keeps the report confidential until a fix is available.

Please include:

- A description of the issue and its impact.
- Steps to reproduce, or a proof of concept.
- The affected version, commit, or build.

You can expect an initial acknowledgement within a reasonable time frame. We
will work with you to understand and resolve the issue before any public
disclosure.

## Scope

Splice Shell hosts an interactive terminal and routes clipboard images into AI
CLIs on Windows. Areas most relevant to security include:

- The ConPTY process host and process spawning (`crates/splice-pty`).
- Clipboard image extraction and temporary file handling
  (`crates/splice-clipboard`).
- The Tauri IPC boundary and any command that touches the local filesystem or
  launches external programs (`apps/desktop/src-tauri`).
- Handling of untrusted terminal output rendered in the UI
  (`apps/desktop/src`).

## Supported versions

Splice Shell is pre-1.0. Security fixes are applied to the `master` branch.
There is no long-term support commitment for older builds yet.

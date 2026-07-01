# Splice Shell Vision

Splice Shell is a Windows-first terminal for developers who work inside interactive AI CLIs and need image paste to feel native.

## Quick path

1. Open Splice Shell.
2. Run an interactive AI CLI.
3. Copy an image.
4. Press `Ctrl+V`.
5. The CLI receives a useful image reference without ceremony.

## Problem

Modern AI CLIs are increasingly multimodal, but terminal paste behavior is still text-first. Screenshots, UI mockups, diagrams, browser states, and visual errors are common developer inputs, yet getting them into an interactive CLI prompt is awkward.

Splice Shell exists to make that workflow boring.

## Product position

Splice Shell is not a Warp clone.

The MVP is a focused terminal host that solves one high-value workflow extremely well: clipboard image paste into AI CLI sessions.

## Target users

- Developers using interactive AI CLIs.
- Builders who debug from screenshots, visual diffs, diagrams, or UI captures.
- Windows users who want a normal-feeling terminal with better multimodal ergonomics.

## MVP outcome

A user can open Splice Shell, run an AI CLI, copy an image, press `Ctrl+V`, and have the image become available to the CLI prompt through an adapter-supported path.

## Non-goals

- Full Warp-style command block system.
- Cloud sync.
- Team collaboration.
- Built-in AI assistant.
- Cross-platform parity on day one.
- Replacing every feature from mature terminal emulators.

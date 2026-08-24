# Ovim GUI

This directory contains the SolidJS frontend embedded by the `ovim-gui` Tauri
binary. It is a projection of the real Ovim editor state: keyboard, paste,
pointer, picker, tab, and file-tree actions are sent back through the Rust
bridge instead of being reimplemented in JavaScript.

## Development

```sh
npm install
npm run check
npm run dev
```

The browser development view uses representative mock state. To exercise the
native bridge, build the checked-in production assets and launch through Ovim:

```sh
npm run build
cargo build -p ovim --bins
target/debug/ovim gui README.md
```

`dist/` is intentionally checked in because Cargo embeds it in the native
binary without requiring Node during a Rust build.

## Current boundary

The GUI renders the focused editor pane, tabs, file tree, diagnostics, Git
state, picker, completion, hover, prompts, and status information. Core Ovim
remains authoritative for modes, commands, selections, edits, and persistence.
For `.strok` buffers, a Vector companion tab asks the native bridge to render
the in-memory source with the installed Strøk CLI; its review form drafts
file-specific feedback in the authoritative core AI chat.
Terminal-only surfaces and exact soft-wrap/multi-split layout parity remain
follow-up work; they do not maintain a second editor implementation in the DOM.

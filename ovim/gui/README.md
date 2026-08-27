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

Browser tabs are also frontend-specific. `workbench.ts` composes stable source,
Vector, and Browser item identities into one selection model. Their shared
Solid toolbar reserves a measured viewport for the selected native Tauri child
webview; the Rust `BrowserHost` owns the bounded session collection,
active-child visibility, navigation policy, page generations, and DOM
evaluation. UI and AI requests go through that one host, so they cannot race
independent browser implementations. A new manual Browser tab is an unloaded
logical session: no child webview exists until its first navigation. Closing
the tab destroys both. The web development view cannot create browser tabs
because it has no native child-webview primitive.

Colon commands use the reusable `SurfaceCommandLine` with a browser-specific
grammar (`:goto`, `:back`, `:forward`, `:reload`, `:stop`, `:q`, and workbench
tab navigation). The address field and `:goto` accept either a network address
or a DuckDuckGo search. Browser shortcuts, page key requests, and colon
commands all reach the same typed `browserWorkbench` controller; the toolbar
does not own a parallel mutation path. A tokenized initialization script in the
remote child webview can send only a small set of typed intents after trusted
keypresses. It cannot invoke Tauri commands or control Ovim directly.

The injected key bridge is split by concern under `browser/key_bridge/` and is
initialized in every frame. It synchronizes only the per-tab enabled and
Normal/Insert state across frames; the per-webview command token never enters
that message channel. Focusing an editable field enters Insert mode
automatically, `i` enters it explicitly, and `Esc` blurs an editable and returns
to Normal mode. The toolbar toggle disables unmodified Vim-style keys for that
tab without disabling native app shortcuts such as `Cmd/Ctrl+W`, `Cmd/Ctrl+T`,
or `Cmd/Ctrl+L`.

The AI browser tools are advertised only when this live host is attached and
the active chat profile sets `scope_network = true`; the built-in `codex_sol`
chat profile enables it by default. See
[`user-docs/ai.md`](../../user-docs/ai.md#shared-embedded-browser-ovim-gui) for
the control and security boundary.

Terminal-only surfaces and exact soft-wrap/multi-split layout parity remain
follow-up work; they do not maintain a second editor implementation in the DOM.

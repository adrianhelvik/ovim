# Frontend API

`ovim::frontend` (`ovim/src/frontend/`) is the frontend-agnostic runtime
plumbing shared by every frontend that embeds the editor core: the TUI, the
headless loop, and the Tauri GUI. `ovim-core` already has zero
ratatui/crossterm dependencies — it defines its own `KeyEvent`/`Color` types
and renders highlights as spans rather than styled terminal cells, so it has
no UI framework to be coupled to in the first place. `ovim::frontend` is the
analogous boundary one layer up, inside the `ovim` lib target: it is
everything a non-terminal frontend can call directly to drive the editor
(tick, refresh, viewport geometry, background picker/file loading), as
opposed to the terminal-specific code (crossterm event handling, shell
suspend/resume, the two event loops themselves) that stays in the binary's
`event_loop.rs`.

## The frontend contract

A frontend embedding the editor core must:

1. Call `handle_viewport_resize` whenever the grid geometry changes
   (terminal resize, window resize, split/pane changes). It relies on the
   public helper `compute_text_width` (wrap width = content width minus
   gutter), kept in sync by hand with
   `ovim::ui::renderer::layout::BufferLayout::compute`, the authority it
   mirrors for gutter/layout sizing.
2. Build a `FrontendChannels` once per `Editor` and run `process_editor_tick`
   on a periodic interval to drive LSP, DAP, syntax highlighting, and other
   background work.
3. Drain background picker results with `process_picker_results` on the same
   cadence as the tick — `process_editor_tick` deliberately does not *drain*
   the preview/file receivers even though it holds them via
   `FrontendChannels`, so a frontend that opens the picker must call this
   itself (see `event_loop.rs`'s TUI loop; the headless loop instead
   receives on `FrontendChannels::preview_rx`/`file_rx` directly for lower
   latency).
4. Call `refresh_after_input` after dispatching input to the editor, then
   call `editor.dispatch_pending_intents().await` right after — otherwise
   LSP-triggered work waits for the next tick.
5. Run the debounced rehighlight (`editor.process_pending_rehighlight()`)
   roughly 200ms after the last edit.
6. Call `process_external_file_change` periodically (roughly every 500ms) so
   externally-modified files are detected and reloaded.
7. On shutdown, call `editor.close_current_file_lsp().await` so the language
   server sees a clean `didClose` instead of a dropped socket.

This list is kept in sync by hand with the doc comment on `ovim/src/frontend/mod.rs`
— treat that module as the source of truth if the two ever drift.

## Input path

A frontend never hands its native input events to the editor directly. It
converts them to `ovim_core`'s own `KeyEvent`/`Modifiers` types at the
boundary, then feeds those into `InputHandler`. The TUI's version of this
conversion lives in `ovim/src/key_convert.rs` — `convert_key_event` maps
crossterm's `KeyEvent` to `ovim_core::key::KeyEvent`, and
`convert_key_modifiers` maps crossterm's modifier bitflags to
`ovim_core::key::Modifiers`. A GUI frontend needs the equivalent conversion
for its own input events, but from that point on the path is the same for
every frontend:

```
frontend key event
  -> ovim_core::key::KeyEvent            (frontend's own conversion)
  -> InputHandler::handle_key_event_no_dirty(&mut editor, event)
  -> ovim::frontend::refresh_after_input(&mut editor)
```

## Colors

Syntax highlighting resolves to `ovim_core::color::Color`
(`ovim-core/src/color.rs`), a terminal-shaped color representation that
mirrors `ratatui::style::Color` without depending on ratatui. `HighlightGroup`
values (`ovim-core/src/syntax/theme.rs`) are mapped through a `Theme` to a
`Color`, which is one of:

- Sixteen ANSI-named variants (`Black`, `Red`, `LightGreen`, `Gray`, ...)
- `Rgb(u8, u8, u8)` — true color
- `Indexed(u8)` — an 8-bit terminal palette index
- `Reset`

A terminal frontend can hand `Rgb` straight to the display and already has a
16-color and 256-color palette to resolve the ANSI-named and `Indexed`
variants against. A GUI frontend has no such built-in palette: it must supply
its own mapping from the ANSI-named and `Indexed` variants to concrete RGB
before it can paint anything. The native reference frontend implements that
mapping in `ovim/src/gui/mod.rs` and serializes resolved CSS colors, keeping
terminal palette assumptions out of the SolidJS layer.

## Reference implementation

`ovim/tests/frontend_api.rs` is an integration test that simulates a minimal
third frontend using only `ovim::` lib API — no binary-target modules, which
are unreachable from an integration test crate regardless. It builds an
`Editor`, resizes the viewport, runs a tick with a fresh `FrontendChannels`,
dispatches keys through `InputHandler` followed by `refresh_after_input`, and
exercises the picker-drain no-op path. Read it alongside this document as the
concrete, compiling version of the contract above.

## What does not go through this path

The REST API (`ovim/src/api/`) is a separate, agent-oriented surface: JSON
over HTTP, poll-only (no push/streaming), meant for CLI tools and AI agents
that talk to a running headless session over the network. The GUI does not
talk to that API — `ovim/src/gui/mod.rs` owns the editor on a dedicated
runtime thread, drives it through `ovim::frontend` and `InputHandler`, and
projects bounded serializable snapshots into the Tauri webview. SSE/streaming
support for the REST API is a separate,
explicitly deferred concern (also listed in
`planning/gui-frontend-prep/PLAN.md`'s non-goals) and is unrelated to
whether a GUI can be built today.

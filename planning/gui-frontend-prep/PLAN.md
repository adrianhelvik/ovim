# GUI frontend prep: extract shared frontend plumbing (ovim-only work)

Status: done (tasks 1-4 committed)
Context: a future GUI frontend (Ligero-based) will link the editor core directly,
like the TUI does. Everything a frontend needs that is not terminal-specific
should live in the `ovim` lib target so the TUI, the headless loop, and a future
GUI share one copy. `ovim-core` already has zero ratatui/crossterm deps; the gap
is that the frontend *runtime plumbing* (tick, LSP init, refresh, viewport
geometry, picker/file background loading) is private to the binary target.

## Current state

Binary-target modules (`main.rs`: `mod api_dispatch; mod event_loop; mod lsp_init;`):

- `event_loop.rs` (~1900 lines) mixes three things:
  1. frontend-agnostic runtime plumbing: `process_editor_tick` + helpers
     (LSP notifications/init/sync, DAP actions, syntax highlight spawn/drain,
     background task polling, picker tick, install spawning, transient UI ticks),
     `refresh_after_input`, `refresh_after_api_mutation`,
     `process_external_file_change`, `handle_terminal_resize`,
     `compute_text_width`, picker preview/file-finder loading + the
     `FILE_LIST_CACHE_RESULTS` static, `process_picker_results`,
     `spawn_gradle_and_wait`, `load_preview_async`
  2. terminal-specific code: crossterm `EventStream` handling,
     `process_input_events`, `execute_shell_command` (suspend/resume),
     agent-attention BEL, `ensure_interaction_modes` reassertion
  3. the two event loops themselves (TUI + headless)
- `lsp_init/` (~2000 lines): imports only `ovim::` lib paths and std. Pure move.
- `api_dispatch.rs` imports `handle_terminal_resize`, `refresh_after_input`,
  `refresh_after_api_mutation` from `event_loop` (its only bin-internal dep
  besides being called by the loops).

Key advantage of moving into the existing lib target (not a new crate): no
Cargo.toml changes, no dependency graph changes, `crate::editor`/`crate::lsp`
etc. already re-exported in `lib.rs`.

## Target layout

```
ovim/src/lib.rs           + pub mod frontend; + pub mod lsp_init;
ovim/src/lsp_init/        (moved verbatim; internal `ovim::` imports -> `crate::`)
ovim/src/frontend/
├── mod.rs                (module docs: the contract a frontend must fulfil —
│                          resize on geometry change, tick at some cadence,
│                          refresh_after_input after key dispatch, debounced
│                          rehighlight, periodic external-file check)
├── tick.rs               process_editor_tick + private helpers
├── refresh.rs            refresh_after_input, refresh_after_api_mutation,
│                          process_external_file_change (all pub)
├── viewport.rs           handle_viewport_resize (renamed from
│                          handle_terminal_resize — it deals in grid cells,
│                          not terminals), compute_text_width
└── loading.rs            picker preview/file-finder spawning,
                           process_picker_results, load_preview_async,
                           FILE_LIST_CACHE_RESULTS (private)
```

`event_loop.rs` keeps: the two loops, crossterm input handling,
`execute_shell_command`, attention bell, `api_session_info`.
`api_dispatch.rs` switches its three imports to `ovim::frontend`.

## Tasks

### Task 1 — move `lsp_init` into the lib target
- Declare `pub mod lsp_init;` in `lib.rs`, drop `mod lsp_init;` from `main.rs`.
- Inside `lsp_init/`, rewrite `use ovim::…` -> `use crate::…`.
- `event_loop.rs`: `crate::lsp_init::…` -> `ovim::lsp_init::…`.
- Gate: `cargo fmt && cargo clippy && cargo test`.

### Task 2 — create `ovim::frontend`, move the plumbing
- Pure code motion per the layout above; minimal API changes:
  - moved items that the bin or api_dispatch call become `pub`; helpers stay private
  - rename `handle_terminal_resize` -> `handle_viewport_resize` (update
    api_dispatch + tests; keep semantics identical)
- Move the unit tests that test moved code (e.g. `apply_java_status`,
  `compute_text_width`, resize/wrap tests, `tick_transient_ui` test) into the
  corresponding frontend modules. Tests that exercise api_dispatch or
  crossterm events stay in the bin.
- lib target already denies print_stdout/print_stderr — moved code must not
  regress that.
- Gate: `cargo fmt && cargo clippy && cargo test`.

### Task 3 — `FrontendChannels`: bundle the tick channels
- New pub struct in `frontend` owning what both loops currently create by hand:
  preview (tx+rx), file results (tx+rx), syntax results (tx+rx), plus the
  `java_status_rx` receiver. Constructor with the current capacities
  (100 / 1000 / 16 / caller-provided java rx).
- `process_editor_tick(editor, &mut FrontendChannels)` replaces the 6-parameter
  signature; `process_picker_results(editor, &mut FrontendChannels)` likewise.
- Both loops in `event_loop.rs` construct one `FrontendChannels` and pass it.
- Gate: `cargo fmt && cargo clippy && cargo test`.

### Task 4 — acceptance test + docs
- `ovim/tests/frontend_api.rs`: simulate a minimal third frontend using ONLY
  public lib API: create `Editor`, `handle_viewport_resize`, build
  `FrontendChannels`, run `process_editor_tick`, dispatch keys via
  `InputHandler`, `refresh_after_input`, assert on state. This is the proof
  that a GUI can be built without touching the binary.
- `code-docs/FRONTEND_API.md`: one page describing the frontend contract and
  pointing at the integration test as the reference implementation.
- Gate: `cargo fmt && cargo clippy && cargo test`.

## Non-goals (deferred until the GUI exists)
- Moving the event loops or `api_dispatch` out of the binary.
- SSE/streaming on the REST API (GUI links the lib; API stays agent-oriented).
- ANSI/indexed `Color` -> RGB palette resolution helper.
- Feature-gating ratatui out of the lib target for GUI consumers (compile-time
  cost only; revisit when a GUI crate exists).
- The `FILE_LIST_CACHE_RESULTS` global static is a known wart; moved as-is.

## Process
- One commit per task once its gate is green (conventional commit style,
  no attribution lines).
- Implementation: Sonnet agents, one task at a time (same files, sequential).
- Review: Opus (Rust reviewer persona) after task 2 and after tasks 3+4.
- If unrelated tests fail, another agent may be active in the repo — stop and
  report rather than stashing.

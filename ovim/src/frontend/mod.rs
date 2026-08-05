//! Frontend-agnostic runtime plumbing shared by every frontend that embeds
//! the editor core (the TUI, the headless loop, and eventually a GUI).
//!
//! `ovim-core` has no ratatui/crossterm dependencies; this module is the
//! analogous boundary inside the `ovim` lib target: everything here is safe
//! for a non-terminal frontend to call directly. Terminal-specific code
//! (crossterm event handling, shell suspend/resume, the two event loops
//! themselves) stays in the binary's `event_loop.rs`.
//!
//! ## The frontend contract
//!
//! A frontend embedding the editor core must:
//!
//! 1. Call [`handle_viewport_resize`] whenever the grid geometry changes
//!    (terminal resize, window resize, split/pane changes).
//! 2. Build a [`FrontendChannels`] once per `Editor` and run
//!    [`process_editor_tick`] on a periodic interval to drive LSP, DAP,
//!    syntax highlighting, and other background work.
//! 3. Drain background picker results with [`process_picker_results`] on the
//!    same cadence as the tick — `process_editor_tick` deliberately does not
//!    *drain* the preview/file receivers even though it holds them via
//!    `FrontendChannels`, so a frontend that opens the picker must call this
//!    itself (see `event_loop.rs`'s TUI loop; the headless loop instead
//!    receives on `FrontendChannels::preview_rx`/`file_rx` directly for
//!    lower latency).
//! 4. Call [`refresh_after_input`] after dispatching input to the editor,
//!    then call `editor.dispatch_pending_intents().await` right after —
//!    otherwise LSP-triggered work waits for the next tick.
//! 5. Run the debounced rehighlight (`editor.process_pending_rehighlight()`)
//!    roughly 200ms after the last edit.
//! 6. Call [`process_external_file_change`] periodically (roughly every
//!    500ms) so externally-modified files are detected and reloaded.
//! 7. On shutdown, call `editor.close_current_file_lsp().await` so the
//!    language server sees a clean `didClose` instead of a dropped socket.

mod channels;
mod loading;
mod refresh;
mod tick;
mod viewport;

pub use channels::FrontendChannels;
pub use loading::process_picker_results;
pub use refresh::{process_external_file_change, refresh_after_api_mutation, refresh_after_input};
pub use tick::process_editor_tick;
pub use viewport::{compute_text_width, handle_viewport_resize};

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
//! 2. Run [`process_editor_tick`] on a periodic interval to drive LSP,
//!    DAP, syntax highlighting, and other background work.
//! 3. Call [`refresh_after_input`] after dispatching input to the editor.
//! 4. Run the debounced rehighlight (`editor.process_pending_rehighlight()`)
//!    roughly 200ms after the last edit.
//! 5. Call [`process_external_file_change`] periodically (roughly every
//!    500ms) so externally-modified files are detected and reloaded.

mod loading;
mod refresh;
mod tick;
mod viewport;

pub use loading::{
    process_picker_results, spawn_file_finder_loading, spawn_picker_preview_loading,
};
pub use refresh::{process_external_file_change, refresh_after_api_mutation, refresh_after_input};
pub use tick::process_editor_tick;
pub use viewport::{compute_text_width, handle_viewport_resize};

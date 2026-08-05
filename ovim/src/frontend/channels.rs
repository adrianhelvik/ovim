use std::path::PathBuf;

use tokio::sync::mpsc;

use crate::buffer::{BufferId, LineHighlights};
use crate::editor;
use crate::syntax::Language;

/// Background-task channels that [`super::process_editor_tick`] and
/// [`super::process_picker_results`] read from and write to.
///
/// A frontend constructs exactly one `FrontendChannels` per `Editor` and
/// passes `&mut` into both functions on its tick cadence. Bundling these
/// channels keeps their capacities and wiring in one place instead of
/// duplicated by hand in every event loop.
///
/// `preview_rx` and `file_rx` are `pub` (rather than accessed only through
/// methods) so a frontend's `select!` loop can borrow them directly when it
/// needs lower latency than the tick cadence provides (see
/// `ovim/src/event_loop.rs::run_headless_loop`, which receives on
/// `preview_rx`/`file_rx` in dedicated branches instead of waiting for the
/// next tick). The remaining fields have no callers outside the `frontend`
/// module, so they are `pub(super)`.
pub struct FrontendChannels {
    pub(super) preview_tx: mpsc::Sender<(String, editor::PreviewCache)>,
    pub preview_rx: mpsc::Receiver<(String, editor::PreviewCache)>,
    pub(super) file_tx: mpsc::Sender<editor::PickerResult>,
    pub file_rx: mpsc::Receiver<editor::PickerResult>,
    pub(super) syntax_tx: mpsc::Sender<(BufferId, Language, Option<LineHighlights>, u64)>,
    pub(super) syntax_rx: mpsc::Receiver<(BufferId, Language, Option<LineHighlights>, u64)>,
    pub(super) file_list_cache_tx: mpsc::Sender<(PathBuf, PathBuf, Vec<editor::PickerResult>)>,
    pub(super) file_list_cache_rx: mpsc::Receiver<(PathBuf, PathBuf, Vec<editor::PickerResult>)>,
    pub(super) java_status_rx: mpsc::Receiver<String>,
}

impl FrontendChannels {
    /// Build the channel set with the capacities every frontend has used
    /// historically: 100 for preview loads, 1000 for file-finder results, 16
    /// for background syntax highlighting, and 4 for the file-list cache
    /// handoff (small because it only ever holds one pending batch).
    ///
    /// `java_status_rx` is caller-provided: the sender side is wired up via
    /// `ovim::lsp_init::init_java_status_sender` in `main.rs`.
    pub fn new(java_status_rx: mpsc::Receiver<String>) -> Self {
        let (preview_tx, preview_rx) = mpsc::channel(100);
        let (file_tx, file_rx) = mpsc::channel(1000);
        let (syntax_tx, syntax_rx) = mpsc::channel(16);
        let (file_list_cache_tx, file_list_cache_rx) = mpsc::channel(4);
        Self {
            preview_tx,
            preview_rx,
            file_tx,
            file_rx,
            syntax_tx,
            syntax_rx,
            file_list_cache_tx,
            file_list_cache_rx,
            java_status_rx,
        }
    }
}

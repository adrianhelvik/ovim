use super::lsp_slot::LspSlots;
use super::lsp_state::{LspIntents, LspState};
use super::lsp_ui::LspUi;
use super::PendingLspInstall;

/// Grouped state for all LSP-related concerns.
#[derive(Default)]
pub(crate) struct LspSubsystem {
    /// Core LSP state (manager, diagnostics, hover, etc.)
    pub(crate) state: LspState,
    /// Generic slots for in-flight LSP requests
    pub(crate) slots: LspSlots,
    /// Per-feature intent flags (replaces old single-slot LspAction dispatch)
    pub(crate) intents: LspIntents,
    /// LSP UI panel state (manager panel and install progress)
    pub(crate) ui: LspUi,
    /// Pending LSP auto-install awaiting user consent
    pub(crate) pending_install: Option<PendingLspInstall>,
    /// Approved LSP install ready to be picked up by the event loop
    pub(crate) approved_install: Option<PendingLspInstall>,
}

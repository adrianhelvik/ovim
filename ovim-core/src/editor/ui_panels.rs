use super::filetree::FileTree;
use super::path_completion::PathCompletionState;
use super::quickfix::{LocationList, QuickfixList};
use super::toast::ToastCenter;
use crate::dashboard::DashboardAnimation;

/// UI panel and overlay state.
///
/// Groups fields for file tree, quickfix/location lists,
/// path completion, dashboard, cat animation, diagnostics, and toast notifications.
#[derive(Default)]
pub struct UiPanels {
    /// Latest user-facing status message shown by the editor UI.
    pub status_message: String,
    /// File tree explorer
    pub file_tree: FileTree,
    /// Quickfix list (global error/location list)
    pub quickfix_list: QuickfixList,
    /// Location list (per-window error/location list)
    pub location_list: LocationList,
    /// Whether quickfix window is open
    pub quickfix_window_open: bool,
    /// Whether location list window is open
    pub location_window_open: bool,
    /// Path completion state for command-line mode
    pub path_completion: PathCompletionState,
    /// Dashboard menu selected index (0-5)
    pub dashboard_selected: usize,
    /// Dashboard animation state (concrete type lives in binary crate)
    pub cat_animation: Option<Box<dyn DashboardAnimation>>,
    /// Whether the diagnostic badge overlay has been dismissed (double-Escape)
    pub diagnostic_badge_dismissed: bool,
    /// Last diagnostic count when badge state was set (for detecting changes)
    pub diagnostic_badge_last_count: (usize, usize),
    /// Last time Escape was pressed in normal mode (for double-Escape detection)
    pub last_escape_time: Option<std::time::Instant>,
    /// Top-right toast notifications (transient and sticky)
    pub toast_center: ToastCenter,
    /// Open branch diff review (`<Space>gd`), if any
    pub diff_review: Option<super::diff_review::DiffReviewState>,
    /// Background `git fetch` started from the diff review
    pub pending_git_fetch: Option<super::diff_review::PendingGitFetch>,
}

/// A shell command queued by `:!cmd` for the event loop to execute
/// with full terminal access (outside the alternate screen).
pub struct PendingShellCommand {
    /// The expanded shell command string
    pub command: String,
}

/// Grouped state for the build/test subsystem (`:make`, `<Space>t` test runs).
#[derive(Default)]
pub(crate) struct BuildState {
    /// Pending `:make` result from background thread
    pub(crate) pending_make: Option<super::PendingMake>,
    /// Streaming output from a `<Space>t` test run
    pub(crate) pending_test: Option<super::test_panel::PendingTest>,
    /// Right-side test panel (run history + open state)
    pub(crate) test_panel: super::test_panel::TestPanelState,
    /// Last test run via `<Space>t` keybindings (for `<Space>tl` repeat and
    /// `<Space>tv` visit)
    pub(crate) last_test: Option<super::test_runner::LastTest>,
    /// Raw output from last `:make` / test run
    pub(crate) last_make_output: Option<String>,
    /// Shell command waiting for the event loop to execute with terminal access
    pub(crate) pending_shell_command: Option<PendingShellCommand>,
    /// Last `:!` command (for bare `:!` repeat)
    pub(crate) last_shell_command: Option<String>,
}

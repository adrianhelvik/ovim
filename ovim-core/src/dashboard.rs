//! Dashboard constants and animation trait for the startup screen.

/// Dashboard tips: (key sequence, label)
///
/// Ordered by what users actually look up here: ovim-specific leader bindings
/// first (unguessable across distros), then in-buffer LSP reassurance.
/// Standard vim (`:`, `:e`, `:q`) is omitted — every vim user already knows it.
pub const MENU_ITEMS: &[(&str, &str)] = &[
    ("<Space>sf", "Find a file"),
    ("<Space>sg", "Search the project"),
    ("<Space><Space>", "AI chat · selected code attaches"),
    ("<Space>tn", "Run nearest test · tt shows results"),
    ("<Space>ca", "Code actions"),
    ("gd", "Jump to definition"),
    ("K", "Hover docs"),
];

/// Trait for dashboard animations (e.g. the idle cat easter egg).
///
/// The editor stores `Option<Box<dyn DashboardAnimation>>` so that the
/// concrete animation type (which depends on ratatui) can live in the
/// binary crate while the editor lives in ovim-core.
pub trait DashboardAnimation: Send {
    /// Advance the animation clock. Returns true if a visual change occurred.
    fn tick(&mut self) -> bool;
    /// Returns true if the animation is still running.
    fn is_active(&self) -> bool;
    /// Trigger a startle reaction (e.g. on terminal resize).
    fn startle(&mut self);
    /// Downcast to concrete type for rendering.
    fn as_any_mut(&mut self) -> &mut dyn std::any::Any;
}

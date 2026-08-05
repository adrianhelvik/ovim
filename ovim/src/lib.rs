//! # TUI Safety: No stdout/stderr output!
//!
//! This is a TUI application. Any output to stdout/stderr will corrupt the terminal display.
//! Use the logging system instead: log_info!, log_warn!, log_error!, log_debug!
//!
//! The following macros are BANNED in source code (tests are OK):
#![cfg_attr(not(test), deny(clippy::print_stdout, clippy::print_stderr))]

pub mod api;
pub mod cli;
pub mod client;
pub mod daemon;
pub mod edit_engine;
pub mod frontend;
pub mod key_convert;
pub mod lsp_init;
pub mod mcp_stdio_server;
pub mod subcommands;
pub mod ui;

// Re-export everything from ovim-core
pub use ovim_core::buffer;
pub use ovim_core::change;
pub use ovim_core::color;
pub use ovim_core::command_result;
pub use ovim_core::commands;
pub use ovim_core::dap;
pub use ovim_core::dashboard;
pub use ovim_core::debug_config;
pub use ovim_core::display;
pub use ovim_core::editor;
pub use ovim_core::fold;
pub use ovim_core::git;
pub use ovim_core::language_config;
pub use ovim_core::log;
pub use ovim_core::lsp;
pub use ovim_core::metrics;
pub use ovim_core::mode;
pub use ovim_core::modeline;
pub use ovim_core::navigation_types;
pub use ovim_core::search;
pub use ovim_core::session;
pub use ovim_core::syntax;
pub use ovim_core::textobjects;
pub use ovim_core::unicode;

#[cfg(feature = "lua")]
pub use ovim_core::lua;

pub use ovim_core::git::{GitBlame, GitStatus, LineBlameInfo, LineStatus};

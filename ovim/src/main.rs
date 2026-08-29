//! # TUI Safety: No stdout/stderr output!
//! Use log_info!, log_warn!, log_error!, log_debug! instead of println!/eprintln!
#![deny(clippy::print_stdout, clippy::print_stderr)]

mod api_dispatch;
mod event_loop;

use anyhow::Result;
use ovim::cli::Cli;
use ovim::editor::Editor;
use ovim::mode::Mode;
use ovim::session::{SessionCapability, SessionGuard, SessionInfo};
use ovim::subcommands;
use ovim::ui::UI;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::SystemTime;
use tokio::signal::unix::{signal, SignalKind};
use tokio::sync::mpsc;

/// Install a panic hook that restores the terminal and logs the crash.
///
/// In TUI mode, panics would otherwise leave the terminal in raw mode with the
/// alternate screen still active and no diagnostic trace. This hook:
/// 1. Restores terminal state so the user gets their shell back cleanly
/// 2. Logs the panic location and backtrace to ovim.log
/// 3. Prints a short message to stderr pointing the user to the log
#[allow(clippy::print_stderr)]
fn install_panic_hook() {
    let default_hook = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        // 1. Restore terminal state — best-effort, ignore errors
        let _ = crossterm::terminal::disable_raw_mode();
        let _ = crossterm::execute!(
            std::io::stdout(),
            crossterm::event::DisableMouseCapture,
            crossterm::event::DisableFocusChange,
            crossterm::event::DisableBracketedPaste,
            crossterm::terminal::LeaveAlternateScreen,
            crossterm::cursor::SetCursorStyle::DefaultUserShape,
        );

        // 2. Log the panic with backtrace
        let backtrace = std::backtrace::Backtrace::force_capture();
        ovim_core::log_error!("PANIC", "{}", info);
        ovim_core::log_error!("PANIC", "Backtrace:\n{}", backtrace);

        // 3. Tell the user where to find the details
        eprintln!("\novim crashed: {}", info);
        eprintln!("Backtrace logged to ~/Library/Caches/ovim/ovim.log");
        eprintln!("(or ~/.cache/ovim/ovim.log on Linux)");

        // Run the default hook too (prints to stderr in debug builds)
        default_hook(info);
    }));
}

/// React to a SIGINT/SIGTERM in headless mode with escalation.
///
/// The first signal requests a graceful shutdown through the channel (the
/// event loop breaks out of its select and cleans up). A second signal means
/// the loop is likely wedged in a long await and will never consume the
/// channel, so delete the session file and exit immediately with 130
/// (terminated by signal), matching the pre-channel behavior.
#[allow(clippy::print_stderr)] // headless-only path; no TUI to corrupt
fn handle_shutdown_signal(
    signal_count: &AtomicUsize,
    shutdown_tx: &mpsc::Sender<()>,
    session_info: &SessionInfo,
) {
    if signal_count.fetch_add(1, Ordering::SeqCst) == 0 {
        let _ = shutdown_tx.try_send(());
    } else {
        eprintln!("Received second signal; forcing shutdown.");
        let _ = session_info.delete();
        std::process::exit(130);
    }
}

#[tokio::main]
async fn main() -> Result<()> {
    // Initialize logging FIRST - before anything else
    if let Err(e) = ovim::log::init() {
        let _ = e;
    }
    ovim_core::log_info!("main", "ovim starting up");

    let cli = Cli::parse_args();

    // Initialize language registry early (needed for both editor and subcommands)
    if let Err(e) = ovim::language_config::LanguageRegistry::init() {
        ovim_core::log_warn!("main", "Failed to initialize language registry: {}", e);
        ovim_core::log_warn!("main", "Continuing with limited language support...");
    }

    // Check if we're running a subcommand (client mode)
    if let Some(command) = cli.command {
        // Client subcommands use reqwest's blocking client. Run them in an
        // explicit blocking region so its private runtime can be dropped
        // safely even though the editor entry point is Tokio-powered.
        return tokio::task::block_in_place(|| subcommands::execute_subcommand(command));
    }

    // Otherwise, run editor mode
    let file_arg = cli.file_arg();
    let headless = cli.headless;
    let session_name = cli.session.clone();
    let dimension = cli.dimension;
    let render = cli.render;
    let resume_conversations = cli.resume;

    // Track runtime mode for components that need different behavior in headless mode.
    ovim::lsp_init::set_headless_mode(headless);

    // Initialize LSP logging to file
    if let Err(e) = ovim::lsp::init_lsp_logging() {
        ovim_core::log_warn!("main", "Failed to initialize LSP logging: {}", e);
    }

    // Load Lua before the initial file so language plugins participate in
    // detection, syntax highlighting, LSP startup, and --render.
    let mut editor = Editor::new();
    if let Err(e) = editor.enable_lua() {
        ovim_core::log_error!("main", "Failed to enable Lua support: {}", e);
    }
    // Renderers without editor access (chat markdown, hover previews) resolve
    // languages through the process-wide catalog.
    editor.language_catalog().install_as_process_catalog();

    // Load file from command line argument if provided
    if let Some(ref file) = file_arg {
        let path = std::path::Path::new(&file.path);
        if path.is_dir() {
            editor.open_directory(path)?;
        } else {
            if let Err(e) = editor.load_file(&file.path) {
                ovim_core::log_warn!(
                    "main",
                    "Could not load file '{}': {}. Starting with empty buffer.",
                    file.path,
                    e
                );
                editor.set_file_path(file.path.clone());
            }
            // Jump to line:col if specified
            if let Some(line) = file.line {
                let line_0 = line.saturating_sub(1);
                let col_0 = file.col.unwrap_or(1).saturating_sub(1);
                editor
                    .buffer_mut()
                    .cursor_mut()
                    .set_position(line_0, ovim_core::unicode::GraphemeCol(col_0));
                editor.buffer_mut().validate_cursor_position();
            }
            // Switch from Dashboard to Normal mode when a file is loaded
            editor.set_mode(Mode::Normal);
        }
    }
    editor.set_ai_conversation_resume_enabled(resume_conversations);
    // Set up cat animation (concrete type lives in binary crate)
    editor.ui_panels.cat_animation = Some(Box::new(ovim::ui::CatAnimation::new()));

    // Handle --render flag (render to ANSI and exit)
    if render {
        editor.buffer_mut().enable_syntax_highlighting();
        let (width, height) = dimension.unwrap_or((80, 24));
        match ovim::ui::render_editor_to_ansi(&mut editor, width, height) {
            Ok(ansi) => {
                #[allow(clippy::print_stdout)]
                {
                    print!("{}", ansi);
                }
                return Ok(());
            }
            Err(e) => {
                #[allow(clippy::print_stderr)]
                {
                    eprintln!("Failed to render: {}", e);
                }
                return Err(e);
            }
        }
    }

    // Enable LSP support
    editor.enable_lsp();

    // Create channel for Java LSP status updates (needed for both headless and TUI modes)
    let (java_status_tx, java_status_rx) = mpsc::channel(64);

    // Initialize the Java status sender in the lsp_init module
    ovim::lsp_init::init_java_status_sender(java_status_tx);

    let start_time = SystemTime::now();

    // Handle headless mode
    // Headless mode uses stderr for user feedback (no TUI), so eprintln! is safe
    #[allow(clippy::print_stderr)]
    if headless {
        // Require --session NAME for headless mode
        let session_name = match session_name {
            Some(name) => {
                // Reject invalid names outright (same rule reads enforce)
                // instead of silently sanitizing into a different or empty
                // name that could not be targeted later.
                if let Err(e) = SessionInfo::validate_session_name(&name) {
                    eprintln!("Error: {}", e);
                    eprintln!("Usage: ovim <file> --headless --session <name>");
                    std::process::exit(1);
                }
                name
            }
            None => {
                eprintln!("Error: --headless requires --session NAME");
                eprintln!("Usage: ovim <file> --headless --session <name>");
                std::process::exit(1);
            }
        };

        // Automation is an explicit headless capability. Interactive TUI
        // processes never open a listener merely to support a later command.
        let (tx, rx) = mpsc::channel(256);
        let (port_tx, port_rx) = tokio::sync::oneshot::channel();
        let capability = SessionCapability::generate();
        let server_capability = capability.clone();
        tokio::spawn(async move {
            if let Err(e) =
                ovim::api::start_server("127.0.0.1:0", tx, port_tx, server_capability).await
            {
                ovim_core::lsp_error!("API", "API server error: {}", e);
            }
        });
        let port = port_rx
            .await
            .map_err(|_| anyhow::anyhow!("API server port channel closed"))?;
        editor.set_api_port(port);

        let file_path = file_arg.map(|f| f.path);
        let headless_dimensions = dimension.unwrap_or((120, 35));
        let session_info = SessionInfo::new(port, file_path, session_name.clone())
            .with_capability(capability)
            .with_dimensions(headless_dimensions.0, headless_dimensions.1);

        if let Err(e) = session_info.write() {
            eprintln!("Warning: Failed to write session info: {}", e);
        } else {
            eprintln!(
                "Session '{}' created at ~/.cache/ovim/sessions/{}.json",
                session_name, session_name
            );
        }

        // Create a guard to ensure cleanup on panic
        let _session_guard = SessionGuard::new(session_info.clone());

        // Set up cleanup on exit - handle both SIGINT and SIGTERM.
        // First signal: graceful shutdown via the channel. Second signal:
        // the event loop may be wedged in a long await and never see the
        // channel, so force cleanup (delete the session file) and exit.
        let (shutdown_tx, shutdown_rx) = mpsc::channel(2);
        let signal_count = Arc::new(AtomicUsize::new(0));

        let shutdown_tx_sigint = shutdown_tx.clone();
        let signal_count_sigint = Arc::clone(&signal_count);
        let session_for_sigint = session_info.clone();
        let sigint_handle = tokio::spawn(async move {
            loop {
                if tokio::signal::ctrl_c().await.is_err() {
                    return;
                }
                handle_shutdown_signal(
                    &signal_count_sigint,
                    &shutdown_tx_sigint,
                    &session_for_sigint,
                );
            }
        });

        let shutdown_tx_sigterm = shutdown_tx.clone();
        let signal_count_sigterm = Arc::clone(&signal_count);
        let session_for_sigterm = session_info.clone();
        let sigterm_handle = tokio::spawn(async move {
            let mut sigterm = match signal(SignalKind::terminate()) {
                Ok(s) => s,
                Err(e) => {
                    eprintln!("Failed to register SIGTERM handler: {}", e);
                    return;
                }
            };
            while sigterm.recv().await.is_some() {
                handle_shutdown_signal(
                    &signal_count_sigterm,
                    &shutdown_tx_sigterm,
                    &session_for_sigterm,
                );
            }
        });

        // Store session info and start time for health checks
        let session_info_arc = Arc::new(Mutex::new(session_info));

        // Run in headless mode (API only, no TUI)
        event_loop::run_headless_loop(
            &mut editor,
            rx,
            java_status_rx,
            start_time,
            session_info_arc,
            headless_dimensions,
            shutdown_rx,
        )
        .await?;
        sigint_handle.abort();
        sigterm_handle.abort();
        let code = editor.exit_code();
        if code != 0 {
            std::process::exit(code);
        }
        return Ok(());
    }

    // TUI mode is network-closed. Automation uses an explicit named headless
    // session, which owns the lifetime of its loopback listener.

    // Install panic hook BEFORE entering raw mode so crashes restore the terminal
    // and leave a diagnostic trace in the log file.
    install_panic_hook();

    // Create UI for TUI mode
    let mut ui = if let Some(dimensions) = dimension {
        UI::with_dimensions(Some(dimensions))?
    } else {
        UI::new()?
    };

    event_loop::run_event_loop(&mut ui, &mut editor, None, java_status_rx, start_time).await?;

    let code = editor.exit_code();

    // Drop UI first to restore terminal before exiting
    drop(ui);

    if code != 0 {
        std::process::exit(code);
    }

    Ok(())
}

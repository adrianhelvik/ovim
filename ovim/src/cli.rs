use clap::{Parser, Subcommand};

#[derive(Parser, Debug)]
#[command(name = "ovim")]
#[command(version, disable_version_flag = true)]
#[command(about = "Oxidized Vim — a snappy, batteries-included terminal editor with Vim keybindings", long_about = None)]
pub struct Cli {
    /// Print version
    #[arg(
        short = 'v',
        visible_short_alias = 'V',
        long = "version",
        action = clap::ArgAction::Version
    )]
    _version: Option<bool>,

    #[command(subcommand)]
    pub command: Option<Command>,

    /// File or directory to open (files support FILE:LINE:COL syntax)
    pub file: Option<String>,

    /// Run in headless mode with REST API enabled (no TUI)
    #[arg(long)]
    pub headless: bool,

    /// Session name for headless mode (required with --headless)
    #[arg(long)]
    pub session: Option<String>,

    /// Resume persisted AI conversations instead of starting fresh chats
    #[arg(long)]
    pub resume: bool,

    /// Set viewport dimensions (e.g., 80x24)
    #[arg(long, value_parser = parse_dimensions)]
    pub dimension: Option<(u16, u16)>,

    /// Render the editor to ANSI and exit (useful for debugging)
    #[arg(long)]
    pub render: bool,
}

/// Parsed file argument with optional line and column
#[derive(Debug, Clone)]
pub struct FileArg {
    pub path: String,
    pub line: Option<usize>,
    pub col: Option<usize>,
}

impl FileArg {
    /// Parse a file argument that may contain :LINE:COL suffix.
    /// Splits from the right, so paths with colons are handled gracefully.
    pub fn parse(input: &str) -> Self {
        // Try to parse trailing :COL and :LINE segments from the right
        // "src/main.rs:42:10" -> path="src/main.rs", line=42, col=10
        // "src/main.rs:42"    -> path="src/main.rs", line=42, col=None
        // "src/main.rs"       -> path="src/main.rs", line=None, col=None
        // "C:\foo\bar"        -> path="C:\foo\bar", line=None, col=None

        let parts: Vec<&str> = input.rsplitn(3, ':').collect();

        match parts.as_slice() {
            [col_str, line_str, path] => {
                if let (Ok(line), Ok(col)) = (line_str.parse::<usize>(), col_str.parse::<usize>()) {
                    if line > 0 && col > 0 && !path.is_empty() {
                        return FileArg {
                            path: path.to_string(),
                            line: Some(line),
                            col: Some(col),
                        };
                    }
                }
                // Fall through: try LINE only
                if let Ok(line) = col_str.parse::<usize>() {
                    // "path:with:colon:42" -> rsplitn(3) gives ["42", "colon", "path:with"]
                    // Reconstruct the path
                    let reconstructed = format!("{}:{}", path, line_str);
                    if line > 0 {
                        return FileArg {
                            path: reconstructed,
                            line: Some(line),
                            col: None,
                        };
                    }
                }
                // Not parseable, treat whole input as path
                FileArg {
                    path: input.to_string(),
                    line: None,
                    col: None,
                }
            }
            [maybe_line, path] => {
                if let Ok(line) = maybe_line.parse::<usize>() {
                    if line > 0 && !path.is_empty() {
                        return FileArg {
                            path: path.to_string(),
                            line: Some(line),
                            col: None,
                        };
                    }
                }
                FileArg {
                    path: input.to_string(),
                    line: None,
                    col: None,
                }
            }
            _ => FileArg {
                path: input.to_string(),
                line: None,
                col: None,
            },
        }
    }
}

#[derive(Subcommand, Debug)]
pub enum Command {
    // ── File Operations ──────────────────────────────────────────────
    /// Replace text in a file
    #[command(next_help_heading = "File Operations")]
    Edit {
        /// File to edit
        file: String,
        /// Line number (1-indexed) to restrict the search to
        #[arg(long)]
        line: Option<usize>,
        /// Text to find (literal match, use \n for newlines)
        #[arg(long)]
        old: String,
        /// Replacement text (use \n for newlines)
        #[arg(long)]
        new: String,
        /// Apply the edit to this live session instead of writing the file directly
        #[arg(short, long)]
        session: Option<String>,
    },

    /// Insert text into a file
    Insert {
        /// File to edit
        file: String,
        /// Insert after this line number (1-indexed, 0 = before first line)
        #[arg(long, conflicts_with = "before")]
        after: Option<usize>,
        /// Insert before this line number (1-indexed)
        #[arg(long, conflicts_with = "after")]
        before: Option<usize>,
        /// Text to insert (use \n for newlines)
        #[arg(long)]
        text: String,
        /// Apply the insertion to this live session instead of writing the file directly
        #[arg(short, long)]
        session: Option<String>,
    },

    /// Delete lines from a file
    DeleteLines {
        /// File to edit
        file: String,
        /// First line to delete (1-indexed)
        #[arg(long)]
        from: usize,
        /// Last line to delete (1-indexed, inclusive)
        #[arg(long)]
        to: usize,
        /// Apply the deletion to this live session instead of writing the file directly
        #[arg(short, long)]
        session: Option<String>,
    },

    /// Read lines from a file
    ReadLines {
        /// File to read
        file: String,
        /// First line to read (1-indexed)
        #[arg(long)]
        from: usize,
        /// Last line to read (1-indexed, inclusive)
        #[arg(long)]
        to: usize,
        /// Output as JSON
        #[arg(long)]
        json: bool,
        /// Read from this live session instead of the file on disk
        #[arg(short, long)]
        session: Option<String>,
    },

    // ── Session Control ──────────────────────────────────────────────
    /// Send key sequence to a session
    #[command(next_help_heading = "Session Control")]
    Send {
        /// Key sequence (Vim keybindings)
        keys: String,
        /// Session name (required)
        #[arg(short, long)]
        session: String,
    },

    /// Paste literal text into a session
    Paste {
        /// Literal text to paste (use \\n for a newline)
        text: String,
        /// Session name (required)
        #[arg(short, long)]
        session: String,
    },

    /// Resize a session's logical viewport
    Resize {
        /// Viewport dimensions (for example 120x40)
        #[arg(value_parser = parse_dimensions)]
        dimension: (u16, u16),
        /// Session name (required)
        #[arg(short, long)]
        session: String,
    },

    /// Execute an ex command in a session
    Exec {
        /// Ex command (without leading colon)
        command: String,
        /// Session name (required)
        #[arg(short, long)]
        session: String,
    },

    /// Get 21-line context window around cursor
    Context {
        /// Session name (optional). If omitted, tries $OVIM_SESSION then "default".
        #[arg(short, long)]
        session: Option<String>,

        /// File to get context for (supports FILE:LINE:COL syntax)
        #[arg(value_name = "FILE")]
        file: Option<String>,
    },

    /// Get buffer content from a session
    Buffer {
        /// Session name (required)
        #[arg(short, long)]
        session: String,
    },

    /// Get snapshot of a session's state
    Snapshot {
        /// Output format (json or pretty)
        #[arg(long, default_value = "json")]
        format: String,
        /// Session name (required)
        #[arg(short, long)]
        session: String,
    },

    /// Search for pattern and jump to first match
    Search {
        /// Search pattern
        pattern: String,
        /// Session name (required)
        #[arg(short, long)]
        session: String,
    },

    /// Jump to next match and return position
    NextMatch {
        /// Session name (required)
        #[arg(short, long)]
        session: String,
    },

    // ── LSP Commands ─────────────────────────────────────────────────
    /// Language Server Protocol commands
    #[command(next_help_heading = "LSP")]
    Lsp {
        #[command(subcommand)]
        command: LspCommand,
    },

    // ── Session Management ───────────────────────────────────────────
    /// Manage ovim sessions
    #[command(next_help_heading = "Session Management")]
    Session {
        #[command(subcommand)]
        command: SessionCommand,
    },

    // ── Integration ──────────────────────────────────────────────────
    /// Start ovim as a long-running MCP server
    #[command(next_help_heading = "Integration")]
    McpServer {
        /// Workspace directory for ovim sessions
        #[arg(long)]
        workspace: Option<String>,

        /// Port to listen on (default: auto)
        #[arg(long)]
        port: Option<u16>,

        /// Session name for this server instance
        #[arg(long)]
        session: Option<String>,
    },

    /// Install ovim as MCP server for supported editors
    Install {
        /// Editor to install for (claude, cursor, or all)
        #[arg(value_name = "EDITOR", default_value = "claude")]
        editor: String,

        /// Show what would be installed without making changes
        #[arg(long)]
        show_config: bool,

        /// Workspace directory for ovim sessions
        #[arg(long)]
        workspace: Option<String>,
    },
}

/// LSP subcommands
#[derive(Subcommand, Debug)]
pub enum LspCommand {
    /// Get LSP status from a session
    Status {
        /// Session name (optional). If omitted and FILE is provided, falls back to `ovim lsp check FILE`.
        #[arg(short, long)]
        session: Option<String>,

        /// File to check (supports FILE:LINE:COL syntax)
        #[arg(value_name = "FILE", conflicts_with = "session")]
        file: Option<String>,
    },

    /// Trigger hover and return hover info
    Hover {
        /// Session name (optional). If omitted, tries $OVIM_SESSION then "default".
        #[arg(short, long)]
        session: Option<String>,

        /// File to use (supports FILE:LINE:COL syntax)
        #[arg(value_name = "FILE", conflicts_with = "session")]
        file: Option<String>,
    },

    /// Trigger goto-definition and return new location
    Definition {
        /// Session name (optional). If omitted, tries $OVIM_SESSION then "default".
        #[arg(short, long)]
        session: Option<String>,

        /// File to use (supports FILE:LINE:COL syntax)
        #[arg(value_name = "FILE", conflicts_with = "session")]
        file: Option<String>,
    },

    /// Trigger find-references and return list
    References {
        /// Session name (optional). If omitted, tries $OVIM_SESSION then "default".
        #[arg(short, long)]
        session: Option<String>,

        /// File to use (supports FILE:LINE:COL syntax)
        #[arg(value_name = "FILE", conflicts_with = "session")]
        file: Option<String>,
    },

    /// Return LSP diagnostic info
    Diagnostics {
        /// Session name (optional). If omitted, tries $OVIM_SESSION then "default".
        #[arg(short, long)]
        session: Option<String>,

        /// File to use (supports FILE:LINE:COL syntax)
        #[arg(value_name = "FILE", conflicts_with = "session")]
        file: Option<String>,
    },

    /// List document symbols
    Symbols {
        /// Session name (optional). If omitted, tries $OVIM_SESSION then "default".
        #[arg(short, long)]
        session: Option<String>,

        /// File to use (supports FILE:LINE:COL syntax)
        #[arg(value_name = "FILE", conflicts_with = "session")]
        file: Option<String>,
    },

    /// Get structural outline of the current document
    Outline {
        /// Session name (optional). If omitted, tries $OVIM_SESSION then "default".
        #[arg(short, long)]
        session: Option<String>,

        /// File to use (supports FILE:LINE:COL syntax)
        #[arg(value_name = "FILE", conflicts_with = "session")]
        file: Option<String>,
    },

    /// Search workspace symbols by name
    Symbol {
        /// Symbol name or partial name to search
        query: String,
        /// Session name (optional). If omitted, tries $OVIM_SESSION then "default".
        #[arg(short, long)]
        session: Option<String>,

        /// File to use as workspace/root hint (supports FILE:LINE:COL syntax)
        #[arg(value_name = "FILE", conflicts_with = "session")]
        file: Option<String>,
    },

    /// Get call hierarchy (incoming/outgoing) for symbol at cursor
    Trace {
        /// Session name (optional). If omitted, tries $OVIM_SESSION then "default".
        #[arg(short, long)]
        session: Option<String>,

        /// File to use (supports FILE:LINE:COL syntax)
        #[arg(value_name = "FILE", conflicts_with = "session")]
        file: Option<String>,
    },

    /// Wait for LSP to be ready (blocks until ready)
    Wait {
        /// Session name (optional). If omitted, tries $OVIM_SESSION then "default".
        #[arg(short, long)]
        session: Option<String>,
        /// Timeout in milliseconds (default: 30000)
        #[arg(long, default_value = "30000")]
        timeout: u64,

        /// File to use (supports FILE:LINE:COL syntax)
        #[arg(value_name = "FILE", conflicts_with = "session")]
        file: Option<String>,
    },

    /// Check language configuration and LSP status for a file (no session needed)
    Check {
        /// File path to check
        file: String,
        /// Show full language configuration
        #[arg(short, long)]
        verbose: bool,
    },

    /// List all configured languages (no session needed)
    Languages {
        /// Show detailed information (LSP command, root markers, etc.)
        #[arg(short, long)]
        verbose: bool,
    },
}

/// Session management subcommands
#[derive(Subcommand, Debug)]
pub enum SessionCommand {
    /// List all running ovim sessions
    List,

    /// Kill a running session
    Kill {
        /// Session name (required)
        #[arg(short, long)]
        session: String,
    },

    /// Check health of a session
    Health {
        /// Session name (required)
        #[arg(short, long)]
        session: String,
    },

    /// Clean up stale, expired, and corrupted session files
    Cleanup {
        /// Maximum session age in days (sessions older than this will be removed)
        #[arg(long)]
        max_age: Option<u64>,

        /// Show what would be cleaned up without actually removing files
        #[arg(long)]
        dry_run: bool,
    },
}

/// Parse dimension string like "80x24" into (width, height)
fn parse_dimensions(s: &str) -> Result<(u16, u16), String> {
    let parts: Vec<&str> = s.split('x').collect();
    if parts.len() != 2 {
        return Err(format!(
            "Invalid dimension format: '{}'. Expected format: WIDTHxHEIGHT (e.g., 80x24)",
            s
        ));
    }

    let width = parts[0]
        .parse::<u16>()
        .map_err(|_| format!("Invalid width: '{}'", parts[0]))?;
    let height = parts[1]
        .parse::<u16>()
        .map_err(|_| format!("Invalid height: '{}'", parts[1]))?;

    if width == 0 || height == 0 {
        return Err("Width and height must be greater than 0".to_string());
    }

    Ok((width, height))
}

impl Cli {
    pub fn parse_args() -> Self {
        Cli::parse()
    }

    /// Check if running in editor mode (no subcommand)
    pub fn is_editor_mode(&self) -> bool {
        self.command.is_none()
    }

    /// Parse the file argument with LINE:COL support
    pub fn file_arg(&self) -> Option<FileArg> {
        self.file.as_ref().map(|f| FileArg::parse(f))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_file_arg_parse_path_only() {
        let arg = FileArg::parse("src/main.rs");
        assert_eq!(arg.path, "src/main.rs");
        assert_eq!(arg.line, None);
        assert_eq!(arg.col, None);
    }

    #[test]
    fn test_file_arg_parse_path_and_line() {
        let arg = FileArg::parse("src/main.rs:42");
        assert_eq!(arg.path, "src/main.rs");
        assert_eq!(arg.line, Some(42));
        assert_eq!(arg.col, None);
    }

    #[test]
    fn test_file_arg_parse_path_line_col() {
        let arg = FileArg::parse("src/main.rs:42:10");
        assert_eq!(arg.path, "src/main.rs");
        assert_eq!(arg.line, Some(42));
        assert_eq!(arg.col, Some(10));
    }

    #[test]
    fn test_file_arg_parse_no_numbers() {
        let arg = FileArg::parse("path:with:colons");
        assert_eq!(arg.path, "path:with:colons");
        assert_eq!(arg.line, None);
        assert_eq!(arg.col, None);
    }

    #[test]
    fn test_file_arg_parse_zero_line() {
        // Line 0 is invalid (1-indexed), treat as path
        let arg = FileArg::parse("file:0");
        assert_eq!(arg.path, "file:0");
        assert_eq!(arg.line, None);
    }

    #[test]
    fn conversation_resume_is_opt_in() {
        let fresh = Cli::try_parse_from(["ovim"]).unwrap();
        assert!(!fresh.resume);

        let resumed = Cli::try_parse_from(["ovim", "--resume"]).unwrap();
        assert!(resumed.resume);
    }

    #[test]
    fn version_flag_reports_the_package_version() {
        for flag in ["-v", "-V", "--version"] {
            let error = Cli::try_parse_from(["ovim", flag]).unwrap_err();

            assert_eq!(error.kind(), clap::error::ErrorKind::DisplayVersion);
            assert!(error.to_string().contains(env!("CARGO_PKG_VERSION")));
        }
    }

    #[test]
    fn lsp_file_and_session_are_mutually_exclusive() {
        let result = Cli::try_parse_from([
            "ovim",
            "lsp",
            "hover",
            "--session",
            "dev",
            "src/main.rs:10:5",
        ]);

        assert!(result.is_err());
    }
}

//! Data-driven `:set` command handler.
//!
//! Replaces the ~400-line match/else-if chain in `commands.rs` with a
//! declarative option table. Adding a new boolean or integer option
//! requires adding a single entry to `BOOL_OPTIONS` or handling it
//! in `handle_value_option`.

use crate::command_result::{CommandResult, ErrorResponse, SuccessResponse};
use crate::editor::{AutoInstallMode, Editor, MarginColor};

// ---------------------------------------------------------------------------
// Boolean option table
// ---------------------------------------------------------------------------

struct BoolOption {
    /// Primary name (e.g. "number")
    name: &'static str,
    /// Short alias (e.g. "nu"), or empty if none
    alias: &'static str,
    /// Getter: reads the current value from EditorOptions
    get: fn(&Editor) -> bool,
    /// Setter: writes a value to EditorOptions
    set: fn(&mut Editor, bool),
}

/// All boolean options recognised by `:set`.
///
/// To add a new boolean option, add an entry here. The handler automatically
/// supports `name`, `noname`, `alias`, `noalias`, and `name?` / `alias?`.
const BOOL_OPTIONS: &[BoolOption] = &[
    BoolOption {
        name: "number",
        alias: "nu",
        get: |e| e.options.number,
        set: |e, v| e.options.number = v,
    },
    BoolOption {
        name: "relativenumber",
        alias: "rnu",
        get: |e| e.options.relative_number,
        set: |e, v| e.options.relative_number = v,
    },
    BoolOption {
        name: "expandtab",
        alias: "et",
        get: |e| e.options.expand_tab,
        set: |e, v| e.options.expand_tab = v,
    },
    BoolOption {
        name: "copyindent",
        alias: "ci",
        get: |e| e.options.copy_indent,
        set: |e, v| e.options.copy_indent = v,
    },
    BoolOption {
        name: "ignorecase",
        alias: "ic",
        get: |e| e.options.ignorecase,
        set: |e, v| e.options.ignorecase = v,
    },
    BoolOption {
        name: "smartcase",
        alias: "scs",
        get: |e| e.options.smartcase,
        set: |e, v| e.options.smartcase = v,
    },
    BoolOption {
        name: "cursorline",
        alias: "cul",
        get: |e| e.options.cursorline,
        set: |e, v| e.options.cursorline = v,
    },
    BoolOption {
        name: "showmatch",
        alias: "sm",
        get: |e| e.options.showmatch,
        set: |e, v| e.options.showmatch = v,
    },
    BoolOption {
        name: "swapfile",
        alias: "swf",
        get: |e| e.options.swapfile,
        set: |e, v| e.options.swapfile = v,
    },
    BoolOption {
        name: "backup",
        alias: "bk",
        get: |e| e.options.backup,
        set: |e, v| e.options.backup = v,
    },
    BoolOption {
        name: "wrap",
        alias: "",
        get: |e| e.options.wrap,
        set: |e, v| e.options.wrap = v,
    },
    BoolOption {
        name: "filetreereveal",
        alias: "",
        get: |e| e.options.file_tree_reveal,
        set: |e, v| e.options.file_tree_reveal = v,
    },
    BoolOption {
        name: "markdownconceal",
        alias: "mdc",
        get: |e| e.options.markdown_conceal,
        set: |e, v| e.options.markdown_conceal = v,
    },
];

fn ok(message: Option<String>) -> CommandResult {
    CommandResult::Success(SuccessResponse {
        success: true,
        message: message.map(std::borrow::Cow::Owned),
        line_count: None,
    })
}

fn err(msg: impl Into<String>) -> CommandResult {
    CommandResult::Error(ErrorResponse { error: msg.into() })
}

// ---------------------------------------------------------------------------
// Query handling (`:set option?`)
// ---------------------------------------------------------------------------

fn query_bool(opt: &BoolOption, editor: &Editor) -> String {
    let val = (opt.get)(editor);
    format!("  {}{}", if val { "" } else { "no" }, opt.name)
}

fn query_option(name: &str, editor: &Editor) -> Option<CommandResult> {
    // Check boolean options
    for opt in BOOL_OPTIONS {
        if name == opt.name || (!opt.alias.is_empty() && name == opt.alias) {
            return Some(ok(Some(query_bool(opt, editor))));
        }
    }

    // Special-case boolean: inlay_hints (kept out of BOOL_OPTIONS because
    // its setter has side effects). See try_set_bool for the toggle path.
    if name == "inlay_hints" || name == "inlayhints" {
        let val = editor.options.inlay_hints;
        return Some(ok(Some(format!(
            "  {}inlay_hints",
            if val { "" } else { "no" }
        ))));
    }

    // Value options
    let opts = &editor.options;
    let msg = match name {
        "tabstop" | "ts" => format!("  tabstop={}", opts.tab_width),
        "shiftwidth" | "sw" => format!("  shiftwidth={}", opts.shift_width),
        "softtabstop" | "sts" => format!("  softtabstop={}", opts.soft_tab_stop),
        "scroll" => format!(
            "  scroll={}",
            opts.scroll
                .map(|s| s.to_string())
                .unwrap_or_else(|| "auto".to_string()),
        ),
        "scrolloff" => format!("  scrolloff={}", opts.scrolloff),
        "textwidth" | "tw" => format!(
            "  textwidth={}",
            opts.textwidth
                .map(|w| w.to_string())
                .unwrap_or_else(|| "0".to_string()),
        ),
        "sidescroll" => format!("  sidescroll={}", opts.sidescroll),
        "sidescrolloff" => format!("  sidescrolloff={}", opts.sidescrolloff),
        "clipboard" | "cb" => {
            if opts.clipboard.is_empty() {
                "  clipboard=".to_string()
            } else {
                format!("  clipboard={}", opts.clipboard)
            }
        }
        "mapleader" => format!("  mapleader={}", editor.leader_key()),
        "margincolor" => format!(
            "  margincolor={}",
            match &opts.margin_color {
                MarginColor::None => "none".to_string(),
                MarginColor::Solid(r, g, b) => format!("#{:02x}{:02x}{:02x}", r, g, b),
            }
        ),
        "marginpadding" => format!("  marginpadding={}", opts.margin_padding),
        "autoinstall" => format!(
            "  autoinstall={}",
            match opts.lsp_auto_install {
                AutoInstallMode::Prompt => "prompt",
                AutoInstallMode::Auto => "auto",
                AutoInstallMode::Off => "off",
            }
        ),
        _ => return None,
    };
    Some(ok(Some(msg)))
}

// ---------------------------------------------------------------------------
// Boolean set/unset
// ---------------------------------------------------------------------------

/// Try to match `opt_name` against the boolean option table.
/// Handles `name`, `noname`, `alias`, `noalias`.
fn try_set_bool(opt_name: &str, editor: &mut Editor) -> Option<CommandResult> {
    for opt in BOOL_OPTIONS {
        // Set: "number", "nu"
        if opt_name == opt.name || (!opt.alias.is_empty() && opt_name == opt.alias) {
            (opt.set)(editor, true);
            return Some(ok(None));
        }
        // Unset: "nonumber", "nonu"
        let no_name = format!("no{}", opt.name);
        let no_alias = if opt.alias.is_empty() {
            String::new()
        } else {
            format!("no{}", opt.alias)
        };
        if opt_name == no_name || (!no_alias.is_empty() && opt_name == no_alias) {
            (opt.set)(editor, false);
            return Some(ok(None));
        }
    }

    // Special case: "noclipboard" / "nocb"
    if opt_name == "noclipboard" || opt_name == "nocb" {
        editor.options.clipboard = String::new();
        return Some(ok(None));
    }

    // Special case: inlay hints — toggling has side effects beyond a flag flip.
    // Toggling off must clear cached hints + decorations immediately so they
    // disappear without waiting for the next buffer mutation. Toggling on must
    // invalidate the slot so the next event-loop tick re-requests hints.
    // (OV-00259)
    if opt_name == "inlay_hints" || opt_name == "inlayhints" {
        editor.options.inlay_hints = true;
        editor.invalidate_inlay_hints_slot();
        return Some(ok(None));
    }
    if opt_name == "noinlay_hints" || opt_name == "noinlayhints" {
        editor.options.inlay_hints = false;
        editor.clear_inlay_hints_state();
        return Some(ok(None));
    }

    None
}

/// Try to toggle a boolean option (`:set option!`), flipping its current value.
/// Handles the option table by name/alias plus the side-effecting `inlay_hints`.
fn try_toggle_bool(opt_name: &str, editor: &mut Editor) -> Option<CommandResult> {
    for opt in BOOL_OPTIONS {
        if opt_name == opt.name || (!opt.alias.is_empty() && opt_name == opt.alias) {
            let new_val = !(opt.get)(editor);
            (opt.set)(editor, new_val);
            return Some(ok(None));
        }
    }
    // inlay_hints has side-effecting set/unset paths — route through try_set_bool
    // so the toggle keeps the same cache-clear / slot-invalidate semantics.
    if opt_name == "inlay_hints" || opt_name == "inlayhints" {
        return if editor.options.inlay_hints {
            try_set_bool("noinlay_hints", editor)
        } else {
            try_set_bool("inlay_hints", editor)
        };
    }
    None
}

// ---------------------------------------------------------------------------
// Value options (`:set option=value`)
// ---------------------------------------------------------------------------

/// Parse a hex color string like "#1a1a1e" into (r, g, b).
fn parse_hex_color(s: &str) -> Option<(u8, u8, u8)> {
    let hex = s.strip_prefix('#')?;
    if hex.len() != 6 {
        return None;
    }
    let r = u8::from_str_radix(&hex[0..2], 16).ok()?;
    let g = u8::from_str_radix(&hex[2..4], 16).ok()?;
    let b = u8::from_str_radix(&hex[4..6], 16).ok()?;
    Some((r, g, b))
}

fn handle_value_option(name: &str, value: &str, editor: &mut Editor) -> Option<CommandResult> {
    let result = match name {
        "tabstop" | "ts" => match value.parse::<usize>() {
            Ok(n) if n > 0 && n <= 16 => {
                editor.options.tab_width = n;
                ok(Some(format!("  tabstop={}", n)))
            }
            Ok(_) => err("tabstop must be between 1 and 16"),
            Err(_) => err(format!("Invalid number: {}", value)),
        },
        "shiftwidth" | "sw" => match value.parse::<usize>() {
            Ok(n) if n > 0 && n <= 16 => {
                editor.options.shift_width = n;
                ok(Some(format!("  shiftwidth={}", n)))
            }
            Ok(_) => err("shiftwidth must be between 1 and 16"),
            Err(_) => err(format!("Invalid number: {}", value)),
        },
        "softtabstop" | "sts" => match value.parse::<isize>() {
            Ok(n) if (-1..=16).contains(&n) => {
                editor.options.soft_tab_stop = n;
                ok(Some(format!("  softtabstop={}", n)))
            }
            Ok(_) => err("softtabstop must be between -1 and 16"),
            Err(_) => err(format!("Invalid number: {}", value)),
        },
        "scroll" => match value.parse::<usize>() {
            Ok(n) if n > 0 => {
                editor.options.scroll = Some(n);
                ok(Some(format!("  scroll={}", n)))
            }
            Ok(_) => err("scroll must be greater than 0"),
            Err(_) => err(format!("Invalid number: {}", value)),
        },
        "scrolloff" => match value.parse::<usize>() {
            Ok(n) => {
                editor.options.scrolloff = n;
                ok(Some(format!("  scrolloff={}", n)))
            }
            Err(_) => err(format!("Invalid number: {}", value)),
        },
        "textwidth" | "tw" => match value.parse::<usize>() {
            Ok(0) => {
                editor.options.textwidth = None;
                ok(Some("  textwidth=0".to_string()))
            }
            Ok(n) if n >= 20 => {
                editor.options.textwidth = Some(n);
                ok(Some(format!("  textwidth={}", n)))
            }
            Ok(_) => err("textwidth must be 0 (disabled) or at least 20"),
            Err(_) => err(format!("Invalid number: {}", value)),
        },
        "mapleader" => {
            let chars: Vec<char> = value.chars().collect();
            if chars.len() == 1 {
                editor.set_leader_key(chars[0]);
                ok(Some(format!("  mapleader={}", chars[0])))
            } else {
                err("mapleader must be a single character")
            }
        }
        "clipboard" | "cb" => match value {
            "unnamedplus" | "unnamed" | "" => {
                editor.options.clipboard = value.to_string();
                ok(Some(if value.is_empty() {
                    "  clipboard=".to_string()
                } else {
                    format!("  clipboard={}", value)
                }))
            }
            _ => err(format!(
                "Invalid clipboard value: {} (use 'unnamedplus', 'unnamed', or '')",
                value
            )),
        },
        "margincolor" => {
            if value == "none" {
                editor.options.margin_color = MarginColor::None;
                ok(Some("  margincolor=none".to_string()))
            } else if let Some((r, g, b)) = parse_hex_color(value) {
                editor.options.margin_color = MarginColor::Solid(r, g, b);
                ok(Some(format!("  margincolor={}", value)))
            } else {
                err(format!(
                    "Invalid color: {} (use hex like #1a1a1e or 'none')",
                    value
                ))
            }
        }
        "marginpadding" => match value.parse::<usize>() {
            Ok(n) => {
                editor.options.margin_padding = n;
                ok(Some(format!("  marginpadding={}", n)))
            }
            Err(_) => err(format!("Invalid number: {}", value)),
        },
        "sidescroll" => match value.parse::<usize>() {
            Ok(n) => {
                editor.options.sidescroll = n;
                ok(Some(format!("  sidescroll={}", n)))
            }
            Err(_) => err(format!("Invalid number: {}", value)),
        },
        "sidescrolloff" => match value.parse::<usize>() {
            Ok(n) => {
                editor.options.sidescrolloff = n;
                ok(Some(format!("  sidescrolloff={}", n)))
            }
            Err(_) => err(format!("Invalid number: {}", value)),
        },
        "makeprg" | "mp" => {
            if value.is_empty() {
                err("makeprg cannot be empty")
            } else {
                // Unescape backslash-space (vim convention: `set makeprg=cargo\ test`)
                let unescaped = value.replace("\\ ", " ");
                editor.options.makeprg = unescaped.clone();
                ok(Some(format!("  makeprg={}", unescaped)))
            }
        }
        "autoinstall" => match value {
            "prompt" => {
                editor.options.lsp_auto_install = AutoInstallMode::Prompt;
                ok(Some("  autoinstall=prompt".to_string()))
            }
            "auto" => {
                editor.options.lsp_auto_install = AutoInstallMode::Auto;
                ok(Some("  autoinstall=auto".to_string()))
            }
            "off" => {
                editor.options.lsp_auto_install = AutoInstallMode::Off;
                ok(Some("  autoinstall=off".to_string()))
            }
            _ => err(format!(
                "Invalid autoinstall value: {} (use 'prompt', 'auto', or 'off')",
                value
            )),
        },
        _ => return None,
    };
    Some(result)
}

// ---------------------------------------------------------------------------
// Public entry point
// ---------------------------------------------------------------------------

/// Handle `:set` commands for options.
///
/// This replaces `handle_set_command` in `commands.rs`.
pub fn handle_set_command(editor: &mut Editor, args: &str) -> CommandResult {
    // Handle empty :set (show all options)
    if args.is_empty() {
        let opts = &editor.options;
        let msg = format!(
            "  {}number\n  {}relativenumber\n  {}expandtab\n  {}copyindent\n  tabstop={}\n  shiftwidth={}\n  softtabstop={}\n  scroll={}\n  scrolloff={}\n  sidescroll={}\n  sidescrolloff={}",
            if opts.number { "" } else { "no" },
            if opts.relative_number { "" } else { "no" },
            if opts.expand_tab { "" } else { "no" },
            if opts.copy_indent { "" } else { "no" },
            opts.tab_width,
            opts.shift_width,
            opts.soft_tab_stop,
            opts.scroll
                .map(|s| s.to_string())
                .unwrap_or_else(|| "auto".to_string()),
            opts.scrolloff,
            opts.sidescroll,
            opts.sidescrolloff
        );
        return ok(Some(msg));
    }

    // Parse option name and optional value
    let (opt_name, opt_value) = if let Some((name, value)) = args.split_once('=') {
        (name.trim(), Some(value.trim()))
    } else {
        (args, None)
    };

    // Check for query (option?)
    if let Some(query_opt) = opt_name.strip_suffix('?') {
        return match query_option(query_opt, editor) {
            Some(result) => result,
            None => err(format!("Unknown option: {}", query_opt)),
        };
    }

    // Check for toggle (option!) — flip a boolean option's current value.
    if let Some(toggle_opt) = opt_name.strip_suffix('!') {
        if opt_value.is_some() {
            return err(format!("Trailing characters: {}", args));
        }
        let toggle_opt = toggle_opt.trim();
        return match try_toggle_bool(toggle_opt, editor) {
            Some(result) => result,
            None => err(format!("Not a boolean option: {}", toggle_opt)),
        };
    }

    // Try boolean set/unset
    if opt_value.is_none() {
        if let Some(result) = try_set_bool(opt_name, editor) {
            return result;
        }
    }

    // Try value-based options
    if let Some(value) = opt_value {
        if let Some(result) = handle_value_option(opt_name, value, editor) {
            return result;
        }
    }

    err(format!("Unknown option: {}", opt_name))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::editor::Editor;

    fn make_editor() -> Editor {
        Editor::new()
    }

    #[test]
    fn set_bool_option() {
        let mut ed = make_editor();
        ed.options.number = false;
        let result = handle_set_command(&mut ed, "number");
        assert!(matches!(result, CommandResult::Success(_)));
        assert!(ed.options.number);
    }

    #[test]
    fn unset_bool_option() {
        let mut ed = make_editor();
        ed.options.number = true;
        let result = handle_set_command(&mut ed, "nonumber");
        assert!(matches!(result, CommandResult::Success(_)));
        assert!(!ed.options.number);
    }

    #[test]
    fn set_bool_alias() {
        let mut ed = make_editor();
        ed.options.number = false;
        let result = handle_set_command(&mut ed, "nu");
        assert!(matches!(result, CommandResult::Success(_)));
        assert!(ed.options.number);
    }

    #[test]
    fn unset_bool_alias() {
        let mut ed = make_editor();
        ed.options.number = true;
        let result = handle_set_command(&mut ed, "nonu");
        assert!(matches!(result, CommandResult::Success(_)));
        assert!(!ed.options.number);
    }

    #[test]
    fn query_bool_option() {
        let mut ed = make_editor();
        ed.options.number = true;
        let result = handle_set_command(&mut ed, "number?");
        match result {
            CommandResult::Success(s) => {
                assert_eq!(s.message.as_deref(), Some("  number"));
            }
            _ => panic!("expected success"),
        }
    }

    #[test]
    fn query_bool_option_disabled() {
        let mut ed = make_editor();
        ed.options.number = false;
        let result = handle_set_command(&mut ed, "number?");
        match result {
            CommandResult::Success(s) => {
                assert_eq!(s.message.as_deref(), Some("  nonumber"));
            }
            _ => panic!("expected success"),
        }
    }

    #[test]
    fn set_tabstop() {
        let mut ed = make_editor();
        let result = handle_set_command(&mut ed, "tabstop=8");
        assert!(matches!(result, CommandResult::Success(_)));
        assert_eq!(ed.options.tab_width, 8);
    }

    #[test]
    fn set_tabstop_invalid() {
        let mut ed = make_editor();
        let result = handle_set_command(&mut ed, "tabstop=0");
        assert!(matches!(result, CommandResult::Error(_)));
    }

    #[test]
    fn set_tabstop_alias() {
        let mut ed = make_editor();
        let result = handle_set_command(&mut ed, "ts=2");
        assert!(matches!(result, CommandResult::Success(_)));
        assert_eq!(ed.options.tab_width, 2);
    }

    #[test]
    fn set_and_query_softtabstop() {
        let mut ed = make_editor();
        let result = handle_set_command(&mut ed, "sts=-1");
        assert!(matches!(result, CommandResult::Success(_)));
        assert_eq!(ed.options.soft_tab_stop, -1);

        let result = handle_set_command(&mut ed, "softtabstop?");
        match result {
            CommandResult::Success(s) => {
                assert_eq!(s.message.as_deref(), Some("  softtabstop=-1"));
            }
            _ => panic!("expected success"),
        }
    }

    #[test]
    fn rejects_invalid_softtabstop() {
        let mut ed = make_editor();
        let result = handle_set_command(&mut ed, "sts=-2");
        assert!(matches!(result, CommandResult::Error(_)));
    }

    #[test]
    fn set_copyindent_by_name_and_alias() {
        let mut ed = make_editor();
        assert!(!ed.options.copy_indent);

        assert!(matches!(
            handle_set_command(&mut ed, "copyindent"),
            CommandResult::Success(_)
        ));
        assert!(ed.options.copy_indent);

        assert!(matches!(
            handle_set_command(&mut ed, "noci"),
            CommandResult::Success(_)
        ));
        assert!(!ed.options.copy_indent);
    }

    #[test]
    fn unknown_option() {
        let mut ed = make_editor();
        let result = handle_set_command(&mut ed, "foobar");
        assert!(matches!(result, CommandResult::Error(_)));
    }

    #[test]
    fn unknown_option_query() {
        let mut ed = make_editor();
        let result = handle_set_command(&mut ed, "foobar?");
        assert!(matches!(result, CommandResult::Error(_)));
    }

    #[test]
    fn set_wrap() {
        let mut ed = make_editor();
        ed.options.wrap = false;
        let result = handle_set_command(&mut ed, "wrap");
        assert!(matches!(result, CommandResult::Success(_)));
        assert!(ed.options.wrap);
    }

    #[test]
    fn set_nowrap() {
        let mut ed = make_editor();
        ed.options.wrap = true;
        let result = handle_set_command(&mut ed, "nowrap");
        assert!(matches!(result, CommandResult::Success(_)));
        assert!(!ed.options.wrap);
    }

    #[test]
    fn set_textwidth() {
        let mut ed = make_editor();
        let result = handle_set_command(&mut ed, "textwidth=80");
        assert!(matches!(result, CommandResult::Success(_)));
        assert_eq!(ed.options.textwidth, Some(80));
    }

    #[test]
    fn set_textwidth_zero_disables() {
        let mut ed = make_editor();
        ed.options.textwidth = Some(80);
        let result = handle_set_command(&mut ed, "tw=0");
        assert!(matches!(result, CommandResult::Success(_)));
        assert_eq!(ed.options.textwidth, None);
    }

    #[test]
    fn set_inlay_hints_toggles_option_and_marks_slot_stale() {
        let mut ed = make_editor();
        assert!(!ed.options.inlay_hints, "default is off (OV-00259)");
        let result = handle_set_command(&mut ed, "inlay_hints");
        assert!(matches!(result, CommandResult::Success(_)));
        assert!(ed.options.inlay_hints);
        // Toggling on must invalidate the slot so the next tick re-requests.
        assert!(ed.lsp.slots.inlay_hints.is_stale());
    }

    #[test]
    fn set_noinlay_hints_clears_state() {
        let mut ed = make_editor();
        ed.options.inlay_hints = true;
        ed.lsp.state.inlay_hints.push(lsp_types::InlayHint {
            position: lsp_types::Position::new(0, 0),
            label: lsp_types::InlayHintLabel::String(": x".to_string()),
            kind: None,
            text_edits: None,
            tooltip: None,
            padding_left: None,
            padding_right: None,
            data: None,
        });
        assert_eq!(ed.lsp.state.inlay_hints.len(), 1);
        let result = handle_set_command(&mut ed, "noinlay_hints");
        assert!(matches!(result, CommandResult::Success(_)));
        assert!(!ed.options.inlay_hints);
        // Decorations / cached hints must be wiped immediately.
        assert!(ed.lsp.state.inlay_hints.is_empty());
    }

    #[test]
    fn query_inlay_hints_default_is_no() {
        let mut ed = make_editor();
        let result = handle_set_command(&mut ed, "inlay_hints?");
        match result {
            CommandResult::Success(s) => {
                assert_eq!(s.message.as_deref(), Some("  noinlay_hints"));
            }
            _ => panic!("expected success"),
        }
    }

    #[test]
    fn empty_set_shows_options() {
        let mut ed = make_editor();
        let result = handle_set_command(&mut ed, "");
        match result {
            CommandResult::Success(s) => {
                let msg = s.message.unwrap();
                assert!(msg.contains("number"));
                assert!(msg.contains("tabstop="));
            }
            _ => panic!("expected success"),
        }
    }

    #[test]
    fn toggle_bool_option() {
        let mut ed = make_editor();
        ed.options.number = false;
        let result = handle_set_command(&mut ed, "number!");
        assert!(matches!(result, CommandResult::Success(_)));
        assert!(ed.options.number);
        let result = handle_set_command(&mut ed, "number!");
        assert!(matches!(result, CommandResult::Success(_)));
        assert!(!ed.options.number);
    }

    #[test]
    fn toggle_bool_alias() {
        let mut ed = make_editor();
        ed.options.wrap = true;
        let result = handle_set_command(&mut ed, "wrap!");
        assert!(matches!(result, CommandResult::Success(_)));
        assert!(!ed.options.wrap);
    }

    #[test]
    fn toggle_inlay_hints_runs_side_effects() {
        let mut ed = make_editor();
        assert!(!ed.options.inlay_hints, "default off (OV-00259)");
        // off -> on must mark the slot stale (same as `:set inlay_hints`).
        let result = handle_set_command(&mut ed, "inlay_hints!");
        assert!(matches!(result, CommandResult::Success(_)));
        assert!(ed.options.inlay_hints);
        assert!(ed.lsp.slots.inlay_hints.is_stale());
        // on -> off must clear cached hints (same as `:set noinlay_hints`).
        ed.lsp.state.inlay_hints.push(lsp_types::InlayHint {
            position: lsp_types::Position::new(0, 0),
            label: lsp_types::InlayHintLabel::String(": x".to_string()),
            kind: None,
            text_edits: None,
            tooltip: None,
            padding_left: None,
            padding_right: None,
            data: None,
        });
        let result = handle_set_command(&mut ed, "inlay_hints!");
        assert!(matches!(result, CommandResult::Success(_)));
        assert!(!ed.options.inlay_hints);
        assert!(ed.lsp.state.inlay_hints.is_empty());
    }

    #[test]
    fn toggle_unknown_option_is_error() {
        let mut ed = make_editor();
        let result = handle_set_command(&mut ed, "tabstop!");
        assert!(
            matches!(result, CommandResult::Error(_)),
            "tabstop is not a boolean"
        );
        let result = handle_set_command(&mut ed, "foobar!");
        assert!(matches!(result, CommandResult::Error(_)));
    }
}

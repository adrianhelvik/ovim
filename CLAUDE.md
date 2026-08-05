# ovim

Oxidized Vim — a snappy, batteries-included terminal editor with Vim keybindings, LSP support, and seamless headless mode.

## Quick Reference

```bash
# Build
cargo build --release

# Run editor (supports FILE:LINE:COL)
./target/release/ovim file.txt
./target/release/ovim src/main.rs:42:10
./target/release/ovim file.rs --headless --session dev

# File operations (stateless — no session needed)
ovim edit src/main.rs --old "foo" --new "bar"
ovim insert src/main.rs --after 42 --text "new line"
ovim delete-lines src/main.rs --from 42 --to 45
ovim read-lines src/main.rs --from 40 --to 60

# Session control (always requires -s SESSION)
ovim send "ggK" -s dev
ovim exec "set number" -s dev
ovim context -s dev
ovim buffer -s dev

# LSP commands (grouped under `lsp`)
ovim lsp status -s dev
ovim lsp hover -s dev
ovim lsp check file.rs              # No session needed
ovim lsp languages --verbose        # No session needed

# Session management
ovim session list
ovim session kill -s dev
ovim session health -s dev
ovim session cleanup --dry-run
ovim session cleanup --max-age 7
```

**Sessions are opt-in.** TUI mode doesn't register a session. Headless mode requires `--session NAME`. TUI users can opt in with `:session start NAME`.

## Architecture

```
ovim-core/               # Shared library crate
├── src/
│   ├── syntax/          # Tree-sitter grammars & highlighting
│   │   ├── languages.rs # Language enum & detection
│   │   └── queries/     # Custom .scm highlight queries
│   ├── buffer/          # Rope-based text buffer (ropey)
│   ├── lsp/             # LSP client implementation
│   └── ...
└── languages.toml       # Language configurations (embedded at compile time)

ovim/                    # Binary crate
├── src/
│   ├── api/             # REST API (Axum) - /health, /lsp/status, /snapshot, etc.
│   ├── editor/          # Core logic, operators, motions, LSP actions
│   │   ├── input.rs     # Key event handling
│   │   ├── operators.rs # d, c, y operators
│   │   ├── motions.rs   # Cursor movement
│   │   └── mod.rs       # Main editor state + LSP integration
│   ├── ui/              # Terminal UI (ratatui + crossterm)
│   ├── cli.rs           # CLI argument parsing
│   ├── subcommands.rs   # CLI subcommand handlers
│   └── main.rs          # Event loops (TUI & headless)
```

## Gotchas

- **tree-sitter version conflicts**: We use `tree-sitter = "0.25"`. Some grammar crates require older versions (0.19, 0.20, 0.24). Check docs.rs for compatibility before adding.
- **Workspace structure**: `ovim-core` contains shared logic (syntax, buffer, LSP types), `ovim` is the binary. Language/syntax code lives in ovim-core.
- **Highlight queries**: Some grammars export `HIGHLIGHTS_QUERY`, others `HIGHLIGHT_QUERY` (singular). Some export neither and need custom `.scm` files.
- **eprintln!()**: Breaks TUI rendering. Use only for headless debugging, then remove before committing.
- **Large files**: `editor/mod.rs` is already ~3k lines. Refactor before adding more code there.
- **Multi-agent work**: If tests fail unexpectedly, another agent may be working on the codebase. Don't `git stash` their changes.

## Common Tasks

**Add new CLI subcommand:**
1. Add variant to `Command` enum (or `LspCommand`/`SessionCommand` for grouped commands) in `cli.rs`
2. Implement handler in `subcommands.rs`
3. For session-addressed commands: use `OvimClient` for HTTP requests
4. For file-addressed commands: use direct file I/O (no session needed)

**Add new REST API endpoint:**
1. Add variant to `ApiRequest` in `api/state.rs`
2. Add variant to `ApiResponse` in `api/state.rs`
3. Add handler in `api/handlers.rs`
4. Add route in `api/routes.rs`
5. Handle in `handle_api_request()` in `main.rs`

**Add new MCP tool:**
1. Add tool definition in `api/mcp.rs::get_tools()`
2. Handle in `mcp_handler.rs::handle_tool_call()`
3. Map to existing `ApiRequest` or add new one

**Add new LSP feature:**
1. Add method to `LspManager` in `lsp/mod.rs`
2. Call from `Editor` LSP action methods
3. Add to `pending_lsp_action` enum if async
4. Process in `process_pending_lsp_actions()`

**Add new operator:**
1. Add function in `editor/operators.rs`
2. Call from operator dispatch in `editor/input.rs`
3. Add tests in `tests/`

**Add new language support:**
1. Check tree-sitter grammar crate compatibility with `tree-sitter = "0.25"` on docs.rs
2. Add grammar crate to `ovim-core/Cargo.toml`
3. Add variant to `Language` enum in `ovim-core/src/syntax/languages.rs`
4. Update these functions in `languages.rs`:
   - `detect_from_extension()` - file extension mappings
   - `get_tree_sitter_language()` - grammar binding (e.g., `tree_sitter_foo::LANGUAGE.into()`)
   - `get_highlight_query()` - query source (official constant or custom `.scm` file)
   - `get_lsp_language_id()` - LSP language identifier string
   - `from_info_string()` - markdown code fence support
5. Add language config block to `ovim-core/languages.toml`
6. If grammar doesn't export highlights query, create `ovim-core/src/syntax/queries/<lang>.scm`
7. Update `user-docs/LANGUAGE_SUPPORT.md`
8. Test with `ovim lsp check test.<ext> --verbose`

## Testing

```bash
cargo fmt      # Format code
cargo clippy   # Lints
cargo test     # All tests

# Test specific areas
cargo test syntax --lib              # Syntax highlighting tests
cargo test buffer --lib              # Buffer tests
cargo test -p ovim-core              # Core library tests only

# Verify new language support
ovim lsp check test.sql --verbose    # Check language detection
ovim lsp languages                   # List all languages
```

**Vim-semantics tests must be derived from actual vim behavior, not from the implementation** — verify in `nvim --clean` before writing the assertion, and cite the reference in a comment. See [notes/TESTING_VIM_SEMANTICS.md](notes/TESTING_VIM_SEMANTICS.md).

## Language Support

Run `ovim lsp languages` to see all supported languages.

**Languages with LSP**: Rust, TypeScript, JavaScript, Astro, Python, Java, Kotlin, Scala, Groovy, SQL, C#, Terraform, Go, C, C++, Ruby, Bash, JSON, YAML, HTML, CSS, TOML, Zig, Lua, Elixir

**Syntax highlighting only**: Markdown, HCL

See [user-docs/LANGUAGE_SUPPORT.md](user-docs/LANGUAGE_SUPPORT.md) for installation instructions.

## REST API & MCP

**HTTP Server**: Runs on `http://127.0.0.1:PORT` (random port, stored in session file)

| Endpoint | Method | Use Case |
|----------|--------|----------|
| `/v1/health` | GET | Health + LSP readiness |
| `/v1/lsp/status` | GET | Server states & pending requests |
| `/v1/snapshot` | GET | Complete editor state |
| `/v1/buffer` | GET/PUT | Buffer content |
| `/v1/keys` | POST | Send keystrokes |
| `/v1/command` | POST | Execute ex command |
| `/v1/mcp` | POST | MCP JSON-RPC 2.0 endpoint |

For MCP protocol details, see [user-docs/MCP.md](user-docs/MCP.md).

## Key Implementation Details

### Session Management
- **Sessions are opt-in**: TUI mode doesn't register. Headless requires `--session NAME`.
- `SessionInfo` struct in `session.rs` with PID, port, file, LSP status
- Session files: macOS `~/Library/Caches/ovim/sessions/`, Linux `~/.cache/ovim/sessions/`

### LSP Integration
- `LspManager` coordinates multiple language servers
- `LanguageServer` handles individual server lifecycle
- Debounced `didChange` notifications (150ms) to reduce traffic
- Flush pending changes before hover/goto_definition to avoid stale data

### API Architecture
- Axum server on random port (port 0 → OS assigns)
- Tokio channels communicate with main event loop
- Thread-safe state mutations on main thread

## Debugging

```bash
# LSP logs (headless mode)
./target/release/ovim file.rs --headless 2>&1 | grep LSP-

# Check session files
cat ~/Library/Caches/ovim/sessions/test.json  # macOS
cat ~/.cache/ovim/sessions/test.json          # Linux

# Test endpoints
curl http://127.0.0.1:PORT/v1/health | jq '.'
```

## Best Practices

- **File size**: Keep files under 2k lines, refactor at 3k. Split large files before adding more code.
- **Debug logging**: Remove `eprintln!()` before committing - breaks TUI.
- **Commits**: Commit early and often when code is in a good state.
- **Multi-agent**: If your changes overlap with another agent's uncommitted work, don't commit. Let the user consolidate.
- **Tests failing unexpectedly**: Another agent may be working. Don't `git stash` to check - you'll lose their progress.

## Documentation

- **notes/**: Internal design docs, implementation status, architecture decisions
- **code-docs/**: Codebase structure documentation
- **user-docs/**: User-facing documentation (language support, MCP, etc.)

## User Instructions

Check `PRIORITIES.md` for current priorities. Only check off tasks when done and verified.

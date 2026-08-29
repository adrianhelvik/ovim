# ovim — Oxidized Vim

A fast, batteries-included editor with Vim keybindings, built in Rust. Use it
in the terminal or open the native Tauri/SolidJS interface with `ovim gui`.

ovim gives you what Neovim distros give you. LSP, tree-sitter highlighting, AI chat, sane defaults.

<img alt="ovim fuzzy finder and AI-assisted editing" src="docs/assets/ovim-agent-showcase.gif" />

## What You Get Out of the Box

- **35+ languages** with tree-sitter syntax highlighting, compiled in
- **LSP auto-install** — open a file, the language server downloads and starts
- **AI chat and editing** — `Space Space` to chat or edit a visual selection
- **Vim keybindings** — operators + motions, text objects, visual mode, macros, marks, registers
- **File explorer** — open a folder directly; filter, create, rename, copy, move, and delete safely
- **Test runner** — `Space t n` runs the test under the cursor (cargo, vitest/jest/bun/node:test, pytest, go test), monorepo-aware, with live-streaming results panel
- **Lua config** — `vim.opt.number = true` just works. Configure when you want to, not because you have to.
- **Headless mode** — run without a terminal, control via REST API
- **Native GUI** — a GPU-composited desktop shell with DOM-native editor widgets

## Install

```bash
brew install ovim-editor/tap/ovim
```

<details>
<summary>Build from source instead</summary>

```bash
# Ubuntu/Debian only: native webview build dependencies
sudo apt install libwebkit2gtk-4.1-dev libappindicator3-dev librsvg2-dev patchelf

npm ci --prefix ovim/gui
npm run build --prefix ovim/gui
cargo build --release -p ovim --bins
# binaries at ./target/release/ovim and ./target/release/ovim-gui

# Optional: build native installers for the current platform
npm run bundle --prefix ovim/gui
```

</details>

## Quick Start

```bash
# Open a file
ovim file.rs

# Open the native GUI
ovim gui file.rs

# Open a folder in the file explorer
ovim .

# Jump to a specific line and column
ovim src/main.rs:42:10
```

LSP starts automatically. Syntax highlighting works. No setup needed.

The GUI uses the same editor core, keymaps, buffers, syntax engine, LSP, and
background tasks as the terminal frontend. `ovim gui` starts the Tauri shell
directly—there is no local server or companion-process lookup. Keyboard, IME,
clipboard, mouse, split-pane, picker, tree, diagnostics, test, debug, LSP
manager, and AI-chat interactions flow through Ovim's normal state machinery.
An event-driven, coalescing channel projects bounded visible state into SolidJS
so idle editors do no DOM work and large files or panels cannot create
unbounded webview payloads.

For GUI development, run `npm test --prefix ovim/gui` for the Solid DOM suite
and `cargo test -p ovim gui:: --lib` for bridge, Unicode geometry, wrapping,
split-layout, and projection tests.

> This README uses `ovim` assuming the binary is on your `PATH`.

### File explorer

Open any directory with `ovim some-folder`, or press `-` while editing to open
the project tree. Use `j`/`k` to move, `Enter` or `l` to open, and `h` to
collapse or move to the parent. The explorer includes safe create, rename,
delete, copy, cut, and paste actions; live filtering; hidden and git-ignored
file toggles; and an in-panel `?` key reference.

## Screenshots

<table>
  <tr>
    <td width="50%"><img alt="Fuzzy file finder" src="https://github.com/user-attachments/assets/9f4fc346-50ea-4733-9386-eeb4b0082822" /></td>
    <td width="50%"><img alt="AI code walkthrough" src="https://github.com/user-attachments/assets/917f4647-df45-4600-a75f-b2cd72f79934" /></td>
  </tr>
  <tr>
    <td align="center"><b>Fuzzy finder</b> — jump to any file, live preview</td>
    <td align="center"><b>AI code walkthrough</b> — step-through explanations inline</td>
  </tr>
</table>

## Language Support

23 languages with LSP auto-install, 2 more with manual LSP setup, plus syntax-only languages.

When you open a file and its language server isn't installed, ovim asks once:

- **Enter** — install now
- **A** — always auto-install
- **Esc** — skip

| Languages | LSP Server |
|-----------|------------|
| Rust | rust-analyzer |
| TypeScript / JavaScript | typescript-language-server |
| Astro | astro-ls |
| Python | pyright |
| Go | gopls |
| Java, Kotlin, Scala, Groovy | hyperion-lsp |
| C# | csharp-ls |
| C / C++ | clangd (manual install) |
| Ruby | solargraph |
| Zig | zls |
| Lua | lua-language-server |
| Elixir | elixir-ls |
| Terraform | terraform-ls |
| Bash, SQL, JSON, YAML, HTML, XML, CSS, TOML | various |
| Markdown, HCL, WGSL | syntax highlighting only |

Run `ovim lsp languages` for the full list. See [Language Support](user-docs/LANGUAGE_SUPPORT.md) for details.

## Configuration

Configuration is optional. Create `~/.config/ovim/init.lua` when you're ready to customize:

```lua
vim.opt.number = true
vim.opt.relativenumber = true
vim.opt.tabstop = 4
vim.opt.shiftwidth = 4
vim.opt.scrolloff = 10

-- AI: Codex supplies inference from your ChatGPT subscription. Ovim is the
-- agent harness: it owns context, tools, approvals, edits, and shell programs.
-- On first use, press Enter in Ovim's sign-in dialog to authenticate in your
-- browser. Ovim keeps and refreshes credentials independently from Codex CLI.
-- The first chat can optionally enable Exa web search; reopen setup with /exa.
vim.ai.setup({
  default_profile = "codex_terra",
  contexts = {
    chat = "codex_sol",
    selection = "codex_terra",
    query = "codex_terra",
  },
  profiles = {
    codex_sol = {
      provider = "codex",
      model = "gpt-5.6-sol",
      reasoning_effort = "medium",
    },
    codex_luna = {
      provider = "codex",
      model = "gpt-5.6-luna",
      reasoning_effort = "max",
    },
    codex_terra = {
      provider = "codex",
      model = "gpt-5.6-terra",
      reasoning_effort = "low",
    },
  },
})
```

All `:set` options mirror Vim — `:set wrap`, `:set clipboard=unnamedplus`, `:set textwidth=80`, etc.

To enable Exa for web searches in the AI chat, get an API key at https://exa.ai and set it up in ovim with `:exa`.

See [Configuration](user-docs/configuration.md) and [Options Reference](user-docs/options.md) for the full list.

## Headless Mode & Automation

ovim can run without a terminal — as a programmable text engine with an
authenticated loopback REST API. Ordinary TUI sessions open no API listener.

```bash
# Start a headless session
ovim file.rs --headless --session dev

# Control it from another terminal
ovim send "iHello, world!<Esc>" -s dev
ovim buffer -s dev
ovim exec "set number" -s dev
ovim context -s dev          # 21-line window around cursor
```

### REST API

Every explicit headless session exposes an HTTP server. Ovim's CLI reads the
owner-private session descriptor and authenticates automatically. Custom HTTP
clients must send its `capability` value as `Authorization: Bearer <capability>`;
never log, commit, or share that value.

| Endpoint | Method | Description |
|----------|--------|-------------|
| `/v1/health` | GET | Health check with LSP readiness |
| `/v1/buffer` | GET / PUT | Buffer content |
| `/v1/keys` | POST | Send keystrokes |
| `/v1/command` | POST | Execute ex command |
| `/v1/snapshot` | GET | Complete editor state |
| `/v1/lsp/status` | GET | LSP server states |
| `/v1/mcp` | POST | MCP JSON-RPC 2.0 |

For AI-agent integration, ovim also speaks [MCP](https://modelcontextprotocol.io) over the same session — `ovim install claude` / `ovim install cursor` wires it up. See [MCP docs](user-docs/MCP.md) if you want it.

## CLI Reference

### File Operations (no session needed)

```bash
ovim edit file.rs --old "foo" --new "bar"
ovim insert file.rs --after 42 --text "new line"
ovim delete-lines file.rs --from 42 --to 45
ovim read-lines file.rs --from 40 --to 60
```

### Session Control

```bash
ovim send "ggK" -s dev           # Send keystrokes
ovim exec "set number" -s dev    # Execute ex command
ovim context -s dev              # 21-line context window
ovim buffer -s dev               # Buffer content
ovim search "pattern" -s dev     # Find pattern
```

### LSP Commands

```bash
ovim lsp status -s dev           # Server states
ovim lsp hover -s dev            # Hover info at cursor
ovim lsp check file.rs           # Check language detection
ovim lsp languages               # List all supported languages
```

### Session Management

```bash
ovim session list                # List active sessions
ovim session kill -s dev         # Kill session
ovim session health -s dev       # Health check
ovim session cleanup --dry-run   # Preview stale session cleanup
```

## Architecture

```
ovim-core/    Shared library — buffer, syntax, LSP, session logic
ovim/         Binary — TUI, editor, CLI, REST API
```

Key modules:

- **buffer/** — rope-based text buffer (ropey)
- **syntax/** — tree-sitter grammars and highlight queries
- **lsp/** — language server client with auto-install
- **editor/** — operators, motions, input handling
- **ui/** — terminal rendering (ratatui + crossterm)
- **api/** — REST API and MCP server (Axum)

## Documentation

- [Getting Started](user-docs/getting-started.md)
- [Configuration](user-docs/configuration.md)
- [AI Setup](user-docs/ai.md)
- [Headless & Automation](user-docs/headless.md)
- [Terminal Sessions](user-docs/terminal.md)
- [Language Support](user-docs/LANGUAGE_SUPPORT.md)
- [Options Reference](user-docs/options.md)
- [MCP](user-docs/MCP.md)
- [Troubleshooting](user-docs/troubleshooting.md)

## Contributing

Contributions are welcome!

The repository's `.editorconfig` is the cross-editor whitespace baseline.
`.gitattributes` keeps committed text on LF line endings. `rustfmt.toml` and
`cargo fmt` remain authoritative for Rust layout, while Clippy covers semantic
linting. The pre-commit hooks enforce both formatter and lint checks before
changes land.

To run the same formatting and Clippy checks as CI before every commit, install
[pre-commit](https://pre-commit.com/) and enable the repository hooks:

```bash
pre-commit install
```

Run the checks manually with `pre-commit run --all-files`. The full test suite
remains available through `cargo test --workspace`.

## License

[MIT](LICENSE)

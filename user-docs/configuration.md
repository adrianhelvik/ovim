# Configuration

## Lua Config (`init.lua`)

Create `~/.config/ovim/init.lua`:

```lua
vim.opt.number = true
vim.opt.relativenumber = false
vim.opt.tabstop = 4
vim.opt.shiftwidth = 4
vim.opt.expandtab = true
```

Reload:

- `:ConfigReload` (ovim-specific)
- `:reload`

## AI Configuration

AI is configured primarily through the Lua API (`vim.ai.setup(...)`) in `init.lua`.

See [ai.md](ai.md) for:

- Lua-first AI profile configuration
- Reusable Agent Skills-compatible Markdown files
- Secure API key setup without `~/.zshrc`
- Legacy `ai.toml` compatibility and provider naming differences

## Options (`:set`)

Options mirror Vim-style `:set` behavior.

Examples:

```vim
:set number
:set nonumber
:set scrolloff=10
:set wrap
:set nowrap
:set clipboard=
:set textwidth=80
```

See [options.md](options.md) for details.

## Project formatting (`.editorconfig`)

ovim reads EditorConfig indentation settings when a file is opened. A minimal
project policy looks like this:

```ini
root = true

[*]
indent_style = space
indent_size = 4

[*.{json,yaml,yml}]
indent_size = 2
```

Use EditorConfig for cross-editor whitespace policy and keep language-native
formatters (such as rustfmt) as the authority for syntax-aware layout. See the
[indentation options](options.md#editorconfig) for supported properties and
precedence.

Inside ovim, use `=` for a fast structural reindent. It understands paired
delimiters, ignores delimiters in comments and strings, and normalizes the
selected indentation to the buffer's effective policy. Use `gq` or `:format`
for full-document formatting through the active language server.

For repositories, the complementary layers are:

- `.editorconfig` for indentation and whitespace policy across editors.
- `.gitattributes` for canonical line endings in Git.
- A language formatter (`rustfmt`, Prettier, Black, and similar) for syntax
  layout.
- Formatter/linter checks in pre-commit and CI so policy is enforced, not only
  suggested.

## Language Configuration (`languages.toml`)

ovim ships with a default language configuration. Override or extend it in:

`~/.config/ovim/languages.toml`

Validate detection and LSP setup for a file without starting a session:

```bash
ovim lsp check path/to/file.rs
ovim lsp check path/to/file.rs --verbose
```

List configured languages:

```bash
ovim lsp languages
ovim lsp languages --verbose
```

See [LANGUAGE_SUPPORT.md](LANGUAGE_SUPPORT.md) for examples.

### Test runners (`[language.test]`)

Rust, JavaScript/TypeScript, Python, and Go have built-in test runners (see
[getting-started.md](getting-started.md#running-tests)). For other languages,
or to override a built-in, add a `test` section to the language:

```toml
[[language]]
id = "elixir"

[language.test]
suite_command = "mix test"
file_command = "mix test {file}"
nearest_command = "mix test {file}:{line}"
root_markers = ["mix.exs"]
```

Placeholders (substituted values arrive already shell-quoted — don't add
your own quotes around them):

- `{file}` — the test file, relative to the project root
- `{line}` — 1-indexed cursor line
- `{name}` — the nearest test's full name (namespaces joined with spaces),
  e.g. `nearest_command = "runner -t {name} {file}"`

Commands run with the resolved project root (first `root_markers` hit walking
up from the file) as working directory. Omitted commands fall back to the
built-in runner if the language has one.

## User Language Plugins

A language plugin adds support for a new language — file detection, a native
Tree-sitter parser, highlight queries, and an LSP server — without rebuilding
ovim. It is a directory containing an `init.lua`, placed under `plugins/` in
your config directory:

```text
~/.config/ovim/plugins/mylang/
├── init.lua
├── parser/mylang.dylib       # .so on Linux, .dll on Windows
└── queries/mylang/highlights.scm
```

```lua
-- ~/.config/ovim/plugins/mylang/init.lua
ovim.languages.register({
  id = "mylang",
  name = "My Language",
  files = { extensions = { "myl" } },
  syntax = {
    parser = { path = "parser/mylang", symbol = "tree_sitter_mylang" },
    highlights = "queries/mylang/highlights.scm",
  },
  lsp = {
    cmd = { "mylang-lsp", "--stdio" },
    language_id = "mylang",
    root_markers = { "mylang.toml", ".git" },
  },
})
```

Relative paths resolve from the `init.lua` that declares the language. If the
parser path has no extension, ovim appends the platform's native-library
extension. Registration validates the parser ABI and the highlight query up
front, so a broken plugin never leaves you with a half-registered language.

While developing a language, point the plugin directory at your checkout with a
symlink, so you don't have to copy files after every grammar rebuild:

```bash
ln -s ~/src/mylang/editors/ovim ~/.config/ovim/plugins/mylang
```

Registered languages work everywhere built-in languages do. Markdown fenced
code blocks (` ```mylang `), LSP hover previews, and AI chat responses resolve
fence labels against language ids, built-in aliases, and file extensions, so
your language also highlights inside documents.

Reloading is safe. Registering the same language id again from the same config
file or plugin replaces the earlier registration, which keeps `:ConfigReload`
and `:source` idempotent. One plugin cannot take over an id that another plugin
— or your own config — already registered. Already-open buffers keep their
current highlighter; reopen the file (or restart ovim) to pick up a changed
parser or query.

When a config or plugin file fails to load, ovim shows a sticky error toast
naming the failing file and writes the full Lua traceback to the log
(`~/.cache/ovim/ovim.log`, or `~/Library/Caches/ovim/ovim.log` on macOS). An
error in `init.lua` stops execution at the failing line, so settings after that
line do not apply.

The current API covers file extensions, a single highlight query, and a single
LSP command per language.

## Session Directory Override (Advanced)

By default, session files are stored under your OS cache directory:

- macOS: `~/Library/Caches/ovim/sessions`
- Linux: `~/.cache/ovim/sessions`

You can override this location by setting:

`OVIM_SESSION_DIR=/path/to/dir`

This affects session file reads/writes and cleanup.
Ovim secures the selected directory for owner-only access on Unix because each
named headless session descriptor contains its API bearer capability.

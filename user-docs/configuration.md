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

See `options.md` for details.

## Language Configuration (`languages.toml`)

ovim ships with default language config, and you can override/extend it with:

`~/.config/ovim/languages.toml`

You can validate detection/LSP setup for a file without starting a session:

```bash
ovim lsp check path/to/file.rs
ovim lsp check path/to/file.rs --verbose
```

List configured languages:

```bash
ovim lsp languages
ovim lsp languages --verbose
```

See `LANGUAGE_SUPPORT.md` for examples.

## User Language Plugins

Add a child directory containing `init.lua` under the active config directory's
`plugins` directory. A plugin can register file detection, a native Tree-sitter
parser, highlight queries, and one LSP command without rebuilding ovim:

```text
~/.config/ovim/plugins/nula/
├── init.lua
├── parser/nula.dylib       # .so on Linux, .dll on Windows
└── queries/nula/highlights.scm
```

```lua
-- ~/.config/ovim/plugins/nula/init.lua
ovim.languages.register({
  id = "nula",
  name = "Nula",
  files = { extensions = { "nula" } },
  syntax = {
    parser = { path = "parser/nula", symbol = "tree_sitter_nula" },
    highlights = "queries/nula/highlights.scm",
  },
  lsp = {
    cmd = { "nula", "lsp" },
    language_id = "nula",
    root_markers = { "nula.toml", ".git" },
  },
})
```

The plugin directory may be a symlink into a local checkout, which is handy
while developing a language:

```bash
ln -s ~/Projects/nula/editors/ovim ~/.config/ovim/plugins/nula
```

Relative paths are resolved from the `init.lua` that declares the language. If
the parser path has no extension, ovim adds the platform's native-library
extension. Registration validates the parser ABI and highlight query before the
language becomes visible.

Re-registering a language id from the same config file or plugin replaces the
earlier registration, so `:ConfigReload` and `:source` are idempotent. A
different owner (another plugin, or the user config vs. a plugin) cannot take
over an already-registered id. Already-open buffers keep their current
highlighter; reopen the file (or restart) to pick up a changed parser or query.

If a config or plugin file fails to load, ovim shows a sticky error toast with
the failing file and writes the full Lua traceback to the log
(`~/.cache/ovim/ovim.log`, or `~/Library/Caches/ovim/ovim.log` on macOS). Note
that an error in `init.lua` stops execution at the failing line, so settings
after it will not apply. The initial API supports extensions, one highlight
query, and one LSP per language.

## Session Directory Override (Advanced)

By default, session files are stored under your OS cache directory:

- macOS: `~/Library/Caches/ovim/sessions`
- Linux: `~/.cache/ovim/sessions`

You can override this location by setting:

`OVIM_SESSION_DIR=/path/to/dir`

This affects session file reads/writes and cleanup.

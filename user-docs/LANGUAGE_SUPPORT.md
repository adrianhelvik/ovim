# Language Support in ovim

**ovim** provides syntax highlighting and Language Server Protocol (LSP) support for multiple programming languages through a declarative configuration system.

## Supported Languages

### Languages with LSP + Auto-Install

These languages have full LSP support and will auto-install the language server when needed:

| Language | Extensions | LSP Server | Install Method |
|----------|------------|------------|----------------|
| Rust | `.rs` | rust-analyzer | rustup |
| TypeScript | `.ts`, `.tsx`, `.mts`, `.cts` | typescript-language-server | npm |
| JavaScript | `.js`, `.jsx`, `.mjs`, `.cjs` | typescript-language-server | npm |
| Python | `.py`, `.pyw`, `.pyi` | pyright-langserver | npm |
| Go | `.go` | gopls | go install |
| SQL | `.sql`, `.mysql`, `.pgsql`, `.sqlite` | sqls | go install |
| C# | `.cs`, `.csx` | csharp-ls | dotnet tool |
| Bash | `.sh`, `.bash` | bash-language-server | npm |
| JSON | `.json`, `.jsonc` | vscode-json-language-server | npm |
| YAML | `.yaml`, `.yml` | yaml-language-server | npm |
| HTML | `.html`, `.htm` | vscode-html-language-server | npm |
| Astro | `.astro` | astro-ls | npm |
| CSS | `.css`, `.scss`, `.sass` | vscode-css-language-server | npm |
| TOML | `.toml` | taplo | cargo |
| Ruby | `.rb`, `.rake`, `.gemspec` | solargraph | gem |
| Java | `.java` | hyperion-lsp | auto-download |
| Kotlin | `.kt`, `.kts` | hyperion-lsp | auto-download |
| Scala | `.scala`, `.sc` | hyperion-lsp | auto-download |
| Groovy | `.groovy`, `.gradle` | hyperion-lsp | auto-download |
| Zig | `.zig` | zls | GitHub release |
| Lua | `.lua` | lua-language-server | GitHub release |
| Terraform | `.tf`, `.tfvars` | terraform-ls | GitHub release |
| Elixir | `.ex`, `.exs` | elixir-ls | GitHub release |

### Languages with LSP (Manual Install Required)

| Language | Extensions | LSP Server | Install Command |
|----------|------------|------------|-----------------|
| C | `.c`, `.h` | clangd | `brew install llvm` / `pacman -S clang` |
| C++ | `.cpp`, `.hpp` | clangd | `brew install llvm` / `pacman -S clang` |

### Syntax Highlighting Only

- Markdown (`.md`, `.markdown`)
- HCL (`.hcl`, `.nomad`, `.vault`)
- WGSL (`.wgsl`), including Bevy shader preprocessor directives

## Auto-Install

When you open a file and its language server isn't installed, ovim will:

1. Show a consent dialog asking if you'd like to install the server
2. Display the install method (e.g., `npm install pyright (sandboxed in ~/.local/share/ovim/lsp)`)
3. Wait for your response:
   - **Enter** — install this time
   - **A** — always auto-install (sets `autoinstall=auto`)
   - **Esc** — skip

### Sandboxed Installs

Node and cargo installs are sandboxed, mason-style: each package gets its own
directory under `~/.local/share/ovim/lsp/` (e.g. `npm/astrojs-language-server/`),
and ovim resolves server commands from each sandbox's bin directory. Your
global npm/cargo namespace is never touched, and pinned dependencies
(astro-ls needs `typescript@6`) can't conflict with the versions your
projects use.

Node packages install with two supply-chain guards: `npm install --before`
restricts resolution to versions published at least 3 days ago (hijacked
releases are typically caught within hours, well inside that window), and
`--ignore-scripts` refuses to run install scripts, the usual malware entry
point. Language servers are plain JavaScript, so neither guard affects
functionality. A brand-new package with no version older than 3 days won't
resolve until it ages; that is the quarantine working as intended.

Servers you've installed yourself always win: ovim checks `PATH` and
project-local `node_modules/.bin` before its own sandbox. To reclaim disk
space, deleting `~/.local/share/ovim/lsp/` is always safe — ovim reinstalls
on demand.

### Configuring Auto-Install Behavior

```vim
:set autoinstall=prompt    " Show consent dialog (default)
:set autoinstall=auto      " Install automatically without asking
:set autoinstall=off       " Never auto-install, only show hints
:set autoinstall?          " Show current setting
```

### Disabling Auto-Install for a Specific Language

Create `~/.config/ovim/languages.toml` and override the language entry:

```toml
[[language]]
id = "python"

[language.lsp]
command = "pyright-langserver"
args = ["--stdio"]
# Omit [language.lsp.auto_install] to disable auto-install for this language
```

## Checking LSP Status

```bash
ovim lsp languages           # List all languages
ovim lsp languages --verbose  # Detailed configuration
ovim lsp check src/main.rs    # Check specific file
```

## Customizing Language Support

Override or extend language support by creating `~/.config/ovim/languages.toml`. User config merges with the built-in config, with user entries taking priority.

### LSP Configuration

```toml
[language.lsp]
command = "rust-analyzer"          # Primary command (searched in PATH)
args = ["--stdio"]                 # Command-line arguments
fallback_commands = [              # Fallback locations
    "~/.local/bin/rust-analyzer"
]
root_markers = ["Cargo.toml"]      # Project root detection
install_hint = "Install with: ..."  # Shown when server not found
```

### Auto-Install Configuration

```toml
# npm-based (sandboxed under ~/.local/share/ovim/lsp; add global = true
# to install with `npm install -g` instead)
[language.lsp.auto_install]
method = { type = "npm", package = "pyright", bin = "pyright-langserver" }

# cargo-based (sandboxed under ~/.local/share/ovim/lsp)
[language.lsp.auto_install]
method = { type = "cargo", package = "taplo-cli", bin = "taplo", features = ["lsp"] }

# GitHub release (supports archives)
[language.lsp.auto_install]
method = { type = "github", repo = "zigtools/zls", asset_pattern = "zls-*-{arch}-{os}*", install_path = "~/.local/bin/zls", binary_name = "zls" }

# Shell command
[language.lsp.auto_install]
method = { type = "shell", command = "gem install solargraph" }
```

Asset patterns support `{os}` (linux/darwin/macos) and `{arch}` (x86_64/aarch64/amd64/arm64) placeholders.

## Troubleshooting

### LSP Not Working

1. Check if language is detected: `ovim lsp check yourfile.ext`
2. Check if LSP server is installed: `which rust-analyzer`
3. List all languages: `ovim lsp languages --verbose`

### LSP Starts But No Features

- **Wrong project root** — add proper `root_markers` to your language config
- **LSP initializing** — some servers take time to index; check the status bar
- **LSP server error** — check logs with `ovim file.rs --headless 2>&1 | grep LSP`

### Markdown UI options
- `:set mdc` / `:set nomdc` — conceal inline link URLs and image targets (cursor line always shows raw markdown).

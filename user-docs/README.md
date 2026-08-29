# ovim User Documentation

User-facing documentation for running and configuring ovim (Oxidized Vim).

## Getting Started

```bash
ovim file.txt                               # Open a file
ovim file.rs --headless --session dev       # Headless mode with named session
```

## Docs

- [Getting Started](getting-started.md) - Build/install, open files, basic workflow
- [Configuration](configuration.md) - `init.lua`, `languages.toml`, and common tweaks
- [AI Setup](ai.md) - ChatGPT sign-in, Lua-first AI config, API keys, and `ai.toml` compatibility
- [Headless & Automation](headless.md) - Sessions, REST API, subcommands
- [Language Support](LANGUAGE_SUPPORT.md) - LSP + syntax support and adding languages
- [Options](options.md) - `:set` options reference (scrolling, wrap, clipboard, etc.)
- [Terminal Sessions](terminal.md) - `:terminal`, `:term`, `:shell`, and `:!command`
- [Troubleshooting](troubleshooting.md) - Common issues (sessions, LSP, dependencies)

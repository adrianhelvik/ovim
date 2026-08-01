---
name: understand-ovim-config
description: Inspect and explain how a user has configured Ovim, including Lua options and keymaps, plugins, language overrides, skills, and AI profiles.
---

Build a concise, evidence-based map of the user's Ovim configuration.

1. Locate configuration before reading it. Respect the active path precedence:
   - `init.lua`: `$OVIM_CONFIG/init.lua`, then `$XDG_CONFIG_HOME/ovim/init.lua`, then `~/.config/ovim/init.lua`, then `~/.ovim/init.lua`; only the first existing file is loaded.
   - Plugins: child directories containing `init.lua` under the corresponding `plugins` directories. Ovim loads built-in Lua defaults first, the user's `init.lua` second, and plugins last.
   - Also check `languages.toml`, the `skills/*.md` metadata, and legacy `ai.toml` only when they are relevant to the question.

2. Inspect only existing, relevant files. Configuration normally lives outside the workspace, so use a narrowly scoped shell read if project file tools cannot access it. Explain the access request rather than broadening it. Never print credential files, secret values, API keys, tokens, or an entire environment; names of referenced environment variables are enough.

3. Trace what the configuration actually does. Summarize user-set options, keymaps, commands, AI profiles/contexts, language overrides, and plugin effects. Follow local Lua files that are loaded by the active configuration when needed. Distinguish unconditional settings from host-, environment-, or file-dependent branches.

4. Do not assume Ovim implements every Neovim API. When a call's behavior matters, verify it against Ovim's user documentation or implementation. Call out unsupported or no-op configuration only when there is evidence.

5. Separate shipped defaults from user overrides and static intent from observed runtime state. If precedence or dynamic Lua prevents a firm conclusion, say so instead of guessing. End with the smallest useful configuration map and any concrete issue the user asked about; do not inventory unrelated settings.

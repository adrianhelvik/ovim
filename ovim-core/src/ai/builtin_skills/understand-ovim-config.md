---
name: understand-ovim-config
description: Inspect and explain how a user has configured Ovim, including Lua options and keymaps, plugins, language overrides, skills, and AI profiles.
---

Build a concise, evidence-based map of the user's Ovim configuration.

1. Locate configuration before reading it. Respect the active path precedence:
   - `init.lua`: `$OVIM_CONFIG/init.lua`, then `$XDG_CONFIG_HOME/ovim/init.lua`, then `~/.config/ovim/init.lua`, then `~/.ovim/init.lua`; only the first existing file is loaded.
   - Plugins: child directories containing `init.lua` under the corresponding `plugins` directories. Ovim loads built-in Lua defaults first, the user's `init.lua` second, and plugins last.
   - Also check `languages.toml`, the `skills/*.md` metadata, and legacy `ai.toml` only when they are relevant to the question.

2. Inspect only existing, relevant files. Configuration normally lives outside the workspace, so pass its absolute path to the native file tools and let Ovim request access when needed. Use a narrowly scoped shell read only if the native tools are insufficient. Never print credential files, secret values, API keys, tokens, or an entire environment; names of referenced environment variables are enough.

3. Trace what the configuration actually does. Summarize user-set options, keymaps, commands, AI profiles/contexts, language overrides, and plugin effects. Follow local Lua files that are loaded by the active configuration when needed. Distinguish unconditional settings from host-, environment-, or file-dependent branches.

4. Do not assume Ovim implements every Neovim API. When a call's behavior matters, verify it against Ovim's user documentation or implementation. Call out unsupported or no-op configuration only when there is evidence.

5. Separate shipped defaults from user overrides and static intent from observed runtime state. If precedence or dynamic Lua prevents a firm conclusion, say so instead of guessing. End with the smallest useful configuration map and any concrete issue the user asked about; do not inventory unrelated settings.

When the user asks to add or manage skills, work in the active configuration root's `skills` directory. Skills are flat Markdown files; nested directories and non-`.md` files are ignored. Each file must begin with YAML frontmatter and may then contain normal Markdown instructions:

```markdown
---
name: review-code
description: Review a change for correctness, regressions, and missing tests.
---

Read the diff first. Report concrete findings before a summary.
```

Names use lowercase ASCII letters, digits, and internal hyphens; descriptions should be short and specific enough for the agent to decide when activation is useful. Keep instructions focused, because the complete body enters the system prompt only after activation. Do not try to replace an Ovim built-in with a user skill of the same name. Restart Ovim after changing skill files so the catalog is rediscovered.

# Troubleshooting

## Sessions not found

Symptoms:

- `Session '<name>' not found`
- `ovim session list` shows no sessions

Checks:

- Ensure you started headless with `--headless --session <name>`.
- Verify the session dir:
  - macOS: `~/Library/Caches/ovim/sessions`
  - Linux: `~/.cache/ovim/sessions`
- If you set `OVIM_SESSION_DIR`, make sure your tooling uses the same location.

## LSP not working

First, check language detection/LSP configuration (no session required):

```bash
ovim lsp check path/to/file.ext
ovim lsp check path/to/file.ext --verbose
```

Then, for a running headless session:

```bash
ovim lsp status -s dev
ovim lsp wait -s dev --timeout 30000
```

Common causes:

- LSP server not installed / not on `PATH`
- Wrong project root (adjust `root_markers` in `languages.toml`)
- Large project indexing delay (wait for readiness)

## Codex sign-in or repeated 401 errors

Ovim signs in to ChatGPT independently from Codex CLI. Open AI chat with
`Space Space`, then press `Enter` in the Ovim sign-in dialog and complete the
browser flow. A visual selection attached with `Space Space` uses the same chat
and sign-in flow.

Ovim refreshes expiring credentials automatically and retries one inference
request after an unexpected `401 Unauthorized`. If the refresh token is
rejected, the dialog asks you to sign in again while preserving the current
draft or unchanged selection.

To force an account change or replace damaged credentials, close Ovim, remove
`ovim/codex-auth.json` from the platform config directory, and reopen AI chat:

- macOS: `~/Library/Application Support/ovim/codex-auth.json`
- Linux: `~/.config/ovim/codex-auth.json`

Do not copy `~/.codex/auth.json` into Ovim. Sharing rotating refresh tokens with
Codex CLI can make either application lose authentication periodically.

## Logs & debug mode

If something “mysteriously” fails (or the UI gets corrupted), the first thing to grab is the log files.

Default locations:

- macOS: `~/Library/Caches/ovim/ovim.log` and `~/Library/Caches/ovim/lsp.log`
- Linux: `~/.cache/ovim/ovim.log` and `~/.cache/ovim/lsp.log`

Overrides:

- `XDG_CACHE_HOME` changes the base cache dir on most systems.

Useful env vars:

- `OVIM_DEBUG=1` enables extra app debug logging.
- `OVIM_LSP_DEBUG=1` enables verbose LSP debug logging (can be noisy).

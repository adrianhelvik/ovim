# Headless & Automation

Headless mode is designed for tests, CI, and automation. It runs ovim without the TUI and exposes a local API.

## Start a Session

```bash
ovim path/to/file.rs --headless --session dev --dimension 100x30
```

The logical viewport is initialized before the API starts accepting input.
Motions, wrapping, scrolling, snapshots, and renders therefore use the same
dimensions. Change it later with `ovim resize -s dev 120x40`.

## Inspect and Control Sessions

### Subcommands

```bash
ovim session list
ovim session health -s dev
ovim snapshot -s dev --format pretty
ovim send -s dev "iHello<Esc>"
ovim paste -s dev 'literal text\nincluding newlines'
ovim resize -s dev 120x40
ovim exec -s dev "w"
ovim session kill -s dev
```

JSON snapshots carry a `schema_version` and a `view` object containing viewport,
scroll, tab, split, file-tree, command/search, and status state. They include an
`ai_chat` object whenever a chat is active. It reports focus, streaming/review
state, current composer text and cursor, pending approval, scheduled inputs,
and message history. The `activity` field is the authoritative lifecycle state:
`idle`, `inference`, `classifying_tool`, `running_shell`, `running_web`,
`waiting_tool_approval`, or `waiting_folder_approval`. Prefer it over inferring
ownership from compatibility booleans such as `waiting` and `streaming`. The
`attention_generation` value increases for each new blocking agent approval,
so a headless client can raise its own notification once per prompt. Completed
tool messages expose a compact summary; their arguments appear when that tool
row is expanded in the UI. Automation can therefore monitor turns without
parsing the rendered terminal grid.

`ovim send` accepts Unicode and Vim-style key names/modifiers. Use `ovim paste`
for literal or multiline input so it is delivered as one bracketed-paste event.

### Editing a live session

The file-operation commands normally read or write the file directly. Add
`--session` to operate on the live editor buffer instead:

```bash
ovim edit src/main.rs --old before --new after --session dev
ovim insert src/main.rs --after 10 --text 'new line' --session dev
ovim delete-lines src/main.rs --from 20 --to 22 --session dev
ovim read-lines src/main.rs --from 1 --to 30 --session dev
ovim exec -s dev w
```

The file argument must match the session's active buffer. Session-aware edits
remain unsaved until `:w`, preserving undo, LSP synchronization, diagnostics,
and render invalidation. A clean headless buffer automatically reloads external
disk changes. If the buffer has local changes, ovim keeps them and refuses a
plain `:w` rather than overwriting the external version; use `:e!` or `:w!` to
make that choice explicitly.

AI chat uses the same background poller and input dispatcher in headless mode
as in the TUI. Open editable chat with `Space Space`, type a request, and submit
with Enter:

```bash
ovim send -s dev "  "
ovim send -s dev "inspect the project<Enter>"
```

For unattended use, complete Ovim's contextual Codex sign-in once in the TUI
before starting the headless session. The credential is stored in the same
platform config directory and refreshes automatically. If credentials need
renewal during a headless session, the auth dialog blocks inference without
consuming the draft; attach a TUI or complete sign-in in a regular Ovim session
before retrying.

If auto mode pauses an Ovim tool for approval, the agent round remains blocked
until the decision arrives. Inspect it with `ovim snapshot -s dev`, then
send `<C-y>` (or `<Enter>`) to allow once, or `<C-n>` (or `<Esc>`) to deny. The
50 ms headless background tick also polls Terra classifier completions; no
renderer or attached terminal is required.

For a trusted session, enter `/yolo on` in the chat composer to bypass Terra and
interactive approvals for that chat; `/yolo off` restores normal policy. The
snapshot's `ai_chat.yolo_mode` field reports the current setting.

Comprehension policy is also scriptable: submit `/comprehension publish`,
`/comprehension commit`, or `/comprehension off`. Snapshots report the selected
mode in `ai_chat.comprehension_policy` and include
`ai_chat.comprehension_checkpoint` only while a recorded checkpoint still
covers the current repository content. Comprehension gates remain active in
YOLO mode.

LSP helpers:

```bash
ovim lsp wait -s dev --timeout 30000
ovim lsp status -s dev
ovim lsp hover -s dev
```

## Session Files

Session files are JSON and live in:

- macOS: `~/Library/Caches/ovim/sessions`
- Linux: `~/.cache/ovim/sessions`

Override with:

```bash
export OVIM_SESSION_DIR=/path/to/ovim-sessions
```

## Cleanup

Remove stale/expired/corrupted sessions:

```bash
ovim session cleanup --dry-run
ovim session cleanup
ovim session cleanup --max-age 7
```

## Output & logs

- Headless mode may print basic status/errors to stderr (safe without the TUI).
- For debugging, check `ovim.log` and `lsp.log` in the ovim cache dir (see `troubleshooting.md`).

## REST API (reference)

When headless, ovim exposes endpoints like:

- `GET /health`
- `GET /snapshot`
- `POST /keys`
- `POST /paste`
- `POST /resize`
- `POST /command`
- `POST /edit`
- `POST /insert`
- `POST /delete-lines`
- `GET /lines`

Use `ovim snapshot -s <name>` instead of calling the API directly unless you need custom tooling.

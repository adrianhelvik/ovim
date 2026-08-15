# AI Setup

This guide covers practical AI configuration in ovim:

- Recommended setup: Codex with a ChatGPT/Codex subscription
- Preferred setup: Lua API in `init.lua`
- Secure API key handling without `~/.zshrc`
- Legacy `ai.toml` compatibility

## Recommendation

Open AI chat with `Space Space`. Visually select text and press `Space Space`
to open the same chat with that code attached to the composer. When a direct
Codex profile needs credentials, Ovim shows a
sign-in dialog at the point of use:

1. Press `Enter` to open ChatGPT sign-in in your browser.
2. Complete sign-in and return to Ovim; the pending draft or unchanged
   selection resumes automatically.
3. Press `Esc` instead to dismiss the dialog without losing the draft or
   selection.

Ovim stores its own refreshable credentials as `ovim/codex-auth.json` in the
platform config directory (`~/.config` on Linux,
`~/Library/Application Support` on macOS). It refreshes them before expiry and,
after an unexpected `401 Unauthorized`, refreshes and retries once.

Ovim deliberately does not import, share, or refresh Codex CLI credentials.
OAuth refresh tokens rotate, so two applications using the same credential
lineage can periodically invalidate each other. Codex CLI may be installed and
signed in separately, but it is not required for Ovim.

The built-in defaults use `gpt-5.6-sol` at medium effort for chat and
`gpt-5.6-terra` at low effort for selection edits and read-only queries. With
the default `codex` provider, Ovim—not Codex app-server—is the agent harness.
Ovim sends its own tool schemas, records tool intent, applies auto-mode policy,
executes approved effects in the repository, and returns results for the next
inference round. Codex's read-only workspace sandbox is therefore not involved
in repository reads or writes.

When Ovim starts outside Git, an editable chat asks before treating the current
folder as its project boundary. Approving creates a durable, folder-scoped chat
identity and enables shell and mutation tools only within that folder. Denying
or dismissing the prompt keeps those tools disabled. Read-only chats never gain
shell or mutation access, even for an approved folder.

On the first direct-Codex chat, Ovim offers to enable live web search through
[Exa](https://dashboard.exa.ai/api-keys). Paste an Exa API key into the dialog,
or click the link to sign in and create one. The dialog is dismissible and can
always be reopened with `/exa`. Ovim stores the key beside `codex-auth.json` as
`ovim/exa-auth.json` in the platform config directory, using an atomic,
owner-only file on Unix. Set `EXA_API_KEY` to supply a key without storing it;
the environment value takes precedence.

The Ovim harness exposes `web_search` and `web_fetch` only while a usable key is
configured. A rejected or revoked stored key reopens the setup dialog. Credit
or budget exhaustion links to Exa's dashboard without discarding the key, and
temporary rate limits and server failures receive one bounded retry. Web tools
are read-only Ovim operations: they do not invoke a shell and do not require a
Codex sandbox or Terra approval.

Auto mode is the default. Read-only local inspection and tests run immediately;
context-dependent commands are reviewed by subscription-backed Terra at low
effort. Terra treats routine project-local formatting, building, linting, and
testing as authorized when they are reasonable steps toward your requested
implementation objective. Elevated privileges, credential access,
outside-project effects, remote-code pipelines, ambiguous authorization, and
classifier failures pause for you. Press Enter or Ctrl-Y to allow once, Ctrl-A
to allow the requested folder for the chat, or Esc/Ctrl-N to deny. Installed
skill packages and source files in Cargo's local registry cache are trusted
read-only inputs and do not trigger outside-project approval prompts. To opt
out of auto mode, set `tool_approval_mode = "sensitive_prompt"` or
`"always_prompt"` in legacy `ai.toml`.

For trusted work where approval interruptions are more costly than the safety
gate, click `YOLO OFF` at the top right of the chat to switch it to `YOLO ON`.
YOLO is opt-in per chat and defaults off. It bypasses Terra and interactive tool
approval prompts, and immediately releases a request already waiting for
approval. It does not disable malformed-input checks, `..` traversal rejection,
project-context requirements, or durable-run ownership checks. Click again or
run `/yolo off` to restore normal policy.

### Comprehension checkpoints

The chat header also has a `COMPREHENSION` control. It is independent from
YOLO: approval bypasses never bypass a comprehension boundary. Clicking the
control toggles the recommended `PUBLISH` policy, which lets the agent work and
make local commits but requires demonstrated understanding before `git push`,
PR creation/merge, or release publication.

Use `/comprehension off|publish|commit` for explicit control. `commit` also
checks local commits. When a boundary is reached, the agent teaches a compact
end-to-end mental model and checks critical concepts one question at a time.
The bar is determined by the change's risk; explanations and hints may be
broken into smaller steps, but required mastery is not relaxed. Questions focus
on behavior, invariants, realistic failure modes, and verification rather than
line-number or syntax trivia.

When the panel is wide enough, the selected model profile and reasoning effort
appear immediately to the left of `COMPREHENSION`. Click either control to open
the combined picker downward. Up/Down changes the active value, Tab switches
between model and effort, and Enter closes the picker. An effort selected here
overrides the profile for this chat only; `default` returns to the profile's
configured effort.

After the user demonstrates the critical concepts, Ovim binds a checkpoint to
the repository's current index and worktree content. Meaningful subsequent
changes make it stale automatically. A local commit that preserves the same
content does not force a duplicate drill before the corresponding push.

The docked chat width is adjustable: drag its left separator toward the editor
to make the chat wider, or toward the right to give the buffer more room. Ovim
keeps the chosen proportion for that chat as the terminal is resized.

When an active agent pauses for one of these approval decisions, Ovim emits the
terminal bell once. Whether that is audible, visual, or suppressed is controlled
by the terminal's bell settings. The notification is tied to the new prompt,
not to rendering, so an unattended prompt does not repeatedly ring.

Chat conversations are owned by Ovim. Each inference request replays the active
conversation branch, tool calls and results, and provider-encrypted reasoning
state. Forking or clearing a conversation therefore changes Ovim's branch
without depending on a hidden provider thread.

Agent turns have no tool-call ceiling by default; the lightning indicator in
the status line is a count, not a countdown. A profile may opt into a finite
guardrail with `max_tool_calls = 100`, in which case Ovim displays both the
current count and limit. Omitting the setting—or setting it to `0` in legacy
configuration—keeps long-running turns unlimited.

## Skills

Ovim includes `understand-ovim-config`, a built-in skill that helps the agent
locate and explain a user's active Lua, plugin, language, skill, and AI
configuration while avoiding secrets and unsupported Neovim assumptions.

You can also add reusable, Agent Skills-compatible instructions as flat
Markdown files. Put each skill in the `skills` directory beneath the Ovim
configuration root:

1. `$OVIM_CONFIG/skills` when `OVIM_CONFIG` is set
2. `$XDG_CONFIG_HOME/ovim/skills` when `XDG_CONFIG_HOME` is set
3. `$HOME/.config/ovim/skills` otherwise

For example, create `~/.config/ovim/skills/learn-codebase.md`:

```markdown
---
name: learn-codebase
description: Teach an unfamiliar codebase one high-impact concept at a time.
---

Start with the concept that unlocks the most understanding. Explain only that
concept with `explain_with_codebase`, then let the user continue when ready.
Keep a lightweight map of what has been taught and what likely remains.
```

`name` must use lowercase letters, digits, and hyphens. `description` is the
short routing hint shown to the model. Standard optional frontmatter such as
`license`, `compatibility`, and `metadata` is accepted.

Skills use progressive disclosure: Ovim loads names and descriptions at
startup, then exposes the full body only after the model calls
`activate_skill` with one of those exact names. A successful activation is
recorded in the active conversation branch, so later turns retain the skill's
instructions. Forking or reverting to a point before activation removes it
from that branch. Restart Ovim after adding or changing a skill file.

This first format supports instruction-only `*.md` files. Skill package
directories and executable bundled scripts are not loaded.

## Read-only delegated agents

Ovim automatically exposes delegated-agent controls during an active durable
AI turn in a Git repository: `spawn_agent`, `list_agents`, `wait_agent`,
`send_message`, `followup_agent`, and `interrupt_agent`. These are harness
capabilities, not profile tools or user configuration. Human controls in the
agent tree remain available while descendants are live, even after the model
turn ends.

Every spawn names an exact catalog model and reasoning effort. Ovim derives the
catalog from configured provider profiles and rejects invalid pairs before
allocating durable state. The catalog ID is `profile/model`, for example
`codex_sol/gpt-5.6-sol`. `codex_app_server` profiles are excluded because their
nested provider session cannot be safely reconstructed in the child harness.

The built-in harness allows explorer and reviewer trees to depth two, with at
most three concurrent agents, eight queued agents, four children per parent,
and eight children per root run. Children are always read-only and
network-isolated. Provider-event, tool-call, and timeout ceilings also apply
across the tree. These safety limits are owned by Ovim rather than `ai.toml`.

Routing guidance is intentionally short: use Luna at `max` for explicit,
well-specified implementation or checklist work; use Terra for bounded
implementation or measurement with an explicit protocol; use Sol for subtle
correctness, concurrency, architecture, or skeptical review. Choose Terra/Sol
effort by task difficulty and risk.

The delegated provider adapter does not yet report trustworthy token or cost
totals, so usage displays `n/r`. Bound spend with the enforced agent-count,
event, tool-call, timeout, and provider-account limits. Changing provider
profiles while Ovim is running requires a restart rather than silently changing
routes beneath queued children.

Children see an immutable content-addressed snapshot captured at dispatch,
including authoritative unsaved editor buffers. Later edits in the root
worktree cannot change what an already-running child reads. A child receives
only bounded snapshot read, list, search, symbol, diagnostic, and
unsaved-buffer tools—no shell, network, navigation, mutation, or approval.
A descendant inherits the same immutable content through a fresh durable
manifest identity, so recursive delegation cannot observe newer editor state.
Dispatch controls disappear at the configured depth. Symbol and diagnostic indexes are copied at
dispatch and remain bound to that manifest; diagnostics without an exact
matching analyzed buffer revision are omitted rather than shown as current.

`spawn_agent` returns the durable task, agent, workspace, manifest, route, and
state immediately; the root should keep working while the child runs.
`list_agents` reports routing and lifecycle state without waiting. `wait_agent`
parks only that provider tool call, not the editor event loop, and completes on
a validated handoff, timeout, or new user steering. Delivered mailbox entries
are acknowledged durably after the wait result wins. `interrupt_agent`
interrupts the named child hierarchy while preserving partial run history.
While a delegated parent waits, it yields its provider-concurrency slot so a
queued descendant can run; the parent reacquires capacity before resuming.

`send_message` queues a bounded parent-authored steer only to a live child.
It does not start a new turn. Ovim records the message before notifying the
child, claims delivery by durable event ID, and presents it to the provider
only between provider/tool operations. Queued, completed, and otherwise idle
targets reject the message explicitly. The human/root authority can steer any
visible live descendant from the agent tree; delegated models can steer their
own direct children. A restart never blindly repeats an
ambiguous provider delivery; the message remains visible as rejected instead.

`followup_agent` starts a fresh turn on a completed or interrupted child while
retaining the same Ovim agent identity, model and reasoning effort, workspace,
capability ceiling, and budget ceiling. The follow-up has a fresh durable turn
ID and monotonically increasing generation. Ovim reuses an idle provider
session only when the provider explicitly supports safe follow-up; otherwise
it opens a fresh provider session with bounded context from the validated prior
handoff. Follow-up cannot reroute the child or widen its authority.

### Inspect and control delegated agents

Delegated agents are switchable conversations rather than a permanent sidebar.
From an empty Primary composer, press Down to open the switcher. Primary is the
first row, followed by delegated agents in hierarchy order. Use Up/Down or
`j`/`k` to highlight a conversation and Enter to open it. Esc returns to
Primary and restores the parked Primary draft.

The selected agent uses the main conversation surface. Its header identifies
task, lifecycle, model, effort, objective, and read-only/writable workspace
mode. Durable operator messages and handoffs appear as conversation entries;
bounded lifecycle and tool events remain available as activity evidence.
Lifecycle and mailbox delivery are separate: a running child can simultaneously
show that one steer is queued for its next safe boundary.

Typing in a live agent conversation sends a durable steer to that exact agent.
The conversation remains selected after submission, so several messages can be
sent without retargeting. Opening a completed or interrupted agent turns the
composer into a follow-up action. Follow-ups preserve identity, route, workspace,
capability ceiling, and the original budget ceiling. The root can control any
descendant; a delegated parent can control any descendant in its own subtree.
Queued or failed agents remain inspectable but do not masquerade as live steer
targets.

The switcher keeps completed, failed, interrupted, and restart-recovered states
distinct. It also shows queued mailbox delivery independently from lifecycle.
`f`/`w` follows or unfollows the highlighted child for status-line monitoring;
`i` interrupts it; and `a`/`d` resolves its pending approval. If the highlighted
child has no approval, approval controls fall back to the oldest pending request.
The prompt identifies child and ancestry, role, requested and effective route,
tool effect, workspace, and reason.

A child pausing for approval does not freeze the editor—only that child waits.
From any focus, `Ctrl-Y` allows and `Ctrl-N` denies the oldest pending child
approval. `Ctrl-T` remains dedicated to the Primary conversation branch tree;
it is not the delegated-agent navigator.

Ovim does not guess how to resume an in-flight child provider session after a
restart. Starting, running, waiting, or effect-ambiguous work is closed with a
validated interrupted handoff; an open tool effect is recorded as unknown
after crash. A child that was durably queued but never started is retried only
after its exact recorded model route still passes the current catalog. Its
captured manifest, typed delegation envelope, per-child budget, symbols, and
diagnostics are reconstructed from the private run directory. Completed and
interrupted children remain available to list, inspect, and follow up, while
stale approvals and ambiguous message deliveries fail closed. Missing handoff
notifications are recreated without duplicating an existing mailbox entry.

Use `vim.ai.setup(...)` in Lua to customize these defaults.

`ai.toml` still works, but it is legacy compatibility.

## Codex configuration

```lua
vim.ai.setup({
  default_profile = "codex_terra",
  contexts = {
    selection = "codex_terra",
    chat = "codex_sol",
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

The `codex` provider does not accept an API key. Ovim normally refreshes its
credentials without intervention. If refresh is rejected, it marks that
credential lineage for renewal and shows the browser sign-in dialog the next
time Codex inference is opened or submitted.

To change accounts manually, remove Ovim's `codex-auth.json`, then open AI chat
or a visual-selection edit and press `Enter` in the sign-in dialog. This does
not sign Codex CLI in or out.

To retain the previous Codex-owned harness explicitly, configure
`provider = "codex_app_server"`. That strategy launches `codex app-server` and
keeps its native threads, sandbox, and orchestration. Ovim never falls back to
it silently when direct inference fails.

In AI chat, a single Escape hides the panel without interrupting the agent or
clearing the conversation—even while a review or approval is pending. You can
navigate and edit the project normally while the turn continues. Open the same
chat again to resume it with its input, queue, review, and view state intact.
While a hidden agent is running, a compact `AI working…` badge appears at the
top right of the editor; a paused approval uses an attention badge instead.
Press Ctrl-C with the chat open to stop the current generation without closing
or clearing the conversation; any partial response remains in history.

Persisted conversations are not restored automatically when starting a new
Ovim process. This avoids accidentally sending a large historical conversation
to a provider. Start Ovim with `--resume` only when you explicitly want to
restore the conversation associated with the file, repository, and chat name:

```sh
ovim --resume path/to/file.rs
```

Without `--resume`, opening AI chat creates a fresh durable conversation while
preserving the previous run on disk. Hiding and reopening chat within the same
Ovim process still keeps the live conversation as described above.

Chat slash commands are handled by ovim rather than sent to the provider:

Typing `/` or a partial command name opens an autocomplete popup. Use Up/Down
to choose, then Tab or Enter to insert the command; click selection is also
supported. Enter again executes a completed command.

- `/model` opens the profile picker.
- `/model codex_sol` switches directly to a named profile.
- `/effort` opens the combined picker on reasoning effort.
- `/effort default|none|low|medium|high|xhigh|max` sets the per-chat effort.
  Provider support remains model-specific; current Codex models accept `max`.
- `/clear` clears the current conversation and starts a fresh provider context.
- `/compact` creates a structured checkpoint for older model context and keeps
  an approximately 8k-token recent complete-turn tail. `/compact aggressive`
  keeps only the tool-call boundary when maximum context recovery is needed.
  Both forms retain the full conversation for display and history.
- `/exa` opens web-search setup to add or replace an Exa API key.
- `/comprehension`, `/comprehension publish`, `/comprehension commit`, and
  `/comprehension off` configure the per-chat mastery boundary.
- `/yolo`, `/yolo on`, and `/yolo off` toggle or set the per-chat approval
  bypass. This is also useful for headless sessions.

While an agent round is running, the composer remains editable:

- Shift-Enter inserts a newline without submitting. Ctrl-J provides the same
  behavior for terminals that encode modified Return as a legacy line feed.
- Enter queues a steer for the active round. Ovim delivers it after the next
  completed tool call. If the round finishes first, it becomes the next-round
  follow-up.
- Tab queues a message for the next round.
- Slash commands can also be queued; they run locally after the active round
  and are displayed as commands rather than user messages.

The composer wraps at word boundaries and keeps the cursor visible when input
grows beyond five rows. Click any visible composer row to place the cursor;
long words are split only when they cannot fit on a row by themselves.

Completed tool calls appear as compact summary rows in chat. Move focus into
message history, select a tool row, and press Enter to expand or collapse its
arguments and result.

Shell calls have a Process Inspector instead of the ordinary expanded row.
Select a live or completed shell row and press Enter to open it. The inspector
shows live stdout and stderr, command phase, working directory, process ID,
elapsed time, and how long the process has been quiet. It follows new output by
default; use Up/Down or Page Up/Page Down to scroll, `G` to return to live
follow mode, `/` to search, and `n` to visit the preceding match.

While the process is running, Ctrl-C requests a normal terminal interrupt for
the process group without cancelling the surrounding agent turn. Ctrl-K force
stops the process group if it does not respond. Escape closes only the
inspector. Shell tools do not accept keyboard input: commands that require an
interactive prompt must use non-interactive flags instead.

Process output is intentionally ephemeral and bounded. Ovim retains at most
512 KiB per process and 2 MiB across the ten most recent completed processes;
older output is marked as discarded or expired while the durable tool result
remains in conversation history.

Walkthroughs combine concept pages and code pages in one linear sequence.
Concept pages use a larger centered panel for an introduction, prerequisite
mental model, transition, or synthesis that does not belong to one source
range. Their body is limited by the current terminal dimensions, and the agent
must split dense material into focused semantic pages rather than truncating or
compressing it. Code pages keep the referenced source selected in the existing
compact card. Ovim captures eligible referenced files when it accepts the
walkthrough, then validates and renders their code pages from that immutable
snapshot. Completed
walkthrough snapshots are retained for replay in a 10 MiB, oldest-first cache,
so later buffer edits, external file changes, renames, or deletions do not replace
cached code. Files of 5 MiB or larger are deliberately not retained and are read
from the current buffer or disk when their page is shown; this keeps a single
large source from displacing useful tutorials. Both page types aim to teach one
new idea at a time and introduce only the context needed for the next page.

Move between pages with Left/Right (or `h`/`l`). Press Space to ask about the
current page; its explanation is attached as quoted context, Enter sends,
Shift-Enter adds a line, and Escape cancels the draft. The root chat agent
answers in the walkthrough using the conversation it already built. Questions
expand into the available terminal space; use Up/Down (or `k`/`j`) or the mouse
wheel to read an answer that is longer than the card. Questions and answers
remain in normal conversation history after the walkthrough ends, so they are
available when the agent returns to implementation. Walkthrough
questions permit read-only investigation but reject navigation, mutations,
external actions, and delegated-agent control until the answer finishes.

Drag across text in message history to select it. Releasing the mouse copies
the selection to the system clipboard; `Ctrl-Y`, `y` while history is focused,
or `Cmd-C` also copies the active selection. Without a text selection,
`Ctrl-Y` keeps copying the complete conversation.

You can drag PNG, JPEG, GIF, or WebP files from the desktop into the chat
composer. Ovim displays attached filenames above the input; press Backspace on
an empty composer to remove the most recent image. Images submitted during an
active agent round are kept together and queued for the next round. Each image
may be up to 20 MiB, with a 40 MiB limit for the pending message.

When the terminal supports image rendering, pending attachments appear above
the composer. After submission, each thumbnail moves into the user message it
was sent with. Ovim emits terminal image data only while that message's complete
thumbnail is visible, so scrolling, new chat output, or hiding the chat cannot
leave a historical image pinned over the current screen. Click a visible
thumbnail to open its larger modal preview.

Ovim automatically prefers the terminal's advertised Kitty, Sixel, or iTerm2
protocol and falls back to Unicode half-block thumbnails when no native graphics
protocol is available. Set `OVIM_IMAGE_PROTOCOL` to `kitty`, `sixel`, `iterm2`,
`halfblocks`, or `off` to override detection. `halfblocks` is the most portable
troubleshooting choice when native image placement flickers or is unstable.

Terminal drag-and-drop is handled as a pasted image path, so the same behavior
is available headlessly with `ovim paste -s SESSION '/path/to/image.png'`.

## API-key providers

Codex is the default hosted path. The following sections apply only when you
deliberately configure a raw API provider.

### OpenAI Key Permissions (Restricted Key)

For ovim's OpenAI integration, a restricted key only needs:

- `Model capabilities`: write/request enabled
- `List models`: optional (`None` is fine)

Everything else can stay `None` for this use case.

### 1) Set API Key Securely (No `~/.zshrc`)

ovim reads API keys from environment variables at runtime. You can inject them only when launching ovim.

### Option A: macOS Keychain + launcher script

Store the key:

```bash
read -s OPENAI_TMP
echo
security add-generic-password -a "$USER" -s ovim-openai -U -w "$OPENAI_TMP"
unset OPENAI_TMP
```

Create `~/bin/ovim-openai`:

```zsh
#!/usr/bin/env zsh
export OPENAI_API_KEY="$(security find-generic-password -a "$USER" -s ovim-openai -w)"
exec ovim "$@"
```

Make it executable:

```bash
chmod +x ~/bin/ovim-openai
```

Use it:

```bash
ovim-openai
```

### Option B: one-shot session variable

```bash
read -s OPENAI_API_KEY
export OPENAI_API_KEY
ovim
```

This keeps the key out of shell startup files.

### 2) Configure an API provider with Lua

Put this in `~/.config/ovim/init.lua`:

```lua
vim.ai.setup({
  default_profile = "openai",
  contexts = {
    selection = "openai",
    chat = "openai",
    query = "openai",
  },
  profiles = {
    openai = {
      provider = "openai",
      model = "gpt-4.1-mini",
      api_key_env = "OPENAI_API_KEY",
      temperature = 0.2,
      max_tokens = 2048,
      edit_mode = "format",
      edit_format = "codeblock",
    },
  },
})
```

Built-in AI keybindings:

- Normal mode `Space Space`: chat
- Normal mode `Space ?`: read-only query
- Visual mode `Space Space`: attach selected code to chat
- AI chat `Ctrl-T`: delegated-agent and conversation-tree sidebar

## Legacy `ai.toml` (Still Supported)

If you prefer TOML or need compatibility:

- macOS: `~/Library/Application Support/ovim/ai.toml`
- Linux: `~/.config/ovim/ai.toml`

```toml
default_profile = "openai"

[profiles.openai]
provider = "open_ai"
model = "gpt-4.1-mini"
api_key_env = "OPENAI_API_KEY"
temperature = 0.2
max_tokens = 2048
extraction = "json"
```

Important:

- Lua uses provider string `openai`
- `ai.toml` uses provider string `open_ai`

This naming difference is expected in the current parser.

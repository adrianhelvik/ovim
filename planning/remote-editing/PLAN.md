# Remote editing over SSH

## Goal

Edit a project on a remote host from the local Ovim GUI, with LSP, git, grep,
tests, debugging, and AI all running remotely against the real toolchain.

## Architecture: remote backend, thin local frontend

The local `ovim-gui` process runs only the Tauri shell and the webview. The
full editor runs on the remote host. The GUI already works this way locally:
`GuiSnapshot` (`ovim/src/gui/mod.rs`) is a fully projected, viewport-sized
frame — resolved highlight segments, layout tree, panels, theme — and
`GuiBridge` is only `send command -> receive snapshot`. Remote editing is
therefore a *transport swap*, not a new editor.

### Why not the VS Code split-services model

VS Code Remote-SSH runs the editor locally and only the services remotely, so
typing never touches the network. That model would require defining an RPC
surface for every subsystem (file I/O, file tree, grep, git blame/diff/status,
LSP with local<->remote URI rewriting, DAP, test runner, AI tool path policy,
terminal) — roughly 5-10x this plan's work. It also forfeits the property that
makes the remote-backend model worth having:

**Session persistence.** Because the whole editor lives remotely, closing the
laptop and reconnecting preserves warm language servers, undo history, running
test runs, and in-flight AI conversations. Ovim already has the session
primitives for this (`ovim session list/health/kill/cleanup`, session files
carrying PID + port + capability). VS Code cannot offer this; a local buffer
cannot survive a disconnect.

A second structural win: there is **no URI translation**. The LSP client, the
language server, and the files are all on one host with one path space. The
warning in `ovim-core/src/buffer/file_io.rs` about URI stability stays
satisfied by construction.

### Why not an SFTP/VFS layer

Putting a `FileSystem` trait behind `buffer/file_io.rs` looks smaller but
silently disables LSP, git, test runner, DAP, and every AI file tool, across
~750 `fs::` call sites. `sshfs` already provides that outcome for free.

### The cost: interaction latency

Every keystroke round-trips. Rough budget:

| Link | RTT | Feel |
|---|---|---|
| LAN / local VM | 1-10ms | indistinguishable from local |
| Same city / DC VPN | 10-25ms | fine |
| Cross-country | 40-70ms | mushy; holding `j` visibly lags |
| Transatlantic | 100-150ms | unpleasant |
| Cafe wifi / tethering | 100-300ms + jitter | unusable |

Above ~40ms RTT, predictive local echo (chunk R7) is **required**, not polish.
The reference design is mosh — speculative echo with visual confirmation state
— not VS Code. In a modal editor only a narrow set of keys is safely
predictable (insert-mode printable characters, plain `hjkl` with the line
content already in the snapshot); anything with a pending operator, count,
register, macro, or auto-indent must round-trip.

Bandwidth is not a concern: snapshots are viewport-sized plus overscan, and
are only published on change. A 60-line viewport is roughly 40-50KB of JSON
uncompressed, ~5-8KB gzipped. Splits multiply it, since each `GuiPane` carries
its own `lines`.

## Verified constraints

- `pub mod gui` is **not** feature-gated in `ovim/src/lib.rs`; only
  `gui::app`, `gui::browser`, `gui::menu` are (`#[cfg(feature = "gui")]`).
  `GuiSnapshot`, `GuiRequest`, and `snapshot()` therefore already compile
  without Tauri, so the headless server can produce snapshots directly.
- `GuiRequest` has 31 variants, each embedding a `oneshot::Sender` reply
  channel. It is not serializable as written and must be split into a
  serializable command payload plus a transport-owned reply channel.
- `GuiSnapshot` and its ~20 nested types derive `Serialize` only.
- The API already authenticates: bearer `SessionCapability` plus a Host-header
  guard (`ovim/src/api/security.rs`).
- The API asserts a loopback bind (`ovim/src/api/mod.rs`). SSH forwarding
  satisfies this rather than violating it — no port is ever exposed.
- `ApiSecurity::host_is_allowed` requires the Host header to equal
  `127.0.0.1:{port}` or `localhost:{port}` for the **server's own** port. Under
  `ssh -L` the local port generally differs, so the client must set the Host
  header explicitly to the remote port. This is a real trap; see R4.
- `gui/mod.rs` is 3648 lines, past the 3k refactor threshold in `CLAUDE.md`.
  Extracting the protocol types is required cleanup, not incidental churn.
- `FileArg::parse` (`ovim/src/cli.rs`) splits on `:` from the right, so any
  URI-style target is mangled. Use a separate `--remote` flag, not a scheme.
- The GUI currently rejects terminal and shell sessions ("External shell
  sessions require the TUI frontend"). Remote work is exactly when a shell is
  wanted; tracked as a follow-up, not in scope here.

## Chunks

Each chunk is independently reviewable and leaves the tree green.

### R1 — Serializable GUI protocol
Extract `GuiSnapshot`, its nested types, and a new serializable `GuiCommand`
(the payload of `GuiRequest` minus reply channels) into `ovim/src/gui/protocol.rs`.
Add `Deserialize` alongside `Serialize`. Add serde round-trip tests. No
behavior change. Also relieves the `gui/mod.rs` size problem.

### R2 — `GuiTransport` abstraction
Introduce a transport trait (`send(GuiCommand) -> GuiReply`, `subscribe() ->
watch::Receiver<GuiSnapshot>`). Reimplement `GuiBridge` over it, with the
existing mpsc channel becoming `LocalTransport`. All 31 Tauri commands must
keep working untouched. Riskiest refactor — isolated deliberately.

### R3 — Server: GUI protocol over the headless API
`POST /v1/gui/command` and `GET /v1/gui/stream` (SSE). Drive a snapshot
producer from the headless event loop. Reuses the existing bearer + Host
security layer.

### R4 — Client: remote transport
`RemoteTransport` implementing `GuiTransport` over reqwest + SSE, with the
explicit Host header workaround and bearer auth from the remote session file.

### R5 — SSH bootstrap and `--remote`
`ovim gui --remote user@host --path /project`. ControlMaster/ControlPath so a
2FA host is not prompted twice per window; two-phase bootstrap (query the
remote endpoint, then forward it); remote binary discovery and version match.

### R6 — Reconnect and session persistence
Resume an existing remote session after a dropped link. The payoff feature.

### R7 — Predictive echo
Required for usable editing above ~40ms RTT. Mosh-style speculation with
confirmation state, limited to safely predictable keys.

### R8 — Clipboard bridging
A yank on the remote fills a remote register. Bridge it to the local system
clipboard, and the reverse for paste. Daily papercut if skipped.

### R9 — Documentation
`user-docs/remote.md`, plus README and CLAUDE.md updates.

## Out of scope

Multi-root or mixed local/remote workspaces; remote terminal (blocked on the
GUI terminal gap); local file drag-drop into a remote window.

## Remote-incompatible surface (audited)

Of 33 Tauri commands in `ovim/src/gui/app.rs`, 28 are pure passthrough to the
bridge and work over any transport unchanged. The exceptions:

1. **`gui_diff_state` / `gui_diff_open_file`** — both take the `PathBuf`
   returned by `GuiCommand::DiffWorkspace` and run `native_diff::review()` /
   `file_patch()` on it locally. The root problem is `DiffWorkspace` returning
   a path at all: it hands a server-side path to a client that assumes it is
   local. Fix in R3 by replacing it with commands that return the computed
   `DiffReview` / patch, so the work happens where the repository is.

2. **`GuiCommand::AttachImages { paths }`** — drag-drop yields paths on the
   laptop, but the handler opens them on the editor's host. `AttachImageData`
   (raw bytes) already exists and is transport-independent; route drag-drop
   through it under a remote transport.

3. **`PendingWindowOpen` / `:openwin`** — broken in *both* directions. A
   remote `:openwin` produces a remote path that is then `fs::metadata`'d
   locally and handed to a locally spawned `current_exe()`; and the local
   directory picker produces a local path handed to a remote editor. The
   window-open request must carry which host the path belongs to.

4. **`render_vector_preview`** — spawns `strok` from the *local* PATH. It
   works over a transport because it operates on the source string rather than
   a path, but it puts a toolchain requirement on the laptop, contradicting the
   "real toolchain runs remotely" premise. Needs an explicit decision, not an
   accident.

5. **Intentionally local, do not "fix"**: `gui_open_external`
   (`open::that_in_background`) and `gui_window_action`. These belong on the
   laptop.

# Getting Started

## Build and Run

From the repo root:

```bash
cargo build --release
./target/release/ovim path/to/file.txt
```

Open a project directory to start in the file explorer:

```bash
ovim path/to/project
```

## File Explorer

Press `-` from a file to reveal it in the project tree. Press `Tab` to move
focus back to the buffer while leaving the tree open, or `q` to close it.

| Key | Action |
|---|---|
| `j` / `k`, arrows | Move down / up |
| `Enter`, `o`, `l` | Open a file or expand a directory |
| `h` | Collapse a directory or select its parent |
| `a` | Create a file; end the name with `/` to create a directory |
| `R` / `d` | Rename / delete (delete requires confirmation) |
| `y` / `X` / `p` | Copy / cut / paste |
| `f` or `/` / `F` | Filter loaded entries / clear the filter |
| `H` / `I` | Toggle hidden / git-ignored entries |
| `r` | Refresh the tree |
| `gg` / `G` | Select first / last entry |
| `?` | Toggle the explorer key reference |
| `Tab` / `q` | Focus the buffer / close the explorer |

Create, rename, and paste operations refuse to overwrite existing paths.
Rename and create prompts also reject paths that escape the selected directory,
and the explorer root cannot be renamed, moved, or deleted.

## Reviewing Branch Changes

Press `<Space>gd` to open a review of everything your branch changed compared
with the default branch, including uncommitted work. The review is a read-only
patch in its own tab with a file summary at the top. Each hunk is coloured with
the grammar of the file it came from — the way `delta` renders a patch — and
added and removed rows carry a background tint.

The toolbar above the patch switches between two layouts. Click a button, press
`s`, or run `:GitDiffLayout split` / `:GitDiffLayout unified`:

- **Unified** shows the raw patch, so the buffer still yanks as a valid patch.
- **Split** puts the old file on the left and the new one on the right, with
  source line numbers on both sides. Long lines wrap inside their column, and
  the view re-flows when the window changes width.

| Key | Action |
|---|---|
| `<Space>gd` | Open the review, return to it from a file, or leave it |
| `s` | Switch between the unified and side-by-side layouts |
| `]c` / `[c` | Next / previous hunk (in ordinary files: next / previous git change) |
| `]f` / `[f` | Next / previous file |
| `Enter`, `gf` | Open the file at the line under the cursor, in the tab you came from |
| `Enter` on a summary row | Jump to that file's section of the patch |
| `r` | Refresh the patch |
| `q` | Close the review |
| `<Space>gf` | Fetch the base branch from its remote, then refresh |

The base is picked automatically: the default branch from `origin/HEAD` (or
`main`, `master`, `develop`, `trunk`), compared at its merge-base with your
branch so commits that landed on main after you branched are not shown. When
both `main` and `origin/main` exist, the one with the newer merge-base wins,
so a stale local or un-fetched remote branch never pollutes the review. The
header shows how many commits you are ahead and behind, and when the remote was
last fetched. On the default branch itself the review shows uncommitted changes.

`:GitDiff` does the same from the command line, `:GitDiff <ref>` compares
against any ref (for example `:GitDiff HEAD` for uncommitted work only, or
`:GitDiff main..feature`), `:GitDiffLayout [split|unified]` picks the layout,
and `:GitFetch` fetches the base branch.

`Enter` works in both layouts: in the side-by-side view the column your cursor
is in decides which side you jump from, and the cursor keeps its column.

## AI Chat and Editing

Press `Space Space` in normal mode to open AI chat. Select text visually and
press `Space Space` to open the same chat with that code attached. The built-in Codex profiles use your
ChatGPT subscription.

On first use, Ovim checks its credentials and opens a sign-in dialog. Press
`Enter` to continue in your browser, or `Esc` to dismiss it. Ovim resumes the
draft or selection after sign-in and refreshes its own credentials
automatically; installing or signing in to Codex CLI is not required.

See [AI Setup](ai.md) for profiles, tools, approval behavior, and alternative
providers.

## Running Tests

vim-test style bindings, prefixed with `Space t` in normal mode:

| Keys | Command | Runs |
|------|---------|------|
| `Space t n` | `:TestNearest` | the test at/near the cursor |
| `Space t f` | `:TestFile` | the current file's tests |
| `Space t a` / `Space t s` | `:TestSuite` | the whole suite |
| `Space t l` | `:TestLast` | the previous test command again |
| `Space t v` | `:TestVisit` | nothing — jumps back to the last-tested spot |
| `Space t t` | `:TestPanel` | nothing — toggles the test panel |
| `Space t o` | `:TestOutput` | nothing — opens the raw log in a buffer |

Running a test opens the **test panel** on the right side of the editor:
output streams in live, the header shows a spinner while the run is in
flight, then flips to a pass/fail verdict with a parsed summary
("12 passed, 1 failed") and duration. The panel keeps a short history of
recent runs. Close it with `Escape` or toggle it with `Space t t`; open it
before any run to see the keybinding cheat sheet. Closing the panel does not
stop a test that is still running.

Commands run in the background in the file's own project root (nearest
`Cargo.toml`, `package.json`, `go.mod`, or pytest marker — resolved per
file, so monorepos just work). Failures also populate the quickfix list
(`:cn` to hop between them); `:TestOutput` shows the raw log.

Built-in runners: `cargo test` (Rust, workspace-aware, with `--test`/`--bin`
target selection), vitest/jest/bun/Node test (detected from dependencies,
config files, lockfiles, and `node:test` imports), pytest (with
uv/poetry/pipenv/pdm prefixes), and `go test`
(with subtest and table-entry `-run` patterns). The nearest test is found via
tree-sitter, so Rust `mod` nesting, nested `describe` blocks, Python classes,
and Go `t.Run` subtests all resolve to correct filters.

Other languages can be wired up with `[language.test]` in `languages.toml` —
see [configuration.md](configuration.md).

## Headless Mode (for automation)

Headless mode runs ovim without the TUI and exposes a local REST API for driving the editor.

```bash
ovim path/to/file.rs --headless --session dev
```

Then, in another terminal:

```bash
ovim session list
ovim send -s dev "iHello<Esc>"
ovim snapshot -s dev --format pretty
ovim session kill -s dev
```

## Configuration (Quick)

- Lua config: `~/.config/ovim/init.lua`
- Language config override: `~/.config/ovim/languages.toml`

See `configuration.md` for details.

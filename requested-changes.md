# Requested Harness Improvements

## Purpose

This document describes improvements that would make Ovim's AI harness safer, faster, and more pleasant for an agent doing ordinary software-engineering work inside the editor.

The harness already has broad capability. An agent can inspect files, search the repository, edit code, query the language server, run shell commands, inspect diagnostics, browse the web, and present code walkthroughs. The main opportunity is therefore not to add unrestricted power. It is to reduce the orchestration and uncertainty involved in turning a user request into a coherent, verified change.

The desired experience is:

1. Orient to the workspace with one concise call.
2. Read code semantically rather than reconstructing structure from line ranges.
3. Make a related set of edits as one versioned transaction.
4. Detect concurrent user or tool changes before overwriting them.
5. Review exactly what the agent changed.
6. Wait for current diagnostics and run targeted verification.
7. Roll back the entire agent change cleanly if needed.

This document prioritizes the in-editor harness. Headless/API improvements are included near the end, but they are secondary.

## Implementation status

Last updated: 2026-07-16

Status legend: **Done** means the documented acceptance criteria are implemented; **In progress** means a usable foundation has landed but listed requirements remain; **Not started** means no implementation from this plan has landed yet.

| Area | Status | Landed | Remaining |
| --- | --- | --- | --- |
| Buffer revisions and optimistic concurrency | **In progress** | Reads report open-buffer revisions; diagnostic tools distinguish the current buffer revision from the LSP document version(s) actually attached to diagnostics and explicitly mark unversioned results; all text-mutating tools require `expected_revision`, reject omissions and mismatches before preparing path targets, return revision transitions, and deterministically reject a second mutation based on the same revision (`4edcec0`, `01114c9`, `5635a05`, `32764fb`). | Detect overlapping intervening edits; support safe non-overlapping rebases and explicit force mode; map LSP protocol versions directly to editor revisions where a server supplies sufficient version information. |
| `workspace_context` | **Done** | Compact workspace, attached-root, bounded Git worktree, active/open buffer, explicit unsaved-editor state, selection, bounded nested project detection, LSP availability, and diagnostic summaries with optional sections (`5e63454`, `3edd3c7`). | Additional attached roots require the controlled multi-root support planned for Milestone 5; the current single attached root is reported. |
| Current-turn edit provenance | **Not started** | Existing review ranges provide a foundation but are not turn-scoped provenance. | Track user, agent turn/tool call, LSP, formatter, and external edits through undo/redo. |
| `read_changes` | **Not started** | — | Add workspace, buffer, staged, unstaged, agent-turn, and change-set scopes with stable hunk IDs and pagination. |
| Mutation result envelopes | **In progress** | Successful mutations now use a common summary with the tool and file, current changed range or deletion point, line diffstat, revision transition, diagnostic refresh state, save outcome, agent provenance, and editor-undo guidance; existing responses retain nearby context (`1b6d1dc`). | Add turn/tool-call provenance identifiers, durable rollback tokens, warnings, and machine-readable error codes. |
| Atomic change sets | **Not started** | — | Implement Milestone 2 in full. |
| Interactive explained change sets | **Not started** | The existing `explain_with_codebase` walkthrough provides the navigation and presentation foundation. | Add pedagogical guidance to both walkthrough tools, then build `talk_through_changes` as an approval layer over atomic change sets. |
| Semantic LSP operations | **Not started** | — | Implement Milestone 3 in full. |
| Revision-aware verification and persistent processes | **Not started** | — | Implement Milestone 4 in full. |
| Scale, multi-root, semantic reads, and pagination | **Not started** | — | Implement Milestone 5 in full. |
| External automation polish | **In progress** | Fresh conversations are now the startup default and persisted history requires explicit `--resume` (`66ea63d`). | Add activity/events API and turn IDs, live-session rendering, and consistent CLI machine-output modes. |

Verification completed for the landed work: `cargo test -p ovim-core --lib` (1067 tests), `cargo test -p ovim --lib` (130 tests), targeted workspace-context, mutation-concurrency and result-envelope, LSP diagnostic-version, CLI resume, and durable reopen tests, CLI help inspection, `cargo fmt --all`, and `git diff --check`.

---

## Design principles

### Preserve composability

Keep low-level tools such as file reads, patches, and Bash. Higher-level tools should make common operations safer and easier, not remove escape hatches.

### Prefer a small set of orthogonal tools

Avoid introducing several tools that perform nearly the same operation. Every tool consumes model context and creates another opportunity for incorrect tool selection.

### Return concise results by default

Tool results should answer what changed, what remains uncertain, and what the agent should do next. Large payloads should be paginated or available through an explicit expanded read.

### Make buffer state authoritative

The harness lives in an editor. Unsaved buffers, user edits, buffer revisions, selections, and LSP state must take precedence over the file currently stored on disk.

### Make user and agent changes distinguishable

An agent must be able to avoid overwriting pre-existing work and avoid taking credit for changes made by the user or another agent turn.

### Make mutations reversible

Every mutation should either be naturally undoable through the editor or return a reliable rollback token. Multi-file operations should be atomic whenever possible.

### Do not hide expensive actions

Formatting, tests, builds, network calls, or broad refactors should not run automatically after every edit. Expose strong verification primitives and let the agent invoke them deliberately.

---

# Priority 0: Core daily workflow

## 1. Add a compact `workspace_context` tool

### Problem

An agent commonly needs several calls at the beginning of a task to discover the active file, selection, open buffers, repository state, diagnostics, project type, and current branch. The information exists, but assembling it costs time and model context.

### Proposed interface

```text
workspace_context(
  include_git=true,
  include_diagnostics_summary=true,
  include_projects=true
)
```

Suggested output:

```text
Workspace: /workspace/example
Branch: feature/billing
Git: 4 modified, 1 untracked

Active buffer:
  apps/web/src/billing.ts:84:12 [modified, unsaved]
Selection:
  apps/web/src/billing.ts:84-101

Open buffers:
  apps/web/src/billing.ts [modified]
  apps/web/src/billing.test.ts

Detected projects:
  TypeScript, pnpm workspace
  Rust, Cargo workspace

Diagnostics:
  2 errors, 4 warnings across 3 files
```

### Requirements

- Keep the response intentionally small.
- Report unsaved buffers, not only disk state.
- Distinguish Git modifications from unsaved editor modifications.
- Include the active workspace root and attached roots.
- Include branch and detached-HEAD state when Git is available.
- Do not include full diagnostic messages unless explicitly requested.
- Allow sections to be disabled to control cost.

### Acceptance criteria

- An agent can orient to a normal repository without calling Bash.
- The result remains useful in a monorepo with multiple detected project types.
- The result clearly identifies pre-existing user changes.
- The response is bounded even when hundreds of files are modified.

---

## 2. Add atomic multi-file change sets

### Problem

A logical refactor often spans several files. Current mutations are independent operations, line numbers can shift between operations, snapshots must be created manually, and a failure halfway through can leave a partial implementation.

### Proposed workflow

```text
begin_change_set(title="Extract billing validation")
apply_patch_to_change_set(...)
create_file_in_change_set(...)
delete_file_in_change_set(...)
validate_change_set()
commit_change_set()
```

A simpler alternative is one `apply_change_set` call containing multiple file operations.

### Required behavior

1. Validate every operation before mutating any buffer.
2. Check expected buffer revisions and patch context.
3. Snapshot all affected files automatically.
4. Apply all operations atomically.
5. Integrate the entire change with editor undo.
6. Return a concise diffstat and changed ranges.
7. Return a single rollback token.
8. Preserve file modes and line endings.
9. Support create, modify, rename, move, and delete.
10. Refuse partial application unless explicitly requested.

Suggested result:

```text
Applied change set cs_018 atomically:
  M src/billing.ts          +18 -11
  M src/billing.test.ts     +34 -2
  A src/billing-errors.ts   +27

Buffers: 3 modified, all unsaved
Diagnostics delta: 2 errors -> 0 errors
Rollback token: rb_018
```

### Preview mode

Support validation and preview without mutation:

```text
preview_change_set(...)
```

This should return affected files, conflicts, a diffstat, and optionally the unified diff.

### Acceptance criteria

- If any hunk fails validation, no file changes.
- One undo or rollback restores all affected buffers.
- The user can continue editing after application without corrupting rollback metadata.
- Change sets work with unsaved buffers.
- Concurrent changes produce a clear conflict instead of a partial edit.

---

## 3. Add interactive explained change sets

### Problem

Some users want to understand an implementation while the agent makes it, rather than receive only a finished diff or a walkthrough after the work is complete. Independent mutation calls do not provide a coherent place to explain how an observed constraint leads to a proposed edit, and immediately applying edits one step at a time would make rejection, rollback, dependency handling, and intermediate invalid states difficult.

### Design

Add a `talk_through_changes` tool that presents a pedagogical, interactive review of a staged atomic change set. It must reuse the same `ChangeSetProposal`, validation, snapshot, commit, undo, provenance, and rollback machinery as `apply_change_set`; it must not introduce a second mutation engine.

The editor should validate and stage the complete proposal before opening the walkthrough, but apply no mutations until the user accepts the entire transaction. Reference steps show relevant base code and explain why it matters. Change steps show the generated diff for a proposed operation and explain how it responds to the preceding constraint. This preserves the experience of implementing with an explanation while retaining atomicity.

Use this tool only when the user explicitly asks for explanation, teaching, or a walkthrough during implementation. Ordinary implementation requests should continue to use normal mutation or change-set tools. Finished explanations after implementation should use `explain_with_codebase`.

### Proposed interface

```text
talk_through_changes(
  title="Extract billing validation",
  change_set={
    operations: [
      {
        id: "extract-validator",
        type: "modify",
        path: "src/billing.ts",
        expected_revision: 47,
        patch: "..."
      },
      {
        id: "add-errors",
        type: "create",
        path: "src/billing-errors.ts",
        expected_revision: 0,
        content: "..."
      },
      {
        id: "remove-legacy-validator",
        type: "delete",
        path: "src/legacy-validation.ts",
        expected_revision: 12
      }
    ]
  },
  steps: [
    {
      type: "code",
      path: "src/billing.ts",
      revision: 47,
      start_line: 84,
      end_line: 101,
      comment: "Validation currently happens inside the request handler, which couples transport concerns to billing rules."
    },
    {
      type: "change",
      operation_id: "extract-validator",
      comment: "We replace the inline logic with a focused validator, leaving the handler responsible only for coordinating the request."
    },
    {
      type: "change",
      operation_id: "add-errors",
      comment: "The validator needs errors callers can distinguish without parsing message strings, so these types get their own module."
    },
    {
      type: "code",
      path: "src/legacy-validation.ts",
      revision: 12,
      start_line: 1,
      end_line: 14,
      comment: "This older helper becomes redundant once every caller uses the new validator."
    },
    {
      type: "change",
      operation_id: "remove-legacy-validator",
      comment: "We can now remove the unused implementation rather than maintain two sources of validation behavior."
    }
  ]
)
```

Keep operations separate from narrative steps so an operation is defined once, the same source or change can be revisited from a different perspective, and the narrative can move naturally between constraints and consequences. Change steps must reference valid operation IDs. Code steps refer to the validated base buffer state; proposed or newly created code is shown through its generated change preview.

The shared change-set representation must support:

- Modify with contextual patches and expected buffer revisions.
- Create with complete content and an assertion that the target does not already exist.
- Delete with an expected buffer revision.
- Rename and move, with content changes represented separately at first if that keeps validation unambiguous.

Initially require at most one content-changing operation per base path. Multiple conceptual edits to one file should be represented as multiple hunks in one operation and may be discussed in multiple walkthrough steps.

### Pedagogical behavior

Update both `explain_with_codebase` and `talk_through_changes` tool instructions to require easy-to-understand, pedagogical steps. The agent should:

- Begin with the user-visible goal or the simplest useful entry point.
- Establish prerequisites and current constraints before their consequences.
- Keep each step focused on one idea or relationship.
- Explain why code or a change exists instead of paraphrasing it.
- Connect each step to the preceding and following steps.
- Introduce unfamiliar terms when needed and adapt to the user's apparent expertise without being condescending.
- For changes, prefer the progression `current behavior or constraint -> design consequence -> proposed change -> verification` when it fits.

The existing soft-wrap-aware visual-row restriction applies to referenced `code` steps. Validation must not silently truncate them and should return the same measured-row and suggested-endpoint guidance as `explain_with_codebase`. Change steps display the complete generated diff and are not rejected merely because the edit exceeds the reference-block line limit; the diff view must be scrollable. Separate limits should still bound operation count, affected files, total payload bytes, walkthrough steps, and comment size.

### Interaction and rejection

The walkthrough blocks further agent execution until accepted or rejected. Use the existing navigation conventions and the established explicit approval keys:

- `Left`/`h`: previous step.
- `Right`/`l` or `Enter`: next step; `Enter` must never directly commit.
- `Ctrl-Y`: move to the final transaction summary; on that summary, accept and commit.
- `Ctrl-N`: reject the entire proposal immediately.
- `Esc`: cancel and reject with no mutations applied.

The final summary should show the operation and file counts, diffstat, and any steps the user has not viewed. An early `Ctrl-Y` should go to this summary rather than commit immediately. Do not add session-wide approval for explained changes, and do not support selective operation acceptance initially: dependent operations form one coherent transaction and are accepted or rejected together.

Rejection is an expected user decision, not a validation failure. Resume the agent with an unambiguous result such as:

```text
The user rejected the proposed change set. No changes were applied.
```

Immediately before commit, recheck every affected buffer revision and operation precondition. If anything changed while the walkthrough was open, reject the commit atomically, report the conflicting operation and current revision, and apply nothing.

### Scale and agent complexity

One coherent call reduces tool selection, repeated reads, line drift, manual snapshots, partial-failure recovery, and approval interruptions. Very large calls increase schema, payload, validation-repair, and narrative complexity. Bound the initial implementation to a moderate number of files, operations, steps, and bytes, with exact limits chosen from model and UI testing. Split larger work into independently coherent and verifiable transactions rather than arbitrary per-file calls.

Validation errors should identify operations by stable ID and report which other operations validated, while preserving all-or-nothing application. Accepted results should return a change-set ID, concise diffstat, provenance, revision transitions, and one rollback token; full review remains available through `read_changes(scope="change_set")`.

### Acceptance criteria

- The tool is used only when the user asks for explanation during implementation.
- Every operation and reference is validated before the walkthrough opens.
- No buffer changes occur before final acceptance.
- Rejecting or dismissing from any step leaves every buffer unchanged.
- A concurrent buffer change prevents commit without partial application.
- Accepting creates one atomic editor undo unit and one rollback token.
- Create, modify, rename, move, and delete operations can all be previewed.
- Referenced code uses authoritative base buffer content, including unsaved changes.
- Referenced blocks retain the walkthrough visual-row limit without imposing that limit on scrollable edit previews.
- Both walkthrough tools teach in small, connected, easy-to-understand steps.
- Replaying a completed walkthrough never reapplies its change set and clearly handles stale source or diff state.

---

## 4. Add buffer revisions and optimistic concurrency

### Problem

Line-based edits can target stale content if the user types, another tool edits the file, formatting runs, or parallel agent work completes after the original read.

### Proposed behavior

Every file or buffer read should include a stable revision:

```text
File: src/billing.ts
Buffer revision: 47
Disk revision: 9c31...
Lines: 80-110
```

Mutation tools should accept:

```text
expected_revision=47
```

On a mismatch:

```text
Edit not applied: src/billing.ts advanced from revision 47 to 49.
Overlapping changes: lines 88-92.
Re-read the affected range and retry.
```

### Requirements

- Revisions must represent editor-buffer state, including unsaved changes.
- Reject stale edits by default.
- Detect whether intervening changes overlap the proposed edit.
- Allow an explicit force mode, but make it uncommon and visible.
- Return the new revision after every successful mutation.
- Include revisions in diagnostics so the agent knows which content was analyzed.

### Acceptance criteria

- An edit based on stale content cannot silently overwrite a user's new edit.
- Non-overlapping edits may be automatically rebased only when correctness is certain.
- Parallel tool calls have deterministic conflict behavior.

---

## 5. Track edit provenance

### Problem

Git only distinguishes repository state from the index or HEAD. It does not distinguish pre-existing user changes, current agent changes, previous agent turns, formatter changes, or subsequent user edits.

### Desired model

Track mutations at buffer-range level where practical:

- User-authored before the current turn
- User-authored during the current turn
- Agent turn and tool-call identifier
- LSP workspace edit
- Formatter/code action
- External filesystem change

### Uses

- Warn before touching lines changed by the user since the agent read them.
- Show only changes attributable to the current agent turn.
- Roll back agent changes without removing unrelated work.
- Prevent the agent from claiming pre-existing changes.
- Explain whether formatting introduced additional modifications.

Suggested warning:

```text
Edit rejected: lines 91-96 contain user changes made after revision 47.
Re-read the range or request an explicit overwrite.
```

### Acceptance criteria

- `read_changes(scope="agent_turn")` excludes pre-existing work.
- Rollback does not remove user edits made outside the agent's changed ranges.
- Provenance remains valid across normal undo/redo operations.

---

## 6. Add a structured, editor-aware diff tool

### Problem

`git diff` through Bash is useful but cannot fully represent unsaved buffers, editor provenance, or changes made only during the current agent turn.

### Proposed interface

```text
read_changes(
  scope="agent_turn|workspace|staged|unstaged|buffer",
  path="optional/path",
  format="summary|hunks|unified"
)
```

Suggested hunk output:

```text
File: src/billing.ts
Hunk: billing.ts#3
Current lines: 84-103
Origin: agent turn turn_42
Diff: +12 -7
Diagnostics in range: none
```

### Requirements

- Include unsaved-buffer changes.
- Support current-turn and current-change-set scopes.
- Identify pre-existing changes.
- Provide stable hunk IDs for discussion and review.
- Bound output and allow hunk-by-hunk continuation.
- Make full unified diff available when requested.

### Acceptance criteria

- An agent can review only its own work without parsing raw Git output.
- Newly created, deleted, renamed, and non-Git files are represented.
- Diff line numbers refer to the current buffer state.

---

# Priority 1: Semantic coding and verification

## 7. Expand LSP-backed agent tools

### Add these operations

- `find_references`
- `rename_symbol`
- `incoming_calls`
- `outgoing_calls`
- `type_hierarchy`
- `list_code_actions`
- `apply_code_action`
- `format_file`
- `format_range`
- `organize_imports`
- `apply_workspace_edit`

### Safety model

Read-only operations can run without mutation approval. Mutating operations should first return a preview when they affect multiple files or user-modified ranges.

Example:

```text
rename_symbol(
  path="src/user.ts",
  line=42,
  column=17,
  new_name="authenticatedUser",
  preview=true
)
```

Result:

```text
Rename affects 13 references in 5 files.
Conflicts: none
User-modified ranges affected: 1
Preview change set: cs_019
```

Applying an LSP workspace edit should use the same transactional machinery as ordinary change sets.

### Acceptance criteria

- Rename uses language semantics rather than text replacement.
- Workspace edits are revision-checked and atomic.
- Code actions include human-readable titles and affected files before application.
- Formatting reports whether it changed content.

---

## 8. Make diagnostics revision-aware and delta-oriented

### Problem

An agent needs to know whether diagnostics correspond to the latest edit and whether it introduced new errors. Reading diagnostics immediately after a mutation may race with the language server.

### Proposed interfaces

```text
wait_for_diagnostics(
  paths=["src/billing.ts"],
  minimum_revision=49,
  timeout_seconds=10
)
```

```text
read_diagnostics(
  paths=[...],
  baseline="before_change_set|turn_start|snapshot_id",
  format="full|summary|delta"
)
```

Suggested delta:

```text
Diagnostics for buffer revision 49:
  Introduced: 0
  Resolved: 2 errors
  Unchanged: 1 pre-existing warning
  LSP status: settled
```

### Requirements

- Tag diagnostics with buffer revision or document version.
- Distinguish pending, settled, timed out, and unavailable LSP states.
- Preserve current full diagnostic reads.
- Group project diagnostics by severity and file.
- Avoid treating lack of an LSP as success.

### Acceptance criteria

- Verification cannot accidentally report stale diagnostics as current.
- The agent can tell whether it introduced a problem without manually comparing lists.

---

## 9. Add persistent process management

### Problem

One-shot Bash is sufficient for many commands but awkward for development servers, watch-mode tests, REPLs, debuggers, and log streams.

### Proposed interfaces

```text
start_process(command, cwd, env, name)
read_process_output(process_id, after_cursor, max_lines)
send_process_input(process_id, text)
stop_process(process_id, signal="TERM")
list_processes()
```

Suggested start result:

```text
Process proc_12 started
PID: 48102
Command: pnpm dev
CWD: /repo/apps/web
Output cursor: 0
```

### Requirements

- Keep stdout and stderr available with monotonic cursors.
- Bound retained logs and report truncation.
- Support cancellation and timeouts.
- Clean up processes when appropriate, with an explicit detach option.
- Clearly identify processes started by the agent.
- Redact configured secrets from echoed environment values.

### One-shot command envelope

Improve Bash results with structured metadata while preserving raw output:

```text
Exit: 1
Duration: 4.2s
Timed out: false
Output: 127 lines, 18 KB
```

### Acceptance criteria

- An agent can start a dev server, observe readiness, run a dependent check, and stop it cleanly.
- Repeated output reads return only new data.

---

## 10. Add a composable verification primitive

### Goal

Make the common post-edit loop easy without forcing expensive checks after every mutation.

### Proposed interface

```text
verify_changes(
  scope="change_set|agent_turn|paths",
  wait_for_lsp=true,
  diagnostics=true,
  format=false,
  test_command="optional explicit command",
  timeout_seconds=120
)
```

Suggested result:

```text
Verification for change set cs_018:
  LSP settled: yes
  Introduced diagnostics: 0
  Formatter check: skipped
  Tests: 24 passed, 0 failed
  Duration: 6.8s
```

### Important constraints

- Do not guess or run broad test commands silently.
- Report exactly which commands ran.
- Permit repositories to define recommended verification in `AGENTS.md` or configuration.
- Keep individual diagnostics and raw command output available on demand.
- Support cancellation.

### Acceptance criteria

- Verification is explicit and reproducible.
- Failures include concise summaries plus access to complete output.
- Pre-existing failures are distinguishable from newly introduced ones where possible.

---

## 11. Support controlled multi-root workspaces

### Problem

Real tasks often span an application, a sibling library, a demo project, or an editor implementation. Bash can access those paths, but project-aware tools, snapshots, symbols, and diagnostics remain scoped to the original workspace.

### Proposed interface

```text
attach_workspace(
  path="~/Projects/shared-library",
  mode="read|write",
  name="shared-library"
)
```

Also support:

```text
list_workspaces()
detach_workspace(name)
```

### Requirements

- Require user approval for roots outside the initial workspace unless previously trusted.
- Preserve per-root read/write permissions.
- Route LSP and project detection correctly per root.
- Show the root name in every path-bearing tool result.
- Prevent ambiguous relative paths across roots.
- Allow a permission to last for a turn, conversation, or trusted configuration.

### Acceptance criteria

- Once attached, normal structured tools work without falling back to absolute-path Bash commands.
- Write access to one root does not imply write access to every attached root.

---

# Priority 2: Reading, search, and tool ergonomics

## 12. Improve project search

### Proposed options

```text
search_project(
  query="BillingError",
  mode="literal|regex|symbol",
  include=["src/**/*.ts"],
  exclude=["**/*.generated.ts"],
  case_sensitive=false,
  context_lines=2,
  scope="workspace|modified|open_buffers",
  max_results=50,
  continuation="optional cursor"
)
```

### Desired behavior

- Make literal versus regex explicit.
- Support include/exclude globs.
- Support context lines.
- Group results by file.
- Return a continuation cursor when truncated.
- Search unsaved open buffers.
- Support modified-files-only search.
- Offer symbol search without requiring text matching.
- Report ignored binary, generated, or oversized files.

### Acceptance criteria

- Common searches do not require falling back to `rg` solely for filtering or context.
- Results remain bounded and deterministic.

---

## 13. Add symbol- and context-oriented reads

### Proposed interfaces

```text
read_symbol(path, symbol, include_signature=true)
read_around(path, line, before=20, after=30)
read_hunk(hunk_id)
```

Suggested `read_symbol` metadata:

```text
Symbol: validateInvoice
Kind: function
Lines: 74-119
Buffer revision: 47
References: 6
Diagnostics in range: 1
```

### Requirements

- Read current buffer content, including unsaved changes.
- Handle overloaded or ambiguous symbols by returning candidates.
- Keep existing exact line-range reads.
- Bound very large symbols and offer continuation.

### Acceptance criteria

- An agent can inspect a normal function with one call instead of first requesting the outline and then calculating a line range.

---

## 14. Improve mutation-tool responses

Every successful mutation should return enough information to decide the next step:

```text
Updated src/billing.ts lines 84-106.
Buffer revision: 47 -> 48
Disk state: unsaved
Diffstat: +12 -7
Formatting: not run
Diagnostics: pending for revision 48
Undo token: rb_020
```

Every failed mutation should distinguish:

- Stale revision
- Patch context mismatch
- Permission denial
- User-change conflict
- Invalid syntax in tool arguments
- Filesystem failure
- LSP workspace-edit rejection

Do not return only a generic success or failure string.

---

## 15. Bound and paginate large tool results

### Problem

Full files, broad searches, compiler output, and completed tool messages can consume substantial model context and make repeated state reads expensive.

### Proposed conventions

Every potentially large result should report:

```text
Returned: lines 1-200 of 1,482
Truncated: yes
Continuation: result_81:200
```

Support:

```text
continue_result(cursor, max_lines)
expand_tool_result(tool_event_id)
```

### Requirements

- Never truncate silently.
- Preserve complete raw output for explicit retrieval when feasible.
- Return a concise summary with truncation metadata.
- Avoid embedding complete historical tool results in every chat-state snapshot.
- Apply predictable byte and line limits.

---

## 16. Add related-test discovery

### Proposed interface

```text
find_related_tests(paths=["src/billing.ts"])
```

Suggested result:

```text
Likely tests:
  src/billing.test.ts             high confidence: matching module name
  tests/invoices/billing.spec.ts  medium confidence: imports billing.ts

Suggested command:
  pnpm vitest run src/billing.test.ts tests/invoices/billing.spec.ts
```

### Requirements

- Use imports/references, naming conventions, and project configuration.
- Return suggestions; do not run tests automatically.
- Explain why each test is considered related.
- Mark uncertainty.

---

## 17. Add a detected project profile

When no user-authored instructions cover the information, expose mechanically detected facts:

```text
Package manager: pnpm
Workspace: pnpm-workspace.yaml
Build: pnpm build
Tests: pnpm test
Type check: pnpm typecheck
Formatter: Prettier
Generated paths: src/generated/**
```

### Requirements

- Clearly label detected facts versus user instructions.
- Give `AGENTS.md` and explicit configuration precedence.
- Cache results and invalidate them when manifests change.
- Never invent a command when confidence is low.
- Include the source used for every detected command.

---

## 18. Add structured user questions when a decision blocks work

An agent can always ask in chat, but a structured question tool would improve choices that have a finite set of options:

```text
ask_user(
  question="Which compatibility target should this API preserve?",
  options=["Node 18", "Node 20", "Both"],
  allow_free_text=true
)
```

### Requirements

- Do not use this for rhetorical or unnecessary confirmations.
- Preserve normal free-form chat questions.
- Make options keyboard-accessible.
- Return a durable answer associated with the current turn.
- Permit the user to decline or provide custom text.

---

## 19. Expose tool cancellation and deadlines consistently

Long searches, builds, web fetches, and language-server requests should support:

- Explicit timeout
- User cancellation
- Agent cancellation
- A clear `cancelled`, `timed_out`, or `failed` status
- Partial results when safe

Cancellation should propagate to child processes and not leave the chat lifecycle stuck in an active state.

---

# Priority 3: Internal architecture and consistency

## 20. Move toward a registry-driven tool architecture

Tool behavior appears to be distributed across schema registration, provider filtering, path/scope policy, approval classification, execution dispatch, and UI summary formatting. This makes schema-policy drift possible as the tool set grows.

Consider a central descriptor model:

```rust
struct ToolDescriptor {
    name: &'static str,
    schema: ToolSchema,
    capability: Capability,
    scope_policy: ScopePolicy,
    approval_policy: ApprovalPolicy,
    executor: ToolExecutor,
    summary_formatter: ToolSummaryFormatter,
    output_policy: OutputPolicy,
}
```

Provider manifests, approval decisions, dispatch, help text, and presentation should derive from the same registry wherever possible.

### Acceptance criteria

- Adding a tool does not require independent string matches in many modules.
- Every registered tool has an executor, approval policy, scope policy, and formatter.
- Tests assert that provider-visible schemas and executable tools remain synchronized.

---

## 21. Standardize tool-result envelopes

All tools should expose common metadata where applicable:

```text
status
started_at / duration
workspace root
buffer revision before/after
truncated / continuation
undo or rollback token
warnings
```

The model-facing representation can remain concise. The point is consistent semantics, not verbose output.

### Error taxonomy

Prefer typed errors such as:

- `not_found`
- `permission_denied`
- `outside_workspace`
- `stale_revision`
- `conflict`
- `invalid_arguments`
- `timeout`
- `cancelled`
- `lsp_unavailable`
- `process_failed`

Typed errors make recovery more reliable than interpreting arbitrary prose.

---

## 22. Add harness-level integration scenarios

Create deterministic integration tests for the workflows agents rely on:

1. Read a buffer, user edits it, stale agent edit is rejected.
2. Multi-file change set fails one hunk and applies nothing.
3. Rename symbol previews and atomically applies an LSP workspace edit.
4. Agent rollback preserves a subsequent non-overlapping user edit.
5. Diagnostics wait for the requested buffer revision.
6. Search includes unsaved buffer text.
7. Attached read-only workspace rejects mutation.
8. Long-running process emits incremental output and is cleaned up.
9. Tool output truncation provides a valid continuation.
10. Current-turn diff excludes pre-existing Git and buffer changes.

The headless mode is well suited to running these scenarios against the real editor event loop.

---

# Secondary: Headless and external-client improvements

These are useful, but they should not displace the in-editor workflow priorities above.

## 23. Explicit fresh-chat isolation

**Status: Done (`66ea63d`).** Starting Ovim now creates a fresh durable AI conversation by default for both TUI and headless sessions. Persisted history is restored only when startup explicitly includes:

```text
ovim app.js --resume
ovim app.js --headless --session test --resume
```

A fresh start preserves the prior run database but moves the repository/file/chat binding to a new run before projecting messages or resuming provider context. Hiding and reopening a live chat within the same process still preserves that live conversation. CLI parsing and durable reopen tests cover the opt-in default, successful restoration with `--resume`, fresh run identity, empty projected history, and preservation of old events.

---

## 24. Add a lightweight activity/events API

Polling a full snapshot should not require repeatedly transferring complete message and tool-result history.

Provide a compact activity endpoint or event stream containing:

```text
conversation_id
turn_id
activity
message_generation
attention_generation
pending approval
last completed turn
```

Possible interfaces:

```text
GET /v1/activity
GET /v1/chat/events?after=42
ovim chat wait -s test --until idle --timeout 120s
```

SSE, newline-delimited JSON, or WebSocket events would all be preferable to frequent full-state polling.

---

## 25. Add first-class live-session rendering

The global `--render` path renders a standalone editor even when `--session` is also supplied. Either reject that flag combination clearly or provide:

```text
ovim render -s session-name
ovim render -s session-name --format plain
ovim render -s session-name --dimension 120x36
```

The REST render endpoint already provides much of the required behavior.

---

## 26. Standardize CLI output modes

Provide consistent global controls:

```text
--format pretty|json|plain
--color auto|always|never
--quiet
--version
```

In particular:

- Disable ANSI by default when stdout is not a TTY.
- Allow `send` and `paste` without printing a complete render.
- Provide JSON for health, LSP status, context, and chat submission.
- Add a conventional version command.

---

## 27. Add semantic headless chat commands and turn IDs

Keep key-driven control for end-to-end parity tests, but add semantic automation commands:

```text
ovim chat submit -s test --text "Inspect this project" --format json
ovim chat status -s test --format json
ovim chat wait -s test --turn turn_42
ovim chat approve -s test --request approval_9
ovim chat cancel -s test --turn turn_42
```

Submission should return stable conversation and turn identifiers. This avoids confusing an old idle state with completion of a newly submitted request.

---

# Suggested implementation sequence

## Milestone 1: State and safety

- [ ] **In progress:** Add buffer revisions to reads, diagnostics, and mutations. Revision reporting, mandatory mutation checks, omission/staleness rejection, revision transitions, deterministic same-revision conflicts, and explicit LSP diagnostic document versions have landed. Overlap detection, rebase/force behavior, and direct LSP-to-editor revision mapping remain (`4edcec0`, `01114c9`, `5635a05`, `32764fb`).
- [x] **Done:** Add `workspace_context`. The compact tool reports the current attached root, bounded Git and nested-project scans, distinct editor unsaved state, LSP availability, buffers, selection, and diagnostics (`5e63454`, `3edd3c7`). Controlled additional roots remain part of Milestone 5.
- [ ] **Not started:** Add current-turn edit provenance.
- [ ] **Not started:** Add `read_changes` for workspace and current-turn scopes.
- [ ] **In progress:** Improve mutation result envelopes. Successful mutations now report a common tool/file summary, current changed range or deletion point, line diffstat, revision transition, diagnostics refresh state, save state, agent provenance, and editor-undo guidance. Turn/tool-call identifiers, durable rollback tokens, warnings, and machine-readable error codes remain (`1b6d1dc`).

This milestone immediately improves safety without requiring a full transactional editing system.

## Milestone 2: Transactions

- [ ] **Not started:** Introduce change-set data structures and automatic snapshots.
- [ ] **Not started:** Support atomic create/modify/delete operations.
- [ ] **Not started:** Add preview, commit, and rollback.
- [ ] **Not started:** Integrate change sets with undo and provenance.
- [ ] **Not started:** Add conflict and partial-failure tests.
- [ ] **Not started:** Add `talk_through_changes` as a pedagogical presentation and approval layer over staged change sets.
- [ ] **Not started:** Add reference and change steps, scrollable diff previews, explicit accept/reject controls, and pre-commit revision revalidation.
- [ ] **Not started:** Make replay explanation-only so it can never reapply a retained change set.

## Milestone 3: Semantic operations

- [ ] **Not started:** Add references and call hierarchy.
- [ ] **Not started:** Route rename through previewed change sets.
- [ ] **Not started:** Add code actions and formatting.
- [ ] **Not started:** Apply all LSP workspace edits transactionally.

## Milestone 4: Verification and processes

- [ ] **Not started:** Add revision-aware diagnostic settling and deltas.
- [ ] **Not started:** Add persistent process management.
- [ ] **Not started:** Add explicit verification composition.
- [ ] **Not started:** Add related-test suggestions.

## Milestone 5: Scale and multi-root

- [ ] **Not started:** Attach controlled additional workspaces.
- [ ] **Not started:** Improve search filters, unsaved-buffer search, and pagination.
- [ ] **Not started:** Add symbol-oriented reads.
- [ ] **Not started:** Standardize truncation and continuation across tools.

## Milestone 6: External automation polish

- [x] **Done:** Fresh-chat isolation. Startup creates fresh chats by default; `--resume` explicitly restores persisted conversation history (`66ea63d`).
- [ ] **Not started:** Activity/events API and stable turn IDs.
- [ ] **Not started:** Live-session rendering command.
- [ ] **Not started:** Consistent CLI machine-output modes.

---

# Success criteria for the overall effort

The improvements are successful when an agent can complete a representative multi-file task with a flow like this:

```text
workspace_context
read_symbol / find_references
begin_change_set
apply edits or preview an LSP rename
commit_change_set
wait_for_diagnostics
read_changes(scope="change_set")
verify_changes
```

And the following guarantees hold:

- The agent cannot silently overwrite a newer user edit.
- A failed multi-file operation leaves no partial changes.
- The agent can review only the changes it made.
- Diagnostics are known to correspond to the latest buffer revision.
- A whole agent change can be rolled back without destroying unrelated work.
- Large tool outputs are bounded without silent data loss.
- Bash remains available for unusual cases.
- Common coding work requires fewer coordination calls, not merely more specialized tools.

The key product direction is to make an agent's work feel like a sequence of **coherent, versioned, reviewable transactions**, rather than a loose series of independent file reads, line edits, shell commands, and diagnostic checks.

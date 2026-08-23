# Screen specifications

This file defines what “complete” means for each major surface. It includes implemented features and explicitly planned GUI expressions of capabilities already present in Ovim.

## 1. Dashboard

Purpose: orient a user with no active file and expose high-frequency actions.

Required:

- Ovim identity, version, workspace context, and no invented marketing copy.
- Open file, Open folder, Recent workspaces, Find file, Search project, and AI chat.
- Canonical shortcuts generated for the platform.
- Recent items with missing-path and privacy behavior.
- First-run help entry and settings link.
- Empty recent list, update available, and recovery after failed open.

The current centered logo and shortcut list is a useful skeleton. Replace the rotated gradient tile, strengthen body type, and make the primary “Open” actions explicit.

## 2. Main editor

Purpose: edit with minimal visual competition.

Required:

- Tabs, optional breadcrumbs, split panes, line numbers, git gutter, diagnostics, current line, selection, search match, cursor, IME, overview ruler, and horizontal offset.
- Focused pane and inactive pane state.
- Modified, read-only, externally changed, and save-failed states.
- Empty/untitled buffer.
- Drag-resizable split separators with keyboard alternatives.
- Minimap remains out of scope unless the core projects sufficient data and user value is demonstrated.

The code viewport owns the strongest color. Reduce pane glows and consolidate indicators where several marks currently compete in the gutter.

## 3. Explorer

Purpose: navigate and safely mutate the project tree.

Required:

- Root header with new file, new folder, refresh, collapse all, and overflow menu.
- File and folder Strøk icons, disclosure chevrons, selection, focus, git state, diagnostics, hidden/ignored state.
- Filtering, hidden toggle, git-ignored toggle, and in-panel key help already supported by the core.
- Create, rename, delete confirmation, copy, cut, paste, and collision/error states.
- Context menu and keyboard commands routed to the same safe operations.
- Loading, empty folder, permission denied, missing path, and very large tree.

## 4. Search

Purpose: find and replace project text without forcing a modal picker for sustained work.

Required:

- Query, replace field, match case, whole word, regex, include, and exclude.
- Result groups by file with match count and expandable previews.
- Keyboard navigation and click-to-open.
- Replace one, replace file, replace all confirmation, and write failure.
- Empty query, no results, search running, cancelled, truncated, and invalid regex.

Quick project search may still use the command overlay; the dock is for persistent inspection and replace.

## 5. Source control

Purpose: understand and act on current repository changes.

Required before the activity item is enabled:

- Repository/branch summary and clean/nonrepository states.
- Staged, changes, conflicts, and untracked groups.
- Open diff, stage/unstage, discard confirmation, refresh, commit message, and commit outcome.
- Ahead/behind and publish/sync only when supported by actual core behavior.
- Clear distinction between Git disk state and unsaved editor state.

The current inactive Source Control button should be hidden, disabled with a reason, or implemented. A live-looking dead navigation item is not acceptable.

## 6. Command center and pickers

Purpose: fast, transient navigation and action.

Required:

- Files, commands, symbols, references, recent, and project-text modes as core capability allows.
- Search field, mode label, result-kind icons, highlighted matches, secondary location/detail, result count, and key hints.
- Virtualized long results, loading, no results, error, and partial/truncated results.
- Preview region when it improves selection without obscuring the list.
- Esc dismissal and exact focus restoration.

## 7. Completion and hover

Completion requires kind icon, label, detail, selected state, documentation/detail expansion, loading, empty, and safe placement near the cursor. It must remain within the editor bounds and avoid the IME surface.

Hover uses rendered Markdown where the core provides it, supports scroll and copy, anchors near the source range, and closes predictably. Documentation should not always occupy the top-right corner independent of the cursor.

## 8. Problems

Purpose: scan and navigate diagnostics across the workspace.

Required:

- Error, warning, information, and hint filters.
- Group by file and flat-list modes.
- Message, source, code, location, related information, and count.
- Selected row navigation, stale/refreshing state, and diagnostics cleared.
- Empty state that does not imply the language server is ready when it is not.

## 9. LSP manager

Purpose: understand language support and resolve setup failures.

Required:

- Sections for running, installed, available, syntax-only, installing, and failed.
- Search/filter, language, command/server, state, and contextual action.
- Install progress, retry, show log, restart, and settings actions when supported.
- No-server, offline, permission failure, and unsupported platform state.
- Clear escape route and focus return.

The existing large centered panel is an appropriate topology. Increase text size, replace the search character, and add explicit row actions and detail state.

## 10. AI chat

Purpose: converse with the Ovim agent harness while retaining code context and control.

Required:

- Primary/delegated conversation switcher.
- Model/profile and reasoning selection.
- Transcript with user, assistant, tool activity, thinking, streaming, error, queued input, and attachments.
- Composer with send, stop, queue, context summary, paste image, and empty input behavior.
- Approval, setup/authentication, provider failure, retry, interrupted turn, and reconnect.
- Copy response/code, inspect tool result, edit/remove queue, jump to latest.
- Reading position and follow state preserved.

The dock must prioritize reading over card chrome. Increase transcript typography and reduce nested borders.

## 11. Code walkthrough

Purpose: teach a concept or proposed change while keeping source context visible.

Required:

- Concept and code steps, progress, previous/next, close, discussion input, answer state, and failure.
- Source range highlight linked to the walkthrough card.
- Safe behavior when the source changes or the referenced buffer closes.
- Keyboard navigation, focus loop, and screen-reader step announcements.
- Long prose scroll, long code range guidance, and narrow-height layout.

Concept steps may center; code steps should dock near the source without covering the highlighted lines when space allows.

## 12. Tests

Purpose: run and interpret tests quickly.

Required:

- Scope, command, working directory, status, elapsed time, summary, and output.
- Run nearest, run file/suite, rerun, stop, reveal failure, and copy command.
- Not found, running, passed, failed, cancelled, tool missing, and truncated output.
- Failure-first hierarchy with links to source.

## 13. Debugger

Purpose: control and inspect a debug session.

Required:

- Continue, pause, step over, step into, step out, restart, and stop.
- Call stack, selected frame, variables/watch when the core projects them, debug console/output, and execution line.
- Starting, running, paused, terminated, adapter error, and missing-source states.
- Breakpoint management only after the GUI/core action contract exists.

## 14. Settings

Purpose: find and change Ovim configuration safely.

Phase-one GUI settings should provide:

- Search.
- Common editor, appearance, language/LSP, AI, and keybinding categories.
- Current value, default value, reset, validation, and scope.
- Link to the authoritative Lua or config file for advanced settings.
- Modified/restart-required state and write failure.

Until this screen exists, the Settings rail action may open the command-based settings route, but it must clearly communicate that behavior.

## 15. Notifications and confirmations

Required shared flows:

- Save success/failure and external-file conflict.
- Delete/overwrite confirmation with exact path.
- LSP installation permission/progress.
- AI approval and authentication.
- Workspace trust or shell execution if those capabilities are introduced.
- Reconnect and stale-state recovery.

Each identifies impact, safe default, primary action, cancellation behavior, and recovery.

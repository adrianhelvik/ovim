# Components and patterns

## 1. Component contract

Every interactive component exposes:

- Semantic element or ARIA role.
- Accessible name and optional description.
- Visual states: idle, hover, active, selected, focus-visible, disabled, loading, error.
- Keyboard interaction and pointer interaction that dispatch the same core action.
- Density and icon-size variant.
- Overflow behavior for long paths, localized text, and large counts.
- Theme-safe tokens rather than direct palette values.

State precedence is disabled → pressed → focus-visible → selected/active → hover → idle. Focus is additive and may never be erased by selection styling.

## 2. Icon

The icon component consumes a registry name and renders the Strøk sprite or generated Solid component.

Required API concepts:

- Name: closed union generated from the manifest.
- Size: 16, 20, or 24.
- Label: absent for decorative use; required when the icon is the only button content.
- Tone: inherit, muted, accent, error, warning, information, success.
- Filled state only when the source glyph defines it. CSS must not convert outline icons into filled ones.

Do not embed path strings in feature components. Do not pass arbitrary stroke widths. Full rules are in [Icons](04-icons.md).

## 3. Icon button and tooltip

Icon buttons use a visible glyph inside a larger hit area:

| Context | Icon | Hit area |
| --- | --- | --- |
| Window chrome | 16px | Platform-specific, currently 46×36px |
| Toolbar | 16px | 28×28px |
| Activity rail | 20px | 44×44px |
| Empty-state action | 20px | At least 36px high |

Tooltips show a plain-language label and formatted shortcut after a 400–600ms pointer delay. Keyboard focus shows the tooltip immediately. Tooltips never replace accessible names and never cover the target or cursor.

## 4. Buttons

### Primary

Use for one dominant action in a dialog or onboarding state. Accent fill, accent foreground, 4px radius, 32px minimum height.

### Secondary

Use for supporting actions. Surface field plus border. It may sit beside primary.

### Ghost

Use inside toolbars, rows, and panel headers. Transparent at rest; tonal field on hover and selected state.

### Destructive

Use error color only at the confirmation step. A delete icon elsewhere stays neutral until hover or danger context is explicit.

Loading buttons retain width, keep their label, add a spinner, and expose busy state. Do not replace “Save” with a spinner-only square.

## 5. Activity item

An activity item contains:

- 20px Strøk icon.
- Leading 2px active rail.
- Optional badge placed away from the icon’s recognition silhouette.
- Tooltip with label and shortcut.
- Active state, attention state, and disabled/unavailable state.

Attention is a small semantic badge or dot, not a pulsing whole icon. Source Control may show a change count; Problems may show an error count; AI may show a live agent indicator.

## 6. Tabs

Editor tabs contain file-kind glyph, filename, modified state, and close action on hover/focus. The active tab owns the canvas tone and a top or bottom active edge.

- Minimum width: 112px.
- Maximum width: 240px.
- Modified state is a dot that becomes the close glyph on hover only if the interaction remains understandable to keyboard users.
- Long filenames use middle-aware path disambiguation when duplicates are open.
- Pinned and preview tabs need distinct non-color cues before they are introduced.
- Dragging, reordering, and moving between splits are later behaviors, not implied by pointer styling before implementation.

## 7. Tree row

The tree row uses:

- 16px chevron in a 20px disclosure hit area.
- 16px folder or file-kind icon.
- Primary filename, optional compact git decoration, and optional diagnostic badge.
- 28px row height at default density.
- Full-row selection, with focus-visible outline inside the row.

Single click selects. Double click or Enter activates. Chevron click only expands/collapses. Context menu and keyboard actions call the same safe file-tree operations.

The current colored CSS “file dots” are placeholders. Replace them with Strøk base file/folder glyphs plus a restrained language-color or two-character language badge. Do not create dozens of unrelated tiny illustrations.

## 8. Breadcrumb

Breadcrumbs are one compact row of path segments separated by the 16px chevron glyph. The current file segment uses primary text; ancestors use secondary text.

- Every segment is clickable only after navigation is implemented.
- Collapse the middle of long paths, never the filename.
- Read-only and remote state live at the trailing edge as labeled badges.
- Hide the breadcrumb row when the user disables it or when it repeats a one-segment filename with no useful context.

## 9. Panel header

A panel header is a single reusable pattern:

- Left: title and optional one-line scope/status.
- Right: contextual icon actions.
- Optional tab strip below when the dock hosts multiple surfaces.
- 40px one-line or 52px two-line height.

Avoid permanent uppercase letterspacing on every header. Use sentence case for surface names and technical mono only for scope or live status.

## 10. List and picker row

Single-line rows are 28–32px. Two-line rows are 40–44px.

- Leading icon communicates result kind.
- Primary label contains highlighted match spans.
- Secondary metadata truncates before the primary label.
- Selected state is a tonal field plus a leading accent edge for command overlays.
- Pointer hover follows the cursor but does not silently change keyboard selection until the pointer moves meaningfully.

Picker footer shows result count and keys. It is supplemental; the selected result remains understandable without it.

## 11. Command center

The command center unifies quick open, commands, symbols, workspace search entry, recent files, and settings navigation without merging their result semantics.

It contains:

1. Search/command input with a clear mode prefix.
2. Optional scope chips.
3. Virtualized results.
4. Selected-result detail when needed.
5. Footer with active key hints.

Modes use labels such as Files, Commands, Symbols, or Text—not cryptic prefix punctuation alone. Expert prefixes remain supported and visible in help.

## 12. Inputs

Text inputs use Surface 1, a structural border, 4px radius, and a 32px minimum height. Focus changes the border to accent and adds a one-pixel halo without shifting layout.

- Placeholder text is a hint, never the only label.
- Errors appear below or beside the field with an error icon and recovery action.
- Search inputs include clear, result count, and active filter affordances as needed.
- IME composition text stays visually attached to the editor cursor and is never clipped by overlays.

## 13. Chat composer

The composer is an anchored work surface, not a decorative card.

- Default body text: 13px/19px.
- Auto-grows to a bounded maximum while preserving transcript space.
- Pending attachments are removable chips with file type, filename, and accessible remove action.
- Footer contains context summary on the left and send/stop controls on the right.
- The send icon becomes stop while generation is live; the text alternative changes too.
- Waiting, disabled, queueing, and approval-blocked states are distinct.
- Model and effort selection move to a compact header trigger or composer control, but their popover uses the shared listbox/dialog patterns.

## 14. Chat transcript

Transcript hierarchy:

- User messages: compact tonal field aligned to the right but never narrower than readable prose.
- Assistant response: lower-chrome reading surface; avoid boxing every paragraph.
- Tool activity: collapsed timeline row with icon, verb, target, outcome, and duration.
- Thinking/activity: bounded live group, not a permanent message card.
- Error: error rail, concise cause, and retry or inspect action.
- Queued input: dashed or queued badge plus edit/remove actions that remain keyboard reachable.

Markdown uses the UI body scale. Code blocks use editor mono, copy action, language label, horizontal scroll, and theme syntax when practical.

Follow-to-bottom behavior:

- Auto-follow only while already near the bottom.
- Show a “New activity” jump control when the user has scrolled away.
- Preserve the reading position when earlier content expands.
- Announce completed responses politely to assistive technology without streaming every token.

## 15. Agent switcher

Agents are conversations in a hierarchy, not equal cards stacked above the transcript.

- Show Primary first, followed by descendants with indentation and connecting rails.
- Each row includes the Strøk agent glyph or a compact state node, task name, model, and lifecycle.
- Selected conversation and followed conversation are separate states.
- Live, queued, completed, interrupted, and failed states pair label and shape with color.
- Large trees scroll independently and preserve selection.

The switcher may collapse into the panel header when no delegated agents exist.

## 16. Problems, tests, debugger, and logs

These surfaces share a dock and list vocabulary but preserve distinct information architecture.

### Problems

Group by file or severity, provide counts, and make every row navigable. Severity icon, message, path, line, and source are independently scannable.

### Tests

Show run scope, command, live duration, outcome, and bounded output. Passed output recedes; the first failing assertion and location lead.

### Debugger

Provide a visible control strip for continue, step over, step into, step out, restart, and stop before styling stack frames as clickable. Execution line is the focal state.

### Logs/output

Use monospaced streaming text, pause/follow controls, clear, copy, wrap toggle, and source selector. Truncation is explicit.

## 17. Dialogs, approvals, and confirmations

Dialogs use a 12px radius, strong focus management, title, consequence copy, optional detail, and a stable footer.

- Approval is visually warning-coded and names the requested operation.
- Destructive file actions include exact path and recoverability.
- Authentication/setup states separate explanation, input, error, and actions.
- Enter activates the safe primary action only when expected. Esc cancels when cancellation is safe.
- Never require users to interpret a raw keyboard prompt inside a styled card.

## 18. Notifications

Use status bar messages for routine transient state. Use toasts only for background outcomes the user may miss.

- Maximum three visible toasts.
- Each has status icon, concise message, optional action, and close control.
- Errors persist until dismissed or resolved; success fades after a short interval.
- Repeated identical events update one toast count.

## 19. Empty, loading, and disconnected states

Every major surface has a designed empty state:

- Explorer: No folder open; Open folder.
- Search: Enter a query; recent queries may appear.
- Source Control: Clean workspace or not a repository.
- Problems: No problems in workspace.
- Tests: No test run yet; Run nearest or Run all.
- Debugger: No active session; choose configuration.
- AI: Setup, fresh conversation, offline/provider error, empty delegated-agent tree.

Loading preserves the layout skeleton and uses short concrete labels. Disconnected state appears in status plus an unobtrusive banner only when editing is affected.

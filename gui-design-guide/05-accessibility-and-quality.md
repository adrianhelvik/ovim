# Accessibility and quality

## 1. Accessibility target

Target WCAG 2.2 AA for GUI chrome and workflows. The editor text surface inherits user theme and font configuration, but the application remains responsible for focus, input, labels, state announcements, and non-code contrast.

## 2. Keyboard model

Ovim is keyboard-first, but DOM controls must behave like their native roles.

- Tab and Shift+Tab move among GUI controls only after the user enters a GUI surface.
- Arrow keys navigate menus, listboxes, trees, tabs, and radio groups according to platform expectations.
- Enter or Space activates the focused control where native semantics call for it.
- Esc closes the top transient surface and restores the previous focus.
- Editor key handling resumes when focus returns to the input sink.
- Vim navigation may supplement native patterns inside editor-owned lists, but never remove arrow-key support.
- Global shortcuts dispatch canonical editor actions and show platform-formatted key labels.

Avoid one global capture handler that intercepts events already owned by buttons, inputs, menus, or contenteditable fields. Composition and dead-key events remain untouched.

## 3. Focus

- Every actionable control has a visible focus indicator with at least 3:1 contrast against adjacent colors.
- Focus-visible styling is not clipped by overflow containers.
- Selection and focus are visually distinct in trees, pickers, tabs, agent switchers, and messages.
- Modals trap focus; popovers either trap or rove focus according to interaction type.
- Closing a surface returns focus to the exact invoking control or editor location.
- Pointer use does not permanently suppress focus indication for later keyboard use.

The current one-pixel inset outline is too easy to lose on selected fields. Use a two-part treatment when needed: accent border plus outer or inner halo without changing control size.

## 4. Semantics and announcements

- Icon-only controls use aria-label or visible text through a shared IconButton.
- Navigation exposes current item with aria-current or selected state as appropriate.
- Tabs use tablist, tab, and tabpanel relationships.
- Trees use tree semantics only if hierarchy, expansion, and keyboard behavior are fully implemented; otherwise use a labeled list with buttons.
- Model selection uses listbox/option; effort choice uses a radio group or pressed buttons consistently.
- Dialogs have label, description, initial focus, and return focus.
- Loading regions use aria-busy. Do not announce every streamed token.
- Completed AI response, test completion, connection loss, save error, and approval request use concise live-region messages.
- Decorative SVG is aria-hidden; semantic meaning belongs to the containing control or status label.

## 5. Color, shape, and zoom

- Error, warning, information, success, selection, followed-agent, and modified-file states are not color-only.
- At 200% zoom, primary tasks remain available without two-dimensional page scrolling. Editor content may scroll horizontally by design when wrap is off.
- At system text scaling, headers and status segments truncate safely and expose full content by tooltip.
- Icons retain vector sharpness and do not depend on CSS filters for their base state.
- High contrast and forced-colors modes preserve borders, selection, focus, and button identity.

## 6. Motion and flashing

- Honor prefers-reduced-motion across CSS and any JS animation.
- No interface region flashes more than three times per second.
- Streaming, progress, and cursor animations can be paused through reduced motion.
- Panel entrance motion never moves the code canvas under a user who is typing; prefer immediate resize or an overlay.
- Hover animation is never required to reveal the only action on touch or keyboard.

## 7. Resilience and edge cases

Test at least:

- Empty workspace and single untitled buffer.
- Very long workspace, file, model, command, and agent names.
- Deep file trees and thousands of search or diagnostic results.
- Files with mixed-width Unicode, combining marks, emoji, right-to-left text, and IME composition.
- Read-only files, external changes, dirty buffers, and failed saves.
- Core disconnected/reconnecting and a stale snapshot arriving after a newer one.
- LSP missing, installing, starting, ready, failed, and multiple servers.
- AI unauthenticated, provider unavailable, waiting, streaming, approval-blocked, queued, interrupted, and failed.
- Tests not found, running, passed, failed, cancelled, and output truncated.
- Debugger inactive, running, paused, terminated, and missing source.
- Narrow and short windows at every supported breakpoint.

## 8. Performance quality

Measure, do not infer:

- Idle mutation count and CPU usage.
- Keydown-to-visible-cursor latency.
- Resize bridge call rate.
- Scroll smoothness in editor, tree, transcript, output, and picker.
- Mount cost for markdown-heavy AI history.
- DOM node counts for maximum projected lists.
- Memory after opening and closing large panels repeatedly.

Budget guidance:

- No unbounded DOM lists.
- No layout animation on editor lines.
- No backdrop blur on several nested layers.
- No full-transcript markdown reparse on one streaming token.
- No CSS rule that mixes all theme colors per row when a precomputed semantic token can do the job.

## 9. Visual review matrix

Review each major surface at:

| Appearance | Size | Input |
| --- | --- | --- |
| Dark reference | 1440×900 | Keyboard |
| Dark reference | 1024×768 | Pointer |
| Light theme | 1280×800 | Keyboard |
| High contrast / forced colors | 1280×800 | Keyboard |
| Reduced motion | 1280×800 | Keyboard and pointer |
| 200% zoom | 1280×800 viewport | Keyboard |

Capture stable screenshots for dashboard, editor, split panes, explorer, picker, completion, hover, AI chat, approval, test failure, problems, debugger pause, and LSP manager.

## 10. Release checklist

- No new raw SVG path map, text-symbol icon, or CSS-drawn product glyph.
- No interactive element lacking an accessible name.
- No selected state that erases focus.
- No body copy below 10px or below 4.5:1 contrast.
- No hover-only action without keyboard and persistent alternative.
- No panel with missing empty, error, loading, or overflow behavior.
- No unbounded list or output region.
- No overlay that breaks Esc order or focus return.
- No hard-coded theme color outside documented fallbacks, platform close affordance, or syntax samples.
- No unexpected layout shift when status text, diagnostics, or model names change.
- All Strøk icons inspected at smallest size and as a contact sheet.
- Solid DOM tests, Rust GUI bridge tests, typecheck, build, and screenshot review pass.

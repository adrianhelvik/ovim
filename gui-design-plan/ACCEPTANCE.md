# GUI acceptance gates

These gates apply to the final P0 implementation and to every feature slice in proportion to its scope.

## 1. Visual system

- Semantic tokens own every recurring color, text role, spacing, radius, elevation, duration, control size, and layer.
- Editor theme projection and GUI constants are clearly separated.
- Chrome never outranks the active code/task in contrast.
- Functional copy is at least 10px and normal body/control copy passes 4.5:1.
- Radius, border, and shadow usage follows the guide.
- No dead control looks enabled.

## 2. Icons

- All current GUI glyphs map to the Strøk registry.
- No feature component embeds raw path strings, Unicode substitute icons, or product CSS glyphs.
- Sprite/component output is generated from committed Strøk source.
- The registry is typed and unknown names fail development/build.
- Every glyph passes 16px and 20px light/dark review plus enlarged geometry review.
- Icon-only controls have accessible names and tooltips where useful.
- The generated manifest and sprite are current.

## 3. Layout

- 1440×900 supports full rail, primary dock, editor, and context dock.
- 1280×800 preserves a usable editor and one contextual dock.
- 1024×768 collapses or overlays docks without horizontal crushing.
- 720×560 recovery layout preserves editing and access to all docks.
- Below the supported minimum, the interface communicates the limitation.
- Dock sizes persist per workspace and restore safely.
- The editor canvas measurement remains accurate through dock and split changes.

## 4. Keyboard and pointer

- Every visible action can be reached and activated by keyboard.
- Pointer and keyboard paths dispatch the same core behavior.
- Arrow-key, Tab, Enter, Space, and Esc behavior matches component semantics.
- Vim navigation supplements rather than replaces native list/menu behavior.
- Focus returns to its origin after picker, popover, dialog, approval, or panel dismissal.
- IME, dead keys, composition, paste, copy, and cut do not regress.

## 5. Focus and semantics

- No focus indicator is clipped or indistinguishable from selection.
- Modals trap focus and have name/description.
- Tabs, listboxes, menus, dialogs, buttons, inputs, and disclosures use correct roles and state attributes.
- Decorative SVG is hidden from accessibility APIs.
- Live announcements are concise and do not announce streaming tokens.
- Status is never communicated through color alone.

## 6. States

Each applicable surface demonstrates:

- Default.
- Hover.
- Focus-visible.
- Selected/active.
- Disabled or unavailable.
- Loading/progress.
- Empty/no results.
- Error with recovery.
- Overflow and very long content.
- Disconnected/stale where core state applies.

High-consequence actions additionally demonstrate confirmation, cancellation, failure, and recoverability.

## 7. Workflows

### Editor

- Open, type, select, search, complete, hover, split, switch tab, save, and handle read-only/external change.

### Explorer

- Navigate, disclose, create, rename, copy/cut/paste, delete, filter, toggle hidden/ignored, and recover from filesystem failure.

### Command overlay

- Open, change mode, filter, navigate, preview if applicable, activate, dismiss, and restore focus.

### AI

- Setup/authenticate, choose profile/effort, send, stream, stop, queue, attach/remove, inspect tool activity, approve/reject, switch agent, scroll away, jump to latest, retry failure.

### Problems/LSP

- Filter, navigate, clear, install/start/fail/retry, and distinguish no diagnostics from no ready server.

### Tests/debug

- Start, running/paused, navigate result/frame, stop, pass/fail/terminate, and handle missing tool/source.

## 8. Performance

- Idle workbench produces no repeated application DOM updates.
- Keydown-to-visible-editor response stays within the project’s interactive budget on representative hardware.
- Resize calls are coalesced.
- File tree, search, picker, transcript, logs, and diagnostics remain bounded.
- Hidden panel bodies do not continue expensive rendering or observers.
- Repeated open/close cycles do not cause unbounded memory growth.
- Reduced motion disables nonessential animation without delaying state.

## 9. Visual regression matrix

Required stable captures:

| Surface | States |
| --- | --- |
| Dashboard | first run, recent work, open failure |
| Editor | normal, split, read-only, modified, diagnostics |
| Explorer | normal, selected, filtered, action prompt, error |
| Picker | populated, no results, long results |
| Completion/hover | near every viewport edge |
| AI | idle, streaming, tools, queued, approval, setup, error, agents |
| Problems | populated, empty, server unavailable |
| Tests | running, passed, failed, truncated |
| Debugger | running, paused, terminated/error |
| LSP manager | running, installing, failed |
| Settings | normal, search, invalid value, restart required |

Capture dark and light at 1440×900 and 1024×768 where the surface applies. Add forced-colors or high-contrast captures for primitives and one full workbench.

## 10. Commands

Run the project’s established gates plus targeted design checks:

~~~sh
npm run check --prefix ovim/gui
npm test --prefix ovim/gui
npm run build --prefix ovim/gui
cargo test -p ovim gui:: --lib
~~~

Regenerate and inspect icons:

~~~sh
strok batch gui-design-guide/icons/src \
  --out gui-design-guide/icons/dist \
  --sizes 16,20,24 \
  --color '#c8d3f5' \
  --bg '#090b12' \
  --sprite gui-design-guide/icons/dist/ovim-icons.svg \
  --manifest gui-design-guide/icons/dist/manifest.json \
  --sheet gui-design-guide/icons/dist/contact-sheet.png \
  --columns 6
~~~

Run formatting and repository checks appropriate to changed Rust code. Do not update screenshot baselines until the rendered change has been reviewed.

## 11. Handoff evidence

Every completed phase reports:

- Files and surfaces changed.
- Before/after screenshots at relevant viewports.
- States exercised.
- Keyboard and accessibility checks.
- Test/build results.
- Known limitations and the exact disabled/omitted UI they affect.
- Strøk source, contact sheet, and audit status when icons change.

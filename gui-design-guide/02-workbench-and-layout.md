# Workbench and layout

## 1. Workbench anatomy

The stable desktop shell uses five regions. The editor canvas owns all remaining space.

~~~text
┌──────────────────── title bar / command center ────────────────────┐
├─ rail ─┬─ primary dock ─┬──────── editor canvas ────────┬ context ┤
│        │ explorer/search│ tabs · breadcrumbs · panes     │ AI/test │
│        │ source control │ completion · hover             │ debug   │
├────────┴─────────────────┴──── bottom panel dock ─────────┴─────────┤
└──────────────────────────── status bar ─────────────────────────────┘
~~~

### Title bar

- Height: 36px target; 38px is acceptable while using a custom Tauri frame.
- Left: application mark and compact workspace identity.
- Center: current file and workspace. It may become the command-center trigger, but never looks like a permanent search input until activated.
- Right: native or platform-correct window controls.
- Draggable regions must exclude buttons, menus, and interactive fields.
- macOS should use native traffic-light placement. Windows and Linux may use custom Strøk controls with platform-expected hover and close behavior.

Replace the current gradient “O” tile with a final Ovim mark before brand completion. Until that asset exists, use a quiet single-color placeholder rather than a glowing app-store tile.

### Activity rail

- Width: 44px target.
- Primary order: Explorer, Search, Source Control, AI.
- Secondary workbench items, when implemented: Run and Debug, Problems.
- Settings remains pinned to the bottom.
- An item has icon, accessible name, tooltip, shortcut, active state, and optional count/status badge.
- One item may be active. Clicking it again collapses the primary dock; clicking another swaps dock content without changing editor focus unless the panel requires input.

The 2px leading active rail is the sole persistent selection signal. A background field may appear on hover, but active icons do not also glow.

### Primary dock

- Default width: 260px.
- User-resizable range: 200–420px.
- Hosts Explorer, Search, Source Control, and other navigation surfaces.
- Header: title, optional scope, and a small action group; no two-line uppercase block when one line is enough.
- Dock state and width persist per workspace.

### Editor canvas

- Minimum practical width: 480px per pane.
- Tabs, breadcrumbs, and pane titles are each independently optional.
- The editor line box, cell width, soft wrapping, IME positioning, and scroll projection remain controlled by the shared frontend contract.
- Split dividers have a 1px visual line with an 8px invisible drag target.
- Focused pane uses one inset accent edge or an active pane header, not both.
- Empty space below a short buffer remains the canvas color and must not read as a broken panel.

### Context dock

- Default width: clamp from 340px to 520px.
- Hosts AI chat, test output, and debugger.
- Only one contextual surface expands at a time. The current implementation vertically divides the dock when several are present; replace this with a dock tab strip and badges so every surface remains usable.
- Opening a context surface should preserve the editor selection and current pane.
- The dock can be resized, collapsed, or moved to the bottom through a future layout command.

### Bottom panel dock

- Hosts Problems, terminal, output/logs, and test output when the user places it there.
- Default height: 220px; user-resizable from 120px to 55% of the workbench.
- The panel tab strip shows name, count, and status. Only the active panel body mounts expensive DOM.
- A panel opened because of an error should not permanently steal layout; Esc returns focus, and closing restores the prior workbench dimensions.

### Status bar

- Height: 24px.
- Left begins with mode, branch, change summary, and diagnostics.
- Right carries language, encoding, line ending, indentation, and cursor position.
- Each interactive segment has a tooltip and a related action.
- Collapse low-value metadata before hiding mode, diagnostics, or cursor position.
- Mode uses a colored leading field, but color is not its only cue; the full mode label remains.

## 2. Spatial hierarchy

The expected contrast order is:

1. Cursor, selection, current line, and active completion.
2. Current pane and active transient task.
3. Tabs, active dock, and panel selection.
4. Secondary chrome and metadata.
5. Inactive dividers and background structure.

Do not let titlebar branding, a dock header, or AI activity become brighter than the code unless a blocking approval is active.

## 3. Responsive desktop behavior

The GUI is desktop-adaptive, not mobile responsive.

| Width | Layout |
| --- | --- |
| 1440px and above | Rail, primary dock, editor, and context dock may coexist |
| 1100–1439px | Keep editor plus the most recently used dock; the other dock collapses to its rail/tab |
| 900–1099px | Rail and editor remain; docks open as nonmodal overlays and close on Esc |
| 720–899px | Recovery layout: editor plus compact top/status chrome; all docks are overlays |
| Below 720px | Unsupported for normal editing; show a clear minimum-size affordance rather than silently crushing content |

Height behavior:

- Below 700px, bottom panels default to 35% height and walkthrough dialogs use nearly the full editor body.
- Below 560px, multi-row modal footers collapse to one primary action plus an overflow menu.
- The composer may grow, but it must leave at least 160px of transcript visible.

No responsive rule may hide the only visible label for a state. If text disappears, the icon gains an accessible tooltip and the state remains available elsewhere.

## 4. Overlay topology

Every transient surface belongs to one layer:

| Layer | Examples | Dismissal |
| --- | --- | --- |
| Inline | Completion, signature help, hover | Editor movement, Esc, or action |
| Popover | Model picker, menu, tooltip | Outside click or Esc |
| Command overlay | Quick open, search picker, command center | Esc returns to editor |
| Modal | Approval, destructive confirmation, setup | Explicit decision or Esc when safe |
| System | File picker, authentication browser, OS permission | Platform owned |
| Notification | Toast, save result, disconnected state | Timeout or explicit close |

Rules:

- Esc dismisses only the topmost layer.
- Opening a higher layer pauses pointer interaction below but does not erase state.
- Focus moves into modal and command layers and returns to the exact origin control.
- Inline completion and hover cannot render above AI chat, modal approval, or command overlay.
- Scrims dim; they do not blur code enough to destroy context. Avoid stacked backdrop filters.
- Use a central overlay manager instead of unrelated z-index values in component CSS.

## 5. Panel switching and state persistence

Persist per workspace:

- Primary and context dock visibility, active surface, width, and placement.
- Bottom panel height and active surface.
- Explorer expansion, filter toggles, and scroll position.
- Open tabs, splits, focused pane, and scroll position through the editor session model.

Do not persist:

- Hover, tooltips, transient menus, pending destructive confirmations, or stale loading spinners.
- A modal that cannot safely resume after restart.

When the frontend reconnects to the core, show a nonblocking reconnect state in the status bar, preserve the DOM shell, and reconcile the next authoritative snapshot without flashing the dashboard.

## 6. Platform behavior

- Use platform menu conventions and native accelerators where Tauri supports them.
- Meta is shown on macOS; Ctrl is shown on Windows/Linux. Shortcut copy is generated, not hand-written.
- Window controls match platform order and hit-area expectations.
- Context menus use native-like placement, keyboard navigation, and dismissal even if rendered in the webview.
- High-DPI scaling must not produce half-pixel 1px dividers or clipped Strøk icons.
- Respect system appearance when the user has not selected an explicit Ovim theme.

## 7. Performance layout contract

- Idle editors perform no DOM work beyond cursor blink.
- Virtualize or bound file trees, picker results, transcripts, logs, and diagnostics.
- Only visible editor lines and bounded overscan are projected.
- Resizing coalesces geometry updates and never sends one bridge call per pointer event.
- Hidden dock bodies do not keep expensive observers or markdown rendering active.
- Animations use transform and opacity where possible and never animate editor line geometry.

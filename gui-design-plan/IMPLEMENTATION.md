# Detailed implementation

## Phase 0 — baseline and contracts

Goal: make change measurable before moving visual foundations.

### 0.1 Stable visual fixtures

- Expand mock.ts into named fixtures: dashboard, editor, split, explorer actions, picker, completion, hover, AI idle/live/approval/error, Problems, test failure, debugger paused, and LSP install failure.
- Select fixtures through a development-only query parameter or story route.
- Keep fixtures aligned with GuiSnapshot types and bounded core payloads.
- Capture 1440×900 and 1024×768 reference images for the current GUI.

### 0.2 Design-token contract

- Define semantic GUI token names for color, type, spacing, radius, control sizes, elevation, motion, and z-layers.
- Map GuiTheme into color roles in one adapter.
- Normalize secondary text contrast rather than using raw muted for all copy.
- Fix the accent-fg/accent-foreground mismatch and define ui-font explicitly.
- Document which tokens are theme-projected and which are GUI constants.

### 0.3 Overlay contract

- Inventory completion, hover, picker, LSP, walkthrough, model picker, approval, setup, and future dialogs.
- Define a single layer ordering and Esc/focus-return policy.
- Add tests for “dismiss topmost only.”

Gate:

- Existing tests pass.
- Every major existing surface has a deterministic fixture.
- Token and overlay names are agreed in code before feature CSS migrates.

## Phase 1 — Strøk icon foundation

Goal: remove the largest visible inconsistency and establish an asset pipeline.

### 1.1 Asset location

Keep authored source and design review output in gui-design-guide/icons. Copy or generate shipping assets under the GUI package during build:

~~~text
ovim/gui/src/icons/
├── Icon.tsx
├── iconNames.generated.ts
└── iconManifest.generated.ts

ovim/gui/public/icons/
└── ovim-icons.svg
~~~

If a public sprite is unreliable under the Tauri relative base, generate Solid components instead. Do not maintain both approaches manually.

### 1.2 Generation

- Add a deterministic script that runs Strøk batch, copies the sprite, and converts manifest names into a TypeScript union.
- Check generated assets into the repository only if release builds cannot require a local Strøk binary.
- CI verifies generated output is current without downloading tools.
- One command regenerates SVG, sprite, manifest, review PNGs, and TypeScript names.

### 1.3 Shared components

- Icon renders one registered glyph at 16/20/24 and remains decorative by default.
- IconButton owns label, tooltip, shortcut, pressed/selected state, and hit-area variant.
- ActivityItem composes IconButton with active rail and badge.
- StatusIcon pairs semantic glyph with label/count.

### 1.4 Current migration

Replace:

- Icon path map in App.tsx.
- Activity, status, and window-control inline SVG.
- Tree, breadcrumb, picker, completion, LSP, attachment, diagnostic, and disclosure characters.
- CSS file dots where base file/folder meaning is required.

Add a lightweight lint/test check that rejects new raw path maps and a reviewed list of forbidden substitute glyphs in TSX.

Gate:

- No product icon in current GUI is handwritten inside a feature component.
- Every icon-only control has an accessible label.
- The contact sheet passes at 16/20/24 on dark and light canvases.
- GUI bundle resolves the sprite in development and packaged Tauri builds.

## Phase 2 — foundations and primitives

Goal: convert one large stylesheet into a coherent system without changing behavior.

### 2.1 Proposed structure

~~~text
ovim/gui/src/
├── design/
│   ├── tokens.css
│   ├── reset.css
│   ├── theme.ts
│   └── layers.css
├── components/
│   ├── primitives/
│   │   ├── Button.tsx
│   │   ├── IconButton.tsx
│   │   ├── Tooltip.tsx
│   │   ├── TextField.tsx
│   │   ├── Badge.tsx
│   │   ├── Spinner.tsx
│   │   └── ScrollArea.tsx
│   ├── navigation/
│   ├── overlays/
│   ├── editor/
│   └── panels/
└── styles/
    ├── workbench.css
    ├── editor.css
    ├── panels.css
    └── markdown.css
~~~

Avoid one CSS file per trivial component. Group by stable subsystem.

### 2.2 Token migration

- Replace repeated literal heights, gaps, radii, shadows, timings, and z-indexes.
- Keep editor cell and line metrics explicit because they are functional geometry.
- Reduce arbitrary radius vocabulary to the guide scale.
- Raise functional 8–9px copy to 10–12px or remove it.
- Replace hard-coded shadows on docked surfaces with tonal layering.

### 2.3 Primitive migration

Convert buttons, panel headers, list rows, fields, badges, keyboard hints, spinners, tooltips, and scroll areas incrementally. Preserve event semantics and tests while styling moves.

Gate:

- No undefined CSS custom property.
- No arbitrary z-index outside the layer contract.
- No functional text below 10px.
- All migrated primitives show focus-visible and disabled states.
- styles.css is reduced to an entry/import file or eliminated.

## Phase 3 — workbench shell

Goal: make the desktop topology usable across real window sizes.

### 3.1 Componentize App

Extract:

- WindowFrame and TitleBar.
- ActivityRail.
- PrimaryDock.
- EditorWorkbench.
- ContextDock.
- BottomDock.
- StatusBar.
- OverlayHost.

App retains subscription, high-level state acceptance, and composition. Event/bridge helpers move to focused modules without duplicating authoritative editor state.

### 3.2 Dock model

- Add frontend layout state for visibility, active surface, size, and overlay mode.
- Persist user layout per workspace.
- Use one active body in the context dock; AI, tests, and debugger become tabs with badges.
- Provide pointer resize handles and keyboard size commands.
- Keep bridge viewport calculations tied to measured editor canvas, not assumed chrome widths.

### 3.3 Responsive regimes

Implement the guide’s four desktop width regimes. Add ResizeObserver coverage and tests ensuring panel collapse does not oscillate around a breakpoint.

### 3.4 Titlebar

- Detect platform behavior.
- Use correct window glyph state for maximize versus restore.
- Exclude controls from drag regions.
- Add double-click titlebar maximize if platform-appropriate.
- Make the center title a future command-center trigger without making it look editable at rest.

Gate:

- Editor remains at least 480px wide when docks are docked.
- Docks overlay rather than crush content below 1100px.
- Resize does not flood the Rust bridge.
- Opening or closing any dock preserves editor selection and input focus.

## Phase 4 — editor and transient UI

Goal: make the surfaces around typing share one visual and interaction grammar.

### 4.1 Tabs and breadcrumbs

- Add tab semantics, close affordance, overflow menu, duplicate-name disambiguation, and focus-visible.
- Replace language text placeholders only where a proper file-kind treatment exists.
- Collapse long breadcrumb paths and hide a redundant one-segment row.

### 4.2 Tree

- Apply TreeRow and Strøk disclosure/file/folder icons.
- Separate select, activate, and disclose hit targets.
- Add visible header actions backed by existing safe core operations.
- Add loading, empty, error, hidden, ignored, and clipboard states.

### 4.3 Completion and hover

- Introduce shared anchored-popover placement that flips and clamps within editor bounds.
- Replace first-letter completion kinds with mapped icons or readable abbreviations.
- Add documentation/detail state, loading, and no-result behavior.
- Render hover Markdown safely and keep it near source context.

### 4.4 Command overlay

- Extract Picker/CommandOverlay.
- Add explicit mode label, result-kind glyphs, virtualization, and preview slot.
- Preserve exact keyboard behavior and focus restoration.

### 4.5 Message, prompts, and notifications

- Give prompt, status message, reconnect, save error, and background notification explicit priority.
- Add toast host for outcomes that must survive focus changes.

Gate:

- Typing latency and IME positioning do not regress.
- Transients remain within window bounds at every supported size.
- Esc order and focus return pass integration tests.
- Tree, picker, completion, and tabs have pointer/keyboard parity.

## Phase 5 — navigation surfaces

Goal: turn the activity rail into a truthful map of real capabilities.

### 5.1 Explorer completion

Project existing file-tree operations through typed GUI actions:

- New file/folder.
- Rename.
- Delete with recoverability.
- Copy, cut, paste.
- Refresh/collapse.
- Show hidden and show ignored.
- Help/key reference.

Add context menus only after every menu command maps to the same typed action as keyboard control.

### 5.2 Persistent search

- Define bounded search result projection in Rust.
- Add query, replace, flags, include/exclude, grouped results, progress, cancellation, and truncation.
- Keep quick search in the command overlay.
- Validate replacement transactions through core-safe operations.

### 5.3 Source control

Before enabling the existing button:

- Define snapshot types for repository, groups, file changes, counts, and operation state.
- Add typed actions for supported inspect/stage/unstage/discard/commit behavior.
- Distinguish unsaved editor changes from Git changes.
- Add clean, no repository, conflict, detached HEAD, operation failure, and refresh states.

If this work is deferred, the rail item is disabled with a clear tooltip or omitted.

Gate:

- Every visible activity item opens a functional surface.
- File mutations and Git discard/overwrite actions have explicit confirmation and failure recovery.
- Large trees and result groups are bounded/virtualized.

## Phase 6 — contextual workflows

Goal: make rich secondary work legible without competing with code.

### 6.1 AI chat

- Raise transcript body type and reduce nested card borders.
- Extract message, tool activity, queued input, approval, setup, agent switcher, and composer primitives.
- Put send/stop and attachment removal in the composer.
- Preserve follow-to-bottom and transcript position through expansions.
- Use one dock tab for AI even with tests/debug active.
- Add accessibility announcements for completion and approval, not token streaming.

### 6.2 Agent switcher

- Render Primary and descendants as a compact navigable tree/list.
- Show selected versus followed state separately.
- Cover live, queued, completed, interrupted, and failed.
- Preserve hierarchy at narrow dock widths.

### 6.3 Problems

- Add severity filters, group mode, source/code detail, stale/refreshing state, and empty state aware of LSP readiness.

### 6.4 Tests

- Add run/rerun/stop controls, failure-first output, source navigation, and lifecycle states.
- Move to bottom dock by default when output benefits from width; retain user placement.

### 6.5 Debugger

- Add the control strip before suggesting interactive stack frames.
- Show execution line, lifecycle, missing source, and adapter failure.
- Add variables/watch only when the core projection supports them.

### 6.6 LSP manager

- Use readable row type and Strøk search/status glyphs.
- Add contextual install/retry/restart/log actions as supported.
- Keep progress and error detail visible without overloading one trailing label.

Gate:

- AI, tests, and debugger never divide one narrow dock into unusable fragments.
- All live processes have stop/cancel where the core supports it.
- Streaming and logs remain bounded and maintain scroll intent.
- Blocking approval is visually and semantically unmistakable.

## Phase 7 — dashboard and settings

Goal: complete entry, empty, and configuration journeys.

### 7.1 Dashboard

- Add Open file, Open folder, recent workspaces, quick actions, and first-run help.
- Use a final Ovim identity asset or a quiet placeholder.
- Show no recent items and failed-open states.
- Keep shortcut hints platform-derived.

### 7.2 Settings

- Define a typed common-settings catalog from current options and AI/LSP configuration.
- Search, category navigation, value editor, validation, reset, scope, and restart-required state.
- Link to the Lua/config source for advanced settings.
- Preserve direct command and config workflows for expert users.

Gate:

- A new user can open work, discover core actions, and configure common behavior without memorizing a command.
- An expert can bypass GUI setup without losing capability.

## Phase 8 — accessibility, performance, and release

Goal: validate the entire system, not only happy-path screenshots.

### 8.1 Automated checks

- TypeScript check, GUI tests, build, Rust GUI bridge tests.
- axe or equivalent DOM accessibility tests on every fixture.
- Keyboard navigation integration tests.
- Icon-registry and no-glyph-substitute checks.
- Screenshot comparison at defined sizes and appearances.

### 8.2 Manual checks

- Screen reader smoke test on macOS and Windows.
- 200% zoom and OS text scaling.
- Forced colors/high contrast.
- Reduced motion.
- IME and non-Latin input.
- High-DPI and fractional scaling.
- Mouse, trackpad, and keyboard-only passes.

### 8.3 Performance

- Record idle CPU/DOM mutations.
- Measure keydown-to-cursor update.
- Stress tree, picker, transcript, logs, diagnostics, and rapid resize.
- Ensure hidden dock bodies do not render or observe.

Gate:

- Every item in [ACCEPTANCE.md](ACCEPTANCE.md) passes.
- No P0 coverage row remains partial.
- Remaining P1/P2 work is explicitly disabled or labeled, not represented as a dead control.

## Rust/frontend action boundary

For every new GUI action:

1. Define a typed command and arguments.
2. Route it through the same editor method used by keys/commands.
3. Return or project authoritative next state.
4. Represent failure in the snapshot or structured command error.
5. Test direct GUI dispatch and canonical key behavior against equivalent outcomes.

Do not mutate snapshot-derived state only in SolidJS when the core owns it.

Frontend-only state is limited to presentation concerns such as open tooltip, dock pixel size, active visual tab for simultaneously available panels, transient pointer hover, and local animation phase.

## Testing strategy

### Component

- State rendering and accessible name.
- Keyboard event and pointer event dispatch the same callback.
- Focus-visible, disabled, loading, and overflow.

### Fixture integration

- Full shell against deterministic GuiSnapshot variants.
- Esc and focus return across nested transient surfaces.
- Responsive regime changes.
- Theme and contrast adapter.

### Rust bridge

- Projection bounds.
- Typed action behavior.
- Unicode geometry, wrapping, splits, and viewport calculation.
- New search/source-control/settings data only when implemented.

### Visual

- Stable screenshot set named by surface, state, appearance, and viewport.
- Review intended diffs rather than updating baselines wholesale.
- Icon contact sheet remains a separate asset-level gate.

## Risk register

| Risk | Mitigation |
| --- | --- |
| Visual refactor regresses editor input | Keep input/geometry code isolated; test typing, IME, paste, and pointer hit testing every phase |
| Docks change viewport math | Measure editor canvas directly and coalesce bridge updates |
| Theme colors fail contrast | Normalize GUI semantic roles; do not alter syntax theme silently |
| Sprite path fails in Tauri bundle | Verify packaged build in Phase 1; switch to generated components if needed |
| App extraction duplicates state | Keep snapshots authoritative and isolate only presentation state |
| Persistent panels project too much data | Add bounded Rust view models and virtualization |
| “Complete” GUI exposes unsupported actions | Disable/omit until a typed core action exists |
| Accessibility added late changes structure | Build semantics into primitives in Phase 2 and gate each later phase |

# Ovim GUI design implementation plan

This plan executes the [GUI design guide](../gui-design-guide/README.md) without forking editor behavior away from the shared Rust core.

## Outcome

Ship a complete, coherent desktop workbench in which:

- The editor remains the primary and fastest surface.
- Navigation, panels, overlays, AI, tests, debugging, and setup share one component system.
- Strøk-authored icons replace all improvised inline paths, Unicode substitutes, and CSS glyphs.
- Theme projection is semantic and accessible in dark and light appearances.
- Keyboard and pointer interactions dispatch the same core actions.
- The GUI is resilient across realistic sizes, states, large data, and failure.

## Baseline audit

The current GUI has a strong bridge and editor projection foundation, but the visual implementation is still prototype-shaped:

- App.tsx owns most surfaces in one file and contains an eight-name inline SVG path map.
- File tree, tabs, picker, completion, diagnostics, LSP filter, attachments, and disclosures still use text glyphs or CSS shapes.
- styles.css has theme colors but no spacing, typography, radius, elevation, motion, z-layer, or control-size tokens.
- Accent foreground is named both accent-fg and accent-foreground; ui-font is referenced but not defined.
- Typography ranges from 8px to 13.5px, making contextual UI too small on a 1440×900 frame.
- The context dock vertically divides AI, tests, and debugger when several are active instead of switching one usable surface.
- Responsive behavior only makes small reductions below 900px; docks can crowd the editor.
- Source Control appears interactive but has no action.
- Settings routes to the command line rather than a designed settings surface.
- Explorer lacks visible GUI actions for core-supported safe file operations and toggles.
- Overlay ordering is encoded through local z-index values rather than a shared manager.
- The GUI test suite covers behavior, but stable visual-state fixtures and screenshot matrices are absent.

## Delivery principles

1. Migrate primitives before redesigning features.
2. Keep each slice runnable and testable.
3. Add bridge data only when a designed state needs it.
4. Preserve core keyboard semantics and authoritative snapshots.
5. Centralize state and layout contracts; do not add more feature-local CSS.
6. Review dark, light, narrow, and keyboard states in every phase.
7. Do not enable navigation for a surface that cannot yet do useful work.

## Milestones

| Phase | Theme | Result |
| --- | --- | --- |
| 0 | Baseline and contracts | Stable screenshots, state fixtures, token and overlay contracts |
| 1 | Strøk icon foundation | Generated registry, shared Icon, no ad hoc glyphs in current surfaces |
| 2 | Foundations and primitives | Semantic tokens and reusable controls replace feature-local styling |
| 3 | Workbench shell | Resizable/collapsible docks, correct titlebar, responsive topology |
| 4 | Editor and transient UI | Tabs, tree, picker, completion, hover, messages, and dialogs converge |
| 5 | Navigation surfaces | Explorer complete; Search and Source Control become real surfaces |
| 6 | Contextual workflows | AI, agents, tests, debugger, Problems, and LSP manager reach full states |
| 7 | Dashboard and settings | Complete empty/start experience and discoverable configuration |
| 8 | Accessibility, performance, and release | AA, scale, performance budgets, screenshot and integration gates |

Detailed work is in [IMPLEMENTATION.md](IMPLEMENTATION.md), current-to-target coverage in [COVERAGE.md](COVERAGE.md), and release gates in [ACCEPTANCE.md](ACCEPTANCE.md).

## Priority cut

### P0 — coherent and shippable

- Phases 0–4.
- Explorer completion.
- AI/chat readability and dock switching.
- Problems and LSP manager hardening.
- Accessibility and performance gates for all existing surfaces.

### P1 — complete editor workbench

- Persistent Search.
- Source Control backed by real core actions.
- Test/debug control strips and failure-first layouts.
- Dashboard and common Settings.

### P2 — advanced completeness

- Layout customization and dock relocation.
- Rich settings/keybinding editor.
- Diff editor and advanced source-control operations.
- Extended icon inventory drawn only for implemented controls.

## Recommended first vertical slice

Deliver the icon and primitive foundation across one real workflow:

1. Generate the icon-name union from the committed Strøk manifest.
2. Add Icon, IconButton, Tooltip, and ActivityItem.
3. Replace the activity rail, window controls, tree disclosure/file/folder, picker search/result, and status diagnostics.
4. Add semantic control-size, text, focus, and state tokens.
5. Capture light/dark 16px and 20px visual checks.
6. Add tests that prohibit an unknown icon name and assert accessible labels for icon-only buttons.

This slice materially improves the current GUI while proving the asset pipeline and component API before larger restructuring.

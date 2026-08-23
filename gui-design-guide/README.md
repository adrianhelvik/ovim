# Ovim GUI design guide

Status: proposed design authority for the native Tauri/SolidJS GUI

Mode: operate

Scope: desktop workbench, editor chrome, panels, transient surfaces, AI workflows, and icons

This guide turns the current GUI prototype into an implementation-grade system. It preserves Ovim’s defining product truth: the GUI is a projection of the same fast, keyboard-first editor core as the TUI. It does not create a second command model or hide Vim semantics behind generic desktop chrome.

The repository does not yet contain a confirmed product brief or design authority. The following assumptions are inferred from the current implementation, README, frontend contract, and the request for a more complete GUI:

- The primary user is a developer who edits for long sessions and moves fluidly between keyboard and pointer.
- Ovim should feel native and legible on desktop while remaining recognizably Ovim, not a visual clone of another editor.
- The editor canvas is always the primary surface. Navigation and agents support the code rather than competing with it.
- Theme colors remain owned by the editor core. GUI semantic tokens derive from them and must retain accessible contrast.
- Strøk source files are the sole source of truth for product icons.
- The first shipping appearance is dark because the current theme and long-session editing context establish that operating scene. The system must still support light themes.

## Direction

**Creative north star: “The Precision Workbench.”**

Ovim should feel like a compact instrument built around live code: dark, sharply structured, information-dense, and quiet until state changes. The visual signature is a set of thin structural rails, clear active edges, and Strøk-authored glyphs whose filled nodes make activity and hierarchy legible at small sizes. Code is the brightest and most colorful material; chrome recedes.

The redesign rejects two defaults:

- It is not a VS Code skin. Familiar desktop-editor topology may remain, but composition, modes, icons, and agent behavior must express Ovim’s own mechanism.
- It is not a glowing “AI IDE.” AI is a capable contextual surface with the same hierarchy and restraint as diagnostics, tests, and debugging.

The result should be dense without being tiny, technical without being brittle, and animated only where motion explains state.

## Guide map

- [Foundations](01-foundations.md) defines color, typography, spacing, shape, depth, and motion.
- [Workbench and layout](02-workbench-and-layout.md) defines the desktop shell, responsive behavior, and overlay topology.
- [Components and patterns](03-components-and-patterns.md) defines reusable controls and editor workflows.
- [Icons](04-icons.md) defines the Strøk contract, shipping assets, inventory, and quality gates.
- [Accessibility and quality](05-accessibility-and-quality.md) defines input parity, contrast, motion, resilience, and release checks.
- [Screen specifications](06-screen-specifications.md) defines the expected complete state of every major GUI surface.
- [Icon source and exports](icons/README.md) explains how to review and regenerate the 26-icon reference set.

The build sequence and file-level work are in [gui-design-plan](../gui-design-plan/README.md).

## Durable design rules

**The Code Is the Light Rule.** Syntax, selection, cursor, and the user’s current action receive the strongest contrast. Chrome uses fewer colors and lower contrast, but never drops below accessible text thresholds.

**The One Active Edge Rule.** A region communicates selection through one crisp edge, rail, or field. Do not stack a glow, border, filled background, and bold label to say the same thing.

**The Keyboard Truth Rule.** GUI controls dispatch editor actions; they do not create behavior that the core cannot represent. Labels expose canonical shortcuts, and focus never steals typing from the active editor without an explicit transition.

**The Context, Not Clutter Rule.** Explorer, source control, AI, tests, debugging, and problems occupy named docks. Only the active contextual surface expands; inactive surfaces retain a compact tab or badge.

**The Authored Glyph Rule.** No Unicode symbol, emoji, improvised CSS shape, or ad hoc inline path may stand in for a product icon when a Strøk glyph exists.

## Definition of done

A GUI feature is visually complete only when it has:

- Default, hover, active, selected, focus-visible, disabled, loading, empty, error, and overflow behavior where those states apply.
- Keyboard and pointer parity against the same editor action.
- A Strøk icon or a deliberate text-only treatment.
- Theme-safe semantic color roles in dark and light appearances.
- Layout behavior at 1440×900, 1280×800, 1024×768, and the supported minimum window.
- Screen-reader naming, non-color state cues, and reduced-motion behavior.
- Component or integration coverage for the state transitions it introduces.

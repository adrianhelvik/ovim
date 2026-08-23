# Foundations

## 1. Semantic color system

The editor core remains the source of truth for theme colors. The GUI must not hard-code a second palette around one theme. Normalize the projected theme into stable semantic roles and derive tonal surfaces with color mixing.

| Semantic role | Current source | Reference dark value | Use |
| --- | --- | --- | --- |
| Canvas | theme.background | #090b12 | Editor and application background |
| Text primary | theme.foreground | #c8d3f5 | Code-adjacent labels and primary text |
| Surface 1 | theme.surface | #111522 | Rails, headers, docked panels |
| Surface 2 | derived | #171c2b | Raised controls and floating panel headers |
| Surface selected | theme.surfaceSelected | #242b45 | Selected rows and active fields |
| Border | theme.border | #252b3d | Structural divisions |
| Text secondary | normalized theme.muted | #74819f | Supporting text; at least 4.5:1 on Surface 1 |
| Accent | theme.accent | #82aaff | Current action, focus, active edge |
| Accent foreground | theme.accentForeground | #101018 | Text on accent fills |
| Error | theme.error | #ff757f | Failure and blocking diagnostics |
| Warning | theme.warning | #ffc777 | Caution and pending approval |
| Information | theme.info | #65bcff | Informational diagnostics and tools |
| Success | theme.success | #c3e88d | Passed and healthy state |

Reference values document the current Tokyo Night mock; semantic roles are normative, not those exact hex values.

### Contrast policy

- Body and control text must reach WCAG 2.2 AA contrast of 4.5:1.
- Large text and non-text state indicators must reach 3:1.
- If a projected theme’s muted color fails 4.5:1 for normal text, blend it toward foreground until it passes.
- Syntax colors are exempt only inside code when the user’s theme controls them. GUI labels must not borrow syntax colors.
- Accent is not a general decoration color. It identifies the current action, focused element, active mode edge, or selected match.
- Error, warning, information, and success always pair color with an icon, label, count, or shape.

**The Scarce Accent Rule.** Accent should occupy less than roughly ten percent of a normal workbench frame. Its rarity makes the current action obvious.

## 2. Typography

Ovim needs two explicit type roles.

### UI stack

Use the native system sans stack for navigation, prose, dialog copy, and controls:

~~~css
-apple-system, BlinkMacSystemFont, "Segoe UI", sans-serif
~~~

Inter may lead only when it is bundled with the application. Do not depend on a network font.

### Editor stack

Use the configured editor font for code, commands, key labels, paths, line/column values, logs, and compact technical metadata. The current fallback stack remains suitable:

~~~css
"Berkeley Mono", "SFMono-Regular", "Cascadia Code", "JetBrains Mono", "Iosevka", monospace
~~~

### Type scale

| Role | Size / line height | Weight | Use |
| --- | --- | --- | --- |
| Panel title | 12px / 16px | 650 | Named dock or modal surface |
| Control | 12px / 16px | 500–600 | Buttons, tabs, tree rows |
| Body | 13px / 19px | 400 | AI messages, help, empty states |
| Technical | 11px / 15px | 450–550 | Paths, status, command metadata |
| Micro | 10px / 13px | 550 | Badges and secondary timestamps only |
| Dialog title | 16px / 22px | 650 | High-consequence transient surface |

Do not ship functional copy below 10px. The current 8–9px AI metadata should be raised or removed. Use uppercase only for short group labels, modes, and severity—not ordinary navigation or sentence copy.

**The Mono With Meaning Rule.** Monospace communicates code, coordinates, commands, keys, and machine state. It is not the default UI personality.

## 3. Spacing and density

Use a 4px base unit with optically useful intermediate steps:

| Token | Value | Typical use |
| --- | --- | --- |
| space-1 | 4px | Icon-to-label micro gap |
| space-2 | 6px | Dense row gap |
| space-3 | 8px | Control inset |
| space-4 | 12px | Panel padding |
| space-5 | 16px | Modal and message padding |
| space-6 | 24px | Empty-state and section separation |
| space-8 | 32px | Major onboarding separation |

Default density targets:

- Activity button: 44×44px.
- Toolbar or compact button: 28–32px high.
- Tree, picker, and command row: 28px compact; 32px when two lines are present.
- Tab: 34px high.
- Status bar: 24px high.
- Touch is not the primary input, but any control expected to work on a touch-capable desktop should expose a 40px hit area.

Density is achieved by reducing redundant borders and copy, not by shrinking labels below legibility.

## 4. Shape

The GUI uses disciplined, low-radius geometry:

| Token | Value | Use |
| --- | --- | --- |
| radius-1 | 2px | Selected rows, tiny badges |
| radius-2 | 4px | Buttons, inputs, tabs, tooltips |
| radius-3 | 8px | Messages, composers, popovers |
| radius-4 | 12px | Dialogs and major floating surfaces |
| radius-pill | 999px | Counts and transient jump controls only |

Nested surfaces must step down in radius. A card inside an 8px container uses 4px or 2px corners. Avoid a screen full of unrelated 5px, 6px, 7px, 9px, and 11px values.

## 5. Borders and depth

The workbench is flat by default. Depth comes from tonal layering and structural borders:

- Dock boundaries: 1px border at the semantic border role.
- Active region: one 2px accent rail or one inset accent edge.
- Floating popover: Surface 2, 1px accent-tinted border, one ambient shadow.
- Modal: stronger ambient shadow plus a dimmed scrim.
- No shadow on ordinary rows, tabs, cards, or docked panels.
- Glows are reserved for active cursor or live process indication and must remain subtle.

Suggested elevation vocabulary:

| Role | Shadow |
| --- | --- |
| Popover | 0 12px 32px rgb(0 0 0 / 45%) |
| Dialog | 0 24px 72px rgb(0 0 0 / 60%) |
| Focus halo | 0 0 0 1px color-mix(in srgb, accent 45%, transparent) |

**The Flat Until Floating Rule.** A shadow means the surface has left the workbench plane. If the surface is docked, use tone and a border.

## 6. Motion

Motion explains cause and state; it does not decorate idle software.

| Token | Duration | Use |
| --- | --- | --- |
| instant | 0ms | Keyboard selection and cursor movement |
| fast | 80ms | Hover and pressed feedback |
| standard | 120ms | Disclosure and selection transitions |
| enter | 180ms | Popover or panel entrance |
| long | 240ms max | Major dock rearrangement |

Use a standard ease-out curve for entrances and ease-in for exits. Never delay keyboard feedback to animate it. Spinners may rotate linearly. Caret blinking follows editor preference and stops under reduced motion.

Reduced motion removes spatial travel, rotation, pulsing glows, and nonessential blinking while preserving immediate state changes.

## 7. Content and voice

Ovim’s UI copy is direct, compact, and operational.

- Name the object and the consequence: “Delete src/cache.rs?” rather than “Are you sure?”
- Put the action first in errors: “Couldn’t save. The file changed on disk.”
- Show the canonical shortcut after the label, not inside it.
- Prefer sentence case. Reserve uppercase for modes and compact category labels.
- Never use “Oops,” celebratory confetti, or human-like filler in editor state.
- AI state uses concrete verbs: Reading, Searching, Editing, Running tests, Waiting for approval.

Buttons use verbs: Save, Retry, Open settings, Stop. Destructive actions name the object where space allows.

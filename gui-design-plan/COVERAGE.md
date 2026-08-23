# GUI coverage matrix

| Surface | Current state | Target | Priority |
| --- | --- | --- | --- |
| Theme projection | Core colors projected inline | Semantic GUI roles, contrast normalization, light/dark | P0 |
| Typography | Mixed UI/mono, 8–13.5px | Two-role type system, 10px minimum functional copy | P0 |
| Icons | 8 inline paths plus glyph substitutes | Strøk registry and complete current-surface migration | P0 |
| Titlebar | Custom frame and controls | Platform-correct controls, command-center entry, quiet brand | P0 |
| Activity rail | Explorer/Search/SCM/AI/Settings | Shared activity items, badges, unavailable states | P0 |
| Explorer | Navigation and selection | Actions, context menu, filter/toggles, safe file states | P0 |
| Tabs | Select and modified | Close, disambiguation, overflow, accessible tab semantics | P0 |
| Breadcrumbs | Static path text | Optional navigation, collapse, read-only badge | P1 |
| Editor panes | Strong projection foundation | Resizable separators, focused state, edge-case polish | P0 |
| Status bar | Mode, Git, diagnostics, file metadata | Interactive segments, collapse priorities, tooltips | P0 |
| Message/prompt | Single compact row | Structured status, prompt, reconnect, error priority | P0 |
| Picker | Modal results and selection | Shared command overlay, kinds, modes, virtualization, preview | P0 |
| Completion | Basic list | Kind icons, docs, bounds, loading/empty, ARIA | P0 |
| Hover | Fixed top-right text surface | Source-anchored Markdown, copy/scroll, safe bounds | P0 |
| Problems | Basic bottom list | Filters, grouping, stale/empty, navigation, severity semantics | P0 |
| LSP manager | Filtered modal list | Actions, progress, failure/retry/log, readable type | P0 |
| AI model picker | Search/list/effort | Shared popover/listbox primitives and overflow behavior | P0 |
| AI transcript | Rich state coverage | Reading-first hierarchy, larger type, reduced card chrome | P0 |
| AI composer | Input and attachments | Send/stop/actions, removable attachments, context summary | P0 |
| Agent switcher | Hierarchical row list | Compact conversation switcher with full lifecycle states | P0 |
| Approval/setup | Cards inside dock | Shared modal/approval patterns, focus and recovery | P0 |
| Code walkthrough | Concept/code overlay | Source-aware placement, accessibility, stale-source state | P1 |
| Tests | Output panel | Controls, failure-first navigation, full lifecycle | P1 |
| Debugger | Stack and output | Control strip, execution focus, lifecycle and errors | P1 |
| Dashboard | Logo and shortcuts | Open/recent/actions, first-run, failure and empty states | P1 |
| Search dock | Not implemented | Persistent query/replace/results surface | P1 |
| Source Control dock | Dead activity button | Real repository groups and supported actions | P1 |
| Settings | Command route | Searchable common settings plus config-file escape hatch | P1 |
| Notifications | Status text/ad hoc cards | Shared toast/status policy and deduplication | P1 |
| Responsive desktop | One breakpoint | Four supported desktop regimes and dock overlays | P0 |
| Accessibility | Partial ARIA/focus | WCAG 2.2 AA and input parity matrix | P0 |
| Visual regression | No stable screen matrix | State fixtures and screenshot review at defined sizes | P0 |

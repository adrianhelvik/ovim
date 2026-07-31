# Syntax highlighting: multiline Rust strings

> **Status:** Ready for implementation. The replacement policy is the leading hypothesis; capture raw Tree-sitter and semantic spans in the regression test before treating it as the confirmed cause.

## Observed behavior

Rust multiline string literals that use an escaped newline are highlighted inconsistently. This is visible in `strok-cli/src/cli.rs` on Clap `long_about` attributes such as:

```rust
#[command(long_about = "\
Print an agent-oriented authoring guide. The guide prioritizes final visual
quality over minimizing commands and includes style decisions, geometry traps,
and the render/review loop.

Topics: illustration, icon, logo, diagram

Examples:
  strok guide illustration
  strok guide icon
  strok guide logo
  strok guide diagram")]
```

In Ovim, the opening and closing portions are recognized as string content, but much of the text on the intermediate lines is rendered with the default foreground color instead of the string highlight group. The Rust source is valid and rust-analyzer remains active; this appears to be a highlighting composition issue rather than a parser or diagnostic error.

The same problem repeats on adjacent Clap attributes that use the same `"\` multiline-string form, making the break easy to reproduce in a single viewport.

## Likely area to investigate

Ovim maintains both Tree-sitter syntax highlights and LSP semantic highlights. In `ovim-core/src/buffer/highlighting.rs`, `Buffer::highlights_for_line` uses semantic highlights as an all-or-nothing replacement whenever a line has at least one semantic span:

```rust
if let Some(ref semantic) = self.semantic_highlights {
    if line_idx < semantic.len() && !semantic[line_idx].is_empty() {
        return Cow::Borrowed(semantic[line_idx].as_slice());
    }
}
```

This discards every Tree-sitter span for that line, including portions not covered by semantic tokens. If rust-analyzer emits sparse or segmented semantic tokens for an escaped multiline string, uncovered string content falls back to the default foreground rather than retaining Tree-sitter's `@string` capture.

Tree-sitter's multiline capture distribution itself appears designed to handle captures spanning lines. Both `highlights_for_all_lines_rope` and `highlights_for_line_range_rope` split a captured node across every intersecting line. The first comparison should therefore be:

1. Tree-sitter highlights with semantic tokens disabled.
2. Decoded rust-analyzer semantic spans for the affected lines.
3. Final `highlights_for_line` output with semantic highlighting enabled.

If disabling semantic highlighting fixes the display, the issue is the replacement policy rather than the Rust query or multiline range splitting.

## Recommended behavior

Semantic highlights should overlay Tree-sitter syntax highlights only over ranges they actually cover. They should not replace the complete line merely because one semantic token exists.

A merge should conceptually:

1. Start with the cached Tree-sitter spans for the line.
2. Remove or split only the syntax ranges intersected by semantic spans.
3. Insert semantic spans at higher priority.
4. Preserve syntax highlighting in every uncovered range.

The merged result must be sorted and non-overlapping so rendering does not depend on incidental span order.

This is especially important for multiline strings, macro bodies, attributes, interpolated constructs, and lines where the language server intentionally emits only a subset of available token classes.

## Tests to add

Add a Rust highlighting regression fixture containing:

```rust
const HELP: &str = "\
first line
second line
third line";
```

Then verify:

1. Tree-sitter classifies all content lines as `String`.
2. A sparse semantic token on one intermediate line does not erase the remaining string range.
3. Semantic spans override overlapping syntax spans.
4. Uncovered syntax spans remain present.
5. The viewport-range path behaves the same as the all-lines path when the string starts above or inside the viewport.
6. Escaped-newline strings and raw multiline strings both render consistently.

## Additional UX note

The screenshot also shows the `LSP` status indicator, which can make the failure look like a rust-analyzer classification problem. A debug command or inspector that displays the winning highlight source and group under the cursor—Tree-sitter versus semantic token—would make issues like this substantially easier to diagnose.


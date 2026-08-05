//! Shared wrap computation used by both `WrapMap` (core) and the renderer.
//!
//! This module provides a single source of truth for how lines are broken
//! into visual rows when soft-wrapping is enabled. Both the structural
//! mapping (`WrapMap`) and the visual rendering (`split_line_into_rows`)
//! call into these functions, guaranteeing consistent behaviour.

use crate::display::char_display_width;

/// Computes the character indices where a line should wrap.
///
/// Returns a `Vec<usize>` of char indices at which a new visual row begins.
/// For example, if the line "abcdefgh" wraps at width 3, the result would be
/// `[3, 6]` — meaning rows are chars `[0..3)`, `[3..6)`, `[6..8)`.
///
/// Wide characters that don't fit at the end of a row are pushed to the
/// next row (the remaining space is padded), matching terminal and Neovim
/// behaviour.
///
/// # Arguments
/// * `line` — the text of a single line (no trailing newline)
/// * `max_width` — the available width in display columns (must be ≥ 1)
/// * `tab_width` — how many display columns a tab occupies (tab stops)
pub fn compute_wrap_points(line: &str, max_width: usize, tab_width: usize) -> Vec<usize> {
    compute_wrap_points_with_decorations(line, max_width, tab_width, &[])
}

/// Returns the number of visual rows a line occupies when wrapped.
///
/// This is the authoritative function both `WrapMap` and the renderer
/// should use. It accounts for wide characters being pushed to the next
/// row, unlike a naïve `display_width / wrap_width` calculation.
pub fn visual_line_count(line: &str, max_width: usize, tab_width: usize) -> usize {
    if line.is_empty() {
        return 1;
    }
    // Number of rows = number of wrap points + 1
    compute_wrap_points(line, max_width, tab_width).len() + 1
}

/// Like [`compute_wrap_points`] but accounts for inline decorations (e.g.
/// inlay hints) that add display width at specific character positions.
///
/// `inline_widths` is a sorted slice of `(char_idx, display_width)` pairs.
/// Each decoration's width is added to the running total just before the
/// character at `char_idx`, matching how the renderer inserts decoration
/// text before splitting into rows.
///
/// **End-of-line decorations** (decorations whose `char_idx` is greater than
/// or equal to the line's character count) are processed in a post-loop
/// drain. The renderer's `apply_inline_decorations` appends such
/// decorations to the line via the trailing
/// `if !found { line.spans.push(...) }` branch, then `split_line_into_rows`
/// wraps the whole thing — so the wrap math here must do the same. Without
/// the drain, `WrapMap` undercounts visual rows for type-after-trailing-
/// identifier hints (OV-00257).
pub fn compute_wrap_points_with_decorations(
    line: &str,
    max_width: usize,
    tab_width: usize,
    inline_widths: &[(usize, usize)],
) -> Vec<usize> {
    let max_width = max_width.max(1);
    let tab_width = tab_width.max(1);
    let mut wrap_points = Vec::new();
    // Columns consumed on the current visual row (content + decorations).
    let mut current_width: usize = 0;
    // Flat content-only display column — the tab-stop base. The renderer
    // expands tabs against the raw line BEFORE splicing decoration spans and
    // BEFORE splitting into rows, so a tab's width depends only on the
    // content columns before it: never on decoration widths, and never on
    // the position within the current visual row.
    let mut content_col: usize = 0;
    let mut dec_idx = 0;
    let mut last_char_idx = 0usize;

    for (char_idx, ch) in line.chars().enumerate() {
        last_char_idx = char_idx + 1;
        // Add decoration width at this character position.  Decoration text
        // is inserted before the character by the renderer, so its width is
        // accumulated before the character's own width check.
        //
        // The renderer inserts decoration text character-by-character and can
        // wrap in the middle of a decoration.  We simulate this by adding
        // decoration width one column at a time, flushing a row each time
        // we fill max_width.  All such mid-decoration wraps are recorded at
        // char_idx (the next real character), matching the renderer's layout.
        while dec_idx < inline_widths.len() && inline_widths[dec_idx].0 <= char_idx {
            let dec_w = inline_widths[dec_idx].1;
            for _ in 0..dec_w {
                // Wrap BEFORE consuming the cell: if the row is already full,
                // this cell starts the next row. (Incrementing first and then
                // resetting silently dropped one decoration column whenever a
                // decoration began on an exactly-full row, undercounting
                // visual rows vs. the rendered line.)
                if current_width >= max_width {
                    wrap_points.push(char_idx);
                    current_width = 0;
                }
                current_width += 1;
            }
            dec_idx += 1;
        }

        if ch == '\t' {
            // Tabs expand to spaces before row-splitting, so the renderer can
            // break a row in the middle of a tab's spaces. Consume the tab
            // column-by-column, wrapping lazily (a row that ends exactly full
            // doesn't wrap until more content follows), with every mid-tab
            // wrap recorded at the tab's own char index.
            let ch_width = tab_width - (content_col % tab_width);
            for _ in 0..ch_width {
                if current_width >= max_width {
                    wrap_points.push(char_idx);
                    current_width = 0;
                }
                current_width += 1;
                content_col += 1;
            }
        } else {
            let ch_width = char_display_width(ch);
            if current_width + ch_width > max_width {
                wrap_points.push(char_idx);
                current_width = ch_width;
            } else {
                current_width += ch_width;
            }
            content_col += ch_width;
        }
    }

    // Post-loop drain: any decoration anchored at or beyond the end of the
    // line text is appended after content (mirroring the renderer's
    // append-after-content fallthrough). All such wrap points are recorded
    // at `last_char_idx` — i.e., the position one-past the last character —
    // so callers asking "where does this row break?" get a stable answer.
    //
    // Wrapping lazily BEFORE each cell means an exact-fill at the very last
    // column does NOT push a spurious wrap point. The renderer's
    // `split_line_into_rows` agrees: when content exactly fills a row and
    // nothing more follows, no extra row is emitted (the trailing-row
    // branch only fires when there's more content or no rows yet). Without
    // this, "ab" + a 3-col EOL hint at width 5 would report 2 rows
    // while the renderer reports 1. And if the row is already full when a
    // cell arrives, that cell starts the next row (rather than being lost
    // to an increment-past-full-then-reset).
    while dec_idx < inline_widths.len() {
        let dec_w = inline_widths[dec_idx].1;
        for _ in 0..dec_w {
            if current_width >= max_width {
                wrap_points.push(last_char_idx);
                current_width = 0;
            }
            current_width += 1;
        }
        dec_idx += 1;
    }

    wrap_points
}

/// Returns the visual position `(sub_line, row_col)` of a flat display
/// column within a wrapped line.
///
/// `col` is a **flat display column** — the sum of content widths
/// (characters plus decorations) from the line start, *without* padding
/// from wide-char pushes. This matches how callers compute it:
/// `char_col_to_display_col(..) + inline_width_before(..)`.
///
/// Simulates the same walk as [`compute_wrap_points_with_decorations`],
/// so a `col` that lands inside a split tab or decoration is attributed
/// to the visual row that actually renders that cell. This is the single
/// source of truth for cursor-to-visual math: `WrapMap` and scrolloff
/// calculations both delegate here (OV-00275).
pub fn visual_position_for_flat_col(
    line_text: &str,
    col: usize,
    max_width: usize,
    tab_width: usize,
    inline_widths: &[(usize, usize)],
) -> (usize, usize) {
    let max_width = max_width.max(1);
    let tab_width = tab_width.max(1);

    // `flat_col` tracks the flat display column (content plus decoration
    // widths, no wrap-boundary padding) — this is the coordinate system
    // `col` lives in. `row_col` tracks display columns consumed on the
    // current visual row (used for wrap decisions). `content_col` is the
    // flat content-only column: the tab-stop base, since the renderer
    // expands tabs against the raw line before splicing decorations or
    // splitting rows.
    let mut flat_col: usize = 0;
    let mut content_col: usize = 0;
    let mut row_col: usize = 0;
    let mut sub_line: usize = 0;
    let mut dec_idx: usize = 0;

    for (char_idx, ch) in line_text.chars().enumerate() {
        // Decoration widths at this char position, added column-by-column
        // to match compute_wrap_points_with_decorations.
        while dec_idx < inline_widths.len() && inline_widths[dec_idx].0 <= char_idx {
            let dec_w = inline_widths[dec_idx].1;
            for _ in 0..dec_w {
                if flat_col == col {
                    return (sub_line, row_col);
                }
                flat_col += 1;
                row_col += 1;
                if row_col >= max_width {
                    sub_line += 1;
                    row_col = 0;
                }
            }
            dec_idx += 1;
        }

        if ch == '\t' {
            // Tabs expand before row-splitting, so a row break can land
            // mid-tab. Consume the tab column-by-column, mirroring
            // compute_wrap_points_with_decorations.
            let ch_width = tab_width - (content_col % tab_width);
            for _ in 0..ch_width {
                if flat_col == col {
                    return (sub_line, row_col);
                }
                flat_col += 1;
                content_col += 1;
                row_col += 1;
                if row_col >= max_width {
                    sub_line += 1;
                    row_col = 0;
                }
            }
        } else {
            let ch_width = char_display_width(ch);

            // Wide char that doesn't fit on current row → push to next row.
            // Padding is NOT added to flat_col (it's a rendering artifact,
            // not content width).
            if row_col + ch_width > max_width {
                sub_line += 1;
                row_col = 0;
            }

            if flat_col == col {
                return (sub_line, row_col);
            }

            flat_col += ch_width;
            content_col += ch_width;
            row_col += ch_width;

            if row_col >= max_width {
                sub_line += 1;
                row_col = 0;
            }
        }
    }

    // Post-loop drain: any decoration anchored at or beyond the end of
    // the line text is appended after content (mirroring the renderer's
    // append-after-content fallthrough in `apply_inline_decorations`).
    // Without this drain, end-of-line inlay hints (e.g. type-after-
    // identifier) would not be counted in the visual row math here,
    // and the cursor would land one row above where the renderer
    // actually drew it. (OV-00257)
    //
    // The `remaining > 0` guard mirrors compute_wrap_points_with_decorations:
    // an exact-fill at the very last column must not advance the visual row,
    // so the cursor stays on the same row the renderer drew the content on.
    let mut remaining: usize = inline_widths[dec_idx..].iter().map(|&(_, w)| w).sum();
    while dec_idx < inline_widths.len() {
        let dec_w = inline_widths[dec_idx].1;
        for _ in 0..dec_w {
            if flat_col == col {
                return (sub_line, row_col);
            }
            flat_col += 1;
            row_col += 1;
            remaining -= 1;
            if row_col >= max_width && remaining > 0 {
                sub_line += 1;
                row_col = 0;
            }
        }
        dec_idx += 1;
    }

    // Col is at or past the end of the line content (and decorations).
    if col <= flat_col {
        return (sub_line, row_col);
    }
    let past = col - flat_col;
    let final_col = row_col + past;
    if final_col >= max_width {
        let extra = final_col / max_width;
        (sub_line + extra, final_col % max_width)
    } else {
        (sub_line, final_col)
    }
}

/// Like [`visual_line_count`] but accounts for inline decorations.
pub fn visual_line_count_with_decorations(
    line: &str,
    max_width: usize,
    tab_width: usize,
    inline_widths: &[(usize, usize)],
) -> usize {
    if line.is_empty() && inline_widths.is_empty() {
        return 1;
    }
    compute_wrap_points_with_decorations(line, max_width, tab_width, inline_widths).len() + 1
}

#[cfg(test)]
mod visual_position_tests {
    use super::*;

    #[test]
    fn cursor_at_start_of_split_tab_stays_on_first_row() {
        // Width 5, tab_width 4: "\t\ta" expands to 8 tab cells + "a".
        // Row 0 holds cells 0-4, so the second tab's first cell (flat col 4)
        // renders on row 0 and its remaining cells on row 1. A cursor on the
        // second tab must be attributed to row 0, col 4 (OV-00275).
        assert_eq!(visual_position_for_flat_col("\t\ta", 4, 5, 4, &[]), (0, 4));
        // The "a" (flat col 8) lands on row 1, col 3.
        assert_eq!(visual_position_for_flat_col("\t\ta", 8, 5, 4, &[]), (1, 3));
    }

    #[test]
    fn cursor_on_plain_wrap_boundary() {
        // "abcdef" at width 5: "f" (flat col 5) starts row 1.
        assert_eq!(visual_position_for_flat_col("abcdef", 4, 5, 4, &[]), (0, 4));
        assert_eq!(visual_position_for_flat_col("abcdef", 5, 5, 4, &[]), (1, 0));
    }

    #[test]
    fn cursor_on_pushed_wide_char_lands_on_next_row() {
        // Width 3: "aa世" wraps the wide char to row 1 (flat col 2 = the
        // wide char, no padding counted in flat cols).
        assert_eq!(visual_position_for_flat_col("aa世", 2, 3, 4, &[]), (1, 0));
    }

    #[test]
    fn agrees_with_wrap_row_count_on_tab_sweep() {
        // The sub_line of the last content cell must never exceed the row
        // count implied by compute_wrap_points for the same line.
        for width in 1..12 {
            for tab_width in 1..8 {
                for text in ["\t", "a\tb", "\t\ta", "ab\tcd\te", "\ta\t"] {
                    let rows = visual_line_count(text, width, tab_width);
                    let total: usize = {
                        let mut content = 0usize;
                        let mut flat = 0usize;
                        for ch in text.chars() {
                            let w = if ch == '\t' {
                                tab_width.max(1) - (content % tab_width.max(1))
                            } else {
                                char_display_width(ch)
                            };
                            content += w;
                            flat += w;
                        }
                        flat
                    };
                    for col in 0..=total {
                        let (sub, _) =
                            visual_position_for_flat_col(text, col, width, tab_width, &[]);
                        assert!(
                            sub < rows + 1,
                            "sub_line {sub} out of range for {text:?} col {col} width {width} tab {tab_width} rows {rows}"
                        );
                    }
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_line() {
        assert_eq!(visual_line_count("", 80, 4), 1);
        assert_eq!(compute_wrap_points("", 80, 4), Vec::<usize>::new());
    }

    #[test]
    fn line_fits() {
        assert_eq!(visual_line_count("hello", 80, 4), 1);
        assert_eq!(compute_wrap_points("hello", 80, 4), Vec::<usize>::new());
    }

    #[test]
    fn line_exactly_fits() {
        assert_eq!(visual_line_count("abcde", 5, 4), 1);
        assert_eq!(compute_wrap_points("abcde", 5, 4), Vec::<usize>::new());
    }

    #[test]
    fn line_wraps_once() {
        // "abcdef" at width 5 → "abcde" + "f" = 2 rows
        assert_eq!(visual_line_count("abcdef", 5, 4), 2);
        assert_eq!(compute_wrap_points("abcdef", 5, 4), vec![5]);
    }

    #[test]
    fn line_wraps_twice() {
        // "abcdefghijk" (11 chars) at width 5 → 3 rows
        assert_eq!(visual_line_count("abcdefghijk", 5, 4), 3);
        assert_eq!(compute_wrap_points("abcdefghijk", 5, 4), vec![5, 10]);
    }

    #[test]
    fn wide_char_pushed_to_next_row() {
        // Width 3: "aa世" = 2 + 2 = 4 display cols
        // Row 1: "aa" (can't fit 世, width 2+2=4 > 3) → wrap before 世
        // Row 2: "世"
        assert_eq!(visual_line_count("aa世", 3, 4), 2);
        assert_eq!(compute_wrap_points("aa世", 3, 4), vec![2]);
    }

    #[test]
    fn wide_chars_cause_extra_rows() {
        // Width 3: "世世世" = 6 display cols
        // Naïve: div_ceil(6, 3) = 2
        // Actual: 世(2) fits row1 (pad 1), 世(2) fits row2 (pad 1), 世(2) fits row3 (pad 1) = 3 rows
        assert_eq!(visual_line_count("世世世", 3, 4), 3);
        assert_eq!(compute_wrap_points("世世世", 3, 4), vec![1, 2]);
    }

    #[test]
    fn wide_char_exactly_fits() {
        // Width 4: "aa世" = 2 + 2 = 4, fits exactly
        assert_eq!(visual_line_count("aa世", 4, 4), 1);
        assert_eq!(compute_wrap_points("aa世", 4, 4), Vec::<usize>::new());
    }

    #[test]
    fn tab_handling() {
        // Width 8, tab_width 4: "\thello" = 4 + 5 = 9 display cols → 2 rows
        assert_eq!(visual_line_count("\thello", 8, 4), 2);
        // Tab takes 4 cols, then "hell" fills to 8, "o" wraps
        assert_eq!(compute_wrap_points("\thello", 8, 4), vec![5]);
    }

    #[test]
    fn tab_at_boundary() {
        // Width 4, tab_width 4: "\ta" = 4 + 1 = 5 display cols → 2 rows
        assert_eq!(visual_line_count("\ta", 4, 4), 2);
        assert_eq!(compute_wrap_points("\ta", 4, 4), vec![1]);
    }

    #[test]
    fn mixed_wide_and_ascii() {
        // Width 5: "ab世cd" = 1+1+2+1+1 = 6
        // Row 1: "ab世" (1+1+2=4, next 'c' would be 5 → fits), so "ab世c" (5)
        // Row 2: "d"
        assert_eq!(visual_line_count("ab世cd", 5, 4), 2);
        assert_eq!(compute_wrap_points("ab世cd", 5, 4), vec![4]);
    }

    #[test]
    fn width_1() {
        // Each character gets its own row (wide chars also get 1 row since width=max(1,1)=1)
        // But wide chars (width 2) can't fit in width 1... they still need to go somewhere.
        // We put them on their own row (width overflows but it's the minimum).
        assert_eq!(visual_line_count("abc", 1, 4), 3);
        assert_eq!(compute_wrap_points("abc", 1, 4), vec![1, 2]);
    }

    #[test]
    fn control_chars() {
        // Control char \x01 has display width 2 (caret notation ^A)
        // Width 3: "a\x01b" = 1 + 2 + 1 = 4 → 2 rows
        // "a\x01" = 3, then "b" = 1 → wraps after char 2
        assert_eq!(visual_line_count("a\x01b", 3, 4), 2);
        assert_eq!(compute_wrap_points("a\x01b", 3, 4), vec![2]);
    }

    // --- decoration-aware wrap tests ---

    #[test]
    fn decoration_no_effect_when_line_fits() {
        // "hello" (5) + ": i32" (5) = 10, fits in width 20
        let decs = vec![(5, 5)];
        assert_eq!(
            compute_wrap_points_with_decorations("hello", 20, 4, &decs),
            Vec::<usize>::new()
        );
        assert_eq!(visual_line_count_with_decorations("hello", 20, 4, &decs), 1);
    }

    #[test]
    fn decoration_causes_wrap() {
        // "let x = 5" (10 chars) at width 12
        // Without decoration: 10 cols, fits in 1 row
        // With ": i32" (5 cols) at char 5: "let x: i32 = 5" = 15 cols → wraps
        let decs = vec![(5, 5)];
        assert_eq!(
            compute_wrap_points_with_decorations("let x = 5", 12, 4, &decs),
            vec![7] // wrap happens at char 7 (after "let x" + ": i32" = 10, then " =" = 12, " " wraps)
        );
    }

    #[test]
    fn decoration_at_start_of_line() {
        // Decoration at char 0 with width 10, line "ab" at width 8
        // Decoration (10) > width (8) → wraps, then "ab" fits in next row
        let decs = vec![(0, 10)];
        let points = compute_wrap_points_with_decorations("ab", 8, 4, &decs);
        assert!(
            !points.is_empty(),
            "should wrap when decoration exceeds width"
        );
        assert_eq!(visual_line_count_with_decorations("ab", 8, 4, &decs), 2);
    }

    #[test]
    fn multiple_decorations_on_same_line() {
        // "a b c" at width 10
        // Dec at char 1: 3 cols, dec at char 3: 3 cols
        // Total: a(1) + dec(3) + " "(1) + b(1) + dec(3) + " "(1) + c(1) = 11 → wraps
        let decs = vec![(1, 3), (3, 3)];
        assert!(visual_line_count_with_decorations("a b c", 10, 4, &decs) >= 2);
    }

    #[test]
    fn empty_decorations_same_as_plain() {
        let line = "abcdefghij";
        for width in [3, 5, 8, 80] {
            assert_eq!(
                compute_wrap_points_with_decorations(line, width, 4, &[]),
                compute_wrap_points(line, width, 4),
                "empty decorations should match plain wrap at width {}",
                width
            );
        }
    }

    #[test]
    fn decoration_spanning_multiple_rows() {
        // "ab" at width 3, decoration "12345" (5 cols) at char 1.
        // Composed: "a12345b" → rows: "a12", "345", "b" = 3 rows.
        // Previously undercounted as 2 because the decoration width
        // was added atomically.
        let decs = vec![(1, 5)];
        assert_eq!(
            visual_line_count_with_decorations("ab", 3, 4, &decs),
            3,
            "decoration spanning multiple rows must count all visual rows"
        );
    }

    #[test]
    fn decoration_exactly_fills_row() {
        // "ab" at width 5, decoration "123" (3 cols) at char 1.
        // Composed: "a123b" → row 0: "a123b" (5 cols, exact fit) = 1 row.
        let decs = vec![(1, 3)];
        assert_eq!(visual_line_count_with_decorations("ab", 5, 4, &decs), 1);
    }

    #[test]
    fn large_decoration_at_start() {
        // "a" at width 3, decoration "1234567" (7 cols) at char 0.
        // Composed: "1234567a" → rows: "123", "456", "7a" = 3 rows.
        let decs = vec![(0, 7)];
        assert_eq!(visual_line_count_with_decorations("a", 3, 4, &decs), 3);
    }

    /// Cross-validation: visual_line_count should agree with
    /// compute_wrap_points().len() + 1 for all inputs.
    #[test]
    fn count_agrees_with_wrap_points() {
        let cases = [
            ("", 80),
            ("hello", 3),
            ("世世世", 3),
            ("世世世", 4),
            ("世世世", 5),
            ("a\tb\tc", 8),
            ("\t\t\t", 4),
            ("abcdefghij", 3),
        ];
        for (line, width) in cases {
            let points = compute_wrap_points(line, width, 4);
            let count = visual_line_count(line, width, 4);
            assert_eq!(
                count,
                points.len() + 1,
                "mismatch for {:?} at width {}: count={}, points={:?}",
                line,
                width,
                count,
                points
            );
        }
    }
}

#[cfg(test)]
mod flat_tab_expansion_consistency {
    use super::*;

    fn expand_flat(text: &str, tab_width: usize) -> String {
        let mut out = String::new();
        let mut col = 0;
        for ch in text.chars() {
            if ch == '\t' {
                let n = tab_width - (col % tab_width);
                out.push_str(&" ".repeat(n));
                col += n;
            } else {
                out.push(ch);
                col += crate::display::char_display_width(ch);
            }
        }
        out
    }

    #[test]
    fn wrap_count_matches_flat_tab_expansion() {
        let mut mismatches = Vec::new();
        let contents: Vec<String> = (0..40)
            .flat_map(|prefix| {
                (1..6).map(move |tabs| {
                    format!(
                        "{}{}{}",
                        "a".repeat(prefix),
                        "\t".repeat(tabs),
                        "b".repeat(7)
                    )
                })
            })
            .chain((0..30).map(|p| format!("{}\tx\ty\tz", "世".repeat(p))))
            .collect();
        for tab_width in [2usize, 4, 8] {
            for width in 3usize..30 {
                for content in &contents {
                    let rowwise = visual_line_count(content, width, tab_width);
                    let expanded = expand_flat(content, tab_width);
                    let flat = visual_line_count(&expanded, width, tab_width);
                    if rowwise != flat {
                        mismatches.push(format!(
                            "width={width} tab={tab_width} content={content:?}: map={rowwise} renderer={flat}"
                        ));
                    }
                }
            }
        }
        assert!(
            mismatches.is_empty(),
            "{} mismatches, first 10:\n{}",
            mismatches.len(),
            mismatches[..mismatches.len().min(10)].join("\n")
        );
    }
}

#[cfg(test)]
mod decoration_composed_consistency {
    use super::*;

    /// Reference: what the renderer actually does — splice decoration text
    /// into the (tab-free) line before the char at its anchor, append
    /// end-of-line decorations, then split the composed text into rows.
    fn composed_row_count(line: &str, max_width: usize, decs: &[(usize, usize)]) -> usize {
        let mut composed = String::new();
        let mut dec_idx = 0;
        for (char_idx, ch) in line.chars().enumerate() {
            while dec_idx < decs.len() && decs[dec_idx].0 <= char_idx {
                composed.push_str(&"x".repeat(decs[dec_idx].1));
                dec_idx += 1;
            }
            composed.push(ch);
        }
        while dec_idx < decs.len() {
            composed.push_str(&"x".repeat(decs[dec_idx].1));
            dec_idx += 1;
        }
        visual_line_count(&composed, max_width, 4)
    }

    #[test]
    fn decoration_row_count_matches_composed_text() {
        let mut mismatches = Vec::new();
        for width in 3usize..14 {
            for line_len in 1usize..3 * width {
                let line: String = "abcdefghij".chars().cycle().take(line_len).collect();
                for anchor in 0..=line_len {
                    for dec_w in 1usize..8 {
                        let decs = vec![(anchor, dec_w)];
                        let ours = visual_line_count_with_decorations(&line, width, 4, &decs);
                        let reference = composed_row_count(&line, width, &decs);
                        if ours != reference {
                            mismatches.push(format!(
                                "width={width} line_len={line_len} anchor={anchor} dec_w={dec_w}: ours={ours} composed={reference}"
                            ));
                        }
                    }
                }
            }
        }
        assert!(
            mismatches.is_empty(),
            "{} mismatches, first 10:\n{}",
            mismatches.len(),
            mismatches[..mismatches.len().min(10)].join("\n")
        );
    }
}

//! Layout of the branch review buffer: a summary header, a toolbar, then the
//! patch in either a unified or a side-by-side layout.
//!
//! Every rendered line records where its text came from ([`ReviewRow`]), so
//! `Enter` can jump to the source, `]c`/`]f` can navigate, and a refresh can
//! put the cursor back on the same change — in either layout.

use std::ops::Range;
use std::time::Duration;

use crate::display::grapheme_display_width;
use crate::native_diff::{BaseKind, DiffFile, PatchLine, PatchLineKind, ReviewPatch};
use crate::syntax::HighlightGroup;
use crate::unicode::grapheme_indices;

use super::highlight::PatchHighlights;

/// Display-name prefix shared with the GUI, which uses it to enable diff
/// line styling for pathless buffers.
pub const DIFF_REVIEW_TITLE_PREFIX: &str = "Diff · ";

/// Width used to lay out the side-by-side view before the editor has rendered
/// once (and in frontends that do not report a text width).
pub const DEFAULT_LAYOUT_WIDTH: usize = 120;

/// Narrowest code column the side-by-side view will lay out. Below this it
/// overflows the viewport rather than shrinking to unreadable slivers.
const MIN_SPLIT_TEXT_WIDTH: usize = 8;

/// Rows one side-by-side line may wrap to. A minified bundle would otherwise
/// turn a single line into thousands of rows.
const MAX_WRAP_ROWS: usize = 12;

const KEY_HINT: &str =
    "# Enter open at cursor · ]c [c hunk · ]f [f file · r refresh · q close · <Space>gf fetch base";

/// How the patch body is laid out.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum DiffLayout {
    /// One column, raw patch text: what `git diff` prints.
    #[default]
    Unified,
    /// Old on the left, new on the right, with source line numbers.
    Split,
}

impl DiffLayout {
    pub fn label(self) -> &'static str {
        match self {
            DiffLayout::Unified => "Unified",
            DiffLayout::Split => "Split",
        }
    }

    pub fn toggled(self) -> Self {
        match self {
            DiffLayout::Unified => DiffLayout::Split,
            DiffLayout::Split => DiffLayout::Unified,
        }
    }

    /// Parses a `:GitDiffLayout` argument.
    pub fn parse(name: &str) -> Option<Self> {
        match name.trim().to_ascii_lowercase().as_str() {
            "unified" | "u" | "inline" => Some(DiffLayout::Unified),
            "split" | "s" | "side" | "side-by-side" => Some(DiffLayout::Split),
            _ => None,
        }
    }
}

/// A stretch of one patch line rendered inside a row.
#[derive(Debug, Clone, Copy)]
pub struct ReviewCell {
    pub info: PatchLine,
    /// Index of the source line in [`ReviewPatch::lines`].
    pub patch_line: usize,
    /// Byte range of the region this cell paints (line numbers, marker, text
    /// and padding), used for the added/removed background tint.
    pub bytes_start: usize,
    pub bytes_end: usize,
    /// Grapheme column in the row where the cell's source text starts.
    pub text_col: usize,
    /// Graphemes of source text this row shows for the cell.
    pub text_len: usize,
    /// Glyph index in the source body at which this row's text begins;
    /// non-zero only on wrapped continuation rows.
    pub src_glyph: usize,
}

impl ReviewCell {
    fn contains(&self, col: usize) -> bool {
        col >= self.text_col && col < self.text_col + self.text_len.max(1)
    }
}

/// One rendered buffer line: the header rows carry no cell, unified rows carry
/// one, and side-by-side rows carry the old side, the new side, or both.
#[derive(Debug, Clone, Default)]
pub struct ReviewRow {
    pub left: Option<ReviewCell>,
    pub right: Option<ReviewCell>,
}

impl ReviewRow {
    fn single(cell: ReviewCell) -> Self {
        Self {
            left: Some(cell),
            right: None,
        }
    }

    /// The patch line this row primarily represents.
    pub fn info(&self) -> Option<PatchLine> {
        self.left.or(self.right).map(|cell| cell.info)
    }

    /// The cell the cursor sits in: the one covering `col`, else the last one
    /// starting at or before it (so the padding after a cell still belongs to
    /// it), else whichever side exists.
    pub fn cell_at(&self, col: usize) -> Option<ReviewCell> {
        let cells = [self.left, self.right];
        cells
            .iter()
            .flatten()
            .find(|cell| cell.contains(col))
            .or_else(|| cells.iter().flatten().rfind(|cell| cell.text_col <= col))
            .copied()
            .or(self.left)
            .or(self.right)
    }

    /// `(byte range, added)` regions of the row to tint.
    pub fn tints(&self) -> impl Iterator<Item = (Range<usize>, bool)> {
        [self.left, self.right]
            .into_iter()
            .flatten()
            .filter_map(|cell| match cell.info.kind {
                PatchLineKind::Added => Some((cell.bytes_start..cell.bytes_end, true)),
                PatchLineKind::Removed => Some((cell.bytes_start..cell.bytes_end, false)),
                _ => None,
            })
    }
}

/// The clickable layout switch drawn above the patch.
#[derive(Debug, Clone, Default)]
pub struct Toolbar {
    /// Buffer line the toolbar occupies.
    pub line: usize,
    /// `(grapheme columns, layout)` for each button.
    pub buttons: Vec<(Range<usize>, DiffLayout)>,
}

impl Toolbar {
    /// The layout a click at `(line, col)` selects, if any.
    pub fn hit(&self, line: usize, col: usize) -> Option<DiffLayout> {
        if line != self.line {
            return None;
        }
        self.buttons
            .iter()
            .find(|(range, _)| range.contains(&col))
            .map(|(_, layout)| *layout)
    }
}

/// Everything a render produces for the review buffer.
pub struct Rendered {
    pub title: String,
    pub text: String,
    pub rows: Vec<ReviewRow>,
    /// `(buffer line, file index)` for each row of the file summary.
    pub stat_rows: Vec<(usize, usize)>,
    pub hunk_lines: Vec<usize>,
    pub file_lines: Vec<usize>,
    pub toolbar: Toolbar,
    pub highlights: Vec<Vec<(Range<usize>, HighlightGroup)>>,
}

/// One rendered grapheme of a patch line body.
///
/// A tab expands to several glyphs that all point back at the tab, so a
/// cursor anywhere in the indentation resolves to the tab character.
#[derive(Debug, Clone, Copy)]
pub struct Glyph<'a> {
    /// Byte offset of the source grapheme this glyph renders.
    pub src_byte: usize,
    pub text: &'a str,
    pub width: usize,
}

/// Splits a patch line body into rendered glyphs.
///
/// The side-by-side layout pads to fixed columns, so it must know the exact
/// width of everything it draws — tabs are expanded here rather than left for
/// the terminal. The unified layout keeps the raw bytes so the buffer still
/// yanks as a valid patch.
pub fn layout_body(body: &str, tab_width: usize, expand_tabs: bool) -> Vec<Glyph<'_>> {
    let tab_width = tab_width.max(1);
    let mut glyphs = Vec::with_capacity(body.len().min(512));
    let mut column = 0;
    for (offset, grapheme) in grapheme_indices(body) {
        if expand_tabs && grapheme == "\t" {
            let stop = ((column / tab_width) + 1) * tab_width;
            for _ in column..stop {
                glyphs.push(Glyph {
                    src_byte: offset,
                    text: " ",
                    width: 1,
                });
            }
            column = stop;
            continue;
        }
        let width = grapheme_display_width(grapheme).max(1);
        glyphs.push(Glyph {
            src_byte: offset,
            text: grapheme,
            width,
        });
        column += width;
    }
    glyphs
}

/// Splits glyphs into runs no wider than `width` display columns. Always
/// returns at least one (possibly empty) run.
fn chunk_glyphs(glyphs: &[Glyph<'_>], width: usize) -> Vec<Range<usize>> {
    let width = width.max(1);
    let mut runs = Vec::new();
    let mut start = 0;
    let mut used = 0;
    for (index, glyph) in glyphs.iter().enumerate() {
        // `used > 0` keeps a glyph wider than the column from looping.
        if used > 0 && used + glyph.width > width {
            runs.push(start..index);
            start = index;
            used = 0;
        }
        used += glyph.width;
    }
    runs.push(start..glyphs.len());
    runs
}

/// Accumulates one rendered line together with its highlight spans.
#[derive(Default)]
struct RowBuilder {
    text: String,
    graphemes: usize,
    spans: Vec<(Range<usize>, HighlightGroup)>,
}

impl RowBuilder {
    fn push(&mut self, text: &str, group: Option<HighlightGroup>) {
        if text.is_empty() {
            return;
        }
        let start = self.text.len();
        self.text.push_str(text);
        if let Some(group) = group {
            self.spans.push((start..self.text.len(), group));
        }
        self.graphemes += grapheme_indices(text).count();
    }

    fn pad(&mut self, spaces: usize) {
        if spaces > 0 {
            self.push(&" ".repeat(spaces), None);
        }
    }
}

/// What [`push_cell`] needs to draw one side of a row.
struct CellSpec<'a> {
    info: PatchLine,
    patch_line: usize,
    glyphs: &'a [Glyph<'a>],
    segment: Range<usize>,
    code: &'a [(Range<usize>, HighlightGroup)],
    /// Source line number, drawn only on the first row of a wrapped line.
    number: Option<usize>,
    number_width: usize,
    marker: Option<char>,
    /// Column width to pad the text to; `None` leaves it ragged (unified).
    pad_to: Option<usize>,
    /// Whether more of this line exists than the row can show.
    elided: bool,
}

/// Draws one cell into `row` and returns its geometry.
fn push_cell(row: &mut RowBuilder, spec: CellSpec<'_>) -> ReviewCell {
    let bytes_start = row.text.len();
    let marker_group = match spec.info.kind {
        PatchLineKind::Added => Some(HighlightGroup::DiffAdded),
        PatchLineKind::Removed => Some(HighlightGroup::DiffRemoved),
        _ => None,
    };

    if spec.number_width > 0 {
        let number = match spec.number {
            Some(number) => format!("{number:>width$} ", width = spec.number_width),
            None => " ".repeat(spec.number_width + 1),
        };
        row.push(&number, Some(HighlightGroup::Comment));
    }
    if let Some(marker) = spec.marker {
        row.push(&marker.to_string(), marker_group);
        if spec.pad_to.is_some() {
            row.push(" ", None);
        }
    }

    let text_col = row.graphemes;
    let src_glyph = spec.segment.start;
    // Reserve the last column for the elision marker so a clipped cell still
    // ends exactly on the column boundary.
    let segment = match (spec.elided, spec.pad_to) {
        (true, Some(pad_to)) => trim_to_width(spec.glyphs, &spec.segment, pad_to.saturating_sub(1)),
        _ => spec.segment.clone(),
    };
    let shown = &spec.glyphs[segment.clone()];
    let groups = resolve_groups(spec.glyphs, &segment, spec.code);

    let mut used = 0;
    let mut index = 0;
    while index < shown.len() {
        let group = groups[index];
        let mut end = index + 1;
        while end < shown.len() && groups[end] == group {
            end += 1;
        }
        let mut text = String::new();
        for glyph in &shown[index..end] {
            text.push_str(glyph.text);
            used += glyph.width;
        }
        row.push(&text, group);
        index = end;
    }
    let text_len = shown.len();

    if let Some(pad_to) = spec.pad_to {
        if spec.elided {
            row.push("…", Some(HighlightGroup::Comment));
            used += 1;
        }
        row.pad(pad_to.saturating_sub(used));
    }

    ReviewCell {
        info: spec.info,
        patch_line: spec.patch_line,
        bytes_start,
        bytes_end: row.text.len(),
        text_col,
        text_len,
        src_glyph,
    }
}

/// Shortens a glyph run so it fits in `width` display columns.
fn trim_to_width(glyphs: &[Glyph<'_>], segment: &Range<usize>, width: usize) -> Range<usize> {
    let mut used = 0;
    let mut end = segment.start;
    for index in segment.clone() {
        let next = used + glyphs[index].width;
        if next > width {
            break;
        }
        used = next;
        end = index + 1;
    }
    segment.start..end
}

/// Highlight group per glyph in `segment`; the most specific span wins, as in
/// the buffer renderer.
fn resolve_groups(
    glyphs: &[Glyph<'_>],
    segment: &Range<usize>,
    code: &[(Range<usize>, HighlightGroup)],
) -> Vec<Option<HighlightGroup>> {
    let mut best: Vec<Option<(HighlightGroup, usize)>> = vec![None; segment.len()];
    for (range, group) in code {
        if range.start >= range.end {
            continue;
        }
        let length = range.end - range.start;
        let lo = glyphs.partition_point(|glyph| glyph.src_byte < range.start);
        let hi = glyphs.partition_point(|glyph| glyph.src_byte < range.end);
        for index in lo.max(segment.start)..hi.min(segment.end) {
            let slot = &mut best[index - segment.start];
            if slot.is_none_or(|(_, current)| length < current) {
                *slot = Some((*group, length));
            }
        }
    }
    best.into_iter()
        .map(|slot| slot.map(|(group, _)| group))
        .collect()
}

/// Accumulates the whole review buffer.
#[derive(Default)]
struct Builder {
    text: String,
    rows: Vec<ReviewRow>,
    highlights: Vec<Vec<(Range<usize>, HighlightGroup)>>,
}

impl Builder {
    fn len(&self) -> usize {
        self.rows.len()
    }

    fn push(&mut self, line: RowBuilder, row: ReviewRow) -> usize {
        let index = self.rows.len();
        self.text.push_str(&line.text);
        self.text.push('\n');
        self.rows.push(row);
        self.highlights.push(line.spans);
        index
    }

    fn header(&mut self, text: &str, group: Option<HighlightGroup>) -> usize {
        let mut line = RowBuilder::default();
        line.push(text, group);
        self.push(line, ReviewRow::default())
    }
}

/// Renders the review buffer.
pub fn render(
    patch: &ReviewPatch,
    layout: DiffLayout,
    width: usize,
    unsaved_buffers: usize,
    tab_width: usize,
) -> Rendered {
    let bodies: Vec<&str> = patch_bodies(patch);
    let code = PatchHighlights::compute(patch, &bodies);

    let mut builder = Builder::default();
    let mut stat_rows = Vec::new();
    render_header(patch, unsaved_buffers, &mut builder, &mut stat_rows);
    let toolbar = render_toolbar(layout, &mut builder);
    builder.header(KEY_HINT, Some(HighlightGroup::Comment));
    builder.header("", None);

    let mut hunk_lines = Vec::new();
    let mut file_lines = vec![usize::MAX; patch.files.len()];
    match layout {
        DiffLayout::Unified => render_unified(
            patch,
            &bodies,
            &code,
            tab_width,
            &mut builder,
            &mut hunk_lines,
            &mut file_lines,
        ),
        DiffLayout::Split => render_split(
            patch,
            &bodies,
            &code,
            tab_width,
            width,
            &mut builder,
            &mut hunk_lines,
            &mut file_lines,
        ),
    }

    // Every file must be addressable by `]f` and by the summary rows, even if
    // it produced no header line of its own.
    let fallback = builder.len().saturating_sub(1);
    let file_lines: Vec<usize> = file_lines
        .into_iter()
        .map(|line| if line == usize::MAX { fallback } else { line })
        .collect();

    Rendered {
        title: format!(
            "{DIFF_REVIEW_TITLE_PREFIX}{} → {}",
            patch.head, patch.base.name
        ),
        text: builder.text,
        rows: builder.rows,
        stat_rows,
        hunk_lines,
        file_lines,
        toolbar,
        highlights: builder.highlights,
    }
}

/// The text of each patch line without its marker column.
fn patch_bodies(patch: &ReviewPatch) -> Vec<&str> {
    let lines: Vec<&str> = patch.text.lines().collect();
    patch
        .lines
        .iter()
        .enumerate()
        .map(|(index, info)| {
            let text = lines.get(index).copied().unwrap_or("");
            match info.kind {
                PatchLineKind::Added | PatchLineKind::Removed | PatchLineKind::Context => {
                    text.get(1..).unwrap_or("")
                }
                _ => text,
            }
        })
        .collect()
}

fn render_header(
    patch: &ReviewPatch,
    unsaved_buffers: usize,
    builder: &mut Builder,
    stat_rows: &mut Vec<(usize, usize)>,
) {
    builder.header(
        &format!("{} → {}", patch.head, patch.base.name),
        Some(HighlightGroup::DiffHeader),
    );
    builder.header(&describe_base(patch), Some(HighlightGroup::Comment));
    if patch.files.is_empty() {
        builder.header("No changes", Some(HighlightGroup::Comment));
    } else {
        let mut line = RowBuilder::default();
        line.push(&format!("{} · ", plural(patch.files.len(), "file")), None);
        line.push(
            &format!("+{}", patch.additions()),
            Some(HighlightGroup::DiffAdded),
        );
        line.push(" ", None);
        line.push(
            &format!("−{}", patch.deletions()),
            Some(HighlightGroup::DiffRemoved),
        );
        builder.push(line, ReviewRow::default());
    }
    if unsaved_buffers > 0 {
        builder.header(
            &format!(
                "! {} with unsaved changes; the review reflects what is on disk",
                plural(unsaved_buffers, "buffer")
            ),
            Some(HighlightGroup::DiffRemoved),
        );
    }
    if patch.truncated {
        builder.header(
            "! Diff truncated at 4 MiB",
            Some(HighlightGroup::DiffRemoved),
        );
    }

    if patch.files.is_empty() {
        return;
    }
    builder.header("", None);
    let label_width = patch
        .files
        .iter()
        .map(|file| text_width(&stat_label(file)))
        .max()
        .unwrap_or(0)
        .min(72);
    for (index, file) in patch.files.iter().enumerate() {
        let label = stat_label(file);
        let mut line = RowBuilder::default();
        line.push("  ", None);
        line.push(
            &status_letter(&file.status).to_string(),
            Some(status_group(&file.status)),
        );
        line.push("  ", None);
        line.push(&label, None);
        line.pad(label_width.saturating_sub(text_width(&label)) + 2);
        if file.binary {
            line.push("binary", Some(HighlightGroup::Comment));
        } else {
            if file.additions > 0 || file.deletions == 0 {
                line.push(
                    &format!("+{}", file.additions),
                    Some(HighlightGroup::DiffAdded),
                );
            }
            if file.deletions > 0 {
                if file.additions > 0 {
                    line.push(" ", None);
                }
                line.push(
                    &format!("−{}", file.deletions),
                    Some(HighlightGroup::DiffRemoved),
                );
            }
        }
        stat_rows.push((builder.len(), index));
        builder.push(line, ReviewRow::default());
    }
}

fn render_toolbar(layout: DiffLayout, builder: &mut Builder) -> Toolbar {
    builder.header("", None);
    let mut line = RowBuilder::default();
    let mut buttons = Vec::new();
    line.push("  ", None);
    for option in [DiffLayout::Unified, DiffLayout::Split] {
        let label = format!("[ {} ]", option.label());
        let start = line.graphemes;
        line.push(
            &label,
            Some(if option == layout {
                HighlightGroup::DiffLocation
            } else {
                HighlightGroup::Comment
            }),
        );
        buttons.push((start..line.graphemes, option));
        line.push("  ", None);
    }
    line.push(
        "· click, or press s, to switch view",
        Some(HighlightGroup::Comment),
    );
    let toolbar_line = builder.push(line, ReviewRow::default());
    Toolbar {
        line: toolbar_line,
        buttons,
    }
}

// ---------------------------------------------------------------------------
// Unified layout
// ---------------------------------------------------------------------------

fn render_unified(
    patch: &ReviewPatch,
    bodies: &[&str],
    code: &PatchHighlights,
    tab_width: usize,
    builder: &mut Builder,
    hunk_lines: &mut Vec<usize>,
    file_lines: &mut [usize],
) {
    let raw: Vec<&str> = patch.text.lines().collect();
    for (index, info) in patch.lines.iter().enumerate() {
        let text = raw.get(index).copied().unwrap_or("");
        let line_number = builder.len();
        match info.kind {
            PatchLineKind::Added | PatchLineKind::Removed | PatchLineKind::Context => {
                let body = bodies[index];
                let glyphs = layout_body(body, tab_width, false);
                let mut line = RowBuilder::default();
                let cell = push_cell(
                    &mut line,
                    CellSpec {
                        info: *info,
                        patch_line: index,
                        glyphs: &glyphs,
                        segment: 0..glyphs.len(),
                        code: code.line(index),
                        number: None,
                        number_width: 0,
                        marker: Some(marker_for(info.kind)),
                        pad_to: None,
                        elided: false,
                    },
                );
                builder.push(line, ReviewRow::single(cell));
            }
            kind => {
                if kind == PatchLineKind::HunkHeader {
                    hunk_lines.push(line_number);
                }
                if kind == PatchLineKind::FileHeader {
                    if let Some(file) = info.file {
                        if let Some(slot) = file_lines.get_mut(file) {
                            if *slot == usize::MAX {
                                *slot = line_number;
                            }
                        }
                    }
                }
                let mut line = RowBuilder::default();
                line.push(text, Some(structural_group(kind, text)));
                builder.push(line, ReviewRow::single(header_cell(*info, index, text)));
            }
        }
    }
}

/// A zero-width cell so header rows still resolve to a file for `Enter`.
fn header_cell(info: PatchLine, patch_line: usize, text: &str) -> ReviewCell {
    ReviewCell {
        info,
        patch_line,
        bytes_start: 0,
        bytes_end: text.len(),
        text_col: 0,
        text_len: 0,
        src_glyph: 0,
    }
}

// ---------------------------------------------------------------------------
// Side-by-side layout
// ---------------------------------------------------------------------------

/// Column geometry of the side-by-side layout.
struct SplitGeometry {
    number_width: usize,
    text_width: usize,
}

impl SplitGeometry {
    fn new(patch: &ReviewPatch, width: usize) -> Self {
        let highest = patch
            .lines
            .iter()
            .filter_map(|info| info.new_line.max(info.old_line))
            .max()
            .unwrap_or(1);
        let number_width = highest.to_string().len().clamp(3, 7);
        // gutter = number + space + marker + space
        let gutter = number_width + 3;
        // " │ " between the columns.
        let text_width = width.saturating_sub(3).saturating_sub(2 * gutter) / 2;
        Self {
            number_width,
            text_width: text_width.max(MIN_SPLIT_TEXT_WIDTH),
        }
    }

    fn row_width(&self) -> usize {
        2 * (self.number_width + 3 + self.text_width) + 3
    }
}

const SEPARATOR: char = '│';

#[allow(clippy::too_many_arguments)]
fn render_split(
    patch: &ReviewPatch,
    bodies: &[&str],
    code: &PatchHighlights,
    tab_width: usize,
    width: usize,
    builder: &mut Builder,
    hunk_lines: &mut Vec<usize>,
    file_lines: &mut [usize],
) {
    let raw: Vec<&str> = patch.text.lines().collect();
    let geometry = SplitGeometry::new(patch, width);
    let mut pending_old: Vec<usize> = Vec::new();
    let mut pending_new: Vec<usize> = Vec::new();
    let mut current_file: Option<usize> = None;

    for (index, info) in patch.lines.iter().enumerate() {
        // A new file starts a fresh banner and flushes anything pending.
        if info.file != current_file {
            flush_pair(
                patch,
                bodies,
                code,
                tab_width,
                &geometry,
                builder,
                &mut pending_old,
                &mut pending_new,
            );
            current_file = info.file;
            if let Some(file) = info.file {
                if let Some(entry) = patch.files.get(file) {
                    let line = builder.len();
                    let mut row = RowBuilder::default();
                    row.push(
                        &file_banner(entry, geometry.row_width()),
                        Some(HighlightGroup::DiffHeader),
                    );
                    builder.push(row, ReviewRow::single(header_cell(*info, index, "")));
                    if let Some(slot) = file_lines.get_mut(file) {
                        if *slot == usize::MAX {
                            *slot = line;
                        }
                    }
                }
            }
        }

        match info.kind {
            // The raw `diff --git` / `index` / `---` / `+++` block is replaced
            // by the banner above.
            PatchLineKind::FileHeader => {}
            PatchLineKind::HunkHeader => {
                flush_pair(
                    patch,
                    bodies,
                    code,
                    tab_width,
                    &geometry,
                    builder,
                    &mut pending_old,
                    &mut pending_new,
                );
                let text = raw.get(index).copied().unwrap_or("");
                hunk_lines.push(builder.len());
                let mut row = RowBuilder::default();
                row.push(text, Some(HighlightGroup::DiffLocation));
                builder.push(row, ReviewRow::single(header_cell(*info, index, text)));
            }
            PatchLineKind::Meta => {
                flush_pair(
                    patch,
                    bodies,
                    code,
                    tab_width,
                    &geometry,
                    builder,
                    &mut pending_old,
                    &mut pending_new,
                );
                let text = raw.get(index).copied().unwrap_or("");
                let mut row = RowBuilder::default();
                row.push(text, Some(HighlightGroup::Comment));
                builder.push(row, ReviewRow::single(header_cell(*info, index, text)));
            }
            PatchLineKind::Context => {
                flush_pair(
                    patch,
                    bodies,
                    code,
                    tab_width,
                    &geometry,
                    builder,
                    &mut pending_old,
                    &mut pending_new,
                );
                emit_pair(
                    patch,
                    bodies,
                    code,
                    tab_width,
                    &geometry,
                    builder,
                    Some(index),
                    Some(index),
                );
            }
            PatchLineKind::Removed => pending_old.push(index),
            PatchLineKind::Added => pending_new.push(index),
        }
    }
    flush_pair(
        patch,
        bodies,
        code,
        tab_width,
        &geometry,
        builder,
        &mut pending_old,
        &mut pending_new,
    );
}

/// Emits the buffered removals and additions of a change block, pairing them
/// row by row so a rewritten line lines up with its replacement.
#[allow(clippy::too_many_arguments)]
fn flush_pair(
    patch: &ReviewPatch,
    bodies: &[&str],
    code: &PatchHighlights,
    tab_width: usize,
    geometry: &SplitGeometry,
    builder: &mut Builder,
    pending_old: &mut Vec<usize>,
    pending_new: &mut Vec<usize>,
) {
    let rows = pending_old.len().max(pending_new.len());
    for row in 0..rows {
        emit_pair(
            patch,
            bodies,
            code,
            tab_width,
            geometry,
            builder,
            pending_old.get(row).copied(),
            pending_new.get(row).copied(),
        );
    }
    pending_old.clear();
    pending_new.clear();
}

#[allow(clippy::too_many_arguments)]
fn emit_pair(
    patch: &ReviewPatch,
    bodies: &[&str],
    code: &PatchHighlights,
    tab_width: usize,
    geometry: &SplitGeometry,
    builder: &mut Builder,
    old: Option<usize>,
    new: Option<usize>,
) {
    let old_glyphs = old.map(|index| layout_body(bodies[index], tab_width, true));
    let new_glyphs = new.map(|index| layout_body(bodies[index], tab_width, true));
    let old_runs = old_glyphs
        .as_ref()
        .map(|glyphs| chunk_glyphs(glyphs, geometry.text_width));
    let new_runs = new_glyphs
        .as_ref()
        .map(|glyphs| chunk_glyphs(glyphs, geometry.text_width));

    let needed = old_runs
        .as_ref()
        .map_or(0, Vec::len)
        .max(new_runs.as_ref().map_or(0, Vec::len));
    let shown = needed.clamp(1, MAX_WRAP_ROWS);

    for row in 0..shown {
        let mut line = RowBuilder::default();
        let left = old.and_then(|index| {
            let glyphs = old_glyphs.as_ref()?;
            let runs = old_runs.as_ref()?;
            Some(push_cell(
                &mut line,
                CellSpec {
                    info: patch.lines[index],
                    patch_line: index,
                    glyphs,
                    segment: runs.get(row).cloned().unwrap_or(glyphs.len()..glyphs.len()),
                    code: code.line(index),
                    number: (row == 0).then(|| patch.lines[index].old_line).flatten(),
                    number_width: geometry.number_width,
                    marker: Some(if row == 0 {
                        marker_for(patch.lines[index].kind)
                    } else {
                        ' '
                    }),
                    pad_to: Some(geometry.text_width),
                    elided: row + 1 == shown && runs.len() > shown,
                },
            ))
        });
        if left.is_none() {
            line.pad(geometry.number_width + 3 + geometry.text_width);
        }

        line.push(" ", None);
        line.push(&SEPARATOR.to_string(), Some(HighlightGroup::Punctuation));
        line.push(" ", None);

        let right = new.and_then(|index| {
            let glyphs = new_glyphs.as_ref()?;
            let runs = new_runs.as_ref()?;
            Some(push_cell(
                &mut line,
                CellSpec {
                    info: patch.lines[index],
                    patch_line: index,
                    glyphs,
                    segment: runs.get(row).cloned().unwrap_or(glyphs.len()..glyphs.len()),
                    code: code.line(index),
                    number: (row == 0).then(|| patch.lines[index].new_line).flatten(),
                    number_width: geometry.number_width,
                    marker: Some(if row == 0 {
                        marker_for(patch.lines[index].kind)
                    } else {
                        ' '
                    }),
                    pad_to: Some(geometry.text_width),
                    elided: row + 1 == shown && runs.len() > shown,
                },
            ))
        });

        builder.push(line, ReviewRow { left, right });
    }
}

fn marker_for(kind: PatchLineKind) -> char {
    match kind {
        PatchLineKind::Added => '+',
        PatchLineKind::Removed => '-',
        _ => ' ',
    }
}

fn file_banner(file: &DiffFile, width: usize) -> String {
    let stats = if file.binary {
        format!("{}  binary", status_letter(&file.status))
    } else {
        format!(
            "{}  +{} −{}",
            status_letter(&file.status),
            file.additions,
            file.deletions
        )
    };
    let label = stat_label(file);
    let head = format!("── {label} ");
    let used = text_width(&head) + text_width(&stats) + 2;
    let fill = width.saturating_sub(used).min(240);
    format!("{head}{} {stats}", "─".repeat(fill))
}

// ---------------------------------------------------------------------------
// Shared formatting
// ---------------------------------------------------------------------------

fn structural_group(kind: PatchLineKind, text: &str) -> HighlightGroup {
    match kind {
        PatchLineKind::HunkHeader => HighlightGroup::DiffLocation,
        PatchLineKind::FileHeader
            if text.starts_with("diff ")
                || text.starts_with("--- ")
                || text.starts_with("+++ ") =>
        {
            HighlightGroup::DiffHeader
        }
        _ => HighlightGroup::Comment,
    }
}

fn status_group(status: &str) -> HighlightGroup {
    match status {
        "added" => HighlightGroup::DiffAdded,
        "deleted" => HighlightGroup::DiffRemoved,
        _ => HighlightGroup::DiffLocation,
    }
}

fn text_width(text: &str) -> usize {
    grapheme_indices(text)
        .map(|(_, grapheme)| grapheme_display_width(grapheme).max(1))
        .sum()
}

pub fn summary_message(patch: &ReviewPatch) -> String {
    if patch.files.is_empty() {
        return format!("No changes: {} matches {}", patch.head, patch.base.name);
    }
    format!(
        "{} → {}: {} · +{} −{}",
        patch.head,
        patch.base.name,
        plural(patch.files.len(), "file"),
        patch.additions(),
        patch.deletions()
    )
}

fn describe_base(patch: &ReviewPatch) -> String {
    let base = &patch.base;
    match base.kind {
        BaseKind::OnDefaultBranch => {
            format!("On {}: uncommitted changes against HEAD", base.name)
        }
        BaseKind::NoDefaultBranch => {
            "No default branch found: uncommitted changes against HEAD".to_string()
        }
        BaseKind::Explicit if !base.spec.ends_with("...WORKTREE") => {
            format!("Comparison {}", base.spec)
        }
        BaseKind::DefaultBranch | BaseKind::Explicit => {
            let mut parts = Vec::new();
            if let Some(merge_base) = &patch.merge_base {
                parts.push(format!("merge-base {merge_base}"));
            }
            parts.push(format!("{} ahead", plural(patch.ahead, "commit")));
            if patch.behind > 0 {
                parts.push(format!("{} behind", patch.behind));
            }
            if base.remote.is_some() {
                let fetched = match (base.ever_fetched, base.fetched_ago) {
                    (false, _) => "never fetched".to_string(),
                    (true, Some(age)) => format!("fetched {}", humanize(age)),
                    (true, None) => "fetched".to_string(),
                };
                parts.push(format!("{} {fetched}", base.name));
            }
            parts.join(" · ")
        }
    }
}

fn stat_label(file: &DiffFile) -> String {
    match &file.old_path {
        Some(old) => format!("{old} → {}", file.path),
        None => file.path.clone(),
    }
}

fn status_letter(status: &str) -> char {
    match status {
        "added" => 'A',
        "deleted" => 'D',
        "renamed" => 'R',
        "copied" => 'C',
        "typechanged" => 'T',
        "conflicted" => 'U',
        _ => 'M',
    }
}

fn plural(count: usize, noun: &str) -> String {
    if count == 1 {
        format!("1 {noun}")
    } else {
        format!("{count} {noun}s")
    }
}

fn humanize(age: Duration) -> String {
    let seconds = age.as_secs();
    if seconds < 90 {
        "just now".to_string()
    } else if seconds < 90 * 60 {
        format!("{} min ago", seconds / 60)
    } else if seconds < 36 * 3600 {
        format!("{} h ago", seconds / 3600)
    } else {
        format!("{} d ago", seconds / 86_400)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn humanize_buckets() {
        assert_eq!(humanize(Duration::from_secs(10)), "just now");
        assert_eq!(humanize(Duration::from_secs(600)), "10 min ago");
        assert_eq!(humanize(Duration::from_secs(3 * 3600)), "3 h ago");
        assert_eq!(humanize(Duration::from_secs(3 * 86_400)), "3 d ago");
    }

    #[test]
    fn tabs_expand_to_the_next_stop_and_point_back_at_the_tab() {
        let glyphs = layout_body("\tx", 4, true);
        assert_eq!(glyphs.len(), 5);
        assert!(glyphs[..4].iter().all(|glyph| glyph.src_byte == 0));
        assert_eq!(glyphs[4].src_byte, 1);

        // Without expansion a tab is a single glyph, so unified rows keep the
        // raw patch bytes.
        let raw = layout_body("\tx", 4, false);
        assert_eq!(raw.len(), 2);
    }

    #[test]
    fn chunking_never_loops_on_a_glyph_wider_than_the_column() {
        let glyphs = layout_body("日本語", 4, true);
        let runs = chunk_glyphs(&glyphs, 1);
        assert_eq!(runs.len(), 3);
        assert_eq!(runs[0], 0..1);
    }

    #[test]
    fn chunking_an_empty_body_yields_one_empty_run() {
        assert_eq!(chunk_glyphs(&[], 20), vec![0..0]);
    }

    #[test]
    fn layout_parses_its_command_argument() {
        assert_eq!(DiffLayout::parse("Split"), Some(DiffLayout::Split));
        assert_eq!(DiffLayout::parse("unified"), Some(DiffLayout::Unified));
        assert_eq!(DiffLayout::parse("nope"), None);
    }
}

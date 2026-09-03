//! Per-language syntax highlighting for the branch review.
//!
//! Delta colours a patch with the grammar of the file each hunk came from
//! rather than with a "diff" grammar, and that is what makes a review
//! readable. We do the same: for every changed file we rebuild the two sides
//! of the patch — context+added for the new side, context+removed for the old
//! one — parse each with that file's grammar and map the resulting spans back
//! onto the patch lines they came from.
//!
//! The reconstruction only contains the lines the patch shows, so a hunk that
//! opens a block comment it never closes can tint the rest of that file's
//! side. That is the price of parsing exactly what is on screen: the work is
//! proportional to the diff, not to the size of the files it touches, and a
//! wrong colour is never a wrong jump target.

use std::ops::Range;

use crate::native_diff::{PatchLineKind, ReviewPatch};
use crate::syntax::{HighlightGroup, Language, LanguageRegistry, SyntaxHighlighter};

/// Total bytes of reconstructed source one review may parse. Past this, the
/// remaining files keep their diff-level colouring instead of stalling the UI.
const MAX_PARSE_BYTES: usize = 2 * 1024 * 1024;

/// Which side of the patch a reconstruction represents.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Side {
    New,
    Old,
}

impl Side {
    fn covers(self, kind: PatchLineKind) -> bool {
        match self {
            Side::New => matches!(kind, PatchLineKind::Added | PatchLineKind::Context),
            Side::Old => matches!(kind, PatchLineKind::Removed | PatchLineKind::Context),
        }
    }
}

/// Language highlights for each line of a patch, in body byte coordinates
/// (i.e. with the leading `+`, `-` or space already stripped).
pub struct PatchHighlights {
    lines: Vec<Vec<(Range<usize>, HighlightGroup)>>,
}

impl PatchHighlights {
    /// `bodies[i]` is the text of patch line `i` without its marker column.
    pub fn compute(patch: &ReviewPatch, bodies: &[&str]) -> Self {
        let mut lines: Vec<Vec<(Range<usize>, HighlightGroup)>> =
            vec![Vec::new(); patch.lines.len()];

        // Group the code lines of the patch by the file they belong to.
        let mut by_file: Vec<Vec<usize>> = vec![Vec::new(); patch.files.len()];
        for (index, info) in patch.lines.iter().enumerate() {
            let is_code = matches!(
                info.kind,
                PatchLineKind::Added | PatchLineKind::Removed | PatchLineKind::Context
            );
            if let (true, Some(file)) = (is_code, info.file) {
                if let Some(rows) = by_file.get_mut(file) {
                    rows.push(index);
                }
            }
        }

        // Grammars are expensive to build (the highlight query is compiled),
        // so keep one per language for the whole review. `None` marks a
        // language whose grammar failed to load, so we try it only once.
        let mut grammars: Vec<(Language, Option<SyntaxHighlighter>)> = Vec::new();
        let mut budget = MAX_PARSE_BYTES;

        for (index, rows) in by_file.iter().enumerate() {
            let Some(file) = patch.files.get(index) else {
                continue;
            };
            if file.binary || rows.is_empty() {
                continue;
            }
            for side in [Side::New, Side::Old] {
                let path = match side {
                    Side::New => file.path.as_str(),
                    // A rename can change the extension; the old side is still
                    // written in the old file's language.
                    Side::Old => file.old_path.as_deref().unwrap_or(file.path.as_str()),
                };
                let Some(language) = LanguageRegistry::detect_from_path(path) else {
                    continue;
                };
                let selected: Vec<usize> = rows
                    .iter()
                    .copied()
                    .filter(|row| {
                        side.covers(patch.lines[*row].kind)
                            // Context lines are identical on both sides, so
                            // whatever the new side produced already stands.
                            && lines[*row].is_empty()
                    })
                    .collect();
                if selected.is_empty() {
                    continue;
                }

                let mut source = String::new();
                for (position, row) in selected.iter().enumerate() {
                    if position > 0 {
                        source.push('\n');
                    }
                    source.push_str(bodies.get(*row).copied().unwrap_or(""));
                }
                if source.len() > budget {
                    return Self { lines };
                }
                budget -= source.len();

                let slot = match grammars.iter().position(|(known, _)| *known == language) {
                    Some(slot) => slot,
                    None => {
                        grammars.push((language, SyntaxHighlighter::new(language).ok()));
                        grammars.len() - 1
                    }
                };
                let Some(highlighter) = grammars[slot].1.as_mut() else {
                    continue;
                };
                highlighter.parse(&source);
                let computed = highlighter.highlights_for_all_lines(&source);
                for (position, row) in selected.iter().enumerate() {
                    // `str::lines` drops a trailing empty line; an empty body
                    // has nothing to highlight either way.
                    if let Some(spans) = computed.get(position) {
                        lines[*row] = spans.clone();
                    }
                }
            }
        }

        Self { lines }
    }

    pub fn line(&self, patch_line: usize) -> &[(Range<usize>, HighlightGroup)] {
        self.lines.get(patch_line).map(Vec::as_slice).unwrap_or(&[])
    }
}

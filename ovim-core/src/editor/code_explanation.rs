use super::ai_chat_input::wrap_chat_input_rows;
use std::path::PathBuf;
use std::sync::Arc;

pub const MAX_WALKTHROUGH_STEPS: usize = 32;
pub const MAX_WALKTHROUGH_CACHE_BYTES: usize = 10 * 1024 * 1024;
pub const MAX_WALKTHROUGH_CACHE_ENTRIES: usize = 64;
pub const MIN_UNCACHEABLE_WALKTHROUGH_FILE_BYTES: usize = 5 * 1024 * 1024;
pub const MAX_WALKTHROUGH_COMMENT_BYTES: usize = 4 * 1024;
pub const MAX_WALKTHROUGH_COMMENT_ROWS: usize = 5;
pub const MAX_WALKTHROUGH_CONCEPT_BODY_BYTES: usize = 8 * 1024;
pub const MAX_WALKTHROUGH_CONCEPT_BODY_ROWS: usize = 12;
pub const MAX_WALKTHROUGH_CONCEPT_TITLE_CHARS: usize = 80;

const FALLBACK_SAFE_CODE_ROWS: usize = 40;
const WALKTHROUGH_CARD_RESERVED_ROWS: usize = 10;
const FALLBACK_COMMENT_WIDTH: usize = 76;
const MIN_VIEWPORT_WIDTH: u16 = 32;
const MIN_VIEWPORT_HEIGHT: u16 = 7;
const MAX_CARD_WIDTH: u16 = 100;
const MIN_CARD_HEIGHT: u16 = 7;
const MAX_CARD_HEIGHT: u16 = 10;
const CARD_NON_COMMENT_ROWS: u16 = 4;
const MIN_CONCEPT_CARD_HEIGHT: u16 = 12;
const MAX_CONCEPT_CARD_HEIGHT: u16 = 20;
const CONCEPT_CARD_NON_BODY_ROWS: u16 = 6;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CodeExplanationStep {
    Concept {
        title: String,
        body: String,
    },
    Code {
        path: String,
        absolute_path: PathBuf,
        /// Immutable invocation-time contents. Files at least 5 MiB are left
        /// uncached and read only when their page is displayed.
        snapshot: Option<Arc<str>>,
        start_line: usize,
        end_line: usize,
        comment: String,
    },
}

/// Stable presentation data for both the terminal UI and headless snapshots.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CodeExplanationView {
    pub current: usize,
    pub total: usize,
    pub answer_scroll: usize,
    pub page: CodeExplanationPageView,
    pub discussion: CodeExplanationDiscussionView,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CodeExplanationPageView {
    Concept {
        title: String,
        body: String,
    },
    Code {
        path: String,
        start_line: usize,
        end_line: usize,
        comment: String,
    },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CodeExplanationDiscussionView {
    Navigating {
        question_count: usize,
        latest_question: Option<String>,
        latest_answer: Option<String>,
        latest_failed: bool,
    },
    Composing {
        input: String,
        cursor: usize,
        question_count: usize,
    },
    Answering {
        question: String,
        answer: String,
        question_count: usize,
    },
}

/// Pure walkthrough-card geometry shared by validation and rendering.
///
/// Keeping this outside the renderer prevents the agent-facing range budget
/// from drifting away from the card that ultimately covers the code pane.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CodeExplanationCardLayout {
    pub width: u16,
    pub height: u16,
    pub comment_width: usize,
    pub comment_rows: usize,
}

/// Large, centered concept-page geometry. The row limit remains deliberately
/// lower than the available panel height so whitespace aids comprehension
/// instead of becoming permission for an essay.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ConceptExplanationCardLayout {
    pub width: u16,
    pub height: u16,
    pub body_width: usize,
    pub body_rows: usize,
    pub body_row_limit: usize,
}

impl CodeExplanationCardLayout {
    pub fn resolve(
        viewport_width: u16,
        viewport_height: u16,
        comment: &str,
        tab_width: usize,
    ) -> Option<Self> {
        if viewport_width < MIN_VIEWPORT_WIDTH || viewport_height < MIN_VIEWPORT_HEIGHT {
            return None;
        }
        let width = viewport_width.saturating_sub(2).min(MAX_CARD_WIDTH);
        let comment_width = width.saturating_sub(2).max(1) as usize;
        let comment_rows = comment_rows(comment, comment_width, tab_width);
        let height = (comment_rows as u16)
            .saturating_add(CARD_NON_COMMENT_ROWS)
            .clamp(MIN_CARD_HEIGHT, MAX_CARD_HEIGHT)
            .min(viewport_height);
        Some(Self {
            width,
            height,
            comment_width,
            comment_rows,
        })
    }
}

impl ConceptExplanationCardLayout {
    pub fn resolve(
        viewport_width: u16,
        viewport_height: u16,
        body: &str,
        tab_width: usize,
    ) -> Option<Self> {
        if viewport_width < MIN_VIEWPORT_WIDTH || viewport_height < MIN_CONCEPT_CARD_HEIGHT {
            return None;
        }
        let width = viewport_width.saturating_sub(4).min(MAX_CARD_WIDTH);
        let body_width = width.saturating_sub(2).max(1) as usize;
        let body_rows = comment_rows(body, body_width, tab_width);
        let body_row_limit = concept_body_row_limit(Some(viewport_height as usize));
        let height = (body_rows as u16)
            .saturating_add(CONCEPT_CARD_NON_BODY_ROWS)
            .clamp(MIN_CONCEPT_CARD_HEIGHT, MAX_CONCEPT_CARD_HEIGHT)
            .min(viewport_height.saturating_sub(2));
        Some(Self {
            width,
            height,
            body_width,
            body_rows,
            body_row_limit,
        })
    }
}

pub fn safe_code_rows(viewport_height: Option<usize>) -> usize {
    match viewport_height {
        Some(height) if height > 0 => height
            .saturating_sub(WALKTHROUGH_CARD_RESERVED_ROWS)
            .clamp(1, FALLBACK_SAFE_CODE_ROWS),
        _ => FALLBACK_SAFE_CODE_ROWS,
    }
}

pub fn comment_rows_for_viewport(
    viewport_width: Option<u16>,
    comment: &str,
    tab_width: usize,
) -> usize {
    let width = viewport_width
        .map(|width| width.saturating_sub(4).min(98) as usize)
        .filter(|width| *width > 0)
        .unwrap_or(FALLBACK_COMMENT_WIDTH);
    comment_rows(comment, width, tab_width)
}

pub fn concept_body_row_limit(viewport_height: Option<usize>) -> usize {
    viewport_height
        .filter(|height| *height > 0)
        .map(|height| {
            height
                .saturating_sub(8)
                .clamp(1, MAX_WALKTHROUGH_CONCEPT_BODY_ROWS)
        })
        .unwrap_or(MAX_WALKTHROUGH_CONCEPT_BODY_ROWS)
}

pub fn concept_body_rows_for_viewport(
    viewport_width: Option<u16>,
    body: &str,
    tab_width: usize,
) -> usize {
    let width = viewport_width
        .map(|width| width.saturating_sub(6).min(98) as usize)
        .filter(|width| *width > 0)
        .unwrap_or(FALLBACK_COMMENT_WIDTH);
    comment_rows(body, width, tab_width)
}

fn comment_rows(comment: &str, width: usize, tab_width: usize) -> usize {
    wrap_chat_input_rows(comment, width, tab_width).len()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn range_budget_reserves_the_largest_card() {
        assert_eq!(safe_code_rows(None), 40);
        assert_eq!(safe_code_rows(Some(24)), 14);
        assert_eq!(safe_code_rows(Some(6)), 1);
    }

    #[test]
    fn card_and_validation_share_comment_wrapping() {
        let comment = "one two three four five six seven";
        let layout = CodeExplanationCardLayout::resolve(32, 24, comment, 4).unwrap();
        assert_eq!(
            layout.comment_rows,
            comment_rows_for_viewport(Some(32), comment, 4)
        );
        assert_eq!(layout.width, 30);
        assert!((7..=10).contains(&layout.height));
    }

    #[test]
    fn card_is_absent_when_the_interaction_cannot_be_rendered() {
        assert!(CodeExplanationCardLayout::resolve(31, 24, "comment", 4).is_none());
        assert!(CodeExplanationCardLayout::resolve(80, 6, "comment", 4).is_none());
    }

    #[test]
    fn concept_layout_is_larger_but_keeps_a_small_body_budget() {
        let layout = ConceptExplanationCardLayout::resolve(
            100,
            24,
            "A concise mental model that introduces one idea.",
            4,
        )
        .unwrap();

        assert_eq!(layout.width, 96);
        assert!(layout.height >= 12);
        assert_eq!(layout.body_row_limit, 12);
        assert!(layout.body_rows < layout.body_row_limit);
        assert_eq!(concept_body_row_limit(Some(16)), 8);
    }
}

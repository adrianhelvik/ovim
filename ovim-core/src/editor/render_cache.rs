#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ChatTextPoint {
    pub row: usize,
    pub column: usize,
}

/// Hit-test geometry produced atomically by the latest chat render pass.
/// Keeping these targets together prevents input from mixing stale controls
/// from one frame with current controls from another.
#[derive(Default)]
pub struct ChatInteractionGeometry {
    pub yolo_toggle: Option<crate::Rect>,
    pub comprehension_toggle: Option<crate::Rect>,
    pub history: Option<crate::Rect>,
    pub slash_completions: Vec<(crate::Rect, usize)>,
    pub branches: Vec<(crate::Rect, crate::ai::chat_types::NodeId)>,
    pub walkthrough_replays: Vec<(crate::Rect, String)>,
}

impl ChatInteractionGeometry {
    pub fn begin_frame(&mut self) {
        *self = Self::default();
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ChatTextSelection {
    pub anchor: ChatTextPoint,
    pub head: ChatTextPoint,
    pub moved: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ChatTextAutoscrollDirection {
    Older,
    Newer,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ChatTextAutoscroll {
    pub direction: ChatTextAutoscrollDirection,
    pub column: usize,
    pub last_tick: u128,
}

/// Cached rendering state used by the UI layer and mouse input.
///
/// Groups fields that bridge between the renderer and input handling:
/// mouse drag state and cached layout geometry from the last render pass.
#[derive(Default)]
pub struct RenderCache {
    /// Mouse interaction state (dragging, drag origin)
    pub mouse_state: super::MouseState,
    /// Cached buffer area from last render (for screen-to-buffer coordinate conversion)
    pub last_buffer_area: Option<crate::Rect>,
    /// Largest valid scroll offset for the visible walkthrough answer.
    pub code_explanation_answer_max_scroll: usize,
    /// Cached gutter width from last render
    pub last_gutter_width: usize,
    /// Cached text width from last render (buffer area width minus gutter, used for wrap calculations)
    pub last_text_width: usize,
    /// Cached blame column width from last render (0 when blame is off)
    pub last_blame_width: usize,
    /// Cached AI prompt input hit-test area from last render.
    pub ai_prompt_input_area: Option<crate::Rect>,
    /// Cached wrapped AI prompt input rows from last render:
    /// `(row_rect, start_byte, end_byte)`.
    pub ai_prompt_input_rows: Vec<(crate::Rect, usize, usize)>,
    /// Cached AI prompt model picker hitboxes from last render.
    pub ai_prompt_model_hitboxes: Vec<(crate::Rect, String)>,
    /// Cached AI prompt model picker trigger hitbox from last render.
    pub ai_prompt_model_trigger_hitbox: Option<crate::Rect>,
    /// Cached AI chat panel area from last render (for mouse scroll hit-testing).
    pub last_chat_area: Option<crate::Rect>,
    /// Shared buffer/chat area used to convert a separator drag into a ratio.
    pub ai_chat_split_area: Option<crate::Rect>,
    /// One-column drag target on the left edge of the docked chat.
    pub ai_chat_separator_area: Option<crate::Rect>,
    /// Whether the chat separator currently owns the mouse drag gesture.
    pub ai_chat_separator_dragging: bool,
    /// Coherent hit-test snapshot from the latest chat render pass.
    pub ai_chat_interactions: ChatInteractionGeometry,
    /// Cached AI chat total rendered row count from last render pass.
    pub ai_chat_last_total_rows: usize,
    /// Wall-clock time spent rendering the chat panel on the last frame.
    pub ai_chat_last_render_micros: u128,
    /// Completed-message bubble cache hits on the last chat render.
    pub ai_chat_last_cache_hits: usize,
    /// Completed-message bubble cache misses on the last chat render.
    pub ai_chat_last_cache_misses: usize,
    /// Cached visible chat row window start (inclusive) from last render.
    pub ai_chat_last_visible_start_row: usize,
    /// Cached visible chat row window end (exclusive) from last render.
    pub ai_chat_last_visible_end_row: usize,
    /// Cached row spans per message from last render (oldest..latest).
    pub ai_chat_last_message_row_spans: Vec<(usize, usize)>,
    /// Absolute rendered row spans for live shell calls, oldest first.
    pub ai_chat_last_shell_row_spans: Vec<(usize, usize)>,
    /// Absolute rendered row spans for scheduled inputs, oldest first.
    pub ai_chat_last_queued_row_spans: Vec<(usize, usize)>,
    /// Plain text for every rendered history row, including off-screen rows.
    pub ai_chat_rendered_text_rows: Vec<String>,
    /// Mouse-selected range in absolute rendered-history coordinates.
    pub ai_chat_text_selection: Option<ChatTextSelection>,
    /// Whether a chat text-selection drag is currently active.
    pub ai_chat_text_selecting: bool,
    /// Continuous edge scrolling while a chat text selection is dragged
    /// above or below the visible history viewport.
    pub ai_chat_text_autoscroll: Option<ChatTextAutoscroll>,
    /// Cached AI chat input area from last render.
    pub ai_chat_input_area: Option<crate::Rect>,
    /// Visible composer rows and their source byte ranges.
    pub ai_chat_input_rows: Vec<(crate::Rect, usize, usize, usize)>,
    /// Display width available to composer text on the last render.
    pub ai_chat_input_content_width: usize,
    /// Cached AI chat input cursor position from last render.
    pub ai_chat_input_cursor_pos: Option<(u16, u16)>,
    /// Click target for the Exa API-key dashboard in the setup dialog.
    pub ai_chat_exa_dashboard_hitbox: Option<crate::Rect>,
    /// Hardware cursor position for the Exa key field.
    pub ai_chat_exa_input_cursor_pos: Option<(u16, u16)>,
    /// Whether the frontend can render images, including cell-based fallbacks.
    pub terminal_image_support: bool,
    /// iTerm's inline-image protocol is sensitive to hardware-cursor movement.
    /// Frontends set this to request a cell-rendered cursor while images are visible.
    pub terminal_images_require_software_cursor: bool,
    /// Focus regain invalidated terminal-owned image placements. The TUI
    /// consumes this before its next draw and forces a full surface refresh
    /// when the previous frame actually contained an image.
    pub terminal_image_refresh_requested: bool,
    /// Render rectangles for clickable chat-image thumbnails.
    pub ai_chat_image_thumbnails: Vec<(crate::Rect, std::path::PathBuf)>,
    /// Absolute 80ms animation bucket used by the AI chat working spinner.
    pub ai_chat_working_animation_tick: u128,
}

#[cfg(test)]
mod tests {
    use super::ChatInteractionGeometry;

    fn rect() -> crate::Rect {
        crate::Rect {
            x: 1,
            y: 2,
            width: 3,
            height: 4,
        }
    }

    #[test]
    fn chat_interactions_begin_frame_clears_every_hit_target() {
        let mut interactions = ChatInteractionGeometry {
            yolo_toggle: Some(rect()),
            comprehension_toggle: Some(rect()),
            history: Some(rect()),
            slash_completions: vec![(rect(), 1)],
            branches: vec![(rect(), 2)],
            walkthrough_replays: vec![(rect(), "call-1".into())],
        };

        interactions.begin_frame();

        assert!(interactions.yolo_toggle.is_none());
        assert!(interactions.history.is_none());
        assert!(interactions.slash_completions.is_empty());
        assert!(interactions.branches.is_empty());
        assert!(interactions.walkthrough_replays.is_empty());
    }
}

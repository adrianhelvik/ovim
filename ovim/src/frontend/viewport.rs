use crate::editor::Editor;

/// Recomputes viewport geometry from raw grid cells: `width`/`height` are the
/// full window in character cells (not the content area — this function
/// subtracts chrome itself: tab bar, file tree, LSP progress line, and the
/// status+command lines). Keeps viewport height, window manager dimensions,
/// the wrap map, and the scroll offset in sync with the new size.
///
/// Call this whenever the grid geometry changes (terminal resize, window
/// resize, split/pane changes) — see the frontend contract in
/// [`crate::frontend`].
pub fn handle_viewport_resize(editor: &mut Editor, width: u16, height: u16) {
    // Recompute the buffer chunk dimensions using the same high-level rules as the renderer.
    // This is intentionally approximate (position doesn't matter here) — it just needs to
    // keep viewport height and wrap width correct for scroll calculations.
    let mut content_width = width;
    let mut content_height = height;

    // Tab bar consumes one row when multiple tabs are present.
    if editor.tab_count() > 1 {
        content_height = content_height.saturating_sub(1);
    }

    // File tree consumes fixed width when visible.
    if editor.file_tree().is_visible() {
        content_width = content_width.saturating_sub(50);
    }

    // Progress line consumes one row when present.
    if editor.lsp_progress_message().is_some() {
        content_height = content_height.saturating_sub(1);
    }

    // Status line + command/message line.
    content_height = content_height.saturating_sub(2);

    // Apply textwidth centering: narrowing changes wrap width, but not viewport height.
    if let Some(textwidth) = editor.options.textwidth {
        let max_width = textwidth as u16;
        if content_width > max_width {
            content_width = max_width;
        }
    }

    // Update cached viewport dimensions for scroll calculations.
    editor.set_viewport_height(content_height as usize);

    // Update window sizes so horizontal scrolling calculations use the latest width.
    if let Some(wm) = editor.window_manager_mut() {
        wm.update_dimensions(content_width, content_height);
    } else {
        editor.init_window_manager(content_width, content_height);
    }

    // Keep the wrap map in sync with the new width so vertical scrolling stays accurate
    // in wrap mode.
    if editor.options.wrap {
        let text_width = compute_text_width(editor, content_width);
        editor.ensure_wrap_map(text_width);
    }

    // Re-run scroll update so the cursor remains visible in the resized viewport.
    editor.update_scroll_offset();
}

/// Computes the wrap width (content width minus gutter) for a given content
/// area width. Kept in sync by hand with
/// `ovim::ui::renderer::layout::BufferLayout::compute`, which is the
/// authority for gutter/layout sizing; this function must not drift from it.
pub fn compute_text_width(editor: &Editor, content_width: u16) -> usize {
    const SIGN_WIDTH: usize = 2;
    const GUTTER_SPACING: usize = 1;

    let show_numbers = editor.options.number || editor.options.relative_number;
    let line_count = editor.buffer().line_count();
    let line_num_width = if show_numbers {
        line_count.to_string().len().max(3)
    } else {
        0
    };

    let blame_width = if editor.options.blame {
        if let Some(blame) = editor.buffer().git_blame() {
            let author_len = blame.max_author_len().min(15);
            1 + 1 + 5 + 1 + author_len.max(3) + 1
        } else {
            0
        }
    } else {
        0
    };

    let gutter_width = blame_width + SIGN_WIDTH + line_num_width + GUTTER_SPACING;

    // Apply textwidth narrowing (OV-00019: must match renderer's BufferLayout
    // which narrows buffer_area to textwidth before computing text_width).
    let effective_width = if let Some(textwidth) = editor.options.textwidth {
        let max = textwidth as u16;
        if content_width > max {
            max
        } else {
            content_width
        }
    } else {
        content_width
    };

    (effective_width as usize).saturating_sub(gutter_width)
}

#[cfg(test)]
mod tests {
    use super::{compute_text_width, handle_viewport_resize};
    use crate::editor::Editor;

    #[test]
    fn resize_updates_viewport_and_wrap_map_and_keeps_cursor_visible() {
        // 200 logical lines, wide enough to exercise gutter sizing.
        let content: String = (1..=200)
            .map(|i| format!("line {i}: {}\n", "x".repeat(120)))
            .collect();

        let mut editor = Editor::with_content(&content);
        editor.options.number = true;
        editor.options.wrap = true;
        editor.options.scrolloff = 0;

        // Initial size.
        handle_viewport_resize(&mut editor, 80, 20);
        assert_eq!(editor.viewport_height(), 18);

        // Move cursor to EOF and ensure scroll offset is set.
        let last_line = editor.buffer().line_count().saturating_sub(1);
        editor
            .buffer_mut()
            .cursor_mut()
            .set_position(last_line, crate::unicode::GraphemeCol::ZERO);
        editor.update_scroll_offset();

        // Shrink the pane; cursor should remain visible in the new viewport.
        handle_viewport_resize(&mut editor, 80, 10);
        assert_eq!(editor.viewport_height(), 8);

        let cursor_line = editor.buffer().cursor().line();
        let scroll_offset = editor.scroll_offset();
        let visible = editor.viewport_height().max(1);
        assert!(
            cursor_line >= scroll_offset && cursor_line < scroll_offset + visible,
            "cursor should remain visible after resize: cursor_line={cursor_line} scroll_offset={scroll_offset} viewport={visible}"
        );

        // Wrap map should match the new text width (buffer width minus gutter).
        let wrap_width = editor.wrap_map().map(|m| m.wrap_width()).unwrap_or(0);
        assert_eq!(wrap_width, compute_text_width(&editor, 80).max(1));
    }
}

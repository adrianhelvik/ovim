use super::Editor;

fn anchored_scroll_offset(
    row_scroll_from_bottom: usize,
    pinned_base_total_rows: Option<usize>,
    current_total_rows: usize,
) -> usize {
    let Some(base) = pinned_base_total_rows else {
        return row_scroll_from_bottom;
    };
    if current_total_rows >= base {
        row_scroll_from_bottom.saturating_add(current_total_rows - base)
    } else {
        row_scroll_from_bottom.saturating_sub(base - current_total_rows)
    }
}

impl Editor {
    /// Select one message on the active conversation branch for GUI/history navigation.
    pub fn ai_chat_history_select_index(&mut self, index: usize) -> bool {
        let node_id = self
            .conversation()
            .and_then(|conversation| conversation.node_ids_for_active_branch().get(index))
            .copied();
        let Some(node_id) = node_id else {
            return false;
        };
        let Some(chat) = self.ai_state.chat.as_mut() else {
            return false;
        };
        chat.history.selected_node_id = Some(node_id);
        chat.history.selected_queued_id = None;
        chat.history.selected_shell_tool_id = None;
        chat.focus = crate::ai::chat_types::ChatFocus::MessageHistory;
        self.ai_chat_history_ensure_cursor_visible();
        true
    }

    /// Selected message index in current conversation.
    pub fn ai_chat_history_selected_index(&self) -> Option<usize> {
        if self.ai_chat_history_selected_queued_id().is_some()
            || self.ai_chat_history_selected_shell_tool_id().is_some()
        {
            return None;
        }
        let conv = self.conversation()?;
        let node_ids = conv.node_ids_for_active_branch();
        if node_ids.is_empty() {
            return None;
        }
        let selected = self
            .ai_state
            .chat
            .as_ref()
            .and_then(|c| c.history.selected_node_id);
        if let Some(sel) = selected {
            if let Some(idx) = node_ids.iter().position(|id| *id == sel) {
                return Some(idx);
            }
        }
        Some(node_ids.len() - 1)
    }

    pub fn ai_chat_history_selected_queued_id(&self) -> Option<u64> {
        let chat = self.ai_state.chat.as_ref()?;
        let id = chat.history.selected_queued_id?;
        chat.queued_inputs
            .iter()
            .any(|item| item.id == id)
            .then_some(id)
    }

    pub fn ai_chat_history_selected_queued_index(&self) -> Option<usize> {
        let id = self.ai_chat_history_selected_queued_id()?;
        self.ai_state
            .chat
            .as_ref()?
            .queued_inputs
            .iter()
            .position(|item| item.id == id)
    }

    pub fn ai_chat_live_shell_tool_ids(&self) -> Vec<String> {
        self.ai_state
            .chat
            .as_ref()
            .map(|chat| {
                chat.streaming_tool_calls
                    .iter()
                    .filter(|call| {
                        call.name == "bash" && chat.shell_transcripts.contains_key(&call.id)
                    })
                    .map(|call| call.id.clone())
                    .collect()
            })
            .unwrap_or_default()
    }

    pub fn ai_chat_history_selected_shell_tool_id(&self) -> Option<&str> {
        let chat = self.ai_state.chat.as_ref()?;
        let selected = chat.history.selected_shell_tool_id.as_deref()?;
        chat.streaming_tool_calls
            .iter()
            .any(|call| call.name == "bash" && call.id == selected)
            .then_some(selected)
    }

    fn ai_chat_history_selected_shell_index(&self) -> Option<usize> {
        let selected = self.ai_chat_history_selected_shell_tool_id()?;
        self.ai_chat_live_shell_tool_ids()
            .iter()
            .position(|id| id == selected)
    }

    /// Whether history selection currently points at latest message.
    pub fn ai_chat_history_is_latest_selected(&self) -> bool {
        let queued_len = self
            .ai_state
            .chat
            .as_ref()
            .map_or(0, |chat| chat.queued_inputs.len());
        if queued_len > 0 {
            return self.ai_chat_history_selected_queued_index() == Some(queued_len - 1);
        }
        let live_shells = self.ai_chat_live_shell_tool_ids();
        if !live_shells.is_empty() {
            return self.ai_chat_history_selected_shell_index() == Some(live_shells.len() - 1);
        }
        let Some(idx) = self.ai_chat_history_selected_index() else {
            return true;
        };
        let len = self.ai_chat_messages().len();
        idx + 1 >= len
    }

    /// Effective row scroll offset for rendering given row count and viewport size.
    pub fn ai_chat_effective_message_scroll(
        &self,
        total_rows: usize,
        visible_rows: usize,
    ) -> usize {
        let Some(chat) = self.ai_state.chat.as_ref() else {
            return 0;
        };
        let viewport = &chat.viewport;
        if viewport.follow_latest || viewport.row_scroll_from_bottom == 0 {
            return 0;
        }
        let max_scroll = total_rows.saturating_sub(visible_rows);
        anchored_scroll_offset(
            viewport.row_scroll_from_bottom,
            viewport.pinned_base_total_rows,
            total_rows,
        )
        .min(max_scroll)
    }

    /// Scroll chat history viewport toward older rows.
    pub fn ai_chat_scroll_viewport_up(&mut self, rows: usize) {
        if rows == 0 {
            return;
        }
        let current_total_rows = self.render_cache.ai_chat_last_total_rows;
        let current_total = if current_total_rows == 0 {
            None
        } else {
            Some(current_total_rows)
        };
        // Clamp against the same cached geometry the render path uses, so
        // over-scrolling past the top never accumulates invisible debt that
        // later scroll-downs would have to burn off.
        let visible_rows = self
            .render_cache
            .ai_chat_last_visible_end_row
            .saturating_sub(self.render_cache.ai_chat_last_visible_start_row);
        if let Some(chat) = self.ai_state.chat.as_mut() {
            let current_offset = if chat.viewport.follow_latest {
                0
            } else if let Some(total_rows) = current_total {
                anchored_scroll_offset(
                    chat.viewport.row_scroll_from_bottom,
                    chat.viewport.pinned_base_total_rows,
                    total_rows,
                )
            } else {
                chat.viewport.row_scroll_from_bottom
            };
            chat.viewport.follow_latest = false;
            let mut target = current_offset.saturating_add(rows);
            if let Some(total_rows) = current_total {
                if visible_rows > 0 {
                    target = target.min(total_rows.saturating_sub(visible_rows));
                }
            }
            chat.viewport.row_scroll_from_bottom = target;
            if chat.viewport.row_scroll_from_bottom == 0 {
                chat.viewport.follow_latest = true;
                chat.viewport.pinned_base_total_rows = None;
                return;
            }
            if current_total.is_some() {
                chat.viewport.pinned_base_total_rows = current_total;
            }
        }
    }

    /// Scroll chat history viewport toward latest rows.
    ///
    /// Returns true when the viewport reached bottom/latest.
    pub fn ai_chat_scroll_viewport_down(&mut self, rows: usize) -> bool {
        if rows == 0 {
            return self
                .ai_state
                .chat
                .as_ref()
                .is_some_and(|c| c.viewport.row_scroll_from_bottom == 0);
        }
        let current_total_rows = self.render_cache.ai_chat_last_total_rows;
        let current_total = (current_total_rows > 0).then_some(current_total_rows);
        if let Some(chat) = self.ai_state.chat.as_mut() {
            let current_offset = if chat.viewport.follow_latest {
                0
            } else if let Some(total_rows) = current_total {
                anchored_scroll_offset(
                    chat.viewport.row_scroll_from_bottom,
                    chat.viewport.pinned_base_total_rows,
                    total_rows,
                )
            } else {
                chat.viewport.row_scroll_from_bottom
            };
            chat.viewport.row_scroll_from_bottom = current_offset.saturating_sub(rows);
            if chat.viewport.row_scroll_from_bottom == 0 {
                chat.viewport.follow_latest = true;
                chat.viewport.pinned_base_total_rows = None;
                return true;
            }
            if current_total.is_some() {
                chat.viewport.pinned_base_total_rows = current_total;
            }
        }
        false
    }

    /// Move message-history selection toward older messages.
    pub fn ai_chat_history_cursor_move_older(&mut self, messages: usize) {
        if messages == 0 {
            return;
        }
        let node_ids = self
            .conversation()
            .map(|conv| conv.node_ids_for_active_branch().to_vec())
            .unwrap_or_default();
        let queued_ids = self
            .ai_state
            .chat
            .as_ref()
            .map(|chat| {
                chat.queued_inputs
                    .iter()
                    .map(|item| item.id)
                    .collect::<Vec<_>>()
            })
            .unwrap_or_default();
        let live_shell_ids = self.ai_chat_live_shell_tool_ids();
        let total = node_ids.len() + live_shell_ids.len() + queued_ids.len();
        if total == 0 {
            return;
        }
        let current = self
            .ai_chat_history_selected_queued_index()
            .map(|index| node_ids.len() + live_shell_ids.len() + index)
            .or_else(|| {
                self.ai_chat_history_selected_shell_index()
                    .map(|index| node_ids.len() + index)
            })
            .or_else(|| self.ai_chat_history_selected_index())
            .unwrap_or(total - 1);
        let target = current.saturating_sub(messages);
        if let Some(chat) = self.ai_state.chat.as_mut() {
            if target < node_ids.len() {
                chat.history.selected_node_id = node_ids.get(target).copied();
                chat.history.selected_queued_id = None;
                chat.history.selected_shell_tool_id = None;
            } else if target < node_ids.len() + live_shell_ids.len() {
                chat.history.selected_node_id = None;
                chat.history.selected_queued_id = None;
                chat.history.selected_shell_tool_id =
                    live_shell_ids.get(target - node_ids.len()).cloned();
            } else {
                chat.history.selected_node_id = None;
                chat.history.selected_shell_tool_id = None;
                chat.history.selected_queued_id = queued_ids
                    .get(target - node_ids.len() - live_shell_ids.len())
                    .copied();
            }
        }
        self.ai_chat_history_ensure_cursor_visible();
    }

    /// Move message-history selection toward latest messages.
    ///
    /// Returns true when selection reached the latest message.
    pub fn ai_chat_history_cursor_move_newer(&mut self, messages: usize) -> bool {
        if messages == 0 {
            return self.ai_chat_history_is_latest_selected();
        }
        let node_ids = self
            .conversation()
            .map(|conv| conv.node_ids_for_active_branch().to_vec())
            .unwrap_or_default();
        let queued_ids = self
            .ai_state
            .chat
            .as_ref()
            .map(|chat| {
                chat.queued_inputs
                    .iter()
                    .map(|item| item.id)
                    .collect::<Vec<_>>()
            })
            .unwrap_or_default();
        let live_shell_ids = self.ai_chat_live_shell_tool_ids();
        let total = node_ids.len() + live_shell_ids.len() + queued_ids.len();
        if total == 0 {
            return true;
        }
        let current = self
            .ai_chat_history_selected_queued_index()
            .map(|index| node_ids.len() + live_shell_ids.len() + index)
            .or_else(|| {
                self.ai_chat_history_selected_shell_index()
                    .map(|index| node_ids.len() + index)
            })
            .or_else(|| self.ai_chat_history_selected_index())
            .unwrap_or(total - 1);
        let target = current.saturating_add(messages).min(total - 1);
        if let Some(chat) = self.ai_state.chat.as_mut() {
            if target < node_ids.len() {
                chat.history.selected_node_id = node_ids.get(target).copied();
                chat.history.selected_queued_id = None;
                chat.history.selected_shell_tool_id = None;
            } else if target < node_ids.len() + live_shell_ids.len() {
                chat.history.selected_node_id = None;
                chat.history.selected_queued_id = None;
                chat.history.selected_shell_tool_id =
                    live_shell_ids.get(target - node_ids.len()).cloned();
            } else {
                chat.history.selected_node_id = None;
                chat.history.selected_shell_tool_id = None;
                chat.history.selected_queued_id = queued_ids
                    .get(target - node_ids.len() - live_shell_ids.len())
                    .copied();
            }
        }
        self.ai_chat_history_ensure_cursor_visible();
        self.ai_chat_history_is_latest_selected()
    }

    fn ai_chat_history_ensure_cursor_visible(&mut self) {
        let span = self
            .ai_chat_history_selected_queued_index()
            .and_then(|index| self.render_cache.ai_chat_last_queued_row_spans.get(index))
            .or_else(|| {
                self.ai_chat_history_selected_shell_index()
                    .and_then(|index| self.render_cache.ai_chat_last_shell_row_spans.get(index))
            })
            .or_else(|| {
                self.ai_chat_history_selected_index()
                    .and_then(|index| self.render_cache.ai_chat_last_message_row_spans.get(index))
            })
            .copied();
        let Some((msg_start, msg_end)) = span else {
            return;
        };
        let vis_start = self.render_cache.ai_chat_last_visible_start_row;
        let vis_end = self.render_cache.ai_chat_last_visible_end_row;
        if vis_end <= vis_start {
            return;
        }

        if msg_end <= vis_start {
            let delta = vis_start.saturating_sub(msg_start).max(1);
            self.ai_chat_scroll_viewport_up(delta);
        } else if msg_start >= vis_end {
            let delta = msg_end.saturating_sub(vis_end).max(1);
            self.ai_chat_scroll_viewport_down(delta);
        }
    }

    /// Ensure history selection references the latest message.
    pub fn ai_chat_reset_history_cursor(&mut self) {
        let latest_queued = self
            .ai_state
            .chat
            .as_ref()
            .and_then(|chat| chat.queued_inputs.back().map(|item| item.id));
        let latest_shell = self.ai_chat_live_shell_tool_ids().last().cloned();
        let latest = self
            .conversation()
            .and_then(|c| c.node_ids_for_active_branch().last().copied());
        if let Some(chat) = self.ai_state.chat.as_mut() {
            chat.history.selected_queued_id = latest_queued;
            chat.history.selected_shell_tool_id = if latest_queued.is_none() {
                latest_shell.clone()
            } else {
                None
            };
            chat.history.selected_node_id = if latest_queued.is_none() && latest_shell.is_none() {
                latest
            } else {
                None
            };
        }
    }
}

#[cfg(test)]
mod tests {
    use crate::ai::chat_types::ChatOpts;
    use crate::editor::Editor;

    fn editor_with_chat_geometry(total_rows: usize, visible_rows: usize) -> Editor {
        let mut editor = Editor::default();
        editor
            .open_ai_chat(ChatOpts {
                name: "chat".to_string(),
                allow_edits: true,
                ..Default::default()
            })
            .expect("open chat");
        editor.render_cache.ai_chat_last_total_rows = total_rows;
        editor.render_cache.ai_chat_last_visible_start_row = total_rows - visible_rows;
        editor.render_cache.ai_chat_last_visible_end_row = total_rows;
        editor
    }

    #[test]
    fn viewport_scroll_up_clamps_to_top_of_history() {
        let mut editor = editor_with_chat_geometry(100, 20);

        // Scroll far past the top: the stored offset must not exceed the
        // maximum the render path would show (total - visible = 80).
        editor.ai_chat_scroll_viewport_up(500);
        let chat = editor.ai_state.chat.as_ref().unwrap();
        assert_eq!(chat.viewport.row_scroll_from_bottom, 80);
        assert_eq!(editor.ai_chat_effective_message_scroll(100, 20), 80);
    }

    #[test]
    fn viewport_scroll_down_responds_immediately_after_over_scroll() {
        let mut editor = editor_with_chat_geometry(100, 20);

        // Hold wheel-up well past the top, then a single wheel-down must move
        // the viewport instead of burning off phantom over-scroll debt.
        for _ in 0..30 {
            editor.ai_chat_scroll_viewport_up(50);
        }
        assert!(!editor.ai_chat_scroll_viewport_down(3));
        assert_eq!(editor.ai_chat_effective_message_scroll(100, 20), 77);
    }

    #[test]
    fn viewport_scroll_up_is_a_no_op_when_everything_fits() {
        let mut editor = editor_with_chat_geometry(10, 10);
        editor.render_cache.ai_chat_last_visible_start_row = 0;

        editor.ai_chat_scroll_viewport_up(5);
        let chat = editor.ai_state.chat.as_ref().unwrap();
        assert!(chat.viewport.follow_latest);
        assert_eq!(chat.viewport.row_scroll_from_bottom, 0);
    }

    #[test]
    fn viewport_scroll_up_without_cached_geometry_still_scrolls() {
        let mut editor = Editor::default();
        editor
            .open_ai_chat(ChatOpts {
                name: "chat".to_string(),
                allow_edits: true,
                ..Default::default()
            })
            .expect("open chat");

        // No render pass yet: no geometry to clamp against, keep old behavior.
        editor.ai_chat_scroll_viewport_up(7);
        let chat = editor.ai_state.chat.as_ref().unwrap();
        assert!(!chat.viewport.follow_latest);
        assert_eq!(chat.viewport.row_scroll_from_bottom, 7);
    }
}

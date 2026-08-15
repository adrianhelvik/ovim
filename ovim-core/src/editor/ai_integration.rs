use super::ai_state::{AiEditRegion, AiRegionStatus, AiSelectionSnapshot, PendingAiJob};
use super::Editor;
use crate::ai::request_ai_edit;
use crate::ai::AiExtractedResponse;
use crate::edit::Edit;
use crate::editor::lsp_state::HoverContentType;
use crate::mode::Mode;
use crate::unicode::GraphemeCol;
use anyhow::{anyhow, Result};
use std::time::Instant;
use tokio::sync::oneshot;
use tokio::sync::oneshot::error::TryRecvError;

impl Editor {
    /// Returns configured AI profile names sorted for deterministic picker navigation.
    pub fn ai_profile_names_sorted(&self) -> Vec<String> {
        let mut names: Vec<String> = self.ai_state.config.profiles.keys().cloned().collect();
        names.sort();
        names
    }

    /// Select a specific AI profile. Reports and returns false when unknown.
    pub fn ai_set_profile(&mut self, profile_name: &str) -> bool {
        let Some(profile) = self.ai_state.config.resolve_profile(profile_name) else {
            self.set_status_message(format!("Unknown AI profile: {profile_name}"));
            return false;
        };
        self.ai_state.active_profile = profile_name.to_string();
        if let Some(chat) = self.ai_state.chat.as_mut() {
            chat.opts.profile = Some(profile_name.to_string());
        }
        self.ai_state.edit_format = profile.edit_format.clone();
        if let Some(idx) = self
            .ai_profile_names_sorted()
            .iter()
            .position(|name| name == profile_name)
        {
            self.ai_state.prompt.model_picker_index = idx;
        }
        self.set_status_message(format!(
            "AI profile: {} ({}/{})",
            profile_name, profile.provider, profile.model
        ));
        true
    }

    /// Cycle AI profile selection in prompt mode.
    pub fn ai_cycle_profile(&mut self, forward: bool) {
        let names = self.ai_profile_names_sorted();
        if names.is_empty() {
            self.set_status_message("No AI profiles configured".to_string());
            return;
        }

        let current_idx = names
            .iter()
            .position(|name| name == &self.ai_state.active_profile)
            .unwrap_or(0);

        let next_idx = if forward {
            (current_idx + 1) % names.len()
        } else if current_idx == 0 {
            names.len() - 1
        } else {
            current_idx - 1
        };

        let selected = self.ai_set_profile(&names[next_idx]);
        debug_assert!(selected, "profile came from the active configuration");
    }

    /// Opens the AI prompt model picker and highlights the current profile.
    pub fn ai_open_model_picker(&mut self) {
        let names = self.ai_profile_names_sorted();
        if names.is_empty() {
            self.ai_state.prompt.model_picker_open = false;
            self.ai_state.prompt.model_picker_index = 0;
            return;
        }

        let current_idx = names
            .iter()
            .position(|name| name == &self.ai_state.active_profile)
            .unwrap_or(0);
        self.ai_state.prompt.model_picker_open = true;
        self.ai_state.prompt.model_picker_index = current_idx;
    }

    /// Closes the AI prompt model picker.
    pub fn ai_close_model_picker(&mut self) {
        self.ai_state.prompt.model_picker_open = false;
    }

    /// Toggles the AI prompt model picker.
    pub fn ai_toggle_model_picker(&mut self) {
        if self.ai_state.prompt.model_picker_open {
            self.ai_close_model_picker();
        } else {
            self.ai_open_model_picker();
        }
    }

    /// Move highlighted profile in the model picker.
    pub fn ai_move_model_picker_selection(&mut self, forward: bool) {
        let names = self.ai_profile_names_sorted();
        if names.is_empty() {
            return;
        }

        if !self.ai_state.prompt.model_picker_open {
            self.ai_open_model_picker();
        }

        let len = names.len();
        let idx = self
            .ai_state
            .prompt
            .model_picker_index
            .min(len.saturating_sub(1));
        self.ai_state.prompt.model_picker_index = if forward {
            (idx + 1) % len
        } else if idx == 0 {
            len - 1
        } else {
            idx - 1
        };
    }

    /// Applies the highlighted model picker selection.
    pub fn ai_apply_model_picker_selection(&mut self) {
        let names = self.ai_profile_names_sorted();
        if names.is_empty() {
            self.set_status_message("No AI profiles configured".to_string());
            self.ai_close_model_picker();
            return;
        }

        let idx = self
            .ai_state
            .prompt
            .model_picker_index
            .min(names.len().saturating_sub(1));
        let selected = self.ai_set_profile(&names[idx]);
        debug_assert!(selected, "profile came from the active configuration");
        self.ai_close_model_picker();
    }

    /// Start AI prompt from the current visual selection.
    pub fn start_ai_prompt_from_visual(&mut self) -> Result<()> {
        if !self.capture_ai_selection_from_visual()? {
            return Ok(());
        }

        self.ai_state.prompt.input.clear();
        self.ai_state.prompt.cursor = 0;
        self.ai_state.prompt.model_picker_open = false;
        self.ai_state.prompt.model_picker_index = 0;
        self.set_mode(Mode::AiPrompt);
        self.set_status_message("AI prompt: type instruction and press Enter".to_string());
        self.maybe_prompt_codex_auth_on_selection_open();
        Ok(())
    }

    /// Start AI chat from the current visual selection.
    ///
    /// Unlike inline prompt mode, chat mode can perform surrounding edits and
    /// use tools while still keeping the visual range as high-priority context.
    pub fn start_ai_chat_from_visual(&mut self) -> Result<()> {
        if !self.capture_ai_selection_from_visual()? {
            return Ok(());
        }

        self.open_ai_chat(crate::ai::chat_types::ChatOpts {
            name: "chat".into(),
            profile: self.ai_chat_context_profile("chat"),
            allow_edits: true,
            ..Default::default()
        })?;
        let selection = self
            .ai_state
            .active_selection
            .as_ref()
            .expect("selection was captured")
            .clone();
        let buffer = self
            .get_buffer_by_id(selection.buffer_id)
            .expect("selected buffer remains open");
        let attachment = super::ai_chat_state::CodeAttachment {
            buffer_id: selection.buffer_id,
            path: buffer.file_path().map(ToString::to_string),
            start_line: selection.start_line,
            end_line: selection.end_line,
            buffer_revision: buffer.version(),
            text: selection.selected_text,
        };
        let label = attachment.label();
        if let Some(chat) = self.ai_state.chat.as_mut() {
            chat.active_buffer_id = selection.buffer_id;
            chat.pending_code_attachment = Some(attachment);
        }
        self.set_status_message(format!("Attached {label} to AI chat"));
        Ok(())
    }

    fn capture_ai_selection_from_visual(&mut self) -> Result<bool> {
        if self.mode() == Mode::VisualBlock {
            self.set_status_message("AI edit does not support visual block mode".to_string());
            return Ok(false);
        }

        let Some(((start_line, start_col), (end_line, end_col))) = self.visual_selection() else {
            self.set_status_message("No visual selection to edit".to_string());
            return Ok(false);
        };

        let rope = self.buffer().rope();
        let rope_len = rope.len_chars();

        let (start_col, end_col, start_char, end_char) = match self.mode() {
            Mode::VisualLine => {
                let start = rope.line_to_char(start_line).min(rope_len);
                let end = if end_line + 1 < self.buffer().raw_line_count() {
                    rope.line_to_char(end_line + 1)
                } else {
                    rope_len
                };
                let end_col =
                    crate::unicode::grapheme_count(&crate::display::line_content(rope, end_line));
                (0, end_col, start, end.min(rope_len))
            }
            _ => {
                let start_line_text = crate::display::line_content(rope, start_line);
                let end_line_text = crate::display::line_content(rope, end_line);
                let end_col = end_col.saturating_add(1);
                let start = rope.line_to_char(start_line)
                    + crate::unicode::grapheme_to_char_col(
                        &start_line_text,
                        GraphemeCol(start_col),
                    )
                    .0;
                let end = rope.line_to_char(end_line)
                    + crate::unicode::grapheme_to_char_col(&end_line_text, GraphemeCol(end_col)).0;
                (start_col, end_col, start.min(rope_len), end.min(rope_len))
            }
        };

        if end_char <= start_char {
            self.set_status_message("Visual selection is empty".to_string());
            return Ok(false);
        }

        let selected_text = rope.slice(start_char..end_char).to_string();
        let snapshot = AiSelectionSnapshot {
            buffer_id: self.buffer().id(),
            start_line,
            start_col,
            end_line,
            end_col,
            start_char,
            end_char,
            anchor_line: start_line,
            selected_text,
            mode_before_prompt: self.mode(),
        };

        self.ai_state.active_selection = Some(snapshot);
        Ok(true)
    }

    /// Submit current AI prompt as an async job.
    pub fn submit_ai_prompt_job(&mut self) -> Result<()> {
        let prompt = self.ai_state.prompt.input.trim().to_string();
        if prompt.is_empty() {
            self.set_status_message("AI prompt is empty".to_string());
            return Ok(());
        }

        let Some(selection) = self.ai_state.active_selection.clone() else {
            self.set_status_message("No selection queued for AI edit".to_string());
            self.set_mode(Mode::Normal);
            return Ok(());
        };

        if !self.ensure_codex_auth_for_selection_submit() {
            return Ok(());
        }

        let profile_name = self.ai_state.active_profile.clone();
        let Some(profile) = self.ai_state.config.resolve_profile(&profile_name).cloned() else {
            self.set_status_message(format!("Unknown AI profile: {}", profile_name));
            return Ok(());
        };
        let api_key_registry = self.ai_state.config.api_key_registry.clone();
        let prompts = self.ai_state.config.prompts.clone();
        let format_prompts = self.ai_state.config.format_prompts.clone();

        let edit_format = self.ai_state.edit_format.clone();
        let (request, mut prep_trace) =
            self.build_ai_request_for_selection(&profile, prompt.clone(), &selection, &edit_format);
        prep_trace.push("waiting for model response...".to_string());

        let lock_id = self.ai_state.next_lock_id;
        self.ai_state.next_lock_id = self.ai_state.next_lock_id.saturating_add(1);
        self.buffer_mut()
            .add_ai_lock(lock_id, selection.start_char, selection.end_char);

        let job_id = self.ai_state.next_job_id;
        self.ai_state.next_job_id = self.ai_state.next_job_id.saturating_add(1);

        let provider_label = format!("{}/{}", profile.provider, profile.model);
        let now = Instant::now();
        self.ai_state.regions.retain(|region| region.id != lock_id);
        self.ai_state.regions.push(AiEditRegion {
            id: lock_id,
            start_char: selection.start_char,
            end_char: selection.end_char,
            status: AiRegionStatus::Running,
            prompt: prompt.clone(),
            original_text: selection.selected_text.clone(),
            generated_text: String::new(),
            profile_name: profile_name.clone(),
            provider_label: provider_label.clone(),
            edit_format,
            reasoning_lines: prep_trace,
            raw_output: None,
            created_at: now,
            updated_at: now,
        });
        self.ai_state.selected_region_id = Some(lock_id);
        self.ai_state.selection_hold_until_exit = false;

        let project_ctx = crate::ai::project_context::load_project_context(
            &self.ai_state.config.project_context,
            self.buffers[self.current_buffer_index].file_path(),
        );

        let (tx, rx) = oneshot::channel();
        let task = tokio::spawn(async move {
            let result = request_ai_edit(
                &profile,
                &request,
                &api_key_registry,
                &prompts,
                &format_prompts,
                &project_ctx,
            )
            .await;
            let clone_for_channel = match &result {
                Ok(ok) => Ok(ok.clone()),
                Err(err) => Err(anyhow!(err.to_string())),
            };
            let _ = tx.send(clone_for_channel);
            result
        });

        self.ai_state.pending_jobs.push(PendingAiJob {
            job_id,
            lock_id,
            selection,
            submitted_at: Instant::now(),
            task,
            receiver: rx,
            completed_result: None,
        });

        self.ai_state.prompt.input.clear();
        self.ai_state.prompt.cursor = 0;
        self.ai_state.prompt.model_picker_open = false;
        self.ai_state.prompt.model_picker_index = 0;
        self.ai_state.active_selection = None;
        self.set_mode(Mode::Normal);
        self.set_status_message(format!("AI job {} started ({})", job_id, provider_label));
        Ok(())
    }

    /// Polls AI jobs; returns true when editor state changed.
    pub fn poll_pending_ai_jobs(&mut self) -> bool {
        let mut changed = false;
        let mut idx = 0;

        while idx < self.ai_state.pending_jobs.len() {
            let defer_apply = self.ai_should_defer_completed_job_apply();
            let mut ready_result = None;

            {
                let pending = &mut self.ai_state.pending_jobs[idx];
                if pending.completed_result.is_none() {
                    match pending.receiver.try_recv() {
                        Ok(result) => {
                            pending.completed_result = Some(result);
                        }
                        Err(TryRecvError::Empty) => {}
                        Err(TryRecvError::Closed) => {
                            pending.completed_result = Some(Err(anyhow!("AI job channel closed")));
                        }
                    }
                }

                if pending.completed_result.is_some() && !defer_apply {
                    ready_result = pending.completed_result.take();
                }
            }

            let Some(result) = ready_result else {
                // Job is still running, or it's done and deferred until current edit
                // transaction has been finalized.
                idx += 1;
                continue;
            };

            let job = self.ai_state.pending_jobs.remove(idx);
            let _ = job.task.is_finished();
            changed = true;

            match result {
                Ok(job_result) => {
                    if let Err(err) = self.apply_ai_job_result(job.lock_id, &job_result) {
                        self.buffer_mut().remove_ai_lock(job.lock_id);
                        self.update_ai_region_failure(
                            job.lock_id,
                            format!("apply failed: {}", err),
                            AiRegionStatus::Failed,
                        );
                        self.set_status_message(format!("AI apply failed: {}", err));
                    } else {
                        self.set_status_message(format!(
                            "AI edit applied ({}/{})",
                            job_result.provider, job_result.model
                        ));
                    }
                }
                Err(err) => {
                    self.buffer_mut().remove_ai_lock(job.lock_id);
                    self.update_ai_region_failure(
                        job.lock_id,
                        format!("request failed: {}", err),
                        AiRegionStatus::Failed,
                    );
                    self.set_status_message(format!("AI job failed: {}", err));
                }
            }
        }

        changed
    }

    pub fn ai_regions(&self) -> &[AiEditRegion] {
        &self.ai_state.regions
    }

    pub fn ai_selected_region_id(&self) -> Option<u64> {
        self.ai_state.selected_region_id
    }

    pub fn ai_region_by_id(&self, id: u64) -> Option<&AiEditRegion> {
        self.ai_state.regions.iter().find(|region| region.id == id)
    }

    pub fn ai_show_reasoning_for_selected_region(&mut self) -> bool {
        let Some(region_id) = self.ai_state.selected_region_id else {
            return false;
        };
        let Some(region) = self
            .ai_state
            .regions
            .iter()
            .find(|region| region.id == region_id)
        else {
            return false;
        };

        let reasoning_text = if region.reasoning_lines.is_empty() {
            "No reasoning available".to_string()
        } else {
            region.reasoning_lines.join("\n")
        };

        let mut message = String::new();
        message.push_str("**AI Edit Details**\n");
        message.push_str(&format!(
            "- profile: {}\n- provider/model: {}\n- edit_format: {}\n- status: {:?}\n\n",
            region.profile_name, region.provider_label, region.edit_format, region.status
        ));
        message.push_str("**Prompt**\n");
        message.push_str(&region.prompt);
        message.push_str("\n\n**Model Notes**\n");
        message.push_str(&reasoning_text);
        if let Some(raw) = &region.raw_output {
            message.push_str("\n\n**Raw Output**\n");
            message.push_str(raw);
        }

        let (line, col) = self.abs_char_to_line_col(region.start_char);
        self.lsp.state.hover_info = Some(message);
        self.lsp.state.hover_scroll = 0;
        self.lsp.state.hover_h_scroll = 0;
        self.lsp.state.hover_position = Some((line, col));
        self.lsp.state.hover_content_type = HoverContentType::AiReasoning;
        self.set_mode(Mode::HoverPreview);
        self.mark_dirty();
        true
    }

    pub fn ai_accept_selected_region(&mut self) -> bool {
        let Some(region_id) = self.ai_state.selected_region_id else {
            return false;
        };

        self.ai_remove_region(region_id);
        self.ai_state.selected_region_id = None;
        self.ai_state.selection_hold_until_exit = true;
        if self.hover_content_type() == HoverContentType::AiReasoning {
            self.clear_hover();
        }
        self.set_status_message("AI region accepted".to_string());
        true
    }

    pub fn ai_revert_selected_region(&mut self) -> Result<bool> {
        let Some(region_id) = self.ai_state.selected_region_id else {
            return Ok(false);
        };
        let Some(region) = self
            .ai_state
            .regions
            .iter()
            .find(|region| region.id == region_id)
            .cloned()
        else {
            return Ok(false);
        };

        if region.status == AiRegionStatus::Running {
            return Ok(self.ai_cancel_selected_region());
        }

        let rope_len = self.buffer().rope().len_chars();
        let start_char = region.start_char.min(rope_len);
        let end_char = region.end_char.min(rope_len).max(start_char);
        let replacement = region.original_text.clone();

        let cursor_before = self.cursor_position();
        let cursor_abs_before = self.cursor_abs_char();
        let ((), edits) = self.buffer_mut().with_ai_lock_bypass(|buf| {
            buf.record(|buf| {
                if end_char > start_char {
                    buf.delete_char_range(start_char, end_char);
                }
                let insert_pos = start_char.min(buf.rope().len_chars());
                let line = buf.rope().char_to_line(insert_pos);
                let col = insert_pos - buf.rope().line_to_char(line);
                if !replacement.is_empty() {
                    buf.insert_text_at(line, crate::unicode::CharCol(col), &replacement);
                }
            })
        });

        if !edits.is_empty() {
            let cursor_abs_after = remap_abs_char_through_edits(cursor_abs_before, &edits)
                .min(self.buffer().rope().len_chars());
            self.set_cursor_from_abs_char(cursor_abs_after);
            let cursor_after = self.cursor_position();
            self.push_recorded_undo(edits, cursor_before, cursor_after);
        }

        self.ai_remove_region(region_id);
        self.ai_state.selected_region_id = None;
        self.ai_state.selection_hold_until_exit = false;
        if self.hover_content_type() == HoverContentType::AiReasoning {
            self.clear_hover();
        }

        if self.buffer().needs_rehighlight() {
            self.process_viewport_rehighlight();
        }
        self.request_diagnostics_refresh();
        self.ai_state.last_observed_buffer_version = self.buffer().version();
        self.set_status_message("AI region reverted".to_string());
        self.mark_dirty();
        Ok(true)
    }

    pub fn ai_retry_selected_region(&mut self) -> Result<bool> {
        let Some(region_id) = self.ai_state.selected_region_id else {
            return Ok(false);
        };
        let Some(region_idx) = self
            .ai_state
            .regions
            .iter()
            .position(|region| region.id == region_id)
        else {
            return Ok(false);
        };

        if self.ai_state.regions[region_idx].status == AiRegionStatus::Running {
            self.set_status_message("AI region is already generating".to_string());
            return Ok(true);
        }

        let start_char = self.ai_state.regions[region_idx].start_char;
        let end_char = self.ai_state.regions[region_idx].end_char;
        let prompt = self.ai_state.regions[region_idx].prompt.clone();
        let profile_name = self.ai_state.regions[region_idx].profile_name.clone();
        let edit_format = self.ai_state.regions[region_idx].edit_format.clone();

        let selected_text = self
            .slice_text_by_chars(start_char, end_char)
            .unwrap_or_default();
        let (start_line, start_col) = self.abs_char_to_line_col(start_char);
        let end_for_col = end_char.saturating_sub(1).max(start_char);
        let (end_line, end_col) = self.abs_char_to_line_col(end_for_col);
        let selection = AiSelectionSnapshot {
            buffer_id: self.buffer().id(),
            start_line,
            start_col,
            end_line,
            end_col,
            start_char,
            end_char,
            anchor_line: start_line,
            selected_text: selected_text.clone(),
            mode_before_prompt: Mode::Normal,
        };

        let Some(profile) = self.ai_state.config.resolve_profile(&profile_name).cloned() else {
            self.set_status_message(format!("Unknown AI profile: {}", profile_name));
            return Ok(true);
        };
        let api_key_registry = self.ai_state.config.api_key_registry.clone();
        let prompts = self.ai_state.config.prompts.clone();
        let format_prompts = self.ai_state.config.format_prompts.clone();

        let (request, mut prep_trace) =
            self.build_ai_request_for_selection(&profile, prompt.clone(), &selection, &edit_format);
        prep_trace.push("retrying with same prompt...".to_string());

        // Replace tracking lock with blocking lock while generation is in flight.
        self.buffer_mut().remove_ai_lock(region_id);
        self.buffer_mut()
            .add_ai_lock(region_id, start_char, end_char);

        let provider_label = format!("{}/{}", profile.provider, profile.model);
        {
            let region = &mut self.ai_state.regions[region_idx];
            region.status = AiRegionStatus::Running;
            region.original_text = selected_text.clone();
            region.generated_text.clear();
            region.provider_label = provider_label.clone();
            region.reasoning_lines = prep_trace;
            region.raw_output = None;
            region.updated_at = Instant::now();
        }

        let project_ctx = crate::ai::project_context::load_project_context(
            &self.ai_state.config.project_context,
            self.buffers[self.current_buffer_index].file_path(),
        );

        let job_id = self.ai_state.next_job_id;
        self.ai_state.next_job_id = self.ai_state.next_job_id.saturating_add(1);

        let (tx, rx) = oneshot::channel();
        let task = tokio::spawn(async move {
            let result = request_ai_edit(
                &profile,
                &request,
                &api_key_registry,
                &prompts,
                &format_prompts,
                &project_ctx,
            )
            .await;
            let clone_for_channel = match &result {
                Ok(ok) => Ok(ok.clone()),
                Err(err) => Err(anyhow!(err.to_string())),
            };
            let _ = tx.send(clone_for_channel);
            result
        });

        self.ai_state.pending_jobs.push(PendingAiJob {
            job_id,
            lock_id: region_id,
            selection,
            submitted_at: Instant::now(),
            task,
            receiver: rx,
            completed_result: None,
        });

        self.ai_state.selection_hold_until_exit = false;
        self.set_status_message(format!("AI retry started ({})", provider_label));
        Ok(true)
    }

    pub fn ai_cancel_selected_region(&mut self) -> bool {
        let Some(region_id) = self.ai_state.selected_region_id else {
            return false;
        };

        if let Some(idx) = self
            .ai_state
            .pending_jobs
            .iter()
            .position(|pending| pending.lock_id == region_id)
        {
            let pending = self.ai_state.pending_jobs.remove(idx);
            pending.task.abort();
        }

        let running = self
            .ai_state
            .regions
            .iter()
            .find(|region| region.id == region_id)
            .map(|region| region.status == AiRegionStatus::Running)
            .unwrap_or(false);

        if running {
            self.buffer_mut().remove_ai_lock(region_id);
            self.ai_remove_region(region_id);
            self.ai_state.selected_region_id = None;
            self.ai_state.selection_hold_until_exit = false;
            if self.hover_content_type() == HoverContentType::AiReasoning {
                self.clear_hover();
            }
            self.set_status_message("AI generation cancelled".to_string());
            return true;
        }

        self.ai_state.selected_region_id = None;
        self.ai_state.selection_hold_until_exit = true;
        if self.hover_content_type() == HoverContentType::AiReasoning {
            self.clear_hover();
        }
        self.set_status_message("AI region selection cleared".to_string());
        true
    }

    pub fn ai_post_input_refresh(&mut self) {
        let current_version = self.buffer().version();
        if self.ai_state.last_observed_buffer_version != current_version {
            self.ai_sync_regions_with_locks();
            self.ai_drop_modified_generated_regions();
            self.ai_state.last_observed_buffer_version = current_version;
        }
        self.ai_refresh_region_selection_from_cursor();
    }

    fn ai_should_defer_completed_job_apply(&self) -> bool {
        // Avoid interleaving async AI edits into an in-progress insert/change
        // transaction. This keeps undo boundaries coherent.
        self.buffer().change_manager().is_building() || self.mode() == Mode::Replace
    }

    pub(crate) fn set_cursor_from_abs_char(&mut self, abs_char: usize) {
        let (line, col) = self.abs_char_to_line_col(abs_char);
        self.buffer_mut()
            .cursor_mut()
            .set_position(line, GraphemeCol(col));

        // In normal-like modes cursor must stay on a valid character cell.
        if !matches!(
            self.mode(),
            Mode::Insert
                | Mode::Replace
                | Mode::AiPrompt
                | Mode::Command
                | Mode::Search
                | Mode::RenameInput
        ) {
            self.buffer_mut().validate_cursor_position();
        }
    }

    fn apply_ai_job_result(&mut self, lock_id: u64, result: &crate::ai::AiJobResult) -> Result<()> {
        let lock = self
            .buffer()
            .ai_locks()
            .iter()
            .find(|lock| lock.id == lock_id)
            .copied()
            .ok_or_else(|| anyhow!("missing AI lock {}", lock_id))?;

        // For Lua formats, run the extract function on the main thread.
        let region_edit_format = self
            .ai_state
            .regions
            .iter()
            .find(|r| r.id == lock_id)
            .map(|r| r.edit_format.clone());
        let result = if let Some(crate::ai::EditFormat::Lua(ref name)) = region_edit_format {
            match self.run_lua_format_extract(name, &result.raw_output) {
                Ok(extracted) => std::borrow::Cow::Owned(crate::ai::AiJobResult {
                    replacement: extracted.replacement,
                    new_import_statements: extracted.new_import_statements,
                    log_lines: extracted.log_lines,
                    raw_output: result.raw_output.clone(),
                    provider: result.provider,
                    profile_name: result.profile_name.clone(),
                    model: result.model.clone(),
                    retry_attempts: result.retry_attempts,
                    elision_markers: result.elision_markers.clone(),
                }),
                Err(e) => {
                    // Fall back to raw extraction result with error logged
                    let mut fallback = result.clone();
                    fallback.log_lines = vec![format!("lua:{} extract failed: {}", name, e)];
                    std::borrow::Cow::Owned(fallback)
                }
            }
        } else {
            std::borrow::Cow::Borrowed(result)
        };
        let result = &*result;

        self.buffer_mut().remove_ai_lock(lock_id);

        let mut top_text = String::new();
        if !result.new_import_statements.is_empty() {
            top_text = result.new_import_statements.join("\n");
            if !top_text.ends_with('\n') {
                top_text.push('\n');
            }
        }

        let cursor_before = self.cursor_position();
        let cursor_abs_before = self.cursor_abs_char();
        let original_text = self
            .ai_state
            .regions
            .iter()
            .find(|region| region.id == lock_id)
            .map(|region| region.original_text.clone())
            .unwrap_or_default();
        let replacement =
            normalize_generated_replacement(&original_text, result.replacement.clone());
        let insert_pos = lock.start_char.min(self.buffer().rope().len_chars());
        let ((), edits) = self.buffer_mut().with_ai_lock_bypass(|buf| {
            buf.record(|buf| {
                if lock.end_char > lock.start_char {
                    buf.delete_char_range(lock.start_char, lock.end_char);
                }

                let current_insert = insert_pos.min(buf.rope().len_chars());
                let line = buf.rope().char_to_line(current_insert);
                let col = current_insert - buf.rope().line_to_char(line);
                if !replacement.is_empty() {
                    buf.insert_text_at(line, crate::unicode::CharCol(col), &replacement);
                }

                if !top_text.is_empty() {
                    buf.insert_text_at(0, crate::unicode::CharCol::ZERO, &top_text);
                }
            })
        });

        if edits.is_empty() {
            return Err(anyhow!("AI produced no edits"));
        }

        let cursor_abs_after = remap_abs_char_through_edits(cursor_abs_before, &edits)
            .min(self.buffer().rope().len_chars());
        self.set_cursor_from_abs_char(cursor_abs_after);
        let cursor_after = self.cursor_position();
        self.push_recorded_undo(edits, cursor_before, cursor_after);

        let top_chars = top_text.chars().count();
        let new_start = insert_pos.saturating_add(top_chars);
        let new_end = new_start.saturating_add(replacement.chars().count());
        self.buffer_mut()
            .add_ai_lock_with_mode(lock_id, new_start, new_end, false);

        if let Some(region) = self
            .ai_state
            .regions
            .iter_mut()
            .find(|region| region.id == lock_id)
        {
            region.start_char = new_start;
            region.end_char = new_end;
            region.status = AiRegionStatus::Generated;
            region.generated_text = replacement.clone();
            region.provider_label = format!("{}/{}", result.provider, result.model);
            region.profile_name = result.profile_name.clone();
            let mut lines = if result.log_lines.is_empty() {
                vec!["generation completed".to_string()]
            } else {
                result.log_lines.clone()
            };
            if result.retry_attempts > 0 {
                lines.push(format!(
                    "extraction succeeded after {} retry attempt{}",
                    result.retry_attempts,
                    if result.retry_attempts == 1 { "" } else { "s" },
                ));
            }
            if !result.elision_markers.is_empty() {
                lines.push(format!(
                    "warning: possible elision detected — {}",
                    result.elision_markers.join("; "),
                ));
            }
            region.reasoning_lines = lines;
            region.raw_output = Some(result.raw_output.clone());
            region.updated_at = Instant::now();
        }

        self.ai_state.selected_region_id = Some(lock_id);
        self.ai_state.selection_hold_until_exit = false;
        self.ai_state.last_observed_buffer_version = self.buffer().version();

        if self.buffer().needs_rehighlight() {
            self.process_viewport_rehighlight();
        }
        self.request_diagnostics_refresh();
        self.mark_dirty();
        Ok(())
    }

    #[cfg(feature = "lua")]
    fn run_lua_format_extract(
        &self,
        format_name: &str,
        raw_output: &str,
    ) -> Result<AiExtractedResponse> {
        let lua_ctx = self
            .lua_context
            .as_ref()
            .ok_or_else(|| anyhow!("Lua not enabled — cannot use lua format"))?;
        let lua = lua_ctx.lua();

        let registry: mlua::Table = lua
            .globals()
            .get("_ovim_format_registry")
            .map_err(|e| anyhow!("format registry not found: {}", e))?;
        let format: mlua::Table = registry
            .get(format_name)
            .map_err(|e| anyhow!("format '{}' not registered: {}", format_name, e))?;
        let extract_fn: mlua::Function = format
            .get("extract")
            .map_err(|e| anyhow!("format '{}' missing extract function: {}", format_name, e))?;

        let result: mlua::Value = extract_fn
            .call(raw_output)
            .map_err(|e| anyhow!("extract function error: {}", e))?;

        match result {
            mlua::Value::String(s) => {
                let replacement = s
                    .to_str()
                    .map_err(|e| anyhow!("extract returned invalid UTF-8: {}", e))?
                    .to_string();
                Ok(AiExtractedResponse {
                    replacement,
                    new_import_statements: Vec::new(),
                    log_lines: vec![format!("lua:{} extract ok", format_name)],
                })
            }
            mlua::Value::Table(t) => {
                let replacement: String = t
                    .get("replacement")
                    .map_err(|_| anyhow!("extract table must have 'replacement' field"))?;
                let imports: Vec<String> = t
                    .get::<_, mlua::Table>("new_import_statements")
                    .ok()
                    .map(|tbl| {
                        tbl.sequence_values::<String>()
                            .filter_map(|r| r.ok())
                            .collect()
                    })
                    .unwrap_or_default();
                let log: Vec<String> = t
                    .get::<_, mlua::Table>("log")
                    .ok()
                    .map(|tbl| {
                        tbl.sequence_values::<String>()
                            .filter_map(|r| r.ok())
                            .collect()
                    })
                    .unwrap_or_else(|| vec![format!("lua:{} extract ok", format_name)]);
                Ok(AiExtractedResponse {
                    replacement,
                    new_import_statements: imports,
                    log_lines: log,
                })
            }
            _ => Err(anyhow!(
                "extract function must return string or table, got {:?}",
                result.type_name()
            )),
        }
    }

    #[cfg(not(feature = "lua"))]
    fn run_lua_format_extract(
        &self,
        format_name: &str,
        _raw_output: &str,
    ) -> Result<AiExtractedResponse> {
        Err(anyhow!(
            "lua:{} format requires Lua feature to be enabled",
            format_name
        ))
    }

    fn update_ai_region_failure(&mut self, lock_id: u64, message: String, status: AiRegionStatus) {
        if let Some(region) = self
            .ai_state
            .regions
            .iter_mut()
            .find(|region| region.id == lock_id)
        {
            region.status = status;
            region.reasoning_lines = vec![message];
            region.updated_at = Instant::now();
        }
    }

    fn ai_refresh_region_selection_from_cursor(&mut self) {
        let cursor_abs = self.cursor_abs_char();
        let hovered_region = self
            .ai_state
            .regions
            .iter()
            .find(|region| {
                matches!(
                    region.status,
                    AiRegionStatus::Running | AiRegionStatus::Generated
                ) && region_contains_char(region, cursor_abs)
            })
            .map(|region| region.id);

        if self.ai_state.selection_hold_until_exit {
            if hovered_region.is_none() {
                self.ai_state.selection_hold_until_exit = false;
            }
            return;
        }

        self.ai_state.selected_region_id = hovered_region;
    }

    fn ai_drop_modified_generated_regions(&mut self) {
        let mut removed_ids = Vec::new();
        for region in &self.ai_state.regions {
            if region.status != AiRegionStatus::Generated {
                continue;
            }
            let matches_current = self
                .slice_text_by_chars(region.start_char, region.end_char)
                .map(|text| text == region.generated_text)
                .unwrap_or(false);
            if !matches_current {
                removed_ids.push(region.id);
            }
        }

        self.ai_state
            .regions
            .retain(|region| !removed_ids.contains(&region.id));

        for id in removed_ids {
            self.buffer_mut().remove_ai_lock(id);
            if self.ai_state.selected_region_id == Some(id) {
                self.ai_state.selected_region_id = None;
            }
        }

        if self.ai_state.selected_region_id.is_none()
            && self.hover_content_type() == HoverContentType::AiReasoning
        {
            self.clear_hover();
        }
    }

    fn ai_sync_regions_with_locks(&mut self) {
        let locks: Vec<(u64, usize, usize)> = self
            .buffer()
            .ai_locks()
            .iter()
            .map(|lock| (lock.id, lock.start_char, lock.end_char))
            .collect();

        for region in &mut self.ai_state.regions {
            if let Some((_, start, end)) = locks.iter().find(|(id, _, _)| *id == region.id) {
                region.start_char = *start;
                region.end_char = *end;
            }
        }
    }

    fn ai_remove_region(&mut self, region_id: u64) {
        self.ai_state
            .regions
            .retain(|region| region.id != region_id);
        self.buffer_mut().remove_ai_lock(region_id);
    }

    pub(crate) fn cursor_abs_char(&self) -> usize {
        let cursor = self.buffer().cursor();
        let rope = self.buffer().rope();
        if rope.len_lines() == 0 {
            return 0;
        }

        let line = cursor.line().min(rope.len_lines().saturating_sub(1));
        let line_start = rope.line_to_char(line);
        let line_end = if line + 1 < rope.len_lines() {
            rope.line_to_char(line + 1)
        } else {
            rope.len_chars()
        };
        let line_content_end = if line_end > line_start && rope.char(line_end - 1) == '\n' {
            line_end.saturating_sub(1)
        } else {
            line_end
        };
        let max_col = line_content_end.saturating_sub(line_start);
        line_start + cursor.col().0.min(max_col)
    }

    fn abs_char_to_line_col(&self, abs_char: usize) -> (usize, usize) {
        let rope = self.buffer().rope();
        let clamped = abs_char.min(rope.len_chars());
        let line = rope.char_to_line(clamped);
        let char_col = clamped.saturating_sub(rope.line_to_char(line));
        let line_text = crate::display::line_content(rope, line);
        let grapheme_col =
            crate::unicode::char_to_grapheme_col(&line_text, crate::unicode::CharCol(char_col));
        (line, grapheme_col.0)
    }

    fn slice_text_by_chars(&self, start_char: usize, end_char: usize) -> Option<String> {
        let rope = self.buffer().rope();
        let len = rope.len_chars();
        let start = start_char.min(len);
        let end = end_char.min(len);
        if end < start {
            return None;
        }
        Some(rope.slice(start..end).to_string())
    }
}

fn region_contains_char(region: &AiEditRegion, abs_char: usize) -> bool {
    if region.end_char > region.start_char {
        abs_char >= region.start_char && abs_char < region.end_char
    } else {
        abs_char == region.start_char
    }
}

pub(crate) fn remap_abs_char_through_edits(mut abs_char: usize, edits: &[Edit]) -> usize {
    for edit in edits {
        match edit {
            Edit::Insert { offset, text } => {
                if *offset <= abs_char {
                    abs_char = abs_char.saturating_add(text.chars().count());
                }
            }
            Edit::Delete { offset, text } => {
                let deleted_len = text.chars().count();
                let delete_end = offset.saturating_add(deleted_len);
                if abs_char >= delete_end {
                    abs_char = abs_char.saturating_sub(deleted_len);
                } else if abs_char > *offset {
                    abs_char = *offset;
                }
            }
        }
    }
    abs_char
}

fn normalize_generated_replacement(original_text: &str, mut replacement: String) -> String {
    // Preserve indentation shape for multiline selections when the model dedents output.
    if original_text.contains('\n') {
        let base_indent = original_text
            .lines()
            .find(|line| !line.trim().is_empty())
            .map(|line| {
                line.chars()
                    .take_while(|ch| *ch == ' ' || *ch == '\t')
                    .collect::<String>()
            })
            .unwrap_or_default();

        if !base_indent.is_empty() {
            let mut has_non_empty = false;
            let mut all_non_empty_already_indented = true;
            for line in replacement.split('\n') {
                if line.trim().is_empty() {
                    continue;
                }
                has_non_empty = true;
                if !line.starts_with(&base_indent) {
                    all_non_empty_already_indented = false;
                    break;
                }
            }

            if has_non_empty && !all_non_empty_already_indented {
                let mut normalized =
                    String::with_capacity(replacement.len() + base_indent.len() * 4);
                for segment in replacement.split_inclusive('\n') {
                    let (line, has_newline) = if let Some(stripped) = segment.strip_suffix('\n') {
                        (stripped, true)
                    } else {
                        (segment, false)
                    };

                    if !line.trim().is_empty() && !line.starts_with(&base_indent) {
                        normalized.push_str(&base_indent);
                    }
                    normalized.push_str(line);
                    if has_newline {
                        normalized.push('\n');
                    }
                }
                replacement = normalized;
            }
        }
    }

    // Preserve trailing newline shape from the original selection.
    if original_text.ends_with('\n') && !replacement.ends_with('\n') {
        replacement.push('\n');
    }

    replacement
}

#[cfg(test)]
mod tests {
    use super::{
        normalize_generated_replacement, remap_abs_char_through_edits, AiRegionStatus, Editor,
    };
    use crate::ai::{AiJobResult, AiProviderKind, EditFormat};
    use crate::edit::Edit;
    use crate::mode::Mode;
    use crate::unicode::GraphemeCol;
    use std::time::Instant;

    #[test]
    fn normalize_generated_replacement_preserves_trailing_newline() {
        let original = "    a();\n";
        let replacement = "    b();".to_string();
        assert_eq!(
            normalize_generated_replacement(original, replacement),
            "    b();\n"
        );
    }

    #[test]
    fn normalize_generated_replacement_restores_missing_base_indent() {
        let original = "    first();\n    second();\n";
        let replacement = "first();\nsecond();".to_string();
        assert_eq!(
            normalize_generated_replacement(original, replacement),
            "    first();\n    second();\n"
        );
    }

    #[test]
    fn normalize_generated_replacement_does_not_double_indent() {
        let original = "    first();\n    second();\n";
        let replacement = "    first();\n    second();\n".to_string();
        assert_eq!(
            normalize_generated_replacement(original, replacement),
            "    first();\n    second();\n"
        );
    }

    #[test]
    fn remap_abs_char_tracks_insertions_and_deletions() {
        let edits = vec![
            Edit::Delete {
                offset: 5,
                text: "xx".to_string(),
            },
            Edit::Insert {
                offset: 0,
                text: "head\n".to_string(),
            },
        ];
        // Start at absolute char 12 in the original document:
        // - delete 2 chars before cursor => 10
        // - insert 5 chars at top before cursor => 15
        assert_eq!(remap_abs_char_through_edits(12, &edits), 15);
    }

    #[test]
    fn remap_abs_char_clamps_into_deleted_span() {
        let edits = vec![Edit::Delete {
            offset: 10,
            text: "abcdef".to_string(),
        }];
        // Cursor inside deleted span should clamp to deletion start.
        assert_eq!(remap_abs_char_through_edits(13, &edits), 10);
    }

    #[test]
    fn visual_ai_selection_uses_half_open_grapheme_columns() {
        let mut editor = Editor::with_content("a👨‍👩‍👧‍👦b\n");
        editor.set_visual_start(0, 1);
        editor
            .buffer_mut()
            .cursor_mut()
            .set_position(0, GraphemeCol(1));
        editor.set_mode(Mode::Visual);

        assert!(editor.capture_ai_selection_from_visual().unwrap());

        let selection = editor.ai_state.active_selection.as_ref().unwrap();
        assert_eq!(selection.selected_text, "👨‍👩‍👧‍👦");
        assert_eq!((selection.start_col, selection.end_col), (1, 2));
        assert_eq!((selection.start_char, selection.end_char), (1, 8));
    }

    #[test]
    fn absolute_char_cursor_conversion_returns_grapheme_column() {
        let mut editor = Editor::with_content("a👨‍👩‍👧‍👦b\n");

        editor.set_cursor_from_abs_char(8);

        assert_eq!(editor.buffer().cursor().col(), GraphemeCol(2));
    }

    #[test]
    fn unknown_profile_selection_is_visible_and_preserves_active_profile() {
        let mut editor = Editor::default();
        let active = editor.ai_state.active_profile.clone();

        assert!(!editor.ai_set_profile("missing-profile"));

        assert_eq!(editor.ai_state.active_profile, active);
        assert_eq!(
            editor.status_message(),
            "Unknown AI profile: missing-profile"
        );
    }

    #[test]
    fn apply_ai_job_result_keeps_cursor_on_same_text_after_top_insertions() {
        let mut editor = Editor::with_content("a\nb\nc\n");
        let start = editor.buffer().rope().line_to_char(1);
        let end = editor.buffer().rope().line_to_char(2);
        editor.buffer_mut().add_ai_lock(99, start, end);

        let now = Instant::now();
        editor.ai_state.regions.push(super::AiEditRegion {
            id: 99,
            start_char: start,
            end_char: end,
            status: AiRegionStatus::Running,
            prompt: "rewrite".to_string(),
            original_text: "b\n".to_string(),
            generated_text: String::new(),
            profile_name: "alpha".to_string(),
            provider_label: "ollama/model".to_string(),
            edit_format: EditFormat::Json,
            reasoning_lines: vec![],
            raw_output: None,
            created_at: now,
            updated_at: now,
        });

        // Cursor starts on the "c" line.
        editor
            .buffer_mut()
            .cursor_mut()
            .set_position(2, crate::unicode::GraphemeCol::ZERO);

        let result = AiJobResult {
            replacement: "b\n".to_string(),
            new_import_statements: vec!["// head".to_string()],
            log_lines: vec![],
            raw_output: "{}".to_string(),
            provider: AiProviderKind::Ollama,
            profile_name: "alpha".to_string(),
            model: "model".to_string(),
            retry_attempts: 0,
            elision_markers: vec![],
        };

        editor
            .apply_ai_job_result(99, &result)
            .expect("apply should succeed");

        assert_eq!(editor.buffer().rope().to_string(), "// head\na\nb\nc\n");
        // Cursor remains anchored to the original "c" line (now shifted down by one).
        assert_eq!(
            editor.cursor_position(),
            crate::change::CursorPos::new(3, crate::unicode::GraphemeCol(0))
        );
    }
}

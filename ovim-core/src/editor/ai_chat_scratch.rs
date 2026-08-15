use anyhow::{anyhow, Result};

use crate::mode::Mode;

use super::ai_chat_state::ScratchBufferState;
use super::Editor;

impl Editor {
    /// Create a scratch buffer from the chat input for editing in Normal mode.
    pub fn open_chat_scratch_editor(&mut self) {
        let chat = match self.ai_state.chat.as_mut() {
            Some(c) => c,
            None => return,
        };

        let original_input = chat.input.clone();
        let original_buffer_id = self.buffer().id();

        // Create a new buffer with the current input
        let mut buffer = crate::buffer::Buffer::default();
        buffer.replace_all(&original_input);
        buffer.set_indent_options(self.indent_options());
        let scratch_buffer_id = buffer.id();
        self.buffers.push(buffer);
        let scratch_index = self.buffers.len() - 1;
        self.current_buffer_index = scratch_index;

        if let Some(chat) = self.ai_state.chat.as_mut() {
            chat.scratch = Some(ScratchBufferState {
                scratch_buffer_id,
                original_buffer_id,
                original_input,
            });
        }

        self.set_mode(Mode::Normal);
    }

    /// Close the scratch buffer and optionally transfer content back to chat input.
    pub fn finish_chat_scratch(&mut self, send: bool) -> Result<()> {
        let chat = match self.ai_state.chat.as_mut() {
            Some(c) => c,
            None => return Ok(()),
        };

        let scratch = match chat.scratch.take() {
            Some(s) => s,
            None => return Ok(()),
        };

        let scratch_content = self
            .get_buffer_by_id(scratch.scratch_buffer_id)
            .map(|buffer| buffer.rope().to_string());

        // Remove scratch buffer
        if let Some(index) = self.find_buffer_index_by_id(scratch.scratch_buffer_id) {
            self.buffers.remove(index);
        }
        if self.buffers.is_empty() {
            let mut buffer = crate::buffer::Buffer::default();
            buffer.set_indent_options(self.options.indent_options());
            self.buffers.push(buffer);
        }
        if let Some(index) = self.find_buffer_index_by_id(scratch.original_buffer_id) {
            self.current_buffer_index = index;
        } else {
            self.current_buffer_index = self.current_buffer_index.min(self.buffers.len() - 1);
        }
        self.set_mode(Mode::AiChat);

        let input = if send {
            scratch_content.ok_or_else(|| anyhow!("chat scratch buffer is no longer open"))?
        } else {
            scratch.original_input
        };
        if let Some(chat) = self.ai_state.chat.as_mut() {
            chat.input = input;
            chat.input_cursor = chat.input.len();
        }

        if self
            .find_buffer_index_by_id(scratch.original_buffer_id)
            .is_none()
        {
            return Err(anyhow!("original chat buffer is no longer open"));
        }

        Ok(())
    }

    /// Check if the current buffer is a chat scratch buffer.
    pub fn is_chat_scratch_buffer(&self) -> bool {
        if let Some(chat) = &self.ai_state.chat {
            if let Some(scratch) = &chat.scratch {
                return self.buffer().id() == scratch.scratch_buffer_id;
            }
        }
        false
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ai::chat_types::ChatOpts;
    use crate::buffer::Buffer;

    #[test]
    fn scratch_survives_buffer_index_changes() {
        let mut editor = Editor::default();
        let disposable_id = editor.buffer().id();
        editor.add_buffer(Buffer::new_from_str("original buffer"));
        let original_id = editor.buffer().id();
        editor.open_ai_chat(ChatOpts::default()).unwrap();
        editor.ai_state.chat.as_mut().unwrap().input = "draft".into();

        editor.open_chat_scratch_editor();
        let scratch_id = editor.buffer().id();
        editor.buffer_mut().replace_all("revised draft");

        let disposable_index = editor.find_buffer_index_by_id(disposable_id).unwrap();
        editor.switch_to_buffer(disposable_index);
        assert!(!editor.delete_current_buffer());
        assert_eq!(editor.buffer().id(), original_id);

        editor.finish_chat_scratch(true).unwrap();

        assert_eq!(editor.buffer().id(), original_id);
        assert!(editor.find_buffer_index_by_id(scratch_id).is_none());
        assert_eq!(editor.ai_chat_input(), "revised draft");
        assert_eq!(editor.mode(), Mode::AiChat);
    }
}

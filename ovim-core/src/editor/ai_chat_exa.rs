use crate::ai::AiProviderKind;

use super::ai_chat_state::ExaSetupDialog;
use super::Editor;

impl Editor {
    pub(crate) fn ai_chat_uses_direct_codex(&self) -> bool {
        self.ai_state
            .config
            .resolve_profile(&self.ai_chat_effective_profile())
            .is_some_and(|profile| profile.provider == AiProviderKind::Codex)
    }

    pub(crate) fn maybe_prompt_exa_on_chat_open(&mut self) {
        if !self.has_codex_auth_dialog()
            && self.ai_chat_uses_direct_codex()
            && crate::ai::exa::should_offer_onboarding()
        {
            self.open_exa_setup_dialog(None);
        }
    }

    pub(crate) fn open_exa_setup_dialog(&mut self, error: Option<String>) {
        let environment_override =
            std::env::var("EXA_API_KEY").is_ok_and(|value| !value.trim().is_empty());
        if let Some(chat) = self.ai_state.chat.as_mut() {
            chat.exa_setup = Some(ExaSetupDialog {
                input: String::new(),
                cursor: 0,
                error,
                environment_override,
            });
        }
    }

    pub fn ai_chat_has_exa_setup_dialog(&self) -> bool {
        self.ai_state
            .chat
            .as_ref()
            .is_some_and(|chat| chat.exa_setup.is_some())
    }

    pub fn ai_chat_exa_setup_summary(&self) -> Option<(String, usize, Option<String>, bool)> {
        let setup = self.ai_state.chat.as_ref()?.exa_setup.as_ref()?;
        Some((
            setup.input.clone(),
            setup.cursor,
            setup.error.clone(),
            setup.environment_override,
        ))
    }

    pub fn ai_chat_exa_dashboard_url(&self) -> &'static str {
        crate::ai::exa::DASHBOARD_URL
    }

    pub(crate) fn dismiss_exa_setup_dialog(&mut self) {
        if let Err(error) = crate::ai::exa::dismiss_onboarding() {
            self.set_status_message(format!("Could not save Exa setup preference: {error}"));
        }
        if let Some(chat) = self.ai_state.chat.as_mut() {
            chat.exa_setup = None;
        }
    }

    pub(crate) fn save_exa_setup_key(&mut self) {
        let setup = self
            .ai_state
            .chat
            .as_ref()
            .and_then(|chat| chat.exa_setup.as_ref())
            .cloned();
        let Some(setup) = setup else {
            return;
        };
        if setup.environment_override {
            if let Some(dialog) = self
                .ai_state
                .chat
                .as_mut()
                .and_then(|chat| chat.exa_setup.as_mut())
            {
                dialog.error = Some(
                    "EXA_API_KEY takes precedence over saved keys. Update or unset it in the environment first."
                        .to_string(),
                );
            }
            return;
        }
        let key = setup.input;
        match crate::ai::exa::save_key(&key) {
            Ok(()) => {
                if let Some(chat) = self.ai_state.chat.as_mut() {
                    chat.exa_setup = None;
                }
                self.set_status_message("Exa web search enabled".to_string());
            }
            Err(error) => {
                if let Some(setup) = self
                    .ai_state
                    .chat
                    .as_mut()
                    .and_then(|chat| chat.exa_setup.as_mut())
                {
                    setup.error = Some(error.to_string());
                }
            }
        }
    }

    pub(crate) fn insert_exa_setup_text(&mut self, text: &str) -> bool {
        let Some(setup) = self
            .ai_state
            .chat
            .as_mut()
            .and_then(|chat| chat.exa_setup.as_mut())
        else {
            return false;
        };
        let normalized = text.trim();
        setup.input.insert_str(setup.cursor, normalized);
        setup.cursor += normalized.len();
        setup.error = None;
        true
    }

    pub(crate) fn handle_exa_setup_key(&mut self, key: crate::KeyEvent) {
        use crate::KeyCode;
        match key.code {
            KeyCode::Esc => self.dismiss_exa_setup_dialog(),
            KeyCode::Enter => self.save_exa_setup_key(),
            KeyCode::Left => {
                if let Some(setup) = self
                    .ai_state
                    .chat
                    .as_mut()
                    .and_then(|c| c.exa_setup.as_mut())
                {
                    setup.cursor = setup.input[..setup.cursor]
                        .char_indices()
                        .next_back()
                        .map(|(index, _)| index)
                        .unwrap_or(0);
                }
            }
            KeyCode::Right => {
                if let Some(setup) = self
                    .ai_state
                    .chat
                    .as_mut()
                    .and_then(|c| c.exa_setup.as_mut())
                {
                    setup.cursor = setup.input[setup.cursor..]
                        .char_indices()
                        .nth(1)
                        .map(|(index, _)| setup.cursor + index)
                        .unwrap_or(setup.input.len());
                }
            }
            KeyCode::Backspace => {
                if let Some(setup) = self
                    .ai_state
                    .chat
                    .as_mut()
                    .and_then(|c| c.exa_setup.as_mut())
                {
                    if let Some((previous, _)) =
                        setup.input[..setup.cursor].char_indices().next_back()
                    {
                        setup.input.drain(previous..setup.cursor);
                        setup.cursor = previous;
                    }
                    setup.error = None;
                }
            }
            KeyCode::Char(character)
                if !key
                    .modifiers
                    .intersects(crate::Modifiers::CONTROL | crate::Modifiers::SUPER) =>
            {
                let mut encoded = [0; 4];
                self.insert_exa_setup_text(character.encode_utf8(&mut encoded));
            }
            _ => {}
        }
    }

    pub(crate) fn note_exa_credential_rejected(&mut self, environment_override: bool) {
        let message = if environment_override {
            "EXA_API_KEY was rejected. Update or unset that environment variable, then reopen /exa."
        } else {
            "The saved Exa API key was rejected or revoked. Paste a replacement key."
        };
        self.open_exa_setup_dialog(Some(message.to_string()));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ai::chat_types::ChatOpts;
    use crate::{KeyCode, KeyEvent, Modifiers};

    fn editor_with_dialog() -> Editor {
        let mut editor = Editor::default();
        let buffer_id = editor.buffer().id();
        editor.ai_state.chat = Some(super::super::ai_chat_state::AiChatState::new(
            ChatOpts::default(),
            buffer_id,
            crate::mode::Mode::Normal,
        ));
        editor.open_exa_setup_dialog(None);
        editor
    }

    #[test]
    fn key_field_edits_without_exposing_the_chat_composer() {
        let mut editor = editor_with_dialog();
        for character in "exa-key".chars() {
            editor.handle_exa_setup_key(KeyEvent {
                code: KeyCode::Char(character),
                modifiers: Modifiers::NONE,
            });
        }
        editor.handle_exa_setup_key(KeyEvent {
            code: KeyCode::Left,
            modifiers: Modifiers::NONE,
        });
        editor.handle_exa_setup_key(KeyEvent {
            code: KeyCode::Backspace,
            modifiers: Modifiers::NONE,
        });
        let (input, cursor, error, _) = editor.ai_chat_exa_setup_summary().unwrap();
        assert_eq!(input, "exa-ky");
        assert_eq!(cursor, 5);
        assert!(error.is_none());
        assert!(editor.ai_chat_input().is_empty());
    }

    #[test]
    fn invalid_key_stays_in_dialog_with_actionable_error() {
        let mut editor = editor_with_dialog();
        editor.insert_exa_setup_text("short");
        editor.save_exa_setup_key();
        let (_, _, error, _) = editor.ai_chat_exa_setup_summary().unwrap();
        assert!(error.unwrap().contains("complete Exa API key"));
    }
}

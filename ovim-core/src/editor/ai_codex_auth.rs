use crate::ai::codex_auth::{self, CredentialReadiness};
use crate::ai::AiProviderKind;
use crate::{KeyCode, KeyEvent};

use super::ai_state::{
    CodexAuthDialog, CodexAuthDialogPhase, CodexAuthDialogSummary, CodexAuthResume,
    PendingCodexAuth, PendingCodexAuthReceiver,
};
use super::Editor;

enum PendingCodexAuthOutcome {
    Completion(anyhow::Result<()>),
    DeviceCode(anyhow::Result<codex_auth::DeviceLoginCode>),
}

fn is_ssh_session() -> bool {
    is_ssh_session_with(|name| std::env::var_os(name))
}

fn is_ssh_session_with(mut value: impl FnMut(&str) -> Option<std::ffi::OsString>) -> bool {
    ["SSH_CONNECTION", "SSH_CLIENT", "SSH_TTY"]
        .into_iter()
        .any(|name| value(name).is_some_and(|value| !value.is_empty()))
}

impl Editor {
    pub fn codex_auth_dialog_summary(&self) -> Option<CodexAuthDialogSummary> {
        let dialog = self.ai_state.codex_auth_dialog.as_ref()?;
        Some(CodexAuthDialogSummary {
            phase: dialog.phase.clone(),
            detail: dialog.detail.clone(),
            authorize_url: dialog.authorize_url.clone(),
            user_code: dialog.user_code.clone(),
        })
    }

    pub fn has_codex_auth_dialog(&self) -> bool {
        self.ai_state.codex_auth_dialog.is_some()
    }

    pub fn take_pending_external_url(&mut self) -> Option<String> {
        self.ai_state.pending_external_url.take()
    }

    pub(crate) fn maybe_prompt_codex_auth_on_chat_open(&mut self) {
        let profile_name = self.ai_chat_effective_profile();
        self.ensure_codex_auth_for_profile(&profile_name, CodexAuthResume::None);
    }

    pub(crate) fn ensure_codex_auth_for_chat_submit(&mut self) -> bool {
        let profile_name = self.ai_chat_effective_profile();
        self.ensure_codex_auth_for_profile(&profile_name, CodexAuthResume::SubmitChat)
    }

    fn ensure_codex_auth_for_profile(
        &mut self,
        profile_name: &str,
        resume: CodexAuthResume,
    ) -> bool {
        let direct_codex = self
            .ai_state
            .config
            .resolve_profile(profile_name)
            .is_some_and(|profile| profile.provider == AiProviderKind::Codex);
        if !direct_codex {
            return true;
        }

        if let Some(dialog) = self.ai_state.codex_auth_dialog.as_mut() {
            if !matches!(resume, CodexAuthResume::None) {
                dialog.resume = resume;
            }
            return false;
        }

        match codex_auth::credential_readiness() {
            CredentialReadiness::Ready => true,
            CredentialReadiness::RefreshRequired => {
                self.start_codex_auth_refresh(resume);
                false
            }
            CredentialReadiness::LoginRequired(detail) => {
                self.ai_state.codex_auth_dialog = Some(CodexAuthDialog {
                    phase: CodexAuthDialogPhase::Offer,
                    detail: Some(detail),
                    authorize_url: None,
                    user_code: None,
                    resume,
                });
                false
            }
        }
    }

    fn start_codex_auth_refresh(&mut self, resume: CodexAuthResume) {
        let (tx, receiver) = tokio::sync::oneshot::channel();
        let task = tokio::spawn(async move {
            let _ = tx.send(codex_auth::refresh_for_ui().await);
        });
        self.ai_state.codex_auth_dialog = Some(CodexAuthDialog {
            phase: CodexAuthDialogPhase::Refreshing,
            detail: None,
            authorize_url: None,
            user_code: None,
            resume,
        });
        self.ai_state.pending_codex_auth = Some(PendingCodexAuth {
            receiver: PendingCodexAuthReceiver::Completion(receiver),
            task,
        });
    }

    fn start_codex_browser_login(&mut self) {
        self.ai_state.pending_codex_auth = None;
        match codex_auth::begin_login() {
            Ok(attempt) => {
                self.ai_state.pending_external_url = Some(attempt.authorize_url.clone());
                if let Some(dialog) = self.ai_state.codex_auth_dialog.as_mut() {
                    dialog.phase = CodexAuthDialogPhase::WaitingForBrowser;
                    dialog.detail = None;
                    dialog.authorize_url = Some(attempt.authorize_url);
                    dialog.user_code = None;
                }
                self.ai_state.pending_codex_auth = Some(PendingCodexAuth {
                    receiver: PendingCodexAuthReceiver::Completion(attempt.receiver),
                    task: attempt.task,
                });
            }
            Err(error) => {
                if let Some(dialog) = self.ai_state.codex_auth_dialog.as_mut() {
                    dialog.phase = CodexAuthDialogPhase::Error;
                    dialog.detail = Some(error.to_string());
                    dialog.authorize_url = None;
                    dialog.user_code = None;
                }
            }
        }
    }

    fn start_codex_device_login(&mut self) {
        self.ai_state.pending_codex_auth = None;
        let (tx, receiver) = tokio::sync::oneshot::channel();
        let task = tokio::spawn(async move {
            let _ = tx.send(codex_auth::request_device_login().await);
        });
        if let Some(dialog) = self.ai_state.codex_auth_dialog.as_mut() {
            dialog.phase = CodexAuthDialogPhase::PreparingDeviceCode;
            dialog.detail = None;
            dialog.authorize_url = None;
            dialog.user_code = None;
        }
        self.ai_state.pending_codex_auth = Some(PendingCodexAuth {
            receiver: PendingCodexAuthReceiver::DeviceCode(receiver),
            task,
        });
    }

    fn start_preferred_codex_login(&mut self) {
        if is_ssh_session() {
            self.start_codex_device_login();
        } else {
            self.start_codex_browser_login();
        }
    }

    pub(crate) fn handle_codex_auth_key(&mut self, key: KeyEvent) {
        let phase = self
            .ai_state
            .codex_auth_dialog
            .as_ref()
            .map(|dialog| dialog.phase.clone());
        match (phase, key.code) {
            (_, KeyCode::Esc) => {
                self.ai_state.pending_codex_auth = None;
                self.ai_state.codex_auth_dialog = None;
                self.ai_state.pending_external_url = None;
                self.set_status_message(
                    "Codex sign-in dismissed; your draft and selection were preserved",
                );
            }
            (Some(CodexAuthDialogPhase::Offer | CodexAuthDialogPhase::Error), KeyCode::Enter) => {
                self.start_preferred_codex_login();
            }
            (
                Some(CodexAuthDialogPhase::Offer | CodexAuthDialogPhase::Error),
                KeyCode::Char('d' | 'D'),
            ) => {
                self.start_codex_device_login();
            }
            (
                Some(CodexAuthDialogPhase::Offer | CodexAuthDialogPhase::Error),
                KeyCode::Char('b' | 'B'),
            ) => {
                self.start_codex_browser_login();
            }
            (Some(CodexAuthDialogPhase::WaitingForBrowser), KeyCode::Char('o' | 'O')) => {
                self.ai_state.pending_external_url = self
                    .ai_state
                    .codex_auth_dialog
                    .as_ref()
                    .and_then(|dialog| dialog.authorize_url.clone());
            }
            _ => {}
        }
    }

    pub fn poll_pending_codex_auth(&mut self) -> bool {
        let outcome = {
            let Some(pending) = self.ai_state.pending_codex_auth.as_mut() else {
                return false;
            };
            match &mut pending.receiver {
                PendingCodexAuthReceiver::Completion(receiver) => match receiver.try_recv() {
                    Ok(result) => Some(PendingCodexAuthOutcome::Completion(result)),
                    Err(tokio::sync::oneshot::error::TryRecvError::Empty) => None,
                    Err(tokio::sync::oneshot::error::TryRecvError::Closed) => {
                        Some(PendingCodexAuthOutcome::Completion(Err(anyhow::anyhow!(
                            "Codex sign-in task stopped unexpectedly"
                        ))))
                    }
                },
                PendingCodexAuthReceiver::DeviceCode(receiver) => match receiver.try_recv() {
                    Ok(result) => Some(PendingCodexAuthOutcome::DeviceCode(result)),
                    Err(tokio::sync::oneshot::error::TryRecvError::Empty) => None,
                    Err(tokio::sync::oneshot::error::TryRecvError::Closed) => {
                        Some(PendingCodexAuthOutcome::DeviceCode(Err(anyhow::anyhow!(
                            "Codex device sign-in task stopped unexpectedly"
                        ))))
                    }
                },
            }
        };
        let Some(outcome) = outcome else {
            return false;
        };
        self.ai_state.pending_codex_auth = None;

        match outcome {
            PendingCodexAuthOutcome::DeviceCode(Ok(code)) => {
                if let Some(dialog) = self.ai_state.codex_auth_dialog.as_mut() {
                    dialog.phase = CodexAuthDialogPhase::WaitingForDeviceCode;
                    dialog.detail = None;
                    dialog.authorize_url = Some(code.verification_url.clone());
                    dialog.user_code = Some(code.user_code.clone());
                }
                let (tx, receiver) = tokio::sync::oneshot::channel();
                let task = tokio::spawn(async move {
                    let _ = tx.send(codex_auth::complete_device_login(code).await);
                });
                self.ai_state.pending_codex_auth = Some(PendingCodexAuth {
                    receiver: PendingCodexAuthReceiver::Completion(receiver),
                    task,
                });
            }
            PendingCodexAuthOutcome::Completion(Ok(())) => {
                let resume = self
                    .ai_state
                    .codex_auth_dialog
                    .take()
                    .map(|dialog| dialog.resume)
                    .unwrap_or(CodexAuthResume::None);
                self.set_status_message("Signed in to Codex for Ovim");
                self.resume_after_codex_auth(resume);
            }
            PendingCodexAuthOutcome::Completion(Err(error))
            | PendingCodexAuthOutcome::DeviceCode(Err(error)) => {
                if let Some(dialog) = self.ai_state.codex_auth_dialog.as_mut() {
                    dialog.phase = CodexAuthDialogPhase::Error;
                    dialog.detail = Some(error.to_string());
                    dialog.authorize_url = None;
                    dialog.user_code = None;
                }
            }
        }
        true
    }

    fn resume_after_codex_auth(&mut self, resume: CodexAuthResume) {
        match resume {
            CodexAuthResume::None => {}
            CodexAuthResume::SubmitChat => {
                if let Err(error) = self.submit_ai_chat_message() {
                    self.set_status_message(format!("Could not resume the chat message: {error}"));
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ai::chat_types::ChatOpts;
    use crate::mode::Mode;
    use crate::Modifiers;

    fn direct_codex_editor() -> Editor {
        let mut editor = Editor::default();
        let profile = editor.ai_state.active_profile.clone();
        editor
            .ai_state
            .config
            .profiles
            .get_mut(&profile)
            .expect("default profile")
            .provider = AiProviderKind::Codex;
        editor
    }

    #[test]
    fn escape_dismisses_dialog_without_touching_drafts() {
        let mut editor = Editor::default();
        editor.open_ai_chat(ChatOpts::default()).expect("open chat");
        editor.ai_state.chat.as_mut().expect("chat").input = "rewrite this".into();
        editor.ai_state.codex_auth_dialog = Some(CodexAuthDialog {
            phase: CodexAuthDialogPhase::Offer,
            detail: None,
            authorize_url: None,
            user_code: None,
            resume: CodexAuthResume::None,
        });
        editor.handle_codex_auth_key(KeyEvent {
            code: KeyCode::Esc,
            modifiers: Modifiers::NONE,
        });
        assert!(!editor.has_codex_auth_dialog());
        assert_eq!(
            editor.ai_state.chat.as_ref().expect("chat").input,
            "rewrite this"
        );
    }

    #[test]
    fn reopen_queues_the_same_authorization_url() {
        let mut editor = Editor::default();
        editor.ai_state.codex_auth_dialog = Some(CodexAuthDialog {
            phase: CodexAuthDialogPhase::WaitingForBrowser,
            detail: None,
            authorize_url: Some("https://example.test/login".into()),
            user_code: None,
            resume: CodexAuthResume::None,
        });
        editor.handle_codex_auth_key(KeyEvent {
            code: KeyCode::Char('o'),
            modifiers: Modifiers::NONE,
        });
        assert_eq!(
            editor.take_pending_external_url().as_deref(),
            Some("https://example.test/login")
        );
    }

    #[test]
    fn opening_direct_codex_chat_prompts_contextually() {
        let mut editor = direct_codex_editor();
        editor.open_ai_chat(ChatOpts::default()).unwrap();
        assert_eq!(editor.mode(), Mode::AiChat);
        assert_eq!(
            editor.codex_auth_dialog_summary().unwrap().phase,
            CodexAuthDialogPhase::Offer
        );
    }

    #[test]
    fn chat_submit_is_guarded_before_consuming_the_draft_or_allocating_a_turn() {
        let mut editor = direct_codex_editor();
        editor.open_ai_chat(ChatOpts::default()).unwrap();
        let chat = editor.ai_state.chat.as_mut().unwrap();
        chat.input = "inspect this project".into();
        chat.input_cursor = chat.input.len();

        editor.submit_ai_chat_message().unwrap();

        let chat = editor.ai_state.chat.as_ref().unwrap();
        assert_eq!(chat.input, "inspect this project");
        assert!(chat.runtime_turn.is_none());
        assert!(chat.pending_job.is_none());
        assert!(matches!(
            editor
                .ai_state
                .codex_auth_dialog
                .as_ref()
                .map(|dialog| &dialog.resume),
            Some(CodexAuthResume::SubmitChat)
        ));
    }

    #[test]
    fn auth_dialog_captures_mode_keys_globally() {
        let mut editor = direct_codex_editor();
        editor.open_ai_chat(ChatOpts::default()).unwrap();
        editor.set_mode(Mode::Normal);

        crate::editor::InputHandler::handle_key_event(
            &mut editor,
            KeyEvent {
                code: KeyCode::Char('i'),
                modifiers: Modifiers::NONE,
            },
        )
        .unwrap();

        assert_eq!(editor.mode(), Mode::Normal);
        assert!(editor.has_codex_auth_dialog());
    }

    #[test]
    fn ssh_detection_accepts_any_nonempty_ssh_marker() {
        let detected =
            is_ssh_session_with(|name| (name == "SSH_CONNECTION").then(|| "client server".into()));
        assert!(detected);
        assert!(!is_ssh_session_with(|_| None));
        assert!(!is_ssh_session_with(|_| Some("".into())));
    }

    #[test]
    fn dialog_summary_exposes_device_code_without_internal_polling_state() {
        let mut editor = Editor::default();
        editor.ai_state.codex_auth_dialog = Some(CodexAuthDialog {
            phase: CodexAuthDialogPhase::WaitingForDeviceCode,
            detail: None,
            authorize_url: Some("https://auth.example/device".into()),
            user_code: Some("ABCD-EFGH".into()),
            resume: CodexAuthResume::None,
        });

        let summary = editor.codex_auth_dialog_summary().unwrap();
        assert_eq!(
            summary.authorize_url.as_deref(),
            Some("https://auth.example/device")
        );
        assert_eq!(summary.user_code.as_deref(), Some("ABCD-EFGH"));
    }
}

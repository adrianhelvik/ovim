use crate::ai::chat_types::ToolCallInfo;
use crate::ai::tools::browser::is_browser_tool;
use crate::ai::tools::ToolResult;

use super::ai_chat_state::{
    BackgroundToolAftermath, BackgroundToolOutcome, PendingBackgroundTool,
    ToolExecutionContinuation,
};
use super::Editor;

impl Editor {
    pub(super) fn is_background_ai_tool(&self, name: &str) -> bool {
        is_browser_tool(name) || matches!(name, "web_search" | "web_fetch")
    }

    pub(super) fn begin_pending_background_tool(
        &mut self,
        call: ToolCallInfo,
        continuation: ToolExecutionContinuation,
    ) -> Result<(), (ToolResult, Box<ToolExecutionContinuation>)> {
        if self.ai_state.chat.is_none() {
            return Err((
                ToolResult::Error("no active chat session".into()),
                Box::new(continuation),
            ));
        }

        let (receiver, task, status) = if is_browser_tool(&call.name) {
            if !self.browser_tool_is_authorized(&call.name) {
                return Err((
                    ToolResult::Error(
                        "embedded browser access is not authorized or available".into(),
                    ),
                    Box::new(continuation),
                ));
            }
            let command = match self.prepare_browser_command(&call) {
                Ok(command) => command,
                Err(result) => return Err((result, Box::new(continuation))),
            };
            let Some(client) = self.services().browser().cloned() else {
                return Err((
                    ToolResult::Error("the embedded browser host is unavailable".into()),
                    Box::new(continuation),
                ));
            };
            let (sender, receiver) = tokio::sync::oneshot::channel();
            let task = tokio::spawn(async move {
                let result = match client.execute(command).await {
                    Ok(response) => super::ai_browser::browser_response_result(response),
                    Err(error) => ToolResult::Error(format!("browser request failed: {error}")),
                };
                let _ = sender.send(BackgroundToolOutcome::plain(result));
            });
            (receiver, task, "Working in the embedded browser")
        } else if matches!(call.name.as_str(), "web_search" | "web_fetch")
            && self.ai_chat_uses_direct_codex()
        {
            let worker_call = call.clone();
            let (sender, receiver) = tokio::sync::oneshot::channel();
            let task = tokio::task::spawn_blocking(move || {
                let outcome = crate::ai::exa::execute(&worker_call.name, &worker_call.arguments);
                let _ = sender.send(BackgroundToolOutcome {
                    result: outcome.result,
                    aftermath: BackgroundToolAftermath::Exa {
                        credential_rejected: outcome.credential_rejected,
                        environment_override: outcome.environment_override,
                        setup_error: outcome.setup_error,
                    },
                });
            });
            (receiver, task, "Searching the web with Exa")
        } else {
            return Err((
                ToolResult::Error(format!("unsupported background tool: {}", call.name)),
                Box::new(continuation),
            ));
        };

        let chat = self
            .ai_state
            .chat
            .as_mut()
            .expect("active chat checked above");
        debug_assert!(chat.pending_background_tool.is_none());
        chat.pending_background_tool = Some(PendingBackgroundTool {
            tool_call: call,
            continuation,
            receiver,
            task,
        });
        chat.waiting = true;
        self.set_status_message(status);
        Ok(())
    }

    fn browser_tool_is_authorized(&self, tool_name: &str) -> bool {
        let Some(profile) = self
            .ai_state
            .chat
            .as_ref()
            .and_then(|chat| chat.opts.profile.as_deref())
            .or(Some(self.ai_state.active_profile.as_str()))
            .and_then(|profile| self.ai_state.config.resolve_profile(profile))
        else {
            return false;
        };
        let capabilities = self.build_chat_capabilities();
        let services = self.build_chat_runtime_services();
        self.ai_state
            .tool_registry
            .tools_for_profile_with_services(profile, &capabilities, services)
            .iter()
            .any(|definition| definition.name == tool_name)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ai::chat_types::ChatOpts;
    use crate::browser::{BrowserCommand, BrowserResponse, BrowserSession};
    use crate::editor::EditorServices;

    fn browser_editor(allow_edits: bool) -> (Editor, crate::browser::BrowserRequestReceiver) {
        let (browser, host) = crate::browser::browser_channel();
        let mut editor =
            Editor::default().with_services(EditorServices::default().with_browser(browser));
        let active = editor.ai_state.active_profile.clone();
        editor
            .ai_state
            .config
            .profiles
            .get_mut(&active)
            .expect("active profile")
            .scope
            .network = true;
        editor
            .open_ai_chat(ChatOpts {
                allow_edits,
                ..ChatOpts::default()
            })
            .unwrap();
        (editor, host)
    }

    fn batch_continuation() -> ToolExecutionContinuation {
        ToolExecutionContinuation::Batch {
            runtime_tool: None,
            runtime_turn: None,
            remaining_tool_calls: Vec::new(),
            model_name: "test".into(),
        }
    }

    #[tokio::test]
    async fn browser_background_tool_reaches_host_and_returns_untrusted_envelope() {
        let (mut editor, mut host) = browser_editor(false);
        let call = ToolCallInfo {
            id: "browser-start".into(),
            name: crate::ai::tools::browser::BROWSER_SESSION_TOOL.into(),
            arguments: serde_json::json!({"action": "start"}),
        };
        assert!(editor
            .begin_pending_background_tool(call, batch_continuation())
            .is_ok());

        let request = host.recv().await.expect("browser host request");
        assert_eq!(
            request.command(),
            &BrowserCommand::Start { incognito: true }
        );
        request.respond(Ok(BrowserResponse::Session(BrowserSession {
            session_id: "browser-1".into(),
            url: "about:blank".into(),
            title: String::new(),
            visible: true,
            loading: false,
            document_id: 0,
        })));

        let pending = editor
            .ai_state
            .chat
            .as_mut()
            .unwrap()
            .pending_background_tool
            .take()
            .unwrap();
        let outcome = pending.receiver.await.unwrap();
        match outcome.result {
            ToolResult::Success(text) => {
                assert!(text.contains("untrusted data"));
                assert!(text.contains("browser-1"));
            }
            ToolResult::Error(error) => panic!("browser tool failed: {error}"),
        }
    }

    #[test]
    fn browser_actions_remain_unavailable_in_read_only_chat() {
        let (mut editor, _host) = browser_editor(false);
        let call = ToolCallInfo {
            id: "browser-click".into(),
            name: crate::ai::tools::browser::BROWSER_ACT_TOOL.into(),
            arguments: serde_json::json!({
                "session_id": "browser-1",
                "document_id": 1,
                "snapshot_id": 1,
                "action": "click",
                "element": "e1"
            }),
        };
        let error = editor
            .begin_pending_background_tool(call, batch_continuation())
            .unwrap_err()
            .0;
        assert!(matches!(error, ToolResult::Error(_)));
    }
}

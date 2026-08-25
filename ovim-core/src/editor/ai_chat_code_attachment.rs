use super::ai_chat_state::CodeAttachment;
use super::Editor;

const OPEN: &str = "<ovim-code-attachment>\n";
const CLOSE: &str = "</ovim-code-attachment>\n";

pub fn compose_code_attachment_message(attachment: &CodeAttachment, input: &str) -> String {
    let path = attachment.path.as_deref().unwrap_or("untitled");
    let context = attachment
        .source_context
        .as_deref()
        .map(|context| format!("context: {context}\n"))
        .unwrap_or_default();
    format!(
        "{OPEN}path: {path}\nlines: {}-{}\nbuffer-revision: {}\n{context}---\n{}\n{CLOSE}{}",
        attachment.start_line + 1,
        attachment.end_line + 1,
        attachment.buffer_revision,
        attachment.text,
        input.trim(),
    )
}

/// Returns the compact attachment label and the user-authored message body.
pub fn split_code_attachment_message(content: &str) -> Option<(String, &str)> {
    let body = content.strip_prefix(OPEN)?;
    let (attachment, input) = body.split_once(CLOSE)?;
    let mut lines = attachment.lines();
    let path = lines.next()?.strip_prefix("path: ")?;
    let range = lines.next()?.strip_prefix("lines: ")?;
    Some((format!("{path}:{range}"), input))
}

impl Editor {
    pub fn ai_chat_pending_code_attachment(&self) -> Option<&CodeAttachment> {
        self.ai_state
            .chat
            .as_ref()
            .and_then(|chat| chat.pending_code_attachment.as_ref())
    }

    pub fn remove_ai_chat_code_attachment(&mut self) -> bool {
        self.ai_state
            .chat
            .as_mut()
            .and_then(|chat| chat.pending_code_attachment.take())
            .is_some()
    }

    pub fn ai_chat_pending_code_attachment_modified(&self) -> bool {
        let Some(attachment) = self.ai_chat_pending_code_attachment() else {
            return false;
        };
        self.get_buffer_by_id(attachment.buffer_id)
            .map(|buffer| buffer.version() != attachment.buffer_revision)
            .unwrap_or(true)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn message_round_trip_keeps_compact_label_and_draft() {
        let attachment = CodeAttachment {
            buffer_id: 7,
            path: Some("src/main.rs".into()),
            start_line: 4,
            start_column: 0,
            end_line: 6,
            end_column: 0,
            linewise: true,
            buffer_revision: 12,
            source_context: None,
            text: "fn main() {}".into(),
        };
        let message = compose_code_attachment_message(&attachment, "Explain this");
        let (label, draft) = split_code_attachment_message(&message).unwrap();
        assert_eq!(label, "src/main.rs:5-7");
        assert_eq!(draft, "Explain this");
        assert!(message.contains("fn main() {}"));
    }

    #[test]
    fn diff_context_is_preserved_in_the_agent_message() {
        let attachment = CodeAttachment {
            buffer_id: 9,
            path: Some("[Diff · src/main.rs]".into()),
            start_line: 8,
            start_column: 0,
            end_line: 9,
            end_column: 0,
            linewise: true,
            buffer_revision: 0,
            source_context: Some("comparison: main...WORKTREE\nfile: src/main.rs".into()),
            text: "+let safe = value?;".into(),
        };
        let message = compose_code_attachment_message(&attachment, "Check this");
        assert!(message.contains("context: comparison: main...WORKTREE"));
        assert!(message.contains("file: src/main.rs"));
        assert!(message.ends_with("Check this"));
    }
}

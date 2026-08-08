//! Delegated-agent conversation switcher and approval projection.
//!
//! Agents use the primary chat surface. This module owns only the compact
//! conversation selector and the transport-neutral approval summary.

use ovim_core::agent_runtime::AgentControlPlaneSnapshot;
use ratatui::{
    style::{Color, Modifier, Style},
    text::{Line, Span},
};
use unicode_segmentation::UnicodeSegmentation;
use unicode_width::UnicodeWidthStr;

use super::ai_chat::{TEXT_DIM, TEXT_NORMAL};

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct AgentApprovalPrompt {
    pub summary: String,
}

pub(crate) fn agent_switcher_lines(
    snapshot: &AgentControlPlaneSnapshot,
    width: usize,
    focused: bool,
    cursor: usize,
    selected_agent_id: Option<&str>,
) -> Vec<Line<'static>> {
    if !focused {
        let current = selected_agent_id
            .and_then(|id| {
                snapshot
                    .agents
                    .iter()
                    .find(|agent| agent.agent_id.as_str() == id)
            })
            .map_or("Primary", |agent| agent.task_name.as_str());
        return vec![Line::from(Span::styled(
            fit(
                &format!("▾ {current} · {} agents · ↓ switch", snapshot.agents.len()),
                width,
            ),
            Style::default().fg(TEXT_DIM).add_modifier(Modifier::BOLD),
        ))];
    }

    let mut lines = Vec::with_capacity(snapshot.agents.len() + 1);
    lines.push(switcher_line(
        width,
        cursor == 0,
        selected_agent_id.is_none(),
        "Primary",
        "main conversation",
    ));
    for (index, agent) in snapshot.hierarchy().iter().enumerate() {
        let pending_messages = agent
            .messages
            .iter()
            .filter(|message| {
                message.recipient_agent_id == agent.agent_id
                    && !message.consumed
                    && message.state == "queued"
            })
            .count();
        let mailbox = if pending_messages > 0 {
            format!(" · {pending_messages} message queued")
        } else {
            String::new()
        };
        let label = format!("{}{}", "  ".repeat(agent.ancestry.len()), agent.task_name);
        let detail = format!("{}{mailbox}", agent.lifecycle.replace('_', " "));
        lines.push(switcher_line(
            width,
            cursor == index + 1,
            selected_agent_id == Some(agent.agent_id.as_str()),
            &label,
            &detail,
        ));
    }
    lines
}

fn switcher_line(
    width: usize,
    highlighted: bool,
    active: bool,
    label: &str,
    detail: &str,
) -> Line<'static> {
    let marker = if highlighted {
        "▸"
    } else if active {
        "●"
    } else {
        " "
    };
    let text = fit(&format!("{marker} {label} · {detail}"), width);
    let mut style = Style::default().fg(if active {
        Color::Rgb(130, 194, 255)
    } else {
        TEXT_NORMAL
    });
    if highlighted {
        style = style
            .bg(Color::Rgb(48, 56, 74))
            .add_modifier(Modifier::BOLD);
    }
    Line::from(Span::styled(text, style))
}

pub(crate) fn project_agent_approval_prompt(
    snapshot: &AgentControlPlaneSnapshot,
) -> Option<AgentApprovalPrompt> {
    let (agent, approval) = snapshot.oldest_pending_approval()?;
    let ancestry = agent
        .ancestry
        .iter()
        .map(|ancestor| {
            snapshot
                .agents
                .iter()
                .find(|candidate| candidate.agent_id == *ancestor)
                .map(|candidate| candidate.task_name.clone())
                .unwrap_or_else(|| {
                    if *ancestor == snapshot.root_agent_id {
                        "root".into()
                    } else {
                        short_id(ancestor.as_str())
                    }
                })
        })
        .collect::<Vec<_>>()
        .join(" › ");
    Some(AgentApprovalPrompt {
        summary: format!(
            "Child: {} ({})\nAncestry: {} › {} · role {}\nRoute: requested {}/{} → effective {}/{}{}\nTool: {} · effect {}\nWorkspace: {} · {} · {}\nReason: {}",
            agent.task_name,
            short_id(agent.agent_id.as_str()),
            if ancestry.is_empty() { "root" } else { &ancestry },
            agent.task_name,
            agent.role,
            agent.requested_route.catalog_model_id,
            agent.requested_route.reasoning_effort,
            agent.resolved_route.catalog_model_id,
            agent.resolved_route.reasoning_effort,
            agent
                .resolved_route
                .fallback_reason
                .as_deref()
                .map(|reason| format!(" · fallback: {reason}"))
                .unwrap_or_default(),
            approval.tool_name,
            approval.effect,
            workspace_label(&agent.workspace.strategy),
            agent.workspace.ownership,
            agent.workspace.root.as_deref().unwrap_or("root not reported"),
            approval.reason
        ),
    })
}

fn workspace_label(strategy: &str) -> &str {
    match strategy {
        "read_only_snapshot" => "snapshot",
        "isolated_worktree" => "worktree",
        "shared_workspace" => "shared",
        other => other,
    }
}

fn short_id(value: &str) -> String {
    value.chars().take(10).collect()
}

fn fit(text: &str, width: usize) -> String {
    if width == 0 {
        return String::new();
    }
    if UnicodeWidthStr::width(text) <= width {
        return text.to_string();
    }
    if width == 1 {
        return "…".into();
    }
    let mut result = String::new();
    let budget = width - 1;
    for grapheme in text.graphemes(true) {
        if UnicodeWidthStr::width(result.as_str()) + UnicodeWidthStr::width(grapheme) > budget {
            break;
        }
        result.push_str(grapheme);
    }
    result.push('…');
    result
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::{json, Value};

    fn agent(id: &str, task: &str, lifecycle: &str) -> Value {
        json!({
            "agent_id": id,
            "parent_agent_id": "agt_root",
            "ancestry": ["agt_root"],
            "children": [],
            "task_name": task,
            "role": "reviewer",
            "objective": format!("Review {task}"),
            "requested_route": {
                "catalog_model_id": "codex_sol/gpt-5.6-sol",
                "reasoning_effort": "high",
                "fallback_policy": "fail_closed",
                "fallback_catalog_model_id": null,
                "fallback_reasoning_effort": null
            },
            "resolved_route": {
                "catalog_generation": "catalog-1",
                "catalog_model_id": "codex_sol/gpt-5.6-sol",
                "profile_name": "codex_sol",
                "provider": "codex",
                "model": "gpt-5.6-sol",
                "reasoning_effort": "high",
                "resolution": "exact",
                "fallback_reason": null
            },
            "lifecycle": lifecycle,
            "turn_generation": 0,
            "turn_id": null,
            "elapsed_millis": {"status": "not_reported"},
            "progress": {"status": "not_reported"},
            "usage": {"status": "not_reported"},
            "workspace": {
                "workspace_id": format!("wsp_{id}"),
                "strategy": "read_only_snapshot",
                "manifest_id": "mft_1",
                "ownership": "ovim",
                "root": null,
                "read_only": true
            },
            "messages": [],
            "approvals": [],
            "handoff": null,
            "artifact_handles": [],
            "attention": {
                "required": false,
                "pending_approvals": 0,
                "pending_messages": 0,
                "pending_notifications": 0
            },
            "recovery_status": "none",
            "trace": []
        })
    }

    fn snapshot(agents: Vec<Value>) -> AgentControlPlaneSnapshot {
        serde_json::from_value(json!({
            "schema_version": 2,
            "run_id": "run_1",
            "root_agent_id": "agt_root",
            "last_sequence": 20,
            "agents": agents,
            "pending_attention": 0,
            "pending_updates": 0
        }))
        .unwrap()
    }

    #[test]
    fn switcher_keeps_primary_first_and_separates_mailbox_from_lifecycle() {
        let mut live = agent("agt_live", "runtime review", "running");
        live["messages"] = json!([{
            "message_event_id": "evt_message_1",
            "sender_agent_id": "agt_root",
            "recipient_agent_id": "agt_live",
            "content": "include unstaged work",
            "state": "queued",
            "detail": null,
            "consumed": false
        }]);
        let snapshot = snapshot(vec![live]);
        let lines = agent_switcher_lines(&snapshot, 80, true, 1, Some("agt_live"));
        let text = lines
            .iter()
            .map(ToString::to_string)
            .collect::<Vec<_>>()
            .join("\n");

        assert!(text.lines().next().unwrap().contains("Primary"));
        assert!(text.contains("runtime review · running · 1 message queued"));
    }

    #[test]
    fn collapsed_switcher_names_the_active_conversation() {
        let snapshot = snapshot(vec![agent("agt_live", "runtime review", "running")]);
        let lines = agent_switcher_lines(&snapshot, 60, false, 0, Some("agt_live"));
        assert!(lines[0]
            .to_string()
            .contains("runtime review · 1 agents · ↓ switch"));
    }
}

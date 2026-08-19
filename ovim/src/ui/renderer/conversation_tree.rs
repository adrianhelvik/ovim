use crate::editor::Editor;
use ovim_core::ai::chat_types::{ChatFocus, ChatRole, ConversationTree, NodeId};
use ratatui::{
    layout::Rect,
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::Paragraph,
    Frame,
};

use super::ai_chat::{BG_PANEL, TEXT_DIM, TEXT_NORMAL};

const BORDER_COLOR: Color = Color::Rgb(60, 66, 80);
const ACTIVE_MARKER: &str = "●";
const INACTIVE_MARKER: &str = "○";

struct FlatNode {
    _node_id: NodeId,
    depth: usize,
    is_last_child: bool,
    is_on_active_branch: bool,
    role: ChatRole,
    preview: String,
}

/// Render the tree panel sidebar.
pub fn render_tree_panel(frame: &mut Frame, editor: &Editor, area: Rect) {
    if area.width < 8 || area.height < 3 {
        return;
    }

    let conv = match editor.conversation() {
        Some(c) => c,
        None => return,
    };
    let root_id = match conv.root_id() {
        Some(id) => id,
        None => return,
    };

    let focus = editor.ai_chat_focus();
    let is_focused = focus == ChatFocus::TreePanel;
    let cursor = editor.ai_chat_tree_panel_cursor();

    // Build set of active branch node IDs for highlighting
    let active_ids: std::collections::HashSet<NodeId> =
        conv.node_ids_for_active_branch().iter().copied().collect();

    // DFS flatten the tree
    let flat = dfs_flatten(conv, root_id, &active_ids);

    let w = area.width as usize;
    let visible_rows = area.height.saturating_sub(1) as usize; // 1 row for header

    // Header
    let header_style = Style::default()
        .fg(if is_focused { Color::White } else { TEXT_DIM })
        .bg(BG_PANEL)
        .add_modifier(Modifier::BOLD);
    let header_text = format!(" Branches{}", " ".repeat(w.saturating_sub(10)));
    let header_line = Line::from(Span::styled(
        header_text.chars().take(w).collect::<String>(),
        header_style,
    ));
    frame.render_widget(
        Paragraph::new(vec![header_line]),
        Rect {
            x: area.x,
            y: area.y,
            width: area.width,
            height: 1,
        },
    );

    // Separator
    let sep_style = Style::default().fg(BORDER_COLOR).bg(BG_PANEL);

    // Scroll to keep cursor visible
    let scroll_offset = if cursor >= visible_rows {
        cursor - visible_rows + 1
    } else {
        0
    };

    for (row_idx, flat_idx) in (scroll_offset..flat.len()).enumerate() {
        if row_idx >= visible_rows {
            break;
        }

        let node = &flat[flat_idx];
        let is_cursor = is_focused && flat_idx == cursor;

        let line = render_tree_line(node, w, is_cursor);

        let y = area.y + 1 + row_idx as u16;
        if y < area.y + area.height {
            frame.render_widget(
                Paragraph::new(vec![line]),
                Rect {
                    x: area.x,
                    y,
                    width: area.width,
                    height: 1,
                },
            );
        }
    }

    // Right border separator
    if area.width > 0 {
        for row in 0..area.height {
            let sep_line = Line::from(Span::styled("│", sep_style));
            frame.render_widget(
                Paragraph::new(vec![sep_line]),
                Rect {
                    x: area.x + area.width - 1,
                    y: area.y + row,
                    width: 1,
                    height: 1,
                },
            );
        }
    }
}

fn dfs_flatten(
    conv: &ConversationTree,
    node_id: NodeId,
    active_ids: &std::collections::HashSet<NodeId>,
) -> Vec<FlatNode> {
    let mut result = Vec::new();
    dfs_visit(conv, node_id, 0, true, active_ids, &mut result);
    result
}

fn dfs_visit(
    conv: &ConversationTree,
    node_id: NodeId,
    depth: usize,
    is_last: bool,
    active_ids: &std::collections::HashSet<NodeId>,
    out: &mut Vec<FlatNode>,
) {
    let node = match conv.node(node_id) {
        Some(n) => n,
        None => return,
    };

    let preview: String = node
        .message
        .content
        .chars()
        .take(25)
        .collect::<String>()
        .replace('\n', " ");

    out.push(FlatNode {
        _node_id: node_id,
        depth,
        is_last_child: is_last,
        is_on_active_branch: active_ids.contains(&node_id),
        role: node.message.role.clone(),
        preview,
    });

    let children = &node.children;
    for (i, &child_id) in children.iter().enumerate() {
        let is_last_child = i == children.len() - 1;
        dfs_visit(conv, child_id, depth + 1, is_last_child, active_ids, out);
    }
}

fn render_tree_line(node: &FlatNode, width: usize, is_cursor: bool) -> Line<'static> {
    let usable = width.saturating_sub(1); // 1 for right border

    // Build indent + connector
    let connector = if node.depth == 0 {
        String::new()
    } else {
        let indent = "│ ".repeat(node.depth.saturating_sub(1));
        let branch = if node.is_last_child {
            "└─"
        } else {
            "├─"
        };
        format!("{}{}", indent, branch)
    };

    // Active/inactive marker
    let marker = if node.is_on_active_branch {
        ACTIVE_MARKER
    } else {
        INACTIVE_MARKER
    };

    // Role prefix
    let role_str = match node.role {
        ChatRole::User => "You",
        ChatRole::Assistant => "AI",
        ChatRole::Thinking => "…",
        ChatRole::Error => "Err",
        ChatRole::Tool => "Tool",
    };

    let prefix = format!("{} {} {}: ", connector, marker, role_str);
    let prefix_width = unicode_width::UnicodeWidthStr::width(prefix.as_str());
    let preview_space = usable.saturating_sub(prefix_width);
    let truncated_preview =
        crate::ui::renderer::helpers::truncate_to_width(&node.preview, preview_space);
    let total_width =
        prefix_width + unicode_width::UnicodeWidthStr::width(truncated_preview.as_str());
    let padding = usable.saturating_sub(total_width);

    let role_color = match node.role {
        ChatRole::User => Color::Cyan,
        ChatRole::Assistant => Color::Green,
        ChatRole::Thinking => Color::Rgb(80, 88, 100),
        ChatRole::Error => Color::Red,
        ChatRole::Tool => Color::Rgb(100, 149, 237),
    };

    let bg = if is_cursor {
        Color::Rgb(40, 50, 65)
    } else {
        BG_PANEL
    };

    let connector_style = Style::default().fg(TEXT_DIM).bg(bg);
    let marker_style = Style::default()
        .fg(if node.is_on_active_branch {
            Color::Yellow
        } else {
            TEXT_DIM
        })
        .bg(bg);
    let role_style = Style::default()
        .fg(role_color)
        .bg(bg)
        .add_modifier(Modifier::BOLD);
    let preview_style = Style::default().fg(TEXT_NORMAL).bg(bg);
    let pad_style = Style::default().bg(bg);

    let connector_str = if node.depth == 0 {
        String::new()
    } else {
        let indent = "│ ".repeat(node.depth.saturating_sub(1));
        let branch = if node.is_last_child {
            "└─"
        } else {
            "├─"
        };
        format!("{}{} ", indent, branch)
    };

    Line::from(vec![
        Span::styled(connector_str, connector_style),
        Span::styled(format!("{} ", marker), marker_style),
        Span::styled(format!("{}: ", role_str), role_style),
        Span::styled(truncated_preview, preview_style),
        Span::styled(" ".repeat(padding), pad_style),
    ])
}

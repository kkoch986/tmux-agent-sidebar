use ratatui::{
    style::{Modifier, Style},
    text::{Line, Span},
};

use super::ctx::RowCtx;
use crate::activity::ActivityEntry;
use crate::tmux::{AgentType, PaneInfo, PaneStatus};
use crate::ui::text::{display_width, truncate_to_width, wait_reason_label};

pub(super) fn task_progress_row(
    task_progress: Option<&crate::activity::TaskProgress>,
    ctx: &RowCtx,
) -> Option<Line<'static>> {
    use crate::activity::TaskStatus;
    let progress = task_progress?;
    if progress.is_empty() {
        return None;
    }

    let mut icons = String::with_capacity(progress.tasks.len() * 3);
    for (_, status) in &progress.tasks {
        let ch = match status {
            TaskStatus::Completed => "✔",
            TaskStatus::InProgress => "◼",
            TaskStatus::Pending => "◻",
        };
        icons.push_str(ch);
    }
    let summary = format!(
        "  {} {}/{}",
        icons,
        progress.completed_count(),
        progress.total()
    );
    let summary = truncate_to_width(&summary, ctx.inner_width);
    let summary_dw = display_width(&summary);
    let task_color = ctx.theme.task_progress;
    Some(ctx.row_line(
        vec![Span::styled(
            summary,
            ctx.apply_bg(Style::default().fg(task_color)),
        )],
        summary_dw,
    ))
}

/// One line saying what the pane is doing now, or what it needs from the
/// user. `None` for plain shells, and for idle agents with nothing to say.
pub(super) fn detail_row(
    pane: &PaneInfo,
    last_activity: Option<&ActivityEntry>,
    ctx: &RowCtx,
) -> Option<Line<'static>> {
    let theme = ctx.theme;
    let attention = Style::default()
        .fg(theme.status_color(&pane.status, true))
        .add_modifier(Modifier::BOLD);

    let (text, style) = if pane.is_unhooked_claude() {
        (
            "no status reported yet".to_string(),
            Style::default().fg(theme.text_inactive),
        )
    } else if pane.agent == AgentType::None {
        return None;
    } else {
        match pane.status {
            PaneStatus::Waiting => (
                format!("▶ {}", waiting_reason(&pane.wait_reason)),
                attention,
            ),
            PaneStatus::Error => {
                let mut reason = wait_reason_label(&pane.wait_reason);
                if reason.is_empty() {
                    reason = response_summary(&pane.prompt);
                }
                if reason.is_empty() {
                    reason = "stopped with an error".to_string();
                }
                (format!("✕ {reason}"), attention)
            }
            _ if pane.attention => {
                let said = response_summary(&pane.prompt);
                let text = if said.is_empty() {
                    "▶ finished".to_string()
                } else {
                    format!("▶ {said}")
                };
                (text, attention)
            }
            PaneStatus::Running => (
                running_detail(pane, last_activity),
                Style::default().fg(theme.text_muted),
            ),
            PaneStatus::Background => (
                background_detail(pane)?,
                Style::default().fg(theme.status_running),
            ),
            PaneStatus::Idle | PaneStatus::Unknown => {
                let said = response_summary(&pane.prompt);
                if said.is_empty() {
                    return None;
                }
                (said, Style::default().fg(theme.text_inactive))
            }
        }
    };

    let text = truncate_to_width(&format!("  {text}"), ctx.inner_width);
    let width = display_width(&text);
    Some(ctx.row_line(vec![Span::styled(text, ctx.apply_bg(style))], width))
}

/// Subagents on one line (`↳ 2 subagents: Explore, Plan`) rather than a
/// line each, so a busy pane doesn't push the rest of the list down.
pub(super) fn subagent_row(subagents: &[String], ctx: &RowCtx) -> Option<Line<'static>> {
    if subagents.is_empty() {
        return None;
    }
    let names: Vec<&str> = subagents
        .iter()
        .map(|s| s.split(" #").next().unwrap_or(s))
        .collect();
    // Same-type subagents usually run in batches; count them, in order.
    let mut kinds: Vec<(&str, usize)> = Vec::new();
    for name in &names {
        match kinds.iter_mut().find(|(k, _)| k == name) {
            Some((_, n)) => *n += 1,
            None => kinds.push((name, 1)),
        }
    }
    let kinds = kinds
        .iter()
        .map(|(k, n)| {
            if *n > 1 {
                format!("{k} ×{n}")
            } else {
                k.to_string()
            }
        })
        .collect::<Vec<_>>()
        .join(", ");
    let label = if names.len() == 1 {
        format!("  ↳ subagent: {kinds}")
    } else {
        format!("  ↳ {} subagents: {kinds}", names.len())
    };
    let text = truncate_to_width(&label, ctx.inner_width);
    let width = display_width(&text);
    Some(ctx.row_line(
        vec![Span::styled(
            text,
            ctx.apply_bg(Style::default().fg(ctx.theme.subagent)),
        )],
        width,
    ))
}

fn waiting_reason(wait_reason: &str) -> String {
    match wait_reason {
        "" => "waiting for you".to_string(),
        "permission_prompt" => "needs your permission".to_string(),
        "elicitation_dialog" => "asking you a question".to_string(),
        "idle_prompt" => "waiting for your reply".to_string(),
        other => wait_reason_label(other),
    }
}

fn running_detail(pane: &PaneInfo, last_activity: Option<&ActivityEntry>) -> String {
    if let Some(entry) = last_activity {
        let tool = short_tool_name(&entry.tool);
        let label = entry.label.trim();
        return if label.is_empty() {
            tool
        } else {
            format!("{tool} {label}")
        };
    }
    if !pane.prompt.is_empty() && !pane.prompt_is_response {
        return format!("› {}", pane.prompt);
    }
    "working…".to_string()
}

/// `None` when subagents are the background work: their own row says so.
fn background_detail(pane: &PaneInfo) -> Option<String> {
    if let Some(cmd) = pane.bg_shell_cmd.as_deref() {
        return Some(format!("$ {}", cmd.trim()));
    }
    pane.subagents
        .is_empty()
        .then(|| "running in the background".to_string())
}

/// `mcp__linear__save_issue` → `linear save_issue`.
fn short_tool_name(tool: &str) -> String {
    match tool.strip_prefix("mcp__") {
        Some(rest) => rest.replacen("__", " ", 1),
        None => tool.to_string(),
    }
}

/// The opening sentence of an agent's last reply, without markdown, so it
/// reads as a one-line summary.
pub(super) fn response_summary(text: &str) -> String {
    let plain: String = text.chars().filter(|c| !matches!(c, '*' | '`')).collect();
    let plain = plain.split_whitespace().collect::<Vec<_>>().join(" ");
    let plain = plain
        .trim_start_matches(['#', ' '])
        .trim_start_matches("- ")
        .to_string();
    const MIN_SENTENCE: usize = 12;
    for (i, _) in plain.match_indices(['.', '?', '!', ':']) {
        let after = plain[i + 1..].chars().next();
        if i >= MIN_SENTENCE && after.is_none_or(char::is_whitespace) {
            return plain[..=i].trim_end_matches(':').to_string();
        }
    }
    plain
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn response_summary_keeps_first_sentence_without_markdown() {
        assert_eq!(
            response_summary("Both code fixes are up as PRs. Neither is merged.  - **typo**"),
            "Both code fixes are up as PRs."
        );
        assert_eq!(
            response_summary("PR #26794 is merged (`d3c94703f`), and the build is running."),
            "PR #26794 is merged (d3c94703f), and the build is running."
        );
        assert_eq!(response_summary("Done"), "Done");
        assert_eq!(response_summary(""), "");
    }

    #[test]
    fn short_tool_name_compacts_mcp_tools() {
        assert_eq!(
            short_tool_name("mcp__linear__save_issue"),
            "linear save_issue"
        );
        assert_eq!(short_tool_name("Bash"), "Bash");
    }
}

use ratatui::{
    style::{Color, Modifier, Style},
    text::{Line, Span},
};

use super::ctx::RowCtx;
use crate::tmux::{AgentType, PaneInfo, PaneStatus, PermissionMode};
use crate::ui::colors::ON_ATTENTION;
use crate::ui::icons::StatusIcons;
use crate::ui::text::{display_width, elapsed_label, truncate_to_width};

/// First line of a pane: status icon and a short summary on the left; the
/// Claude account, permission badge and run time on the right. A pane that
/// needs the user is drawn as a solid bar with a `NEEDS YOU` / `YOUR TURN`
/// label instead of the run time.
pub(super) fn status_row(
    pane: &PaneInfo,
    ctx: &RowCtx,
    icons: &StatusIcons,
    spinner_frame: usize,
    now: u64,
) -> Line<'static> {
    let theme = ctx.theme;
    let needs_you = pane.needs_attention();
    let attention_bg = theme.status_color(&pane.status, true);
    let bar_ctx = RowCtx {
        marker_char: ctx.marker_char,
        marker_style: ctx.marker_style,
        inner_width: ctx.inner_width,
        theme,
        bg: Some(attention_bg),
        active: ctx.active,
    };
    let ctx = if needs_you { &bar_ctx } else { ctx };

    let (icon, pulse_color) = running_icon_for(&pane.status, spinner_frame, icons);
    let icon_style = if needs_you {
        Style::default().fg(ON_ATTENTION)
    } else {
        Style::default().fg(pulse_color.unwrap_or_else(|| theme.status_color(&pane.status, false)))
    };
    let summary_style = if needs_you {
        Style::default()
            .fg(ON_ATTENTION)
            .add_modifier(Modifier::BOLD)
    } else if pane.agent == AgentType::None && pane.claude.is_none() {
        Style::default().fg(theme.text_inactive)
    } else {
        Style::default().fg(theme.text_active)
    };

    let mut right: Vec<(String, Style)> = Vec::new();
    let badge = permission_badge(&pane.permission_mode);
    if !badge.is_empty() {
        let fg = if needs_you {
            ON_ATTENTION
        } else {
            badge_color(&pane.permission_mode, ctx)
        };
        right.push((badge.to_string(), Style::default().fg(fg)));
    }
    let account = pane.account();
    if !account.is_empty() {
        let fg = if needs_you {
            ON_ATTENTION
        } else {
            theme.agent_claude
        };
        right.push((account.to_string(), Style::default().fg(fg)));
    }
    if needs_you {
        right.push((
            attention_label(pane).to_string(),
            Style::default()
                .fg(ON_ATTENTION)
                .add_modifier(Modifier::BOLD),
        ));
    } else {
        let elapsed = elapsed_label(pane.started_at, now);
        if !elapsed.is_empty() {
            let fg = if pane.status.is_active() {
                theme.text_active
            } else {
                theme.text_muted
            };
            right.push((elapsed, Style::default().fg(fg)));
        }
    }

    // The right-hand labels are short and carry the state, so they keep
    // their room and the summary truncates instead.
    let mut right_spans: Vec<Span<'static>> = Vec::new();
    let mut right_width = 0;
    for (text, style) in right {
        let gap = usize::from(right_width > 0);
        right_width += gap + display_width(&text);
        if gap > 0 {
            right_spans.push(Span::styled(" ", ctx.apply_bg(Style::default())));
        }
        right_spans.push(Span::styled(text, ctx.apply_bg(style)));
    }
    let icon_width = display_width(icon) + 1;
    let right_width = if icon_width + right_width + 1 > ctx.inner_width {
        right_spans.clear();
        0
    } else {
        right_width
    };
    let summary = truncate_to_width(
        &pane_summary(pane),
        ctx.inner_width.saturating_sub(icon_width + right_width + 1),
    );

    let left_width = icon_width + display_width(&summary);
    let left_spans = vec![
        Span::styled(icon.to_string(), ctx.apply_bg(icon_style)),
        Span::styled(" ", ctx.apply_bg(Style::default())),
        Span::styled(summary, ctx.apply_bg(summary_style)),
    ];
    ctx.row_line_split(left_spans, left_width, right_spans, right_width)
}

/// Short name for what the pane is: the session name a user set with
/// `/rename`, else the title Claude Code keeps current, else the program.
pub(super) fn pane_summary(pane: &PaneInfo) -> String {
    if pane.agent == AgentType::None && pane.claude.is_none() {
        return if pane.current_command.is_empty() {
            "shell".to_string()
        } else {
            pane.current_command.clone()
        };
    }
    if !pane.session_name.is_empty() {
        return pane.session_name.clone();
    }
    if !pane.title.is_empty() {
        return pane.title.clone();
    }
    match pane.agent {
        AgentType::None => crate::tmux::CLAUDE_AGENT.to_string(),
        ref agent => agent.label().to_string(),
    }
}

fn attention_label(pane: &PaneInfo) -> &'static str {
    match pane.status {
        PaneStatus::Error => "FAILED",
        PaneStatus::Waiting => "NEEDS YOU",
        _ => "YOUR TURN",
    }
}

/// Bypass is how most sessions run, so it would only add noise; the
/// other modes change how the agent behaves and stay visible.
fn permission_badge(mode: &PermissionMode) -> &str {
    match mode {
        PermissionMode::BypassPermissions => "",
        other => other.badge(),
    }
}

fn badge_color(mode: &PermissionMode, ctx: &RowCtx) -> Color {
    let theme = ctx.theme;
    match mode {
        PermissionMode::BypassPermissions => theme.badge_danger,
        PermissionMode::Plan => theme.badge_plan,
        PermissionMode::Auto
        | PermissionMode::DontAsk
        | PermissionMode::AcceptEdits
        | PermissionMode::Defer => theme.badge_auto,
        PermissionMode::Default => theme.text_muted,
    }
}

pub(super) fn running_icon_for<'a>(
    status: &PaneStatus,
    spinner_frame: usize,
    icons: &'a StatusIcons,
) -> (&'a str, Option<Color>) {
    use crate::SPINNER_PULSE;

    match status {
        PaneStatus::Running => {
            let color_idx = SPINNER_PULSE[spinner_frame % SPINNER_PULSE.len()];
            (icons.status_icon(status), Some(Color::Indexed(color_idx)))
        }
        _ => (icons.status_icon(status), None),
    }
}

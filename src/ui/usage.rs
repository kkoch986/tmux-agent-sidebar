//! The Usage section: one row per Claude account with a session open,
//! showing how much of its 5-hour and weekly limit is spent. A window close
//! to its limit adds a line saying when it resets. Collapses to zero height
//! when no account has been looked up yet.

use ratatui::{
    Frame,
    layout::Rect,
    style::{Color, Style},
    text::{Line, Span},
    widgets::Paragraph,
};

use crate::state::AppState;
use crate::ui::colors::ColorTheme;
use crate::ui::text::{display_width, truncate_to_width};
use crate::usage::{AccountUsage, UsageWindow};

const CHROME_HEIGHT: u16 = 2;
/// From this share of a limit on, the bar turns amber and the reset time shows.
const WARN_PCT: f64 = 70.0;
const DANGER_PCT: f64 = 90.0;

pub fn usage_panel_height(state: &AppState, available: u16) -> u16 {
    if state.usage.is_empty() {
        return 0;
    }
    let rows: usize = state
        .usage
        .iter()
        .map(|u| 1 + reset_notes(u, state.now).len())
        .sum();
    (rows as u16 + CHROME_HEIGHT).min(available.saturating_sub(3))
}

pub fn draw_usage(frame: &mut Frame, state: &AppState, area: Rect) {
    if area.height < CHROME_HEIGHT + 1 || area.width < 10 {
        return;
    }
    let theme = &state.theme;
    let border = Style::default().fg(theme.border_inactive);
    let width = area.width as usize;
    let inner = width.saturating_sub(2);

    let title = "Usage";
    let fill = width.saturating_sub(display_width(title) + 4);
    let mut lines = vec![Line::from(vec![
        Span::styled("╭ ", border),
        Span::styled(title, Style::default().fg(theme.section_title)),
        Span::styled(format!(" {}╮", "─".repeat(fill)), border),
    ])];

    let label_width = state
        .usage
        .iter()
        .map(|u| display_width(&u.account))
        .max()
        .unwrap_or(0)
        .min(8);
    let capacity = area.height.saturating_sub(CHROME_HEIGHT) as usize;
    let mut body: Vec<Line<'static>> = Vec::new();
    for usage in &state.usage {
        body.push(account_line(usage, label_width, inner, theme));
        for note in reset_notes(usage, state.now) {
            let text = truncate_to_width(&format!(" {:label_width$} {note}", ""), inner);
            body.push(Line::from(Span::styled(
                text,
                Style::default().fg(theme.status_waiting),
            )));
        }
    }
    for line in body.into_iter().take(capacity) {
        let used: usize = line.spans.iter().map(|s| display_width(&s.content)).sum();
        let mut spans = vec![Span::styled("│", border)];
        spans.extend(line.spans);
        spans.push(Span::raw(" ".repeat(inner.saturating_sub(used))));
        spans.push(Span::styled("│", border));
        lines.push(Line::from(spans));
    }
    lines.push(Line::from(Span::styled(
        format!("╰{}╯", "─".repeat(inner)),
        border,
    )));
    frame.render_widget(Paragraph::new(lines), area);
}

/// ` ken  5h ███░░░░░  12%  wk ████░░░░  38%` — bars share whatever width
/// the label and percentages leave.
fn account_line(
    usage: &AccountUsage,
    label_width: usize,
    inner: usize,
    theme: &ColorTheme,
) -> Line<'static> {
    let label = truncate_to_width(&usage.account, label_width);
    let mut spans = vec![Span::styled(
        format!(" {label:label_width$} "),
        Style::default().fg(theme.agent_claude),
    )];
    if let Some(reason) = &usage.error {
        let room = inner.saturating_sub(label_width + 2);
        spans.push(Span::styled(
            truncate_to_width(reason, room),
            Style::default().fg(theme.text_inactive),
        ));
        return Line::from(spans);
    }
    // Each window is "5h " + bar + " " + "100%"; two windows, a 2-column gap.
    const WINDOW_CHROME: usize = 8;
    let bar = inner
        .saturating_sub(label_width + 2 + 2 + 2 * WINDOW_CHROME)
        .saturating_div(2)
        .clamp(2, 20);
    spans.extend(window_spans("5h", usage.five_hour.as_ref(), bar, theme));
    spans.push(Span::raw("  "));
    spans.extend(window_spans("wk", usage.seven_day.as_ref(), bar, theme));
    Line::from(spans)
}

fn window_spans(
    name: &str,
    window: Option<&UsageWindow>,
    bar: usize,
    theme: &ColorTheme,
) -> Vec<Span<'static>> {
    let muted = Style::default().fg(theme.text_inactive);
    let Some(window) = window else {
        return vec![
            Span::styled(format!("{name} "), muted),
            Span::styled(format!("{:bar$}    -", ""), muted),
        ];
    };
    let pct = window.used_pct.clamp(0.0, 100.0);
    let filled = ((pct / 100.0) * bar as f64).round() as usize;
    let color = level_color(pct, theme);
    vec![
        Span::styled(format!("{name} "), muted),
        Span::styled("█".repeat(filled), Style::default().fg(color)),
        Span::styled(
            "░".repeat(bar - filled.min(bar)),
            Style::default().fg(theme.border_inactive),
        ),
        Span::styled(
            format!(" {:>3}%", pct.round() as u32),
            Style::default().fg(color),
        ),
    ]
}

fn level_color(pct: f64, theme: &ColorTheme) -> Color {
    if pct >= DANGER_PCT {
        theme.status_error
    } else if pct >= WARN_PCT {
        theme.status_waiting
    } else {
        theme.status_running
    }
}

/// `5h resets in 1h12m` for each window at or past [`WARN_PCT`].
fn reset_notes(usage: &AccountUsage, now: u64) -> Vec<String> {
    [("5h", &usage.five_hour), ("wk", &usage.seven_day)]
        .into_iter()
        .filter_map(|(name, window)| {
            let window = window.as_ref()?;
            if window.used_pct < WARN_PCT {
                return None;
            }
            let at = window.resets_at?;
            Some(format!(
                "{name} resets in {}",
                duration_label(at.saturating_sub(now))
            ))
        })
        .collect()
}

fn duration_label(secs: u64) -> String {
    let (d, h, m) = (secs / 86_400, secs % 86_400 / 3600, secs % 3600 / 60);
    if d > 0 {
        format!("{d}d{h}h")
    } else if h > 0 {
        format!("{h}h{m:02}m")
    } else {
        format!("{m}m")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn usage(account: &str, five: f64, week: f64) -> AccountUsage {
        AccountUsage {
            account: account.into(),
            five_hour: Some(UsageWindow {
                used_pct: five,
                resets_at: Some(1000 + 4320),
            }),
            seven_day: Some(UsageWindow {
                used_pct: week,
                resets_at: Some(1000 + 2 * 86_400 + 3600),
            }),
            error: None,
        }
    }

    fn render(state: &mut AppState, width: u16) -> String {
        let height = usage_panel_height(state, 40);
        let backend = ratatui::backend::TestBackend::new(width, height);
        let mut terminal = ratatui::Terminal::new(backend).unwrap();
        terminal.draw(|f| draw_usage(f, state, f.area())).unwrap();
        let buf = terminal.backend().buffer().clone();
        (0..buf.area.height)
            .map(|y| {
                (0..buf.area.width)
                    .map(|x| buf[(x, y)].symbol().to_string())
                    .collect::<String>()
            })
            .collect::<Vec<_>>()
            .join("\n")
    }

    #[test]
    fn snapshot_usage_rows_with_reset_note_and_error() {
        let mut state = AppState::new("%0".into());
        state.now = 1000;
        state.usage = vec![
            usage("ken", 12.0, 38.0),
            usage("k1", 81.0, 64.0),
            AccountUsage {
                account: "k0".into(),
                five_hour: None,
                seven_day: None,
                error: Some("token expired".into()),
            },
        ];
        insta::assert_snapshot!(render(&mut state, 44), @"
        ╭ Usage ───────────────────────────────────╮
        │ ken 5h █░░░░░░░░  12%  wk ███░░░░░░  38% │
        │ k1  5h ███████░░  81%  wk ██████░░░  64% │
        │     5h resets in 1h12m                   │
        │ k0  token expired                        │
        ╰──────────────────────────────────────────╯
        ");
    }

    #[test]
    fn usage_panel_collapses_when_empty() {
        let state = AppState::new("%0".into());
        assert_eq!(usage_panel_height(&state, 40), 0);
    }
}

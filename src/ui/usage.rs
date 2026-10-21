//! The Usage section: one row per Claude account on the machine, showing
//! how much of its 5-hour and weekly limit is spent. A maxed-out account is
//! drawn in red with a line per maxed limit, and each window shows how long
//! until it resets. Collapses to zero height until the first look-up lands.

use ratatui::{
    Frame,
    layout::Rect,
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::Paragraph,
};

use crate::state::AppState;
use crate::ui::colors::ColorTheme;
use crate::ui::colors::ON_ATTENTION;
use crate::ui::text::{display_width, truncate_to_width};
use crate::usage::{AccountUsage, UsageWindow};

const CHROME_HEIGHT: u16 = 2;
/// From this share of a limit on, the bar turns amber.
const WARN_PCT: f64 = 70.0;
const DANGER_PCT: f64 = 90.0;

pub fn usage_panel_height(state: &AppState, available: u16) -> u16 {
    if state.usage.is_empty() {
        return 0;
    }
    let rows: usize = state
        .usage
        .iter()
        .map(|u| 1 + notes(u, state.now).len())
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

    let maxed = state.usage.iter().filter(|u| u.is_maxed()).count();
    let mut title = vec![Span::styled(
        "Usage",
        Style::default().fg(theme.section_title),
    )];
    if maxed > 0 {
        title.push(Span::styled(
            format!(" · {maxed} maxed"),
            Style::default()
                .fg(theme.status_error)
                .add_modifier(Modifier::BOLD),
        ));
    }
    let title_width: usize = title.iter().map(|s| display_width(&s.content)).sum();
    let fill = width.saturating_sub(title_width + 4);
    let mut header = vec![Span::styled("╭ ", border)];
    header.extend(title);
    header.push(Span::styled(format!(" {}╮", "─".repeat(fill)), border));
    let mut lines = vec![Line::from(header)];

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
        body.push(account_line(usage, label_width, inner, state.now, theme));
        for (note, kind) in notes(usage, state.now) {
            let text = truncate_to_width(&format!(" {:label_width$} {note}", ""), inner);
            let style = match kind {
                Note::Maxed => Style::default()
                    .fg(theme.status_error)
                    .add_modifier(Modifier::BOLD),
                Note::Stale => Style::default().fg(theme.status_waiting),
            };
            body.push(Line::from(Span::styled(text, style)));
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

/// ` ken 5h ███░░░  12% 2h10m  wk ████░░  38% 5d20h` — bars share whatever
/// width the label, percentages and time left leave.
fn account_line(
    usage: &AccountUsage,
    label_width: usize,
    inner: usize,
    now: u64,
    theme: &ColorTheme,
) -> Line<'static> {
    let label = truncate_to_width(&usage.account, label_width);
    // A maxed account's label is a solid red block, so it reads as
    // unusable at a glance.
    let label_style = if usage.is_maxed() {
        Style::default()
            .fg(ON_ATTENTION)
            .bg(theme.status_error)
            .add_modifier(Modifier::BOLD)
    } else {
        Style::default().fg(theme.agent_claude)
    };
    let mut spans = vec![
        Span::raw(" "),
        Span::styled(format!("{label:label_width$}"), label_style),
        Span::raw(" "),
    ];
    if let Some(reason) = &usage.error {
        let room = inner.saturating_sub(label_width + 2);
        spans.push(Span::styled(
            truncate_to_width(reason, room),
            Style::default().fg(theme.text_inactive),
        ));
        return Line::from(spans);
    }
    // Each window is "5h " + bar + " 100%" + " " + time left (up to 6
    // columns, e.g. `23h59m`); two windows, a 2-column gap.
    const WINDOW_CHROME: usize = 15;
    let bar = inner
        .saturating_sub(label_width + 2 + 2 + 2 * WINDOW_CHROME)
        .saturating_div(2)
        .max(2);
    spans.extend(window_spans(
        "5h",
        usage.five_hour.as_ref(),
        bar,
        now,
        theme,
    ));
    spans.push(Span::raw("  "));
    spans.extend(window_spans(
        "wk",
        usage.seven_day.as_ref(),
        bar,
        now,
        theme,
    ));
    Line::from(spans)
}

fn window_spans(
    name: &str,
    window: Option<&UsageWindow>,
    bar: usize,
    now: u64,
    theme: &ColorTheme,
) -> Vec<Span<'static>> {
    let muted = Style::default().fg(theme.text_inactive);
    let Some(window) = window else {
        return vec![
            Span::styled(format!("{name} "), muted),
            Span::styled(format!("{:bar$}    - {:6}", "", ""), muted),
        ];
    };
    let left = window
        .resets_at
        .map(|at| duration_label(at.saturating_sub(now)))
        .unwrap_or_default();
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
        Span::styled(format!(" {left:<6}"), Style::default().fg(theme.text_muted)),
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

enum Note {
    Maxed,
    Stale,
}

/// Lines under an account: one per maxed limit
/// (`✕ 5h maxed · resets in 2h10m`; per-model caps such as `Opus weekly`
/// have no bar, so this is the only place they show), then
/// `⚠ rate limited · as of 12m ago` when the latest check failed and the
/// row shows an older reading.
fn notes(usage: &AccountUsage, now: u64) -> Vec<(String, Note)> {
    let mut out: Vec<(String, Note)> = usage
        .maxed
        .iter()
        .map(|m| {
            let resets = m
                .resets_at
                .map(|at| format!(" · resets in {}", duration_label(at.saturating_sub(now))))
                .unwrap_or_default();
            (format!("✕ {} maxed{resets}", m.label), Note::Maxed)
        })
        .collect();
    if let Some(reason) = &usage.stale_reason {
        let age = usage
            .fetched_at
            .map(|at| format!(" · as of {} ago", duration_label(now.saturating_sub(at))))
            .unwrap_or_default();
        out.push((format!("⚠ {reason}{age}"), Note::Stale));
    }
    out
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
            maxed: Vec::new(),
            error: None,
            fetched_at: Some(1000),
            stale_reason: None,
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
            AccountUsage {
                fetched_at: Some(1000 - 720),
                stale_reason: Some(crate::usage::RATE_LIMITED.into()),
                ..usage("k1", 81.0, 64.0)
            },
            AccountUsage {
                account: "k0".into(),
                five_hour: None,
                seven_day: None,
                maxed: Vec::new(),
                error: Some("token expired".into()),
                fetched_at: None,
                stale_reason: None,
            },
            AccountUsage {
                maxed: vec![
                    crate::usage::MaxedLimit {
                        label: "5h".into(),
                        resets_at: Some(1000 + 7800),
                    },
                    crate::usage::MaxedLimit {
                        label: "Opus weekly".into(),
                        resets_at: Some(1000 + 3 * 86_400),
                    },
                ],
                ..usage("k2", 100.0, 55.0)
            },
        ];
        insta::assert_snapshot!(render(&mut state, 44), @"
        ╭ Usage · 1 maxed ─────────────────────────╮
        │ ken 5h ░░  12% 1h12m   wk █░  38% 2d1h   │
        │ k1  5h ██  81% 1h12m   wk █░  64% 2d1h   │
        │     ⚠ rate limited · as of 12m ago       │
        │ k0  token expired                        │
        │ k2  5h ██ 100% 1h12m   wk █░  55% 2d1h   │
        │     ✕ 5h maxed · resets in 2h10m         │
        │     ✕ Opus weekly maxed · resets in 3d0h │
        ╰──────────────────────────────────────────╯
        ");
    }

    #[test]
    fn snapshot_bars_grow_with_the_sidebar() {
        let mut state = AppState::new("%0".into());
        state.now = 1000;
        state.usage = vec![usage("ken", 50.0, 25.0)];
        insta::assert_snapshot!(render(&mut state, 84), @"
        ╭ Usage ───────────────────────────────────────────────────────────────────────────╮
        │ ken 5h ███████████░░░░░░░░░░░  50% 1h12m   wk ██████░░░░░░░░░░░░░░░░  25% 2d1h   │
        ╰──────────────────────────────────────────────────────────────────────────────────╯
        ");
    }

    #[test]
    fn usage_panel_collapses_when_empty() {
        let state = AppState::new("%0".into());
        assert_eq!(usage_panel_height(&state, 40), 0);
    }
}

//! The Deploys section: one stacked panel between the pane list and the
//! bottom panel, listing GitHub Actions deploy runs that are in flight or
//! finished recently. It collapses to zero height when there is nothing to
//! show, so the pane list keeps the full sidebar on a quiet day.

use ratatui::{
    Frame,
    layout::Rect,
    style::{Color, Style},
    text::{Line, Span},
    widgets::Paragraph,
};

use crate::deploy::{DeployRun, DeployStatus};
use crate::state::AppState;
use crate::ui::text::{display_width, truncate_to_width};

/// Border rows (top + bottom) wrapping the run rows.
const CHROME_HEIGHT: u16 = 2;

/// Height the section needs, or 0 when it should not render at all.
/// `available` is the height of the region the pane list and this section share,
/// and the pane list keeps at least 3 rows of it.
pub fn deploy_panel_height(state: &AppState, available: u16) -> u16 {
    if state.deploys.is_empty() {
        return 0;
    }
    let wanted = state.deploys.len() as u16 + CHROME_HEIGHT;
    let ceiling = available.saturating_sub(3);
    wanted.min(ceiling)
}

pub fn draw_deploys(frame: &mut Frame, state: &AppState, area: Rect) {
    if area.height < CHROME_HEIGHT + 1 || area.width < 4 {
        return;
    }
    let theme = &state.theme;
    let border = theme.border_inactive;
    let width = area.width as usize;

    let active_count = state
        .deploys
        .iter()
        .filter(|run| run.status.is_active())
        .count();
    let title = if active_count > 0 {
        format!("Deploys {active_count} live")
    } else {
        "Deploys".to_string()
    };
    let fill = width.saturating_sub(display_width(&title) + 4);
    frame.render_widget(
        Paragraph::new(Line::from(vec![
            Span::styled("╭ ", Style::default().fg(border)),
            Span::styled(title, Style::default().fg(theme.section_title)),
            Span::styled(
                format!(" {}╮", "─".repeat(fill)),
                Style::default().fg(border),
            ),
        ])),
        Rect::new(area.x, area.y, area.width, 1),
    );

    let row_capacity = area.height.saturating_sub(CHROME_HEIGHT) as usize;
    let inner_width = width.saturating_sub(2);
    let border_span = || Span::styled("│", Style::default().fg(border));
    let rows: Vec<Line<'_>> = state
        .deploys
        .iter()
        .take(row_capacity)
        .map(|run| {
            let mut spans = vec![border_span()];
            spans.extend(run_line(run, state, inner_width).spans);
            spans.push(border_span());
            Line::from(spans)
        })
        .collect();
    frame.render_widget(
        Paragraph::new(rows),
        Rect::new(
            area.x,
            area.y + 1,
            area.width,
            area.height.saturating_sub(CHROME_HEIGHT),
        ),
    );

    frame.render_widget(
        Paragraph::new(Line::from(Span::styled(
            format!("╰{}╯", "─".repeat(width.saturating_sub(2))),
            Style::default().fg(border),
        ))),
        Rect::new(area.x, area.y + area.height - 1, area.width, 1),
    );
}

/// `● listing v1.2.3          2m` — icon and repo on the left, age on the
/// right, the git ref taking whatever room is left in between.
fn run_line<'a>(run: &'a DeployRun, state: &AppState, width: usize) -> Line<'a> {
    let theme = &state.theme;
    let (icon, color) = match run.status {
        DeployStatus::Running => (
            crate::SPINNER_ICON,
            Color::Indexed(crate::SPINNER_PULSE[state.spinner_frame]),
        ),
        DeployStatus::Queued => ("◌", theme.status_waiting),
        DeployStatus::Success => ("✓", theme.diff_added),
        DeployStatus::Failure => ("✕", theme.status_error),
        DeployStatus::Cancelled => ("⊘", theme.text_inactive),
    };

    let age = format_age(run.elapsed_secs(state.now));
    let repo = run.repo.as_str();
    // icon + space + repo + space + … + space + age
    let ref_room =
        width.saturating_sub(display_width(icon) + display_width(repo) + display_width(&age) + 3);
    let git_ref = truncate_to_width(&run.git_ref, ref_room);
    let gap = width
        .saturating_sub(
            display_width(icon)
                + display_width(repo)
                + display_width(&git_ref)
                + display_width(&age)
                + 2,
        )
        .max(1);

    Line::from(vec![
        Span::styled(icon, Style::default().fg(color)),
        Span::raw(" "),
        Span::styled(repo, Style::default().fg(theme.text_active)),
        Span::raw(" "),
        Span::styled(git_ref, Style::default().fg(theme.branch)),
        Span::raw(" ".repeat(gap)),
        Span::styled(age, Style::default().fg(theme.text_muted)),
    ])
}

/// Compact age: `42s`, `7m`, `3h`, `2d`.
fn format_age(secs: u64) -> String {
    match secs {
        s if s < 60 => format!("{s}s"),
        s if s < 3_600 => format!("{}m", s / 60),
        s if s < 86_400 => format!("{}h", s / 3_600),
        s => format!("{}d", s / 86_400),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn run(status: DeployStatus) -> DeployRun {
        DeployRun {
            repo: "listing".into(),
            workflow: "deploy".into(),
            git_ref: "v1.2.3".into(),
            status,
            started_at: 1_000,
            updated_at: 1_100,
            url: String::new(),
        }
    }

    #[test]
    fn height_is_zero_without_runs() {
        let state = AppState::new("%99".into());
        assert_eq!(deploy_panel_height(&state, 40), 0);
    }

    #[test]
    fn height_covers_every_run_plus_borders() {
        let mut state = AppState::new("%99".into());
        state.deploys = vec![run(DeployStatus::Running), run(DeployStatus::Success)];
        assert_eq!(deploy_panel_height(&state, 40), 4);
    }

    #[test]
    fn height_leaves_three_rows_for_the_pane_list() {
        let mut state = AppState::new("%99".into());
        state.deploys = (0..10).map(|_| run(DeployStatus::Running)).collect();
        assert_eq!(deploy_panel_height(&state, 8), 5);
    }

    #[test]
    fn run_line_fits_the_width_and_keeps_the_age() {
        let mut state = AppState::new("%99".into());
        state.now = 1_400;
        let deploy = run(DeployStatus::Running);

        let line = run_line(&deploy, &state, 30);
        let rendered: String = line.spans.iter().map(|s| s.content.as_ref()).collect();

        assert_eq!(display_width(&rendered), 30, "{rendered:?}");
        assert!(rendered.starts_with("● listing v1.2.3"), "{rendered:?}");
        assert!(rendered.ends_with("6m"), "active run counts up to now");
    }

    #[test]
    fn run_line_truncates_a_long_ref_instead_of_overflowing() {
        let mut state = AppState::new("%99".into());
        state.now = 1_100;
        let mut deploy = run(DeployStatus::Failure);
        deploy.git_ref = "release-2026-08-17-very-long-tag-name".into();

        let line = run_line(&deploy, &state, 24);
        let rendered: String = line.spans.iter().map(|s| s.content.as_ref()).collect();

        assert!(display_width(&rendered) <= 24, "{rendered:?}");
        assert!(rendered.contains('…'), "{rendered:?}");
    }

    #[test]
    fn format_age_scales_units() {
        assert_eq!(format_age(42), "42s");
        assert_eq!(format_age(7 * 60), "7m");
        assert_eq!(format_age(3 * 3_600), "3h");
        assert_eq!(format_age(2 * 86_400), "2d");
    }
}

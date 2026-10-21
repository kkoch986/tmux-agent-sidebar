use ratatui::{
    style::{Modifier, Style},
    text::{Line, Span},
};

use super::SPAWN_BUTTON;
use super::row;
use crate::group::{PaneGitInfo, RepoGroup};
use crate::state::{AppState, Focus};
use crate::tmux::PaneInfo;
use crate::ui::colors::ColorTheme;
use crate::ui::text::{display_width, truncate_to_width};

/// Tree glyphs mirroring `choose-tree` (`prefix + w`): a window hangs off its
/// session with `├`, the session's last window with `└`, and pane rows sit
/// under a `│` trunk that stops at the last window.
const TREE_BRANCH: &str = "├";
const TREE_ELBOW: &str = "└";
const TREE_TRUNK: &str = "│";
/// Columns the tree gutter consumes: one glyph plus one space.
const GUTTER_WIDTH: usize = 2;

#[derive(Debug, Default)]
pub(super) struct CollectedRows {
    pub lines: Vec<Line<'static>>,
    pub line_to_row: Vec<Option<usize>>,
    pub pending_spawn: Vec<(usize, String, String)>,
    pub pending_remove: Vec<(usize, u16, String)>,
}

/// A window group that survived filtering, with the panes that survived it.
struct VisibleGroup<'a> {
    group: &'a RepoGroup,
    panes: Vec<&'a (PaneInfo, PaneGitInfo)>,
    /// Last *visible* window of its session. Recomputed here because
    /// `RepoGroup::is_last_in_session` describes the unfiltered tree.
    is_last_in_session: bool,
}

/// Drop panes and windows the status and repo filters exclude, then mark the
/// last surviving window of each session so the elbow glyph lands correctly.
fn visible_groups(state: &AppState) -> Vec<VisibleGroup<'_>> {
    let status_filter = state.global.status_filter;
    let repo_filter = &state.global.repo_filter;

    let mut visible: Vec<VisibleGroup<'_>> = state
        .repo_groups
        .iter()
        .filter_map(|group| {
            let panes: Vec<_> = group
                .panes
                .iter()
                .filter(|(pane, git)| {
                    status_filter.matches(&pane.status) && repo_filter.matches_pane(pane, git)
                })
                .collect();
            (!panes.is_empty()).then_some(VisibleGroup {
                group,
                panes,
                is_last_in_session: false,
            })
        })
        .collect();

    for i in 0..visible.len() {
        let is_last = visible
            .get(i + 1)
            .is_none_or(|next| next.group.session_name != visible[i].group.session_name);
        visible[i].is_last_in_session = is_last;
    }
    visible
}

pub(super) fn collect(state: &AppState, width: u16) -> CollectedRows {
    let width = width as usize;
    let theme = &state.theme;

    let mut collected = CollectedRows::default();
    let mut row_index: usize = 0;
    let visible = visible_groups(state);
    let mut prev_session: Option<&str> = None;

    for (idx, visible_group) in visible.iter().enumerate() {
        let group = visible_group.group;
        let filtered_panes = &visible_group.panes;

        // Session header whenever the session changes, matching the
        // `choose-tree` top level. The window count is of *visible* windows,
        // so it never disagrees with the rows drawn under it.
        if prev_session != Some(group.session_name.as_str()) {
            if idx > 0 {
                collected.lines.push(Line::from(""));
                collected.line_to_row.push(None);
            }
            let window_count = visible[idx..]
                .iter()
                .take_while(|g| g.group.session_name == group.session_name)
                .count();
            let session_has_focus = visible[idx..]
                .iter()
                .take_while(|g| g.group.session_name == group.session_name)
                .any(|g| g.group.has_focus);
            let label = format!(
                "{}: {} window{}",
                group.session_name,
                window_count,
                if window_count == 1 { "" } else { "s" }
            );
            let color = if session_has_focus {
                theme.accent
            } else {
                theme.session_header
            };
            collected
                .lines
                .push(Line::from(Span::styled(label, Style::default().fg(color))));
            collected.line_to_row.push(None);
            prev_session = Some(group.session_name.as_str());
        }

        let group_has_focused_pane = state
            .focus_state
            .focused_pane_id
            .as_ref()
            .is_some_and(|fid| group.panes.iter().any(|(p, _)| p.pane_id == *fid));

        // Window row: tree glyph, `<index>: <name>`, and a right-aligned
        // `[+]` spawn button when the window's panes agree on one repo root.
        let elbow = if visible_group.is_last_in_session {
            TREE_ELBOW
        } else {
            TREE_BRANCH
        };
        let window_needs_you = filtered_panes.iter().any(|(p, _)| p.needs_attention());
        let title = if window_needs_you {
            format!("▶ {}", group.name)
        } else {
            group.name.clone()
        };
        let title_style = if window_needs_you {
            Style::default()
                .fg(theme.status_waiting)
                .add_modifier(Modifier::BOLD)
        } else if group_has_focused_pane {
            Style::default()
                .fg(theme.accent)
                .add_modifier(Modifier::BOLD)
        } else {
            Style::default()
                .fg(theme.text_active)
                .add_modifier(Modifier::BOLD)
        };
        let gutter_style = Style::default().fg(theme.text_inactive);
        let spawn_root = group.spawn_repo_root();
        let title_room = width
            .saturating_sub(GUTTER_WIDTH)
            .saturating_sub(if spawn_root.is_some() { 2 } else { 0 });
        let title = truncate_to_width(&title, title_room);
        let mut spans: Vec<Span<'static>> = vec![
            Span::styled(elbow.to_string(), gutter_style),
            Span::raw(" "),
            Span::styled(title.clone(), title_style),
        ];
        if let Some(root) = spawn_root {
            let used = GUTTER_WIDTH + display_width(&title);
            let pad_width = width
                .saturating_sub(used)
                .saturating_sub(SPAWN_BUTTON.len());
            collected
                .pending_spawn
                .push((collected.lines.len(), group.name.clone(), root));
            let button_color = if group_has_focused_pane {
                theme.accent
            } else {
                theme.text_active
            };
            spans.push(Span::raw(" ".repeat(pad_width)));
            spans.push(Span::styled(
                SPAWN_BUTTON,
                Style::default().fg(button_color),
            ));
        }
        collected.lines.push(Line::from(spans));
        collected.line_to_row.push(None);

        // Pane rows are rendered into the width left of the gutter, then
        // shifted right by it, so the trunk column stays clear.
        let trunk = if visible_group.is_last_in_session {
            " "
        } else {
            TREE_TRUNK
        };
        let pane_width = width.saturating_sub(GUTTER_WIDTH);

        for (pane, git_info) in filtered_panes.iter() {
            let is_selected = state.focus_state.sidebar_focused
                && state.focus_state.focus == Focus::Panes
                && row_index == state.global.selected_pane_row;

            let is_active = state.focus_state.focused_pane_id.as_ref() == Some(&pane.pane_id);

            let pane_state = state.pane_state(&pane.pane_id);
            let ports = pane_state.map(|s| s.ports.as_slice());
            let task_progress = pane_state.and_then(|s| s.task_progress.as_ref());
            let last_activity = pane_state.and_then(|s| s.last_activity.as_ref());
            let status_line_idx = collected.lines.len();
            let pane_lines = row::render_pane_lines_with_ports(
                pane,
                git_info,
                ports,
                task_progress,
                last_activity,
                is_selected,
                is_active,
                pane_width,
                &state.icons,
                theme,
                state.spinner_frame,
                state.now,
            );
            let pane_line_count = pane_lines.len();
            for line in pane_lines {
                collected.lines.push(indent_under_trunk(line, trunk, theme));
                collected.line_to_row.push(Some(row_index));
            }

            // The branch row is always `status_line_idx + 1` when
            // `branch_ports_row` emits a line (which requires a
            // non-empty branch). Look up the exact column of the
            // trailing `×` from the row helper so the click target
            // lines up with the rendered glyph even when the branch
            // name truncates.
            if pane.sidebar_spawned
                && git_info.is_worktree
                && pane_line_count >= 2
                && let Some(x) = row::sidebar_remove_marker_col(
                    git_info,
                    ports,
                    true,
                    pane_width.saturating_sub(2),
                )
            {
                collected.pending_remove.push((
                    status_line_idx + 1,
                    x + GUTTER_WIDTH as u16,
                    pane.pane_id.clone(),
                ));
            }

            row_index += 1;
        }
    }

    collected
}

/// Shift a pane row right by the tree gutter, prefixing the session trunk.
/// The gutter deliberately carries no selection background so the trunk stays
/// readable through a highlighted row.
fn indent_under_trunk(mut line: Line<'static>, trunk: &str, theme: &ColorTheme) -> Line<'static> {
    line.spans.insert(
        0,
        Span::styled(
            format!("{trunk} "),
            Style::default().fg(theme.text_inactive),
        ),
    );
    line
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::group::{PaneGitInfo, RepoGroup};
    use crate::state::{AppState, StatusFilter};
    use crate::tmux::{AgentType, PaneInfo, PaneStatus, PermissionMode, WorktreeMetadata};

    fn make_pane(id: &str, status: PaneStatus) -> PaneInfo {
        PaneInfo {
            pane_id: id.into(),
            pane_active: false,
            status,
            attention: false,
            agent: AgentType::Claude,
            path: "/tmp/repo".into(),
            current_command: String::new(),
            prompt: String::new(),
            prompt_is_response: false,
            started_at: None,
            wait_reason: String::new(),
            permission_mode: PermissionMode::Default,
            subagents: vec![],
            pane_pid: None,
            worktree: WorktreeMetadata::default(),
            session_id: None,
            session_name: String::new(),
            sidebar_spawned: false,
            bg_shell_cmd: None,
            title: String::new(),
            claude: None,
        }
    }

    #[test]
    fn collect_empty_repo_groups_produces_no_lines() {
        let state = AppState::new("%0".into());
        let collected = collect(&state, 40);
        assert!(collected.lines.is_empty());
        assert!(collected.line_to_row.is_empty());
        assert!(collected.pending_spawn.is_empty());
        assert!(collected.pending_remove.is_empty());
    }

    #[test]
    fn collect_skips_group_when_status_filter_excludes_all_panes() {
        let mut state = AppState::new("%0".into());
        // The group has only Running panes, so filter to Waiting to drop them all.
        state.global.status_filter = StatusFilter::Waiting;
        state.repo_groups = vec![RepoGroup {
            name: "repo".into(),
            session_name: "main".into(),
            has_focus: false,
            panes: vec![(make_pane("%1", PaneStatus::Running), PaneGitInfo::default())],
        }];
        let collected = collect(&state, 40);
        assert!(collected.lines.is_empty());
        assert!(collected.pending_spawn.is_empty());
    }

    #[test]
    fn collect_records_pending_spawn_when_repo_root_present() {
        let mut state = AppState::new("%0".into());
        let git_info = PaneGitInfo {
            repo_root: Some("/tmp/repo".into()),
            branch: None,
            is_worktree: false,
            worktree_name: None,
        };
        state.repo_groups = vec![RepoGroup {
            name: "repo".into(),
            session_name: "main".into(),
            has_focus: false,
            panes: vec![(make_pane("%1", PaneStatus::Running), git_info)],
        }];
        let collected = collect(&state, 40);
        assert_eq!(
            collected.pending_spawn.len(),
            1,
            "groups with a repo_root should emit a spawn target"
        );
        assert_eq!(collected.pending_spawn[0].1, "repo");
        assert_eq!(collected.pending_spawn[0].2, "/tmp/repo");
        // At least the header plus one pane row should have been pushed.
        assert!(!collected.lines.is_empty());
    }

    #[test]
    fn collect_no_pending_spawn_without_repo_root() {
        let mut state = AppState::new("%0".into());
        state.repo_groups = vec![RepoGroup {
            name: "raw-path".into(),
            session_name: "main".into(),
            has_focus: false,
            panes: vec![(make_pane("%1", PaneStatus::Running), PaneGitInfo::default())],
        }];
        let collected = collect(&state, 40);
        assert!(
            collected.pending_spawn.is_empty(),
            "groups without repo_root must not produce spawn targets"
        );
    }

    #[test]
    fn collect_pending_spawn_grows_with_repo_root_bearing_groups() {
        let mut state = AppState::new("%0".into());
        let with_root = |root: &str, name: &str, pane_id: &str| RepoGroup {
            name: name.into(),
            session_name: "main".into(),
            has_focus: false,
            panes: vec![(
                make_pane(pane_id, PaneStatus::Running),
                PaneGitInfo {
                    repo_root: Some(root.into()),
                    branch: None,
                    is_worktree: false,
                    worktree_name: None,
                },
            )],
        };
        state.repo_groups = vec![
            with_root("/repo/a", "a", "%1"),
            with_root("/repo/b", "b", "%2"),
            with_root("/repo/c", "c", "%3"),
        ];
        let collected = collect(&state, 40);
        assert_eq!(collected.pending_spawn.len(), 3);
    }
}

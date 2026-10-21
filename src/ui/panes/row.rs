use ratatui::{style::Style, text::Line};

use crate::ui::colors::ColorTheme;
use crate::ui::icons::StatusIcons;

mod body;
mod branch;
mod ctx;
mod status;

use body::{detail_row, subagent_row, task_progress_row};
use branch::branch_ports_row;
use ctx::{ATTENTION_MARKER, RowCtx, SELECTION_MARKER};
#[cfg(test)]
use status::running_icon_for;
use status::status_row;

pub(super) use branch::sidebar_remove_marker_col;

/// Lines for one pane: a status row (summary, account, run time), one
/// detail row (what it is doing or what it needs), then subagents, task
/// progress and the branch/ports row when there is something to show.
#[allow(clippy::too_many_arguments)]
pub(super) fn render_pane_lines_with_ports(
    pane: &crate::tmux::PaneInfo,
    git_info: &crate::group::PaneGitInfo,
    ports: Option<&[u16]>,
    task_progress: Option<&crate::activity::TaskProgress>,
    last_activity: Option<&crate::activity::ActivityEntry>,
    selected: bool,
    active: bool,
    width: usize,
    icons: &StatusIcons,
    theme: &ColorTheme,
    spinner_frame: usize,
    now: u64,
) -> Vec<Line<'static>> {
    let bg = selected.then_some(theme.selection_bg);
    let with_bg = |style: Style| match bg {
        Some(c) => style.bg(c),
        None => style,
    };
    // A pane that needs the user carries a bar down every one of its
    // lines, so it is findable at a glance however long the list is. The
    // focused pane otherwise gets the thinner accent marker on its first
    // row only.
    let needs_you = pane.needs_attention();
    let attention_color = theme.status_color(&pane.status, true);
    let (first_marker, first_marker_style) = if needs_you {
        (
            ATTENTION_MARKER,
            with_bg(Style::default().fg(attention_color)),
        )
    } else if active {
        (SELECTION_MARKER, with_bg(Style::default().fg(theme.accent)))
    } else {
        (" ", with_bg(Style::default()))
    };
    let (rest_marker, rest_marker_style) = if needs_you {
        (
            ATTENTION_MARKER,
            with_bg(Style::default().fg(attention_color)),
        )
    } else {
        (" ", with_bg(Style::default()))
    };
    let first_ctx = RowCtx {
        marker_char: first_marker,
        marker_style: first_marker_style,
        inner_width: width.saturating_sub(2),
        theme,
        bg,
        active,
    };
    let ctx = &RowCtx {
        marker_char: rest_marker,
        marker_style: rest_marker_style,
        inner_width: width.saturating_sub(2),
        theme,
        bg,
        active,
    };

    let mut out: Vec<Line<'static>> = Vec::with_capacity(5);
    out.push(status_row(pane, &first_ctx, icons, spinner_frame, now));
    // The branch row must stay directly under the status row: the
    // worktree remove `×` click target is placed by that offset.
    let has_ports = ports.is_some_and(|p| !p.is_empty());
    if (git_info.is_worktree || pane.sidebar_spawned || has_ports)
        && let Some(line) = branch_ports_row(git_info, ports, pane.sidebar_spawned, ctx)
    {
        out.push(line);
    }
    if let Some(line) = detail_row(pane, last_activity, ctx) {
        out.push(line);
    }
    if let Some(line) = subagent_row(&pane.subagents, ctx) {
        out.push(line);
    }
    if let Some(line) = task_progress_row(task_progress, ctx) {
        out.push(line);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::group::PaneGitInfo;
    use crate::tmux::PaneStatus;
    use crate::tmux::{AgentType, PaneInfo, PermissionMode, WorktreeMetadata};
    use crate::ui::icons::StatusIcons;
    use crate::ui::text::display_width;
    use ratatui::style::{Color, Modifier};

    fn pane(permission_mode: PermissionMode, status: PaneStatus, prompt: &str) -> PaneInfo {
        pane_with_response(permission_mode, status, prompt, false)
    }

    fn pane_with_response(
        permission_mode: PermissionMode,
        status: PaneStatus,
        prompt: &str,
        is_response: bool,
    ) -> PaneInfo {
        PaneInfo {
            pane_id: "%1".into(),
            pane_active: false,
            status,
            attention: false,
            agent: AgentType::Codex,
            path: "/tmp/project".into(),
            current_command: String::new(),
            prompt: prompt.into(),
            prompt_is_response: is_response,
            started_at: None,
            wait_reason: String::new(),
            permission_mode,
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

    fn line_text(line: &Line<'_>) -> String {
        line.spans
            .iter()
            .map(|span| span.content.as_ref())
            .collect()
    }

    fn test_ctx<'a>(theme: &'a ColorTheme, inner_width: usize, active: bool) -> RowCtx<'a> {
        RowCtx {
            marker_char: " ",
            marker_style: Style::default(),
            inner_width,
            theme,
            bg: None,
            active,
        }
    }

    fn render_text(
        pane: &PaneInfo,
        last_activity: Option<&crate::activity::ActivityEntry>,
    ) -> String {
        let theme = ColorTheme::default();
        render_pane_lines_with_ports(
            pane,
            &PaneGitInfo::default(),
            None,
            None,
            last_activity,
            false,
            false,
            44,
            &StatusIcons::default(),
            &theme,
            0,
            100,
        )
        .iter()
        .map(line_text)
        .collect::<Vec<_>>()
        .join("\n")
    }

    fn claude_pane(status: PaneStatus) -> PaneInfo {
        let mut p = pane(PermissionMode::BypassPermissions, status, "");
        p.agent = AgentType::Claude;
        p.title = "Fix CSV aliases".into();
        p.claude = Some(crate::tmux::ClaudeProcess {
            account: "k1".into(),
        });
        p
    }

    #[test]
    fn snapshot_running_pane_shows_summary_account_and_current_tool() {
        let mut p = claude_pane(PaneStatus::Running);
        p.started_at = Some(40);
        let entry = crate::activity::ActivityEntry {
            timestamp: "10:03".into(),
            tool: "Bash".into(),
            label: "cargo test".into(),
        };
        insta::assert_snapshot!(render_text(&p, Some(&entry)), @"
        ● Fix CSV aliases                  k1 1m0s
          Bash cargo test
        ");
    }

    #[test]
    fn snapshot_permission_wait_is_flagged_as_needs_you() {
        let mut p = claude_pane(PaneStatus::Waiting);
        p.wait_reason = "permission_prompt".into();
        insta::assert_snapshot!(render_text(&p, None), @"
        ▌ ◐ Fix CSV aliases             k1 NEEDS YOU
        ▌   ▶ needs your permission
        ");
    }

    #[test]
    fn snapshot_finished_turn_is_your_turn_with_reply_summary() {
        let mut p = claude_pane(PaneStatus::Idle);
        p.attention = true;
        p.prompt = "Both fixes are up as PRs. Neither is merged.".into();
        p.prompt_is_response = true;
        insta::assert_snapshot!(render_text(&p, None), @"
        ▌ ○ Fix CSV aliases             k1 YOUR TURN
        ▌   ▶ Both fixes are up as PRs.
        ");
    }

    #[test]
    fn snapshot_background_pane_collapses_subagents_to_one_line() {
        let mut p = claude_pane(PaneStatus::Background);
        p.subagents = vec!["Explore #ab12".into(), "Plan #cd34".into()];
        insta::assert_snapshot!(render_text(&p, None), @"
        ◎ Fix CSV aliases                       k1
          ↳ 2 subagents: Explore, Plan
        ");
    }

    #[test]
    fn snapshot_unhooked_claude_says_status_is_unknown() {
        let mut p = pane(PermissionMode::Default, PaneStatus::Idle, "");
        p.agent = AgentType::None;
        p.title = "Triage ZEN-6863".into();
        p.claude = Some(crate::tmux::ClaudeProcess {
            account: "ken".into(),
        });
        insta::assert_snapshot!(render_text(&p, None), @"
        ○ Triage ZEN-6863                      ken
          no status reported yet
        ");
    }

    #[test]
    fn snapshot_plain_shell_is_one_dim_line() {
        let mut p = pane(PermissionMode::Default, PaneStatus::Idle, "");
        p.agent = AgentType::None;
        p.current_command = "nvim".into();
        insta::assert_snapshot!(render_text(&p, None), @"  ○ nvim");
    }

    #[test]
    fn needs_you_pane_has_attention_bar_on_every_line() {
        let mut p = claude_pane(PaneStatus::Waiting);
        p.subagents = vec!["Explore #ab12".into()];
        let theme = ColorTheme::default();
        let lines = render_pane_lines_with_ports(
            &p,
            &PaneGitInfo::default(),
            None,
            None,
            None,
            false,
            false,
            44,
            &StatusIcons::default(),
            &theme,
            0,
            0,
        );
        assert_eq!(lines.len(), 3);
        for line in &lines {
            assert_eq!(line.spans[0].content, ATTENTION_MARKER);
            assert_eq!(line.spans[0].style.fg, Some(theme.status_waiting));
        }
        assert_eq!(lines[0].spans[2].style.bg, Some(theme.status_waiting));
    }

    #[test]
    fn render_pane_lines_shows_session_name_instead_of_agent() {
        let theme = ColorTheme::default();
        let mut p = pane(PermissionMode::Default, PaneStatus::Running, "");
        p.session_name = "fix-csv-aliases".into();
        let lines = render_pane_lines_with_ports(
            &p,
            &PaneGitInfo::default(),
            None,
            None,
            None,
            false,
            false,
            40,
            &StatusIcons::default(),
            &theme,
            0,
            0,
        );

        let status = line_text(&lines[0]);
        assert!(
            status.contains("fix-csv-aliases"),
            "session name should appear in status row, got: {status}"
        );
        assert!(
            !status.contains("codex"),
            "agent label should be replaced by session name, got: {status}"
        );
    }

    #[test]
    fn render_pane_lines_truncates_long_session_name_to_keep_elapsed_visible() {
        // Regression: a user-supplied `/rename` title can be arbitrarily
        // long and would push the elapsed counter off-screen if we did
        // not truncate it first. The width budget reserves room for the
        // status icon, the badge, and the elapsed label.
        let theme = ColorTheme::default();
        let mut p = pane(PermissionMode::Default, PaneStatus::Running, "");
        p.session_name = "this-is-a-ridiculously-long-session-name-that-will-not-fit".into();
        // started_at must be > 0 for elapsed_label to render.
        // started_at=1, now=66 → elapsed=65s → "1m5s".
        p.started_at = Some(1);
        let lines = render_pane_lines_with_ports(
            &p,
            &PaneGitInfo::default(),
            None,
            None,
            None,
            false,
            false,
            30,
            &StatusIcons::default(),
            &theme,
            0,
            66,
        );

        let status = line_text(&lines[0]);
        // The elapsed counter must remain visible — that is the whole
        // point of capping the title width.
        assert!(
            status.contains("1m5s"),
            "elapsed must stay visible when session name is long, got: {status}"
        );
        // The full title must NOT fit; it should be replaced by a
        // truncated form ending in the standard ellipsis character.
        assert!(
            !status.contains("not-fit"),
            "long session name must be truncated, got: {status}"
        );
        // Each rendered cell should fit inside the 30-column width.
        let visible_width = display_width(&status);
        assert!(
            visible_width <= 30,
            "status row width {visible_width} must not exceed inner_width 30: {status}"
        );
    }

    #[test]
    fn render_pane_lines_shows_branch_and_ports_on_same_row() {
        let theme = ColorTheme::default();
        let pane = pane(PermissionMode::Default, PaneStatus::Running, "");
        let ports = vec![3000, 5173];
        let lines = render_pane_lines_with_ports(
            &pane,
            &PaneGitInfo {
                repo_root: Some("/tmp/project".into()),
                branch: Some("feature/sidebar".into()),
                is_worktree: false,
                worktree_name: None,
            },
            Some(&ports),
            None,
            None,
            false,
            false,
            40,
            &StatusIcons::default(),
            &theme,
            0,
            0,
        );

        assert!(lines.len() >= 2);
        let branch_port_line = line_text(&lines[1]);
        assert!(branch_port_line.contains("feature/sidebar"));
        assert!(branch_port_line.contains(":3000, 5173"));
        assert!(branch_port_line.find("feature/sidebar") < branch_port_line.find(":3000, 5173"));
    }

    #[test]
    fn render_pane_lines_truncates_long_branch_when_ports_present() {
        let theme = ColorTheme::default();
        let pane = pane(PermissionMode::Default, PaneStatus::Running, "");
        let ports = vec![3000];
        let lines = render_pane_lines_with_ports(
            &pane,
            &PaneGitInfo {
                repo_root: Some("/tmp/project".into()),
                branch: Some("feature/sidebar/really-long-branch-name-that-should-truncate".into()),
                is_worktree: false,
                worktree_name: None,
            },
            Some(&ports),
            None,
            None,
            false,
            false,
            40,
            &StatusIcons::default(),
            &theme,
            0,
            0,
        );

        assert!(lines.len() >= 2);
        let branch_port_line = line_text(&lines[1]);
        assert!(
            branch_port_line.contains('…'),
            "long branch should be truncated"
        );
        assert!(branch_port_line.contains(":3000"));
        assert!(
            branch_port_line.find('…') < branch_port_line.find(":3000"),
            "branch truncation should remain left of the port text"
        );
    }

    #[test]
    fn render_pane_lines_uses_injected_now_for_elapsed() {
        let theme = ColorTheme::default();
        let mut pane = pane(PermissionMode::Default, PaneStatus::Running, "");
        pane.started_at = Some(1_000_000 - 125);
        let lines = render_pane_lines_with_ports(
            &pane,
            &PaneGitInfo::default(),
            None,
            None,
            None,
            false,
            false,
            40,
            &StatusIcons::default(),
            &theme,
            0,
            1_000_000,
        );

        let status = line_text(&lines[0]);
        assert!(status.contains("2m5s"));
    }

    #[test]
    fn running_icon_for_all_statuses() {
        let icons = StatusIcons::default();
        assert_eq!(running_icon_for(&PaneStatus::Idle, 0, &icons), ("○", None));
        assert_eq!(
            running_icon_for(&PaneStatus::Waiting, 0, &icons),
            ("◐", None)
        );
        assert_eq!(running_icon_for(&PaneStatus::Error, 0, &icons), ("✕", None));
        assert_eq!(
            running_icon_for(&PaneStatus::Unknown, 0, &icons),
            ("·", None)
        );
        assert_eq!(
            running_icon_for(&PaneStatus::Background, 0, &icons),
            ("◎", None)
        );

        let (icon, color) = running_icon_for(&PaneStatus::Running, 0, &icons);
        assert_eq!(icon, "●");
        assert_eq!(color, Some(Color::Indexed(82)));
    }

    #[test]
    fn render_pane_lines_shows_background_command_when_known() {
        let theme = ColorTheme::default();
        let mut p = pane(PermissionMode::Default, PaneStatus::Background, "");
        p.bg_shell_cmd = Some("cargo build --release".into());
        let lines = render_pane_lines_with_ports(
            &p,
            &PaneGitInfo::default(),
            None,
            None,
            None,
            false,
            false,
            40,
            &StatusIcons::default(),
            &theme,
            0,
            0,
        );

        assert_eq!(lines.len(), 2);
        let hint = line_text(&lines[1]);
        assert!(hint.contains("cargo build --release"), "got: {hint}");
        assert!(
            !hint.contains("Background shell running"),
            "fallback text must not appear when a command is known, got: {hint}"
        );
    }

    #[test]
    fn render_pane_lines_truncates_long_background_command() {
        let theme = ColorTheme::default();
        let mut p = pane(PermissionMode::Default, PaneStatus::Background, "");
        p.bg_shell_cmd = Some("cargo run --bin very-long-command-name --flag".into());
        // Narrow width forces the ellipsis path in `truncate_to_width`.
        let lines = render_pane_lines_with_ports(
            &p,
            &PaneGitInfo::default(),
            None,
            None,
            None,
            false,
            false,
            20,
            &StatusIcons::default(),
            &theme,
            0,
            0,
        );

        let hint = line_text(&lines[1]);
        assert!(hint.contains("cargo run"), "command missing in {hint}");
        assert!(hint.contains('\u{2026}'), "ellipsis missing in {hint}");
        assert!(
            display_width(&hint) <= 20,
            "row width {w} must not exceed inner_width 20: {hint}",
            w = display_width(&hint)
        );
    }

    #[test]
    fn render_pane_lines_normal_prompt_not_detected_as_response() {
        let theme = ColorTheme::default();
        let p = pane(PermissionMode::Default, PaneStatus::Running, "fix the bug");
        let lines = render_pane_lines_with_ports(
            &p,
            &PaneGitInfo::default(),
            None,
            None,
            None,
            false,
            false,
            40,
            &StatusIcons::default(),
            &theme,
            0,
            0,
        );

        assert!(lines.len() >= 2);
        let prompt_line = line_text(&lines[1]);
        assert!(!prompt_line.contains("▷"));
        assert!(prompt_line.contains("fix the bug"));
    }

    #[test]
    fn branch_ports_row_renders_port_only_without_branch() {
        let theme = ColorTheme::default();
        let ctx = test_ctx(&theme, 40, false);
        let ports = vec![3000];
        let line = branch_ports_row(&PaneGitInfo::default(), Some(&ports), false, &ctx)
            .expect("should render port line");
        assert!(line_text(&line).contains(":3000"));
    }

    #[test]
    fn branch_ports_row_renders_plus_marker_for_non_spawned_worktree() {
        let theme = ColorTheme::default();
        let ctx = test_ctx(&theme, 40, false);
        let git = PaneGitInfo {
            repo_root: Some("/r".into()),
            branch: Some("feat/x".into()),
            is_worktree: true,
            worktree_name: None,
        };
        let line = branch_ports_row(&git, None, false, &ctx).expect("branch row should render");
        let text = line_text(&line);
        assert!(text.contains("+ feat/x"), "plain + marker: {text}");
        assert!(!text.contains('×'), "non-spawned must not render ×");
    }

    #[test]
    fn branch_ports_row_pins_trailing_x_to_right_edge_for_sidebar_spawned_worktree() {
        let theme = ColorTheme::default();
        let inner_width = 40usize;
        let ctx = test_ctx(&theme, inner_width, false);
        let git = PaneGitInfo {
            repo_root: Some("/r".into()),
            branch: Some("feat/x".into()),
            is_worktree: true,
            worktree_name: None,
        };
        let line = branch_ports_row(&git, None, true, &ctx).expect("branch row should render");
        let text = line_text(&line);
        assert!(
            text.contains("+ feat/x"),
            "+ worktree marker must be preserved: {text}"
        );
        // The trailing `×` must sit at the very last row column
        // (= inner_width + 1), mirroring the repo header's
        // right-aligned `+` spawn button.
        assert_eq!(
            rendered_x_col(&text),
            inner_width + 1,
            "× should pin to the rightmost column"
        );
        // The `×` suffix must come AFTER the branch text, not before.
        let plus_idx = text.find("+ feat/x").unwrap();
        let x_idx = text.find('×').unwrap();
        assert!(
            plus_idx < x_idx,
            "`+ feat/x` must precede the trailing `×`, got: {text}"
        );
        // Branch text stays in the normal branch color.
        let body_span = line
            .spans
            .iter()
            .find(|s| s.content.contains("feat/x"))
            .expect("branch body span");
        assert_eq!(body_span.style.fg, Some(theme.branch));
        // The trailing `×` span is painted with status_error so the
        // glyph reads as a remove action.
        let marker_span = line
            .spans
            .iter()
            .find(|s| s.content == "×")
            .expect("× span");
        assert_eq!(marker_span.style.fg, Some(theme.status_error));
    }

    /// Display column (in terminal cells, not bytes) where the
    /// first `×` appears in the rendered row. `text.find` returns a
    /// byte index which skews for multibyte glyphs like `…`, so
    /// tests that truncate branches need to measure in display cells.
    fn rendered_x_col(text: &str) -> usize {
        let idx = text.find('×').expect("× should be present");
        display_width(&text[..idx])
    }

    #[test]
    fn branch_ports_row_truncates_long_branch_but_keeps_x_at_right_edge() {
        let theme = ColorTheme::default();
        // Narrow ctx forces the branch text to truncate, but the
        // `×` must still render at the right edge — the action
        // affordance cannot be the thing that gets clipped.
        let inner_width = 18usize;
        let ctx = test_ctx(&theme, inner_width, false);
        let git = PaneGitInfo {
            repo_root: Some("/r".into()),
            branch: Some("feature/really-long-branch-name".into()),
            is_worktree: true,
            worktree_name: None,
        };
        let line = branch_ports_row(&git, None, true, &ctx).expect("branch row should render");
        let text = line_text(&line);
        assert!(
            text.contains('×'),
            "× must remain visible even when branch truncates: {text}"
        );
        assert!(
            text.contains('…'),
            "branch text should show truncation ellipsis: {text}"
        );
        assert_eq!(
            rendered_x_col(&text),
            inner_width + 1,
            "× stays pinned to right edge under truncation"
        );
        // Total row width = marker(1) + space(1) + inner_width.
        let rendered_width = display_width(text.trim_end());
        assert!(
            rendered_width <= inner_width + 2,
            "row must fit within marker + inner width (={}), got {rendered_width}: {text}",
            inner_width + 2
        );
    }

    #[test]
    fn sidebar_remove_marker_col_matches_branch_ports_row_layout() {
        // The click-target math in panes.rs uses
        // `sidebar_remove_marker_col` to line up the hit region with
        // the rendered `×`. Verify the two agree by counting the `×`
        // position in the rendered text and comparing to the helper.
        let theme = ColorTheme::default();
        let ctx = test_ctx(&theme, 40, false);
        let git = PaneGitInfo {
            repo_root: Some("/r".into()),
            branch: Some("feat/abc".into()),
            is_worktree: true,
            worktree_name: None,
        };
        let line = branch_ports_row(&git, None, true, &ctx).expect("branch row should render");
        let text = line_text(&line);
        let computed = sidebar_remove_marker_col(&git, None, true, ctx.inner_width)
            .expect("col should be Some");
        assert_eq!(
            computed as usize,
            rendered_x_col(&text),
            "computed × col must match rendered position"
        );
    }

    #[test]
    fn sidebar_remove_marker_col_does_not_depend_on_branch_length() {
        // Right-edge pinning means the col is determined by the row
        // width alone. A short and a long branch must produce the
        // same col.
        let short = PaneGitInfo {
            repo_root: Some("/r".into()),
            branch: Some("x".into()),
            is_worktree: true,
            worktree_name: None,
        };
        let long = PaneGitInfo {
            repo_root: Some("/r".into()),
            branch: Some("feature/really-long-branch-name".into()),
            is_worktree: true,
            worktree_name: None,
        };
        let col_short = sidebar_remove_marker_col(&short, None, true, 40);
        let col_long = sidebar_remove_marker_col(&long, None, true, 40);
        assert_eq!(col_short, col_long);
        assert_eq!(col_short, Some(41));
    }

    #[test]
    fn sidebar_remove_marker_col_returns_none_for_non_spawned() {
        let git = PaneGitInfo {
            repo_root: Some("/r".into()),
            branch: Some("feat/x".into()),
            is_worktree: true,
            worktree_name: None,
        };
        assert_eq!(sidebar_remove_marker_col(&git, None, false, 40), None);
    }

    #[test]
    fn sidebar_remove_marker_col_returns_none_for_non_worktree_branch() {
        // sidebar_spawned=true but the branch label has no `+` prefix
        // (is_worktree=false). No × should be rendered or registered.
        let git = PaneGitInfo {
            repo_root: Some("/r".into()),
            branch: Some("main".into()),
            is_worktree: false,
            worktree_name: None,
        };
        assert_eq!(sidebar_remove_marker_col(&git, None, true, 40), None);
    }

    #[test]
    fn branch_ports_row_keeps_x_at_right_edge_when_ports_are_present() {
        // Ports (`  :3000`) eat space on the right side of the row,
        // but the `×` must still pin to the very last column with
        // ports stacked just to its left.
        let theme = ColorTheme::default();
        let inner_width = 40usize;
        let ctx = test_ctx(&theme, inner_width, false);
        let git = PaneGitInfo {
            repo_root: Some("/r".into()),
            branch: Some("feat/abc".into()),
            is_worktree: true,
            worktree_name: None,
        };
        let ports = [3000u16];
        let line =
            branch_ports_row(&git, Some(&ports), true, &ctx).expect("branch row should render");
        let text = line_text(&line);
        assert_eq!(
            rendered_x_col(&text),
            inner_width + 1,
            "× must pin to right edge regardless of port presence"
        );
        let port_idx = text.find(":3000").expect(":3000 should be present");
        let x_idx = text.find('×').unwrap();
        assert!(
            port_idx < x_idx,
            "ports should sit to the LEFT of the × marker: {text}"
        );
    }

    #[test]
    fn branch_ports_row_keeps_plain_branch_when_sidebar_spawned_but_not_worktree() {
        // Edge case: sidebar_spawned=true but is_worktree=false.
        // `branch_label` does not emit the "+ " prefix, so
        // branch_ports_row must not try to swap anything and the
        // resulting row must stay plain.
        let theme = ColorTheme::default();
        let ctx = test_ctx(&theme, 40, false);
        let git = PaneGitInfo {
            repo_root: Some("/r".into()),
            branch: Some("main".into()),
            is_worktree: false,
            worktree_name: None,
        };
        let line = branch_ports_row(&git, None, true, &ctx).expect("branch row should render");
        let text = line_text(&line);
        assert!(text.contains("main"));
        assert!(!text.contains('×'));
        assert!(!text.contains('+'));
    }

    #[test]
    fn render_pane_lines_selected_applies_background_to_spans() {
        let theme = ColorTheme::default();
        let pane = pane(PermissionMode::Auto, PaneStatus::Running, "do work");
        let lines = render_pane_lines_with_ports(
            &pane,
            &PaneGitInfo::default(),
            None,
            None,
            None,
            true, // selected
            false,
            40,
            &StatusIcons::default(),
            &theme,
            0,
            0,
        );

        // Every inner (non-marker) span on the status line must carry the selection bg.
        // The left marker uses marker_style only.
        let status = &lines[0];
        let has_bg = status
            .spans
            .iter()
            .any(|s| s.style.bg == Some(theme.selection_bg));
        assert!(
            has_bg,
            "selected row should apply selection_bg to inner spans"
        );
    }

    #[test]
    fn render_pane_lines_active_shows_left_marker_on_status_row() {
        let theme = ColorTheme::default();
        let pane = pane(PermissionMode::Default, PaneStatus::Running, "");
        let lines = render_pane_lines_with_ports(
            &pane,
            &PaneGitInfo::default(),
            None,
            None,
            None,
            false,
            true, // active
            40,
            &StatusIcons::default(),
            &theme,
            0,
            0,
        );

        // The status row (line 0) must start with the SELECTION_MARKER in the
        // accent fg; no BOLD is applied to the title span.
        let marker_span = &lines[0].spans[0];
        assert_eq!(marker_span.content, SELECTION_MARKER);
        assert_eq!(marker_span.style.fg, Some(theme.accent));

        let title_span = lines[0]
            .spans
            .iter()
            .find(|s| s.content.contains("codex"))
            .expect("title span should be present");
        assert!(
            !title_span.style.add_modifier.contains(Modifier::BOLD),
            "active pane title should not be BOLD"
        );
    }

    #[test]
    fn status_row_default_permission_mode_omits_badge() {
        let theme = ColorTheme::default();
        let ctx = test_ctx(&theme, 40, false);
        let pane = pane(PermissionMode::Default, PaneStatus::Running, "");
        let line = status_row(&pane, &ctx, &StatusIcons::default(), 0, 0);
        let text = line_text(&line);
        // Default mode has an empty badge string — no extra badge token should appear.
        assert!(
            !text.contains(" auto") && !text.contains(" plan") && !text.contains(" !"),
            "default permission mode should not render a badge, got: {text}"
        );
    }
}

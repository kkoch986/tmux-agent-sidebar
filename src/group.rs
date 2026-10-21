use crate::git::run_git;
use crate::tmux::PaneInfo;

/// Per-pane git metadata resolved from the pane's working directory.
#[derive(Debug, Clone, Default)]
pub struct PaneGitInfo {
    pub repo_root: Option<String>,
    pub branch: Option<String>,
    pub is_worktree: bool,
    pub worktree_name: Option<String>,
}

/// A group of panes living in the same tmux window.
#[derive(Debug, Clone, Default)]
pub struct RepoGroup {
    /// Display name for the window row: `"<index>: <window_name>"`
    pub name: String,
    /// Session the window belongs to; groups are emitted session by session
    /// and the UI draws a header whenever this changes. Which window is a
    /// session's *last* one is deliberately not stored — only the UI knows
    /// that, after filtering has hidden windows.
    pub session_name: String,
    /// Whether any pane in the group belongs to the focused (active) window
    pub has_focus: bool,
    /// Panes in this group, with their git info
    pub panes: Vec<(PaneInfo, PaneGitInfo)>,
}

impl RepoGroup {
    /// Repo basenames the window's panes sit in, deduped in encounter order.
    /// The repo filter used to key off `name` back when a group *was* a repo;
    /// now that a group is a window, it has to read the panes instead.
    pub fn repo_names(&self) -> Vec<String> {
        let mut names: Vec<String> = Vec::new();
        for (pane, git) in &self.panes {
            if let Some(name) = repo_display_name(pane, git)
                && !names.contains(&name)
            {
                names.push(name);
            }
        }
        names
    }

    /// The repo root to spawn a new worktree from, when the window's panes
    /// agree on exactly one. A window straddling two repos has no unambiguous
    /// answer, so it gets no `[+]` button.
    pub fn spawn_repo_root(&self) -> Option<String> {
        let mut root: Option<&str> = None;
        for (_, git) in &self.panes {
            let candidate = git.repo_root.as_deref()?;
            match root {
                None => root = Some(candidate),
                Some(existing) if existing == candidate => {}
                Some(_) => return None,
            }
        }
        root.map(str::to_string)
    }
}

/// Name a pane's repo the way the old repo grouping did: the basename of the
/// git repo root, falling back to the basename of the pane's cwd when the
/// pane is not in a repo at all.
pub fn repo_display_name(pane: &PaneInfo, git: &PaneGitInfo) -> Option<String> {
    let key = git.repo_root.as_deref().unwrap_or(pane.path.as_str());
    if key.is_empty() {
        return None;
    }
    Some(key.rsplit('/').next().unwrap_or(key).to_string())
}

/// Resolve git info for a single pane path.
pub fn resolve_pane_git_info(path: &str) -> PaneGitInfo {
    if path.is_empty() {
        return PaneGitInfo::default();
    }

    // Single git call for all three values (one line per arg)
    let combined = run_git(
        path,
        &[
            "rev-parse",
            "--abbrev-ref",
            "HEAD",
            "--git-common-dir",
            "--git-dir",
        ],
    );
    let (branch, git_common_dir, git_dir) = match combined {
        Some(output) => {
            let mut lines = output.lines();
            let b = lines.next().map(|s| s.to_string());
            let c = lines.next().map(|s| s.to_string());
            let d = lines.next().map(|s| s.to_string());
            (b, c, d)
        }
        None => (None, None, None),
    };

    let is_worktree = match (&git_common_dir, &git_dir) {
        (Some(common), Some(dir)) => {
            let common_path = resolve_git_path(path, common);
            let dir_path = resolve_git_path(path, dir);
            common_path != dir_path
        }
        _ => false,
    };

    // --git-common-dir returns the .git dir of the main worktree;
    // its parent is the repo root, so worktrees share the same group key.
    let repo_root = git_common_dir
        .as_ref()
        .and_then(|common| {
            let abs = resolve_git_path(path, common);
            abs.parent().map(|p| p.to_string_lossy().to_string())
        })
        .or_else(|| run_git(path, &["rev-parse", "--show-toplevel"]));

    PaneGitInfo {
        repo_root,
        branch,
        is_worktree,
        worktree_name: None,
    }
}

/// Group all panes into the tmux session → window → pane tree that
/// `choose-tree` (`prefix + w`) shows: one group per window, emitted in
/// session-then-window-index order. Window order comes from
/// `tmux::query::finalize_sessions`, so this function preserves input order
/// rather than sorting.
pub fn group_panes_by_window(sessions: &[crate::tmux::SessionInfo]) -> Vec<RepoGroup> {
    let mut groups: Vec<RepoGroup> = Vec::new();
    let mut git_cache: std::collections::HashMap<String, PaneGitInfo> =
        std::collections::HashMap::new();

    for session in sessions {
        for window in &session.windows {
            let mut group = RepoGroup {
                name: format!("{}: {}", window.window_index, window.window_name),
                session_name: session.session_name.clone(),
                has_focus: false,
                panes: Vec::new(),
            };

            for pane in &window.panes {
                // Cache the base git info per path. `get` first avoids a key
                // clone on cache hits; misses fall through to `insert` which
                // owns the key plus the (expensive) git-command lookup.
                let mut git_info = match git_cache.get(pane.path.as_str()) {
                    Some(cached) => cached.clone(),
                    None => {
                        let resolved = resolve_pane_git_info(&pane.path);
                        git_cache.insert(pane.path.clone(), resolved.clone());
                        resolved
                    }
                };

                // Override with hook-provided worktree info (Claude Code
                // provides this; Codex does not, so the git-command base
                // remains as fallback).
                if !pane.worktree.name.is_empty() {
                    git_info.worktree_name = Some(pane.worktree.name.clone());
                    git_info.is_worktree = true;
                }
                if !pane.worktree.branch.is_empty() {
                    git_info.branch = Some(pane.worktree.branch.clone());
                    git_info.is_worktree = true;
                }

                if window.window_active && pane.pane_active {
                    group.has_focus = true;
                }

                group.panes.push((pane.clone(), git_info));
            }

            if !group.panes.is_empty() {
                groups.push(group);
            }
        }
    }

    groups
}

/// Resolve a possibly-relative git path to an absolute canonical path.
fn resolve_git_path(base: &str, git_path: &str) -> std::path::PathBuf {
    let p = if std::path::Path::new(git_path).is_absolute() {
        std::path::PathBuf::from(git_path)
    } else {
        std::path::PathBuf::from(base).join(git_path)
    };
    p.canonicalize().unwrap_or(p)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn resolve_git_info_returns_none_for_empty_path() {
        let info = resolve_pane_git_info("");
        assert!(info.branch.is_none());
        assert!(!info.is_worktree);
        assert!(info.repo_root.is_none());
    }

    #[test]
    fn resolve_git_info_for_real_repo() {
        // This test runs in the actual repo, so git commands work
        let info = resolve_pane_git_info(env!("CARGO_MANIFEST_DIR"));
        assert!(info.repo_root.is_some(), "should detect git repo");
        assert!(info.branch.is_some(), "should detect branch");
        let root = info.repo_root.unwrap();
        let root = std::fs::canonicalize(&root).unwrap();
        let manifest_dir = std::fs::canonicalize(env!("CARGO_MANIFEST_DIR")).unwrap();
        assert_eq!(root, manifest_dir, "repo root should be manifest dir");
    }

    #[test]
    fn worktree_and_main_share_same_repo_root() {
        // Both main and worktree should resolve to the same repo_root
        // We can only test the main worktree here, but verify the logic is consistent
        let info = resolve_pane_git_info(env!("CARGO_MANIFEST_DIR"));
        assert!(
            !info.is_worktree,
            "main checkout should not be detected as worktree"
        );
        assert!(info.repo_root.is_some());
    }

    // ─── resolve_git_path tests ─────────────────────────────────────

    #[test]
    fn resolve_git_path_absolute() {
        let result = resolve_git_path("/base", "/absolute/path");
        assert_eq!(result, std::path::PathBuf::from("/absolute/path"));
    }

    #[test]
    fn resolve_git_path_relative() {
        let result = resolve_git_path("/base/dir", "relative");
        assert_eq!(result, std::path::PathBuf::from("/base/dir/relative"));
    }

    // ─── group_panes_by_window tests ────────────────────────────────

    fn test_pane(id: &str, path: &str) -> PaneInfo {
        PaneInfo {
            pane_id: id.into(),
            pane_active: false,
            status: crate::tmux::PaneStatus::Running,
            attention: false,
            agent: crate::tmux::AgentType::Claude,
            path: path.into(),
            current_command: String::new(),
            prompt: String::new(),
            prompt_is_response: false,
            started_at: None,
            wait_reason: String::new(),
            permission_mode: crate::tmux::PermissionMode::Default,
            subagents: vec![],
            pane_pid: None,
            worktree: crate::tmux::WorktreeMetadata::default(),
            session_id: None,
            session_name: String::new(),
            sidebar_spawned: false,
            bg_shell_cmd: None,
            title: String::new(),
            claude: None,
        }
    }

    fn test_window(
        index: &str,
        name: &str,
        panes: Vec<PaneInfo>,
        active: bool,
    ) -> crate::tmux::WindowInfo {
        crate::tmux::WindowInfo {
            window_id: format!("@{index}"),
            window_index: index.into(),
            window_name: name.into(),
            window_active: active,
            auto_rename: false,
            panes,
        }
    }

    fn test_session(name: &str, windows: Vec<crate::tmux::WindowInfo>) -> crate::tmux::SessionInfo {
        crate::tmux::SessionInfo {
            session_name: name.into(),
            windows,
        }
    }

    #[test]
    fn group_panes_empty_sessions() {
        let groups = group_panes_by_window(&[]);
        assert!(groups.is_empty());
    }

    #[test]
    fn group_panes_one_group_per_window() {
        // Two panes in the same repo but different windows must NOT merge —
        // this is the whole point of the window tree, and the inverse of what
        // the old repo grouping did.
        let manifest_dir = env!("CARGO_MANIFEST_DIR");
        let sessions = vec![test_session(
            "main",
            vec![
                test_window("0", "editor", vec![test_pane("%1", manifest_dir)], true),
                test_window("1", "tests", vec![test_pane("%2", manifest_dir)], false),
            ],
        )];
        let groups = group_panes_by_window(&sessions);

        assert_eq!(groups.len(), 2, "each window gets its own group");
        assert_eq!(groups[0].name, "0: editor");
        assert_eq!(groups[1].name, "1: tests");
        assert_eq!(groups[0].panes[0].0.pane_id, "%1");
        assert_eq!(groups[1].panes[0].0.pane_id, "%2");
    }

    #[test]
    fn group_panes_keeps_multiple_panes_of_one_window_together() {
        let manifest_dir = env!("CARGO_MANIFEST_DIR");
        let sessions = vec![test_session(
            "main",
            vec![test_window(
                "3",
                "work",
                vec![test_pane("%1", manifest_dir), test_pane("%2", "/tmp/other")],
                true,
            )],
        )];
        let groups = group_panes_by_window(&sessions);

        assert_eq!(groups.len(), 1);
        assert_eq!(groups[0].panes.len(), 2);
    }

    #[test]
    fn group_panes_preserves_session_and_window_order() {
        // Groups must come out in input order, not sorted by name, so the tree
        // matches `choose-tree`. Ordering itself is `finalize_sessions`' job.
        let sessions = vec![
            test_session(
                "alpha",
                vec![
                    test_window("0", "zzz", vec![test_pane("%1", "/tmp/a")], true),
                    test_window("1", "aaa", vec![test_pane("%2", "/tmp/b")], false),
                ],
            ),
            test_session(
                "beta",
                vec![test_window(
                    "9",
                    "mmm",
                    vec![test_pane("%3", "/tmp/c")],
                    false,
                )],
            ),
        ];
        let groups = group_panes_by_window(&sessions);

        let names: Vec<&str> = groups.iter().map(|g| g.name.as_str()).collect();
        assert_eq!(names, vec!["0: zzz", "1: aaa", "9: mmm"]);
        let owners: Vec<&str> = groups.iter().map(|g| g.session_name.as_str()).collect();
        assert_eq!(owners, vec!["alpha", "alpha", "beta"]);
    }

    #[test]
    fn group_panes_skips_windows_with_no_panes() {
        let sessions = vec![test_session(
            "main",
            vec![
                test_window("0", "empty", vec![], true),
                test_window("1", "busy", vec![test_pane("%1", "/tmp/a")], false),
            ],
        )];
        let groups = group_panes_by_window(&sessions);

        assert_eq!(groups.len(), 1);
        assert_eq!(groups[0].name, "1: busy");
    }

    #[test]
    fn group_panes_has_focus_from_active_window_and_pane() {
        let manifest_dir = env!("CARGO_MANIFEST_DIR");
        let mut pane = test_pane("%1", manifest_dir);
        pane.pane_active = true;

        let sessions = vec![test_session(
            "main",
            vec![test_window("0", "w", vec![pane], true)],
        )];
        let groups = group_panes_by_window(&sessions);

        assert!(
            groups[0].has_focus,
            "active pane in active window should set has_focus"
        );
    }

    #[test]
    fn group_panes_no_focus_when_window_inactive() {
        let manifest_dir = env!("CARGO_MANIFEST_DIR");
        let mut pane = test_pane("%1", manifest_dir);
        pane.pane_active = true;

        let sessions = vec![test_session(
            "main",
            vec![test_window("0", "w", vec![pane], false)],
        )];
        let groups = group_panes_by_window(&sessions);

        assert!(
            !groups[0].has_focus,
            "active pane in inactive window should not set has_focus"
        );
    }

    // ─── repo_names / spawn_repo_root tests ─────────────────────────

    fn group_with(panes: Vec<(PaneInfo, PaneGitInfo)>) -> RepoGroup {
        RepoGroup {
            panes,
            ..Default::default()
        }
    }

    fn git_at(root: &str) -> PaneGitInfo {
        PaneGitInfo {
            repo_root: Some(root.into()),
            ..Default::default()
        }
    }

    #[test]
    fn repo_names_dedupes_and_keeps_encounter_order() {
        let group = group_with(vec![
            (test_pane("%1", "/src/beta"), git_at("/src/beta")),
            (test_pane("%2", "/src/alpha"), git_at("/src/alpha")),
            (test_pane("%3", "/src/beta"), git_at("/src/beta")),
        ]);
        assert_eq!(group.repo_names(), vec!["beta", "alpha"]);
    }

    #[test]
    fn repo_names_falls_back_to_cwd_basename_outside_a_repo() {
        let group = group_with(vec![(
            test_pane("%1", "/tmp/no-git-here"),
            PaneGitInfo::default(),
        )]);
        assert_eq!(group.repo_names(), vec!["no-git-here"]);
    }

    #[test]
    fn repo_names_skips_panes_with_no_path() {
        let group = group_with(vec![(test_pane("%1", ""), PaneGitInfo::default())]);
        assert!(group.repo_names().is_empty());
    }

    #[test]
    fn spawn_repo_root_when_window_agrees_on_one_repo() {
        let group = group_with(vec![
            (test_pane("%1", "/src/app"), git_at("/src/app")),
            (test_pane("%2", "/src/app/sub"), git_at("/src/app")),
        ]);
        assert_eq!(group.spawn_repo_root().as_deref(), Some("/src/app"));
    }

    #[test]
    fn spawn_repo_root_none_when_window_straddles_repos() {
        let group = group_with(vec![
            (test_pane("%1", "/src/app"), git_at("/src/app")),
            (test_pane("%2", "/src/lib"), git_at("/src/lib")),
        ]);
        assert!(group.spawn_repo_root().is_none());
    }

    #[test]
    fn spawn_repo_root_none_when_any_pane_is_outside_a_repo() {
        let group = group_with(vec![
            (test_pane("%1", "/src/app"), git_at("/src/app")),
            (test_pane("%2", "/tmp"), PaneGitInfo::default()),
        ]);
        assert!(group.spawn_repo_root().is_none());
    }
}

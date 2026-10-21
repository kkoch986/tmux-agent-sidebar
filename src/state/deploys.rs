use std::collections::HashSet;

use super::AppState;

impl AppState {
    /// Claude accounts with a session open in some pane, one entry per
    /// config dir, sorted by label. Published to the usage poll thread.
    pub fn usage_accounts(&self) -> Vec<crate::usage::AccountRef> {
        let mut accounts: Vec<crate::usage::AccountRef> = self
            .repo_groups
            .iter()
            .flat_map(|g| g.panes.iter())
            .filter_map(|(pane, _)| pane.claude.as_ref())
            .filter(|c| !c.config_dir.is_empty())
            .map(|c| crate::usage::AccountRef {
                account: c.account.clone(),
                config_dir: c.config_dir.clone(),
            })
            .collect();
        accounts.sort();
        accounts.dedup_by(|a, b| a.config_dir == b.config_dir);
        accounts
    }

    /// Distinct git repo roots across every pane currently in the sidebar.
    /// Published to the deploy poll thread, which turns each into an
    /// `owner/name` slug and asks GitHub for its deploy runs.
    pub fn repo_roots(&self) -> Vec<String> {
        let mut seen = HashSet::new();
        let mut roots = Vec::new();
        for group in &self.repo_groups {
            for (_, git) in &group.panes {
                let Some(root) = git.repo_root.as_ref() else {
                    continue;
                };
                if seen.insert(root.clone()) {
                    roots.push(root.clone());
                }
            }
        }
        roots
    }
}

#[cfg(test)]
mod tests {
    use crate::group::{PaneGitInfo, RepoGroup};
    use crate::state::AppState;
    use crate::tmux::{AgentType, PaneInfo, PaneStatus, PermissionMode, WorktreeMetadata};

    fn pane(id: &str) -> PaneInfo {
        PaneInfo {
            pane_id: id.into(),
            pane_active: false,
            status: PaneStatus::Running,
            attention: false,
            agent: AgentType::Claude,
            path: "/tmp".into(),
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

    fn git_in(root: Option<&str>) -> PaneGitInfo {
        PaneGitInfo {
            repo_root: root.map(Into::into),
            ..Default::default()
        }
    }

    #[test]
    fn repo_roots_dedupes_and_skips_panes_without_a_repo() {
        let mut state = AppState::new("%99".into());
        state.repo_groups = vec![
            RepoGroup {
                name: "0: listing".into(),
                session_name: "main".into(),
                has_focus: true,
                panes: vec![
                    (pane("%1"), git_in(Some("/src/listing"))),
                    (pane("%2"), git_in(Some("/src/listing"))),
                    (pane("%3"), git_in(None)),
                ],
            },
            RepoGroup {
                name: "1: etp".into(),
                session_name: "main".into(),
                has_focus: false,
                panes: vec![(pane("%4"), git_in(Some("/src/etp")))],
            },
        ];

        assert_eq!(state.repo_roots(), vec!["/src/listing", "/src/etp"]);
    }

    #[test]
    fn repo_roots_empty_without_panes() {
        let state = AppState::new("%99".into());
        assert!(state.repo_roots().is_empty());
    }
}

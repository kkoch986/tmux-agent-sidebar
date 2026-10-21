use std::collections::{HashMap, HashSet};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, Receiver};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use crate::deploy::{self, DeployConfig, DeployRun, SlugCache};
use crate::git::{self, GitData};
use crate::session;
use crate::state::{AppState, BottomTab};
use crate::tmux;
use crate::usage::{self, AccountRef, AccountUsage};
use crate::version::{self, UpdateNotice};

/// Channels and shared flags produced by [`spawn`] that the main event loop
/// drains every tick.
pub(super) struct Workers {
    pub git_rx: Receiver<GitData>,
    pub session_rx: Receiver<HashMap<String, String>>,
    pub version_rx: Receiver<UpdateNotice>,
    pub deploy_rx: Receiver<Vec<DeployRun>>,
    /// Repo roots the deploy thread should poll. The main loop overwrites this
    /// after each refresh; the thread reads it at the top of every cycle.
    pub deploy_repos: Arc<Mutex<Vec<String>>>,
    pub usage_rx: Receiver<Vec<AccountUsage>>,
    /// Accounts the usage thread should look up, refreshed like `deploy_repos`.
    pub usage_accounts: Arc<Mutex<Vec<AccountRef>>>,
    pub git_tab_active: Arc<AtomicBool>,
}

/// Spawn the background threads (git polling, session-name polling, version
/// notice fetch) that feed the event loop.
pub(super) fn spawn(state: &AppState) -> Workers {
    let (git_tx, git_rx) = mpsc::channel::<GitData>();
    let (session_tx, session_rx) = mpsc::channel::<HashMap<String, String>>();
    let (version_tx, version_rx) = mpsc::channel::<UpdateNotice>();
    let (deploy_tx, deploy_rx) = mpsc::channel::<Vec<DeployRun>>();
    let deploy_repos = Arc::new(Mutex::new(state.repo_roots()));
    let deploy_repos_clone = Arc::clone(&deploy_repos);
    let tmux_pane_clone = state.tmux_pane.clone();
    let git_tab_active = Arc::new(AtomicBool::new(state.bottom_tab == BottomTab::GitStatus));
    let git_tab_flag = Arc::clone(&git_tab_active);
    std::thread::spawn(move || {
        git_poll_loop(&tmux_pane_clone, &git_tx, &git_tab_flag);
    });
    std::thread::spawn(move || {
        session_poll_loop(&session_tx);
    });
    std::thread::spawn(move || {
        if let Some(notice) = version::fetch_update_notice() {
            let _ = version_tx.send(notice);
        }
    });
    std::thread::spawn(move || {
        deploy_poll_loop(&deploy_tx, &deploy_repos_clone);
    });
    let (usage_tx, usage_rx) = mpsc::channel::<Vec<AccountUsage>>();
    let usage_accounts = Arc::new(Mutex::new(state.usage_accounts()));
    let usage_accounts_clone = Arc::clone(&usage_accounts);
    std::thread::spawn(move || {
        usage_poll_loop(&usage_tx, &usage_accounts_clone);
    });

    Workers {
        git_rx,
        session_rx,
        version_rx,
        deploy_rx,
        deploy_repos,
        usage_rx,
        usage_accounts,
        git_tab_active,
    }
}

/// Usage polling thread. Looks accounts up every [`USAGE_POLL_INTERVAL`],
/// and straight away when the set of accounts in use changes, so a newly
/// opened account shows without waiting out the interval.
pub(super) fn usage_poll_loop(
    tx: &mpsc::Sender<Vec<AccountUsage>>,
    accounts: &Mutex<Vec<AccountRef>>,
) {
    const USAGE_POLL_INTERVAL: Duration = Duration::from_secs(120);
    const CHECK_INTERVAL: Duration = Duration::from_secs(5);
    let mut last: Option<(Vec<AccountRef>, std::time::Instant)> = None;
    loop {
        let current = accounts.lock().map(|a| a.clone()).unwrap_or_default();
        let due = match &last {
            None => true,
            Some((prev, at)) => *prev != current || at.elapsed() >= USAGE_POLL_INTERVAL,
        };
        if due {
            let now = crate::time::now_epoch_secs();
            let usage = current
                .iter()
                .map(|account| usage::fetch_account(account, now))
                .collect();
            if tx.send(usage).is_err() {
                return;
            }
            last = Some((current, std::time::Instant::now()));
        }
        std::thread::sleep(CHECK_INTERVAL);
    }
}

/// Deploy polling thread. Every [`DEPLOY_POLL_INTERVAL`] it re-reads the
/// tmux options (so toggling `@sidebar_deploys` takes effect without a
/// restart) and asks GitHub for each repo's deploy runs. Every hop here is a
/// subprocess or a network call, which is exactly why none of it may run on
/// the render thread.
pub(super) fn deploy_poll_loop(tx: &mpsc::Sender<Vec<DeployRun>>, repos: &Mutex<Vec<String>>) {
    const DEPLOY_POLL_INTERVAL: Duration = Duration::from_secs(30);
    let mut slugs = SlugCache::default();
    loop {
        let config = DeployConfig::from_options(&tmux::get_all_global_options());
        let roots = repos.lock().map(|r| r.clone()).unwrap_or_default();
        let runs = if config.enabled {
            collect_deploy_runs(&roots, &mut slugs, &config)
        } else {
            Vec::new()
        };
        if tx.send(runs).is_err() {
            return;
        }
        std::thread::sleep(DEPLOY_POLL_INTERVAL);
    }
}

/// Merge the deploy runs of every repo into one capped, sorted list.
/// Worktrees of the same repo resolve to a single slug, so poll it once.
fn collect_deploy_runs(
    repo_roots: &[String],
    slugs: &mut SlugCache,
    config: &DeployConfig,
) -> Vec<DeployRun> {
    let now = crate::time::now_epoch_secs();
    let mut polled: HashSet<String> = HashSet::new();
    let mut runs = Vec::new();
    for root in repo_roots {
        let Some(slug) = slugs.resolve(root) else {
            continue;
        };
        if !polled.insert(slug.clone()) {
            continue;
        }
        if let Some(repo_runs) = deploy::fetch_repo_runs(&slug, config, now) {
            runs.extend(repo_runs);
        }
    }
    deploy::sort_and_cap(runs, config.max_rows)
}

/// Session name polling thread. Scans `~/.claude/sessions/*.json` every 10
/// seconds so the main TUI thread never performs blocking filesystem I/O
/// to refresh `/rename`-assigned labels.
pub(super) fn session_poll_loop(tx: &mpsc::Sender<HashMap<String, String>>) {
    loop {
        std::thread::sleep(Duration::from_secs(10));
        let names = session::scan_session_names();
        if tx.send(names).is_err() {
            return;
        }
    }
}

/// Git data polling thread. Fetches git status every 2 seconds while the Git
/// tab is active. Skips fetching when the tab is not visible. PR numbers go
/// through an in-memory `(path, branch)`-keyed cache so `gh pr view` (the only
/// hop that costs GitHub API quota) runs at most once per `PR_CACHE_TTL`
/// instead of every tick.
pub(super) fn git_poll_loop(tmux_pane: &str, git_tx: &mpsc::Sender<GitData>, active: &AtomicBool) {
    let mut last_path: Option<String> = None;
    let mut pr_cache = git::PrCache::new();
    loop {
        std::thread::sleep(Duration::from_secs(2));

        if !active.load(Ordering::Relaxed) {
            continue;
        }

        // When the sidebar has focus, focused_pane_path returns None.
        // Reuse the last known path so git data keeps updating.
        if let Some(p) = tmux::focused_pane_path(tmux_pane) {
            last_path = Some(p);
        }
        if let Some(ref path) = last_path {
            let mut data = git::fetch_git_data(path);
            data.pr_number = pr_cache.get_or_fetch(
                path,
                &data.branch,
                std::time::Instant::now(),
                git::fetch_pr_number,
            );
            if git_tx.send(data).is_err() {
                return;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_git_poll_skips_when_inactive() {
        let active = Arc::new(AtomicBool::new(false));
        let (tx, rx) = mpsc::channel::<GitData>();

        let flag = Arc::clone(&active);
        let handle = std::thread::spawn(move || {
            // Simulate the poll loop check without actually sleeping 2s
            for _ in 0..3 {
                if !flag.load(Ordering::Relaxed) {
                    continue;
                }
                let _ = tx.send(GitData::default());
            }
        });

        handle.join().unwrap();
        // No data should have been sent since active=false
        assert!(
            rx.try_recv().is_err(),
            "should not poll when git tab is inactive"
        );
    }

    #[test]
    fn test_git_poll_sends_when_active() {
        let active = Arc::new(AtomicBool::new(true));
        let (tx, rx) = mpsc::channel::<GitData>();

        let flag = Arc::clone(&active);
        let handle = std::thread::spawn(move || {
            // active=true, so it should send
            if flag.load(Ordering::Relaxed) {
                let _ = tx.send(GitData::default());
            }
        });

        handle.join().unwrap();
        assert!(rx.try_recv().is_ok(), "should poll when git tab is active");
    }

    #[test]
    fn test_git_poll_reacts_to_flag_change() {
        let active = Arc::new(AtomicBool::new(false));
        let (tx, rx) = mpsc::channel::<GitData>();

        // Initially inactive
        assert!(!active.load(Ordering::Relaxed));

        // Switch to active
        active.store(true, Ordering::Relaxed);

        let flag = Arc::clone(&active);
        let handle = std::thread::spawn(move || {
            if flag.load(Ordering::Relaxed) {
                let _ = tx.send(GitData::default());
            }
        });

        handle.join().unwrap();
        assert!(
            rx.try_recv().is_ok(),
            "should poll after flag switches to active"
        );
    }

    #[test]
    fn test_git_poll_stops_on_sender_closed() {
        let active = AtomicBool::new(true);
        let (tx, rx) = mpsc::channel::<GitData>();
        drop(rx); // Close receiver

        let result = tx.send(GitData::default());
        assert!(result.is_err(), "send should fail when receiver is dropped");

        // Verify the flag check pattern used in git_poll_loop
        assert!(active.load(Ordering::Relaxed));
    }
}

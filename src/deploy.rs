//! GitHub Actions deploy tracking for the sidebar's Deploys section.
//!
//! Every repo represented by a pane in the sidebar is polled through
//! `gh run list`, and runs whose workflow name matches the configured pattern
//! (`deploy` by default) are surfaced: everything currently queued or running,
//! plus recently finished runs so a deploy that just went red is still visible.
//! All work here happens on the deploy poll thread — `gh` is a network call and
//! must never touch the render loop.

use std::collections::HashMap;
use std::io::Read;
use std::process::{Command, Stdio};
use std::time::Duration;

/// Per-repo `gh` deadline. A wedged `gh` (expired auth prompt, dead network)
/// must not stall the whole poll cycle.
const GH_TIMEOUT: Duration = Duration::from_secs(15);

/// Runs fetched per repo before filtering. Deploy workflows are a minority of
/// runs in a busy repo, so look back far enough to still see the last one.
const RUNS_PER_REPO: usize = 20;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DeployStatus {
    Queued,
    Running,
    Success,
    Failure,
    Cancelled,
}

impl DeployStatus {
    pub fn is_active(self) -> bool {
        matches!(self, DeployStatus::Queued | DeployStatus::Running)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DeployRun {
    /// Repo name without the owner (`listing`), which is all that fits.
    pub repo: String,
    pub workflow: String,
    /// Tag or branch the run was dispatched from — for `/deploy` that is the
    /// release tag, which is the single most useful identifier here.
    pub git_ref: String,
    pub status: DeployStatus,
    pub started_at: u64,
    pub updated_at: u64,
    pub url: String,
}

impl DeployRun {
    /// Seconds the run has been going (active) or ran for (finished).
    pub fn elapsed_secs(&self, now: u64) -> u64 {
        let end = if self.status.is_active() {
            now
        } else {
            self.updated_at
        };
        end.saturating_sub(self.started_at)
    }
}

/// Poll settings read from tmux options once per cycle.
#[derive(Debug, Clone)]
pub struct DeployConfig {
    pub enabled: bool,
    /// Case-insensitive substring a run's workflow name must contain.
    pub workflow_match: String,
    /// Case-insensitive substrings that disqualify a workflow even when it
    /// matched. `deploy-mcp` ships a debug binary to zeus on every master
    /// commit, so it fires constantly and would crowd out real releases.
    pub workflow_exclude: Vec<String>,
    /// How far back a finished run stays on screen.
    pub recent_window: Duration,
    pub max_rows: usize,
}

impl Default for DeployConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            workflow_match: "deploy".into(),
            workflow_exclude: vec!["deploy-mcp".into()],
            recent_window: Duration::from_secs(60 * 60),
            max_rows: 5,
        }
    }
}

impl DeployConfig {
    pub fn from_options(opts: &HashMap<String, String>) -> Self {
        let defaults = Self::default();
        let enabled = opts
            .get(crate::tmux::SIDEBAR_DEPLOYS)
            .map(|s| s.trim().to_ascii_lowercase())
            .map(|s| matches!(s.as_str(), "on" | "true" | "1" | "yes"))
            .unwrap_or(defaults.enabled);
        let workflow_match = opts
            .get(crate::tmux::SIDEBAR_DEPLOY_MATCH)
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty())
            .unwrap_or(defaults.workflow_match);
        // An explicitly empty value means "exclude nothing", so only an absent
        // option falls back to the default list.
        let workflow_exclude = opts
            .get(crate::tmux::SIDEBAR_DEPLOY_EXCLUDE)
            .map(|s| parse_exclude_list(s))
            .unwrap_or(defaults.workflow_exclude);
        let recent_window = opts
            .get(crate::tmux::SIDEBAR_DEPLOY_RECENT_MINS)
            .and_then(|s| s.trim().parse::<u64>().ok())
            .map(|mins| Duration::from_secs(mins * 60))
            .unwrap_or(defaults.recent_window);
        let max_rows = opts
            .get(crate::tmux::SIDEBAR_DEPLOY_MAX)
            .and_then(|s| s.trim().parse::<usize>().ok())
            .unwrap_or(defaults.max_rows);
        Self {
            enabled,
            workflow_match,
            workflow_exclude,
            recent_window,
            max_rows,
        }
    }

    /// Whether a workflow name counts as a deploy: it must contain the match
    /// substring and none of the excluded ones.
    fn accepts_workflow(&self, workflow: &str) -> bool {
        let name = workflow.to_ascii_lowercase();
        name.contains(&self.workflow_match.to_ascii_lowercase())
            && !self
                .workflow_exclude
                .iter()
                .any(|excluded| name.contains(&excluded.to_ascii_lowercase()))
    }
}

/// Comma-separated exclude list, e.g. `deploy-mcp, deploy-docs`.
fn parse_exclude_list(raw: &str) -> Vec<String> {
    raw.split(',')
        .map(|part| part.trim())
        .filter(|part| !part.is_empty())
        .map(str::to_string)
        .collect()
}

/// Resolve `owner/name` for a repo root by reading its `origin` remote.
/// Results are cached by the caller — a repo's slug does not change.
pub fn repo_slug(repo_root: &str) -> Option<String> {
    let url = crate::git::run_git_capture(repo_root, &["remote", "get-url", "origin"]).ok()?;
    slug_from_remote_url(&url)
}

/// `git@github.com:hdtradeservices/listing.git` → `hdtradeservices/listing`.
/// Non-GitHub remotes return `None`; `gh run list` has nothing to say on them.
pub fn slug_from_remote_url(url: &str) -> Option<String> {
    let normalized = crate::git::normalize_git_url(url);
    let rest = normalized
        .split_once("github.com/")
        .map(|(_, rest)| rest)?
        .trim_end_matches('/');
    let mut parts = rest.split('/');
    let owner = parts.next()?;
    let name = parts.next()?;
    if owner.is_empty() || name.is_empty() {
        return None;
    }
    Some(format!("{owner}/{name}"))
}

/// Fetch matching deploy runs for one repo. `None` on any failure (no `gh`, no
/// auth, timeout) so the caller can leave the previous snapshot in place.
pub fn fetch_repo_runs(slug: &str, config: &DeployConfig, now: u64) -> Option<Vec<DeployRun>> {
    let json = run_gh(&[
        "run",
        "list",
        "--repo",
        slug,
        "--limit",
        &RUNS_PER_REPO.to_string(),
        "--json",
        "status,conclusion,workflowName,headBranch,displayTitle,createdAt,updatedAt,url",
    ])?;
    Some(parse_runs(&json, slug, config, now))
}

pub fn parse_runs(json: &str, slug: &str, config: &DeployConfig, now: u64) -> Vec<DeployRun> {
    let Ok(serde_json::Value::Array(items)) = serde_json::from_str::<serde_json::Value>(json)
    else {
        return Vec::new();
    };
    let repo = slug.rsplit('/').next().unwrap_or(slug).to_string();

    let mut out = Vec::new();
    for item in items {
        let workflow = item["workflowName"]
            .as_str()
            .unwrap_or_default()
            .to_string();
        if !config.accepts_workflow(&workflow) {
            continue;
        }
        let status = parse_status(
            item["status"].as_str().unwrap_or_default(),
            item["conclusion"].as_str().unwrap_or_default(),
        );
        let updated_at = parse_rfc3339_epoch(item["updatedAt"].as_str().unwrap_or_default());
        if !status.is_active() && now.saturating_sub(updated_at) > config.recent_window.as_secs() {
            continue;
        }
        let git_ref = item["headBranch"]
            .as_str()
            .filter(|s| !s.is_empty())
            .or_else(|| item["displayTitle"].as_str())
            .unwrap_or_default()
            .to_string();
        out.push(DeployRun {
            repo: repo.clone(),
            workflow,
            git_ref,
            status,
            started_at: parse_rfc3339_epoch(item["createdAt"].as_str().unwrap_or_default()),
            updated_at,
            url: item["url"].as_str().unwrap_or_default().to_string(),
        });
    }
    out
}

/// Active runs first, then the most recently finished. `gh` already returns
/// newest-first, but runs are merged across repos so they need re-sorting.
pub fn sort_and_cap(mut runs: Vec<DeployRun>, max_rows: usize) -> Vec<DeployRun> {
    runs.sort_by(|a, b| {
        b.status
            .is_active()
            .cmp(&a.status.is_active())
            .then_with(|| b.updated_at.cmp(&a.updated_at))
    });
    runs.truncate(max_rows);
    runs
}

fn parse_status(status: &str, conclusion: &str) -> DeployStatus {
    match status {
        "completed" => match conclusion {
            "success" => DeployStatus::Success,
            "cancelled" | "skipped" | "neutral" | "stale" => DeployStatus::Cancelled,
            _ => DeployStatus::Failure,
        },
        "in_progress" => DeployStatus::Running,
        _ => DeployStatus::Queued,
    }
}

/// Parse the `2026-08-17T15:41:02Z` shape `gh` emits into epoch seconds.
/// Anything else yields `0`, which reads as "unknown age" downstream rather
/// than pulling in a date crate for one format.
pub fn parse_rfc3339_epoch(ts: &str) -> u64 {
    let Some((date, rest)) = ts.split_once('T') else {
        return 0;
    };
    let mut d = date.split('-');
    let (Some(y), Some(m), Some(day)) = (d.next(), d.next(), d.next()) else {
        return 0;
    };
    let time = rest.trim_end_matches('Z');
    let time = time.split(['+', '.']).next().unwrap_or(time);
    let mut t = time.split(':');
    let (Some(hh), Some(mm), Some(ss)) = (t.next(), t.next(), t.next()) else {
        return 0;
    };
    let (Ok(y), Ok(m), Ok(day), Ok(hh), Ok(mm), Ok(ss)) = (
        y.parse::<i64>(),
        m.parse::<i64>(),
        day.parse::<i64>(),
        hh.parse::<u64>(),
        mm.parse::<u64>(),
        ss.parse::<u64>(),
    ) else {
        return 0;
    };
    let days = days_from_civil(y, m, day);
    (days * 86_400 + (hh * 3600 + mm * 60 + ss) as i64).max(0) as u64
}

/// Days since 1970-01-01 (Howard Hinnant's `days_from_civil`).
fn days_from_civil(y: i64, m: i64, d: i64) -> i64 {
    let y = if m <= 2 { y - 1 } else { y };
    let era = if y >= 0 { y } else { y - 399 } / 400;
    let yoe = y - era * 400;
    let mp = (m + 9) % 12;
    let doy = (153 * mp + 2) / 5 + d - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146_097 + doe - 719_468
}

fn run_gh(args: &[&str]) -> Option<String> {
    let mut child = Command::new("gh")
        .env("GIT_OPTIONAL_LOCKS", "0")
        .args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .ok()?;
    let mut stdout = child.stdout.take()?;
    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let mut buf = String::new();
        let _ = tx.send(stdout.read_to_string(&mut buf).ok().map(|_| buf));
    });
    match rx.recv_timeout(GH_TIMEOUT) {
        Ok(Some(out)) => {
            let ok = matches!(child.wait(), Ok(status) if status.success());
            ok.then_some(out)
        }
        Ok(None) => {
            let _ = child.wait();
            None
        }
        Err(_) => {
            let _ = child.kill();
            let mut child = child;
            std::thread::spawn(move || {
                let _ = child.wait();
            });
            None
        }
    }
}

/// Slug lookup cache owned by the poll thread.
#[derive(Debug, Default)]
pub struct SlugCache {
    entries: HashMap<String, Option<String>>,
}

impl SlugCache {
    /// Cached both ways: a repo with no GitHub remote is remembered as a miss
    /// so the poll thread does not re-run `git remote get-url` every cycle.
    pub fn resolve(&mut self, repo_root: &str) -> Option<String> {
        if let Some(cached) = self.entries.get(repo_root) {
            return cached.clone();
        }
        let slug = repo_slug(repo_root);
        self.entries.insert(repo_root.to_string(), slug.clone());
        slug
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn config() -> DeployConfig {
        DeployConfig::default()
    }

    #[test]
    fn slug_from_remote_url_handles_ssh_and_https() {
        assert_eq!(
            slug_from_remote_url("git@github.com:hdtradeservices/listing.git").as_deref(),
            Some("hdtradeservices/listing")
        );
        assert_eq!(
            slug_from_remote_url("https://github.com/hdtradeservices/etp").as_deref(),
            Some("hdtradeservices/etp")
        );
        assert_eq!(slug_from_remote_url("git@gitlab.com:acme/thing.git"), None);
    }

    #[test]
    fn parse_rfc3339_epoch_matches_known_instant() {
        assert_eq!(parse_rfc3339_epoch("1970-01-01T00:00:00Z"), 0);
        assert_eq!(parse_rfc3339_epoch("2026-08-17T15:41:02Z"), 1_786_981_262);
        assert_eq!(parse_rfc3339_epoch("garbage"), 0);
    }

    #[test]
    fn parse_runs_keeps_active_and_drops_stale_completions() {
        let now = parse_rfc3339_epoch("2026-08-17T16:00:00Z");
        let json = r#"[
          {"status":"in_progress","conclusion":"","workflowName":"deploy","headBranch":"v1.2.3",
           "displayTitle":"ship it","createdAt":"2026-08-17T15:55:00Z","updatedAt":"2026-08-17T15:55:00Z","url":"u1"},
          {"status":"completed","conclusion":"failure","workflowName":"deploy","headBranch":"v1.2.2",
           "displayTitle":"oops","createdAt":"2026-08-17T15:30:00Z","updatedAt":"2026-08-17T15:40:00Z","url":"u2"},
          {"status":"completed","conclusion":"success","workflowName":"deploy","headBranch":"v1.0.0",
           "displayTitle":"old","createdAt":"2026-08-16T10:00:00Z","updatedAt":"2026-08-16T10:10:00Z","url":"u3"},
          {"status":"completed","conclusion":"success","workflowName":"go-ci","headBranch":"main",
           "displayTitle":"ci","createdAt":"2026-08-17T15:58:00Z","updatedAt":"2026-08-17T15:59:00Z","url":"u4"}
        ]"#;

        let runs = parse_runs(json, "hdtradeservices/listing", &config(), now);

        let refs: Vec<&str> = runs.iter().map(|r| r.git_ref.as_str()).collect();
        assert_eq!(refs, vec!["v1.2.3", "v1.2.2"], "stale + non-deploy dropped");
        assert_eq!(runs[0].repo, "listing");
        assert_eq!(runs[0].status, DeployStatus::Running);
        assert_eq!(runs[1].status, DeployStatus::Failure);
    }

    #[test]
    fn parse_runs_drops_excluded_workflows() {
        let now = parse_rfc3339_epoch("2026-08-17T16:00:00Z");
        let json = r#"[
          {"status":"in_progress","conclusion":"","workflowName":"deploy-mcp","headBranch":"master",
           "displayTitle":"mcp","createdAt":"2026-08-17T15:58:00Z","updatedAt":"2026-08-17T15:58:00Z","url":"u1"},
          {"status":"in_progress","conclusion":"","workflowName":"deploy","headBranch":"v1.2.3",
           "displayTitle":"live","createdAt":"2026-08-17T15:55:00Z","updatedAt":"2026-08-17T15:55:00Z","url":"u2"}
        ]"#;

        let runs = parse_runs(json, "hdtradeservices/listing", &config(), now);

        let workflows: Vec<&str> = runs.iter().map(|r| r.workflow.as_str()).collect();
        assert_eq!(
            workflows,
            vec!["deploy"],
            "deploy-mcp is excluded by default"
        );
    }

    #[test]
    fn accepts_workflow_honors_an_empty_exclude_list() {
        let mut config = DeployConfig::default();
        assert!(!config.accepts_workflow("deploy-mcp"));

        config.workflow_exclude.clear();
        assert!(config.accepts_workflow("deploy-mcp"));
    }

    #[test]
    fn parse_exclude_list_splits_and_trims() {
        assert_eq!(
            parse_exclude_list(" deploy-mcp , deploy-docs ,,"),
            vec!["deploy-mcp", "deploy-docs"]
        );
        assert!(parse_exclude_list("  ").is_empty());
    }

    #[test]
    fn parse_runs_falls_back_to_display_title_without_a_ref() {
        let now = parse_rfc3339_epoch("2026-08-17T16:00:00Z");
        let json = r#"[{"status":"queued","conclusion":"","workflowName":"deploy","headBranch":"",
          "displayTitle":"manual dispatch","createdAt":"2026-08-17T15:59:00Z",
          "updatedAt":"2026-08-17T15:59:00Z","url":"u"}]"#;

        let runs = parse_runs(json, "hdtradeservices/catalog", &config(), now);

        assert_eq!(runs.len(), 1);
        assert_eq!(runs[0].git_ref, "manual dispatch");
        assert_eq!(runs[0].status, DeployStatus::Queued);
    }

    #[test]
    fn sort_and_cap_puts_active_first_then_newest() {
        let run = |git_ref: &str, status, updated_at| DeployRun {
            repo: "listing".into(),
            workflow: "deploy".into(),
            git_ref: git_ref.into(),
            status,
            started_at: 0,
            updated_at,
            url: String::new(),
        };
        let runs = vec![
            run("done-old", DeployStatus::Success, 100),
            run("active", DeployStatus::Running, 50),
            run("done-new", DeployStatus::Failure, 200),
        ];

        let sorted = sort_and_cap(runs, 2);

        let refs: Vec<&str> = sorted.iter().map(|r| r.git_ref.as_str()).collect();
        assert_eq!(refs, vec!["active", "done-new"]);
    }

    #[test]
    fn elapsed_secs_freezes_for_finished_runs() {
        let mut run = DeployRun {
            repo: "listing".into(),
            workflow: "deploy".into(),
            git_ref: "v1".into(),
            status: DeployStatus::Running,
            started_at: 1_000,
            updated_at: 1_060,
            url: String::new(),
        };
        assert_eq!(run.elapsed_secs(1_500), 500, "active runs count up to now");

        run.status = DeployStatus::Success;
        assert_eq!(run.elapsed_secs(1_500), 60, "finished runs keep their span");
    }

    #[test]
    fn config_from_options_reads_overrides() {
        let mut opts = HashMap::new();
        opts.insert(crate::tmux::SIDEBAR_DEPLOYS.to_string(), "off".to_string());
        opts.insert(
            crate::tmux::SIDEBAR_DEPLOY_MATCH.to_string(),
            "release".to_string(),
        );
        opts.insert(
            crate::tmux::SIDEBAR_DEPLOY_RECENT_MINS.to_string(),
            "15".to_string(),
        );
        opts.insert(crate::tmux::SIDEBAR_DEPLOY_MAX.to_string(), "3".to_string());
        opts.insert(
            crate::tmux::SIDEBAR_DEPLOY_EXCLUDE.to_string(),
            "nightly,canary".to_string(),
        );

        let config = DeployConfig::from_options(&opts);

        assert!(!config.enabled);
        assert_eq!(config.workflow_match, "release");
        assert_eq!(config.workflow_exclude, vec!["nightly", "canary"]);
        assert_eq!(config.recent_window, Duration::from_secs(900));
        assert_eq!(config.max_rows, 3);
    }

    #[test]
    fn config_from_options_defaults_when_unset() {
        let config = DeployConfig::from_options(&HashMap::new());
        assert!(config.enabled);
        assert_eq!(config.workflow_match, "deploy");
        assert_eq!(config.workflow_exclude, vec!["deploy-mcp"]);
        assert_eq!(config.max_rows, 5);
    }

    #[test]
    fn config_from_options_treats_an_empty_exclude_as_exclude_nothing() {
        let mut opts = HashMap::new();
        opts.insert(
            crate::tmux::SIDEBAR_DEPLOY_EXCLUDE.to_string(),
            "".to_string(),
        );

        let config = DeployConfig::from_options(&opts);

        assert!(config.workflow_exclude.is_empty());
    }
}

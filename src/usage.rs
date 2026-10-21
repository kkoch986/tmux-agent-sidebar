//! Claude subscription usage per account: how much of the 5-hour and the
//! weekly limit each account has spent, and which limits are maxed out.
//! Read from the same OAuth usage endpoint Claude Code's `/usage` uses,
//! with the token each account already keeps in
//! `<config dir>/.credentials.json`.

use std::io::Write;
use std::path::Path;
use std::process::{Command, Stdio};

use serde_json::Value;

const USAGE_URL: &str = "https://api.anthropic.com/api/oauth/usage";

/// One account's place in a usage window.
#[derive(Debug, Clone, PartialEq)]
pub struct UsageWindow {
    /// Percent of the limit used, 0–100.
    pub used_pct: f64,
    /// When the window resets, in epoch seconds.
    pub resets_at: Option<u64>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct AccountUsage {
    pub account: String,
    pub five_hour: Option<UsageWindow>,
    pub seven_day: Option<UsageWindow>,
    /// Limits at 100% or locked, which block the account until they reset.
    pub maxed: Vec<MaxedLimit>,
    /// Why usage could not be read (`token expired`, `http 429`, …).
    pub error: Option<String>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct MaxedLimit {
    /// `5h`, `weekly`, or a per-model cap such as `Opus weekly`.
    pub label: String,
    pub resets_at: Option<u64>,
}

impl AccountUsage {
    pub fn is_maxed(&self) -> bool {
        !self.maxed.is_empty()
    }
}

/// An account to look up: its label and the config dir holding its token.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub struct AccountRef {
    pub account: String,
    pub config_dir: String,
}

pub const TOKEN_EXPIRED: &str = "token expired";

/// Look an account up. With `refresh` set, an expired token is renewed
/// through `claude auth status` first: an account nobody has run for a
/// few hours otherwise has no usable token.
pub fn fetch_account(account: &AccountRef, now: u64, refresh: bool) -> AccountUsage {
    let mut usage = AccountUsage {
        account: account.account.clone(),
        five_hour: None,
        seven_day: None,
        maxed: Vec::new(),
        error: None,
    };
    let mut result = fetch_body(&account.config_dir, now);
    if refresh && result.as_ref().is_err_and(|e| e == TOKEN_EXPIRED) {
        refresh_token(&account.config_dir);
        result = fetch_body(&account.config_dir, crate::time::now_epoch_secs());
    }
    match result.and_then(|body| parse_usage(&body)) {
        Ok(parsed) => {
            usage.five_hour = parsed.five_hour;
            usage.seven_day = parsed.seven_day;
            usage.maxed = parsed.maxed;
        }
        Err(reason) => usage.error = Some(reason),
    }
    usage
}

/// Let Claude Code renew the account's token the way it does on start.
/// The default `~/.claude` dir must run without `CLAUDE_CONFIG_DIR`, since
/// its profile lives in `~/.claude.json`, outside the dir.
fn refresh_token(config_dir: &str) {
    let mut cmd = Command::new("claude");
    cmd.args(["auth", "status"])
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    let default_dir = std::env::var("HOME").map(|h| format!("{h}/.claude"));
    if default_dir.as_deref() == Ok(config_dir) {
        cmd.env_remove("CLAUDE_CONFIG_DIR");
    } else {
        cmd.env("CLAUDE_CONFIG_DIR", config_dir);
    }
    let Ok(mut child) = cmd.spawn() else {
        return;
    };
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(30);
    while std::time::Instant::now() < deadline {
        if !matches!(child.try_wait(), Ok(None)) {
            return;
        }
        std::thread::sleep(std::time::Duration::from_millis(100));
    }
    let _ = child.kill();
    let _ = child.wait();
}

/// Every logged-in account on the machine: `~/.claude` and each
/// `~/.claude-<name>` dir that holds a `.credentials.json`.
pub fn discover_accounts(home: &Path, default_account: &str) -> Vec<AccountRef> {
    let mut dirs = vec![home.join(".claude")];
    if let Ok(entries) = std::fs::read_dir(home) {
        let mut extra: Vec<_> = entries
            .flatten()
            .map(|e| e.path())
            .filter(|p| {
                p.file_name()
                    .and_then(|n| n.to_str())
                    .is_some_and(|n| n.starts_with(".claude-"))
            })
            .collect();
        extra.sort();
        dirs.extend(extra);
    }
    dirs.into_iter()
        .filter(|d| d.join(".credentials.json").is_file())
        .map(|d| {
            let dir = d.to_string_lossy().to_string();
            AccountRef {
                account: account_label(Some(&dir), default_account),
                config_dir: dir,
            }
        })
        .collect()
}

/// `~/.claude-k1` → `k1`; unset or `~/.claude` → `default_account`.
pub fn account_label(config_dir: Option<&str>, default_account: &str) -> String {
    let Some(dir) = config_dir
        .map(|d| d.trim_end_matches('/'))
        .filter(|d| !d.is_empty())
    else {
        return default_account.to_string();
    };
    let base = Path::new(dir)
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or(dir);
    match base.strip_prefix(".claude") {
        Some("") => default_account.to_string(),
        Some(rest) => rest.trim_start_matches(['-', '_', '.']).to_string(),
        None => base.to_string(),
    }
}

fn fetch_body(config_dir: &str, now: u64) -> Result<String, String> {
    let creds_path = format!("{config_dir}/.credentials.json");
    let creds: Value = std::fs::read_to_string(&creds_path)
        .ok()
        .and_then(|s| serde_json::from_str(&s).ok())
        .ok_or("no credentials")?;
    let oauth = &creds["claudeAiOauth"];
    let token = oauth["accessToken"].as_str().ok_or("no token")?;
    // expiresAt is epoch ms. The endpoint refuses an expired token, and
    // Claude Code refreshes it the next time that account runs.
    if oauth["expiresAt"]
        .as_u64()
        .is_some_and(|ms| ms > 0 && ms / 1000 <= now)
    {
        return Err(TOKEN_EXPIRED.into());
    }

    // Headers go in on stdin (`-H @-`) so the token never shows up in `ps`.
    let mut child = Command::new("curl")
        .args([
            "-sS",
            "-m",
            "10",
            "-H",
            "@-",
            "-w",
            "\n%{http_code}",
            USAGE_URL,
        ])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .map_err(|_| "curl not found")?;
    if let Some(mut stdin) = child.stdin.take() {
        let headers = format!("Authorization: Bearer {token}\nanthropic-beta: oauth-2025-04-20\n");
        let _ = stdin.write_all(headers.as_bytes());
    }
    let output = child.wait_with_output().map_err(|_| "curl failed")?;
    if !output.status.success() {
        return Err("network error".into());
    }
    let text = String::from_utf8_lossy(&output.stdout);
    let (body, code) = text.rsplit_once('\n').unwrap_or((&text, ""));
    if code.trim() != "200" {
        return Err(format!("http {}", code.trim()));
    }
    Ok(body.to_string())
}

struct ParsedUsage {
    five_hour: Option<UsageWindow>,
    seven_day: Option<UsageWindow>,
    maxed: Vec<MaxedLimit>,
}

fn parse_usage(body: &str) -> Result<ParsedUsage, String> {
    let json: Value = serde_json::from_str(body).map_err(|_| "bad response")?;
    let window = |key: &str| {
        let w = &json[key];
        w["utilization"].as_f64().map(|used_pct| UsageWindow {
            used_pct,
            resets_at: w["resets_at"].as_str().and_then(parse_rfc3339_utc),
        })
    };
    let five_hour = window("five_hour");
    let seven_day = window("seven_day");
    if five_hour.is_none() && seven_day.is_none() {
        return Err("no usage in response".into());
    }

    // `limits` covers the plan-wide windows plus per-model weekly caps;
    // a limit is maxed at 100% whatever its kind.
    let mut maxed: Vec<MaxedLimit> = json["limits"]
        .as_array()
        .into_iter()
        .flatten()
        .filter(|l| l["percent"].as_f64().is_some_and(|p| p >= 100.0))
        .map(|l| MaxedLimit {
            label: limit_label(l),
            resets_at: l["resets_at"].as_str().and_then(parse_rfc3339_utc),
        })
        .collect();
    // Older responses carry only the two windows; a lock counts too.
    for (key, label) in [("five_hour", "5h"), ("seven_day", "weekly")] {
        let w = &json[key];
        let full = w["utilization"].as_f64().is_some_and(|p| p >= 100.0);
        let locked = !w["locked_reason"].is_null();
        if (full || locked) && !maxed.iter().any(|m| m.label == label) {
            maxed.push(MaxedLimit {
                label: label.to_string(),
                resets_at: w["resets_at"].as_str().and_then(parse_rfc3339_utc),
            });
        }
    }
    Ok(ParsedUsage {
        five_hour,
        seven_day,
        maxed,
    })
}

fn limit_label(limit: &Value) -> String {
    match limit["kind"].as_str() {
        Some("session") => "5h".to_string(),
        Some("weekly_all") => "weekly".to_string(),
        _ => {
            let model = limit["scope"]["model"]["display_name"].as_str();
            match (model, limit["group"].as_str()) {
                (Some(m), Some(g)) => format!("{m} {g}"),
                (Some(m), None) => m.to_string(),
                (None, Some(g)) => g.to_string(),
                (None, None) => "limit".to_string(),
            }
        }
    }
}

/// `2026-10-21T22:50:00.233318+00:00` → epoch seconds. Only the forms the
/// endpoint sends: UTC, written as `Z` or `+00:00`.
fn parse_rfc3339_utc(s: &str) -> Option<u64> {
    let (date, time) = s.split_once('T')?;
    let mut d = date.split('-').map(|p| p.parse::<i64>().ok());
    let (y, m, day) = (d.next()??, d.next()??, d.next()??);
    let time = time.trim_end_matches('Z').trim_end_matches("+00:00");
    let time = time.split('.').next()?;
    let mut t = time.split(':').map(|p| p.parse::<i64>().ok());
    let (hh, mm, ss) = (t.next()??, t.next()??, t.next()??);
    // Days from civil date (Howard Hinnant's algorithm).
    let y = if m <= 2 { y - 1 } else { y };
    let era = y.div_euclid(400);
    let yoe = y - era * 400;
    let mp = (m + 9) % 12;
    let doy = (153 * mp + 2) / 5 + day - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    let days = era * 146_097 + doe - 719_468;
    u64::try_from(days * 86_400 + hh * 3600 + mm * 60 + ss).ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_rfc3339_utc_matches_known_epochs() {
        assert_eq!(parse_rfc3339_utc("1970-01-01T00:00:00Z"), Some(0));
        assert_eq!(
            parse_rfc3339_utc("2026-10-21T22:50:00.233318+00:00"),
            Some(1_792_623_000)
        );
        assert_eq!(parse_rfc3339_utc("garbage"), None);
    }

    #[test]
    fn parse_usage_reads_both_windows() {
        let body = r#"{"five_hour":{"utilization":12.0,"resets_at":"2026-10-21T22:50:00+00:00"},
                       "seven_day":{"utilization":64.0,"resets_at":null}}"#;
        let parsed = parse_usage(body).unwrap();
        assert_eq!(parsed.five_hour.unwrap().used_pct, 12.0);
        let seven = parsed.seven_day.unwrap();
        assert_eq!(seven.used_pct, 64.0);
        assert_eq!(seven.resets_at, None);
        assert!(parsed.maxed.is_empty());
    }

    #[test]
    fn parse_usage_flags_maxed_windows_scoped_caps_and_locks() {
        let body = r#"{
          "five_hour":{"utilization":100.0,"resets_at":"2026-10-21T22:50:00+00:00","locked_reason":null},
          "seven_day":{"utilization":40.0,"resets_at":null,"locked_reason":"weekly_limit"},
          "limits":[
            {"kind":"session","group":"session","percent":100,"resets_at":"2026-10-21T22:50:00+00:00"},
            {"kind":"weekly_scoped","group":"weekly","percent":100,"resets_at":null,
             "scope":{"model":{"display_name":"Opus"}}},
            {"kind":"weekly_all","group":"weekly","percent":40,"resets_at":null}
          ]}"#;
        let labels: Vec<String> = parse_usage(body)
            .unwrap()
            .maxed
            .into_iter()
            .map(|m| m.label)
            .collect();
        assert_eq!(labels, ["5h", "Opus weekly", "weekly"]);
    }

    #[test]
    fn account_label_maps_config_dirs() {
        assert_eq!(account_label(None, "ken"), "ken");
        assert_eq!(account_label(Some("/home/u/.claude"), "ken"), "ken");
        assert_eq!(account_label(Some("/home/u/.claude-k1/"), "ken"), "k1");
        assert_eq!(account_label(Some("/opt/work"), "ken"), "work");
    }

    #[test]
    fn discover_accounts_keeps_logged_in_dirs_only() {
        let home = tempfile::tempdir().unwrap();
        for (dir, creds) in [
            (".claude", true),
            (".claude-k1", true),
            (".claude-k2", false),
        ] {
            let path = home.path().join(dir);
            std::fs::create_dir(&path).unwrap();
            if creds {
                std::fs::write(path.join(".credentials.json"), "{}").unwrap();
            }
        }
        std::fs::write(home.path().join(".claude-accounts"), "ken\nk1\n").unwrap();
        let labels: Vec<String> = discover_accounts(home.path(), "ken")
            .into_iter()
            .map(|a| a.account)
            .collect();
        assert_eq!(labels, ["ken", "k1"]);
    }

    #[test]
    fn parse_usage_rejects_a_body_without_usage() {
        assert!(parse_usage(r#"{"error":"nope"}"#).is_err());
    }
}

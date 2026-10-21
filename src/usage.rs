//! Claude subscription usage per account: how much of the 5-hour and the
//! weekly limit each account in use has spent. Read from the same OAuth
//! usage endpoint Claude Code's `/usage` uses, with the token each account
//! already keeps in `<config dir>/.credentials.json`.

use std::io::Write;
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
    /// Why usage could not be read (`token expired`, `http 429`, …).
    pub error: Option<String>,
}

/// An account to look up: its label and the config dir holding its token.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub struct AccountRef {
    pub account: String,
    pub config_dir: String,
}

pub fn fetch_account(account: &AccountRef, now: u64) -> AccountUsage {
    let mut usage = AccountUsage {
        account: account.account.clone(),
        five_hour: None,
        seven_day: None,
        error: None,
    };
    match fetch_body(&account.config_dir, now).and_then(|body| parse_usage(&body)) {
        Ok((five_hour, seven_day)) => {
            usage.five_hour = five_hour;
            usage.seven_day = seven_day;
        }
        Err(reason) => usage.error = Some(reason),
    }
    usage
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
        return Err("token expired".into());
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

type Windows = (Option<UsageWindow>, Option<UsageWindow>);

fn parse_usage(body: &str) -> Result<Windows, String> {
    let json: Value = serde_json::from_str(body).map_err(|_| "bad response")?;
    let window = |key: &str| {
        let w = &json[key];
        w["utilization"].as_f64().map(|used_pct| UsageWindow {
            used_pct,
            resets_at: w["resets_at"].as_str().and_then(parse_rfc3339_utc),
        })
    };
    let windows = (window("five_hour"), window("seven_day"));
    if windows.0.is_none() && windows.1.is_none() {
        return Err("no usage in response".into());
    }
    Ok(windows)
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
        let (five, seven) = parse_usage(body).unwrap();
        assert_eq!(five.unwrap().used_pct, 12.0);
        let seven = seven.unwrap();
        assert_eq!(seven.used_pct, 64.0);
        assert_eq!(seven.resets_at, None);
    }

    #[test]
    fn parse_usage_rejects_a_body_without_usage() {
        assert!(parse_usage(r#"{"error":"nope"}"#).is_err());
    }
}

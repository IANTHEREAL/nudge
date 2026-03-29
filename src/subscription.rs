use anyhow::{bail, Result};
use chrono::Utc;
use serde::{Deserialize, Serialize};

use crate::cli::{OnArgs, WaitArgs};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Subscription {
    pub id: String,
    pub source: String,
    pub condition: serde_json::Value,
    pub mode: String,       // "wait" or "on"
    pub callback: Option<String>,
    pub status: String,     // "active", "fired", "expired", "cancelled"
    pub created_at: i64,    // epoch seconds
    pub expires_at: Option<i64>,
    pub event_data: Option<serde_json::Value>,
}

impl Subscription {
    pub fn from_wait_args(args: &WaitArgs) -> Result<Self> {
        let condition = parse_condition(&args.source, &args.args, args.repo.as_deref())?;
        let expires_at = args.timeout.as_deref().map(parse_duration_secs).transpose()?
            .map(|d| Utc::now().timestamp() + d);

        Ok(Self {
            id: uuid::Uuid::new_v4().to_string()[..8].to_string(),
            source: args.source.clone(),
            condition,
            mode: "wait".into(),
            callback: None,
            status: "active".into(),
            created_at: Utc::now().timestamp(),
            expires_at,
            event_data: None,
        })
    }

    pub fn from_on_args(args: &OnArgs) -> Result<Self> {
        let condition = parse_condition(&args.source, &args.args, args.repo.as_deref())?;
        let expires_at = args.timeout.as_deref().map(parse_duration_secs).transpose()?
            .map(|d| Utc::now().timestamp() + d);

        Ok(Self {
            id: uuid::Uuid::new_v4().to_string()[..8].to_string(),
            source: args.source.clone(),
            condition,
            mode: "on".into(),
            callback: Some(args.run.clone()),
            status: "active".into(),
            created_at: Utc::now().timestamp(),
            expires_at,
            event_data: None,
        })
    }

    pub fn condition_summary(&self) -> String {
        match self.source.as_str() {
            "timer" => {
                if let Some(dur) = self.condition.get("duration").and_then(|v| v.as_str()) {
                    format!("timer {dur}")
                } else {
                    "timer".into()
                }
            }
            "github" => {
                let kind = self.condition.get("kind").and_then(|v| v.as_str()).unwrap_or("?");
                let number = self.condition.get("number").and_then(|v| v.as_i64()).unwrap_or(0);
                let event = self.condition.get("event").and_then(|v| v.as_str()).unwrap_or("?");
                let repo = self.condition.get("repo").and_then(|v| v.as_str()).unwrap_or("");
                format!("{repo} {kind} #{number} {event}")
            }
            _ => format!("{}", self.condition),
        }
    }

    pub fn is_expired(&self) -> bool {
        if let Some(exp) = self.expires_at {
            Utc::now().timestamp() >= exp
        } else {
            false
        }
    }
}

fn parse_condition(source: &str, args: &[String], repo: Option<&str>) -> Result<serde_json::Value> {
    match source {
        "timer" => {
            let duration = args.first().ok_or_else(|| anyhow::anyhow!("timer requires a duration (e.g., 30m, 2h)"))?;
            let secs = parse_duration_secs(duration)?;
            Ok(serde_json::json!({
                "duration": duration,
                "fire_at": Utc::now().timestamp() + secs,
            }))
        }
        "github" => {
            // nudge wait github pr 42 merged --repo owner/repo
            // nudge wait github issue 2208 new-comment --repo owner/repo
            // nudge wait github ci 2199 success --repo owner/repo
            if args.len() < 3 {
                bail!("github requires: <pr|issue|ci> <number> <event> [--repo owner/repo]");
            }
            let kind = &args[0];    // pr, issue, ci
            let number: i64 = args[1].parse().map_err(|_| anyhow::anyhow!("invalid number: {}", args[1]))?;
            let event = &args[2];   // merged, closed, new-comment, success, etc.

            let detected = detect_repo();
            let repo = repo.or(detected.as_deref())
                .ok_or_else(|| anyhow::anyhow!("--repo is required (or run from a git repo)"))?;

            let mut condition = serde_json::json!({
                "kind": kind,
                "number": number,
                "event": event,
                "repo": repo,
            });

            // For new-comment detection, snapshot current comment count as baseline
            if event == "new-comment" {
                if let Ok(count) = get_comment_count(repo, number) {
                    condition["comment_count_at_subscribe"] = serde_json::json!(count);
                }
            }

            Ok(condition)
        }
        "webhook" => {
            let path = args.first().ok_or_else(|| anyhow::anyhow!("webhook requires a path"))?;
            Ok(serde_json::json!({
                "path": path,
            }))
        }
        _ => bail!("unknown source: {source}. Supported: github, timer, webhook"),
    }
}

fn get_comment_count(repo: &str, number: i64) -> Result<i64> {
    let output = std::process::Command::new("gh")
        .args(["api", &format!("repos/{repo}/issues/{number}"), "--jq", ".comments"])
        .output()?;
    if !output.status.success() {
        bail!("failed to get comment count");
    }
    let count: i64 = String::from_utf8_lossy(&output.stdout).trim().parse()?;
    Ok(count)
}

fn detect_repo() -> Option<String> {
    let output = std::process::Command::new("gh")
        .args(["repo", "view", "--json", "nameWithOwner", "--jq", ".nameWithOwner"])
        .output()
        .ok()?;
    if output.status.success() {
        Some(String::from_utf8_lossy(&output.stdout).trim().to_string())
    } else {
        None
    }
}

/// Parse duration string like "30s", "5m", "2h", "7d" into seconds.
pub fn parse_duration_secs(s: &str) -> Result<i64> {
    let s = s.trim();
    if s.is_empty() {
        bail!("empty duration");
    }
    let (num_str, unit) = s.split_at(s.len() - 1);
    let num: i64 = num_str.parse().map_err(|_| anyhow::anyhow!("invalid duration: {s}"))?;
    match unit {
        "s" => Ok(num),
        "m" => Ok(num * 60),
        "h" => Ok(num * 3600),
        "d" => Ok(num * 86400),
        _ => bail!("unknown duration unit: {unit}. Use s/m/h/d"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_duration() {
        assert_eq!(parse_duration_secs("30s").unwrap(), 30);
        assert_eq!(parse_duration_secs("5m").unwrap(), 300);
        assert_eq!(parse_duration_secs("2h").unwrap(), 7200);
        assert_eq!(parse_duration_secs("7d").unwrap(), 604800);
    }

    #[test]
    fn test_parse_condition_timer() {
        let cond = parse_condition("timer", &["30m".into()], None).unwrap();
        assert_eq!(cond["duration"], "30m");
        assert!(cond["fire_at"].as_i64().unwrap() > 0);
    }

    #[test]
    fn test_parse_condition_github() {
        let cond = parse_condition(
            "github",
            &["pr".into(), "42".into(), "merged".into()],
            Some("foo/bar"),
        ).unwrap();
        assert_eq!(cond["kind"], "pr");
        assert_eq!(cond["number"], 42);
        assert_eq!(cond["event"], "merged");
        assert_eq!(cond["repo"], "foo/bar");
    }
}

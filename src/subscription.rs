use anyhow::{bail, Result};
use chrono::Utc;
use serde::{Deserialize, Serialize};

use crate::cli::{OnArgs, WaitArgs};

/// Typed condition enum — replaces raw serde_json::Value.
/// Stored as JSON string in SQLite, but in-memory it's typed.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "source", content = "condition")]
pub enum Condition {
    Timer {
        duration: String,
        fire_at: i64,
    },
    GitHub(GitHubCondition),
}

impl Condition {
    pub fn source_name(&self) -> &str {
        match self {
            Condition::Timer { .. } => "timer",
            Condition::GitHub(_) => "github",
        }
    }
}

/// Typed GitHub condition — compiler enforces exhaustive matching (no silent `_ =>` arm).
/// Each variant carries its required baseline data.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "event")]
pub enum GitHubCondition {
    PrMerged { repo: String, number: i64 },
    PrClosed { repo: String, number: i64 },
    PrLabel { repo: String, number: i64, name: String, present_at_subscribe: bool },
    PrNewComment { repo: String, number: i64, baseline: i64 },
    PrCiPassed { repo: String, number: i64 },
    IssueClosed { repo: String, number: i64 },
    IssueLabel { repo: String, number: i64, name: String, present_at_subscribe: bool },
    IssueNewComment { repo: String, number: i64, baseline: i64 },
    CiSuccess { repo: String, number: i64 },
    CiFailure { repo: String, number: i64 },
    CiCompleted { repo: String, number: i64 },
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Subscription {
    pub id: String,
    pub source: String,
    pub condition: Condition,
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
            id: uuid::Uuid::new_v4().to_string(),
            source: condition.source_name().to_string(),
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
            id: uuid::Uuid::new_v4().to_string(),
            source: condition.source_name().to_string(),
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
        match &self.condition {
            Condition::Timer { duration, .. } => format!("timer {duration}"),
            Condition::GitHub(gh) => match gh {
                GitHubCondition::PrMerged { repo, number } => format!("{repo} pr #{number} merged"),
                GitHubCondition::PrClosed { repo, number } => format!("{repo} pr #{number} closed"),
                GitHubCondition::PrLabel { repo, number, name, .. } => format!("{repo} pr #{number} label:{name}"),
                GitHubCondition::PrNewComment { repo, number, .. } => format!("{repo} pr #{number} new-comment"),
                GitHubCondition::PrCiPassed { repo, number } => format!("{repo} pr #{number} ci-passed"),
                GitHubCondition::IssueClosed { repo, number } => format!("{repo} issue #{number} closed"),
                GitHubCondition::IssueLabel { repo, number, name, .. } => format!("{repo} issue #{number} label:{name}"),
                GitHubCondition::IssueNewComment { repo, number, .. } => format!("{repo} issue #{number} new-comment"),
                GitHubCondition::CiSuccess { repo, number } => format!("{repo} ci #{number} success"),
                GitHubCondition::CiFailure { repo, number } => format!("{repo} ci #{number} failure"),
                GitHubCondition::CiCompleted { repo, number } => format!("{repo} ci #{number} completed"),
            },
        }
    }

    #[allow(dead_code)]
    pub fn is_expired(&self) -> bool {
        if let Some(exp) = self.expires_at {
            Utc::now().timestamp() >= exp
        } else {
            false
        }
    }
}

fn parse_condition(source: &str, args: &[String], repo: Option<&str>) -> Result<Condition> {
    match source {
        "timer" => {
            let duration = args.first().ok_or_else(|| anyhow::anyhow!("timer requires a duration (e.g., 30m, 2h)"))?;
            let secs = parse_duration_secs(duration)?;
            Ok(Condition::Timer {
                duration: duration.clone(),
                fire_at: Utc::now().timestamp() + secs,
            })
        }
        "github" => {
            if args.len() < 3 {
                bail!("github requires: <pr|issue|ci> <number> <event> [--repo owner/repo]");
            }
            let kind = &args[0];
            let number: i64 = args[1].parse().map_err(|_| anyhow::anyhow!("invalid number: {}", args[1]))?;
            let event = &args[2];

            let detected = detect_repo();
            let repo = repo.or(detected.as_deref())
                .ok_or_else(|| anyhow::anyhow!("--repo is required (or run from a git repo)"))?
                .to_string();

            let inner = match (kind.as_str(), event.as_str()) {
                ("pr", "merged") => GitHubCondition::PrMerged { repo, number },
                ("pr", "closed") => GitHubCondition::PrClosed { repo, number },
                ("pr", e) if e.starts_with("label:") => {
                    let name = e["label:".len()..].to_string();
                    let present = check_label_present(&repo, kind, number, &name)?;
                    GitHubCondition::PrLabel { repo, number, name, present_at_subscribe: present }
                }
                ("pr", "new-comment") => {
                    let baseline = get_comment_count(&repo, number)?;
                    GitHubCondition::PrNewComment { repo, number, baseline }
                }
                ("pr", "ci-passed") => GitHubCondition::PrCiPassed { repo, number },
                ("issue", "closed") => GitHubCondition::IssueClosed { repo, number },
                ("issue", e) if e.starts_with("label:") => {
                    let name = e["label:".len()..].to_string();
                    let present = check_label_present(&repo, kind, number, &name)?;
                    GitHubCondition::IssueLabel { repo, number, name, present_at_subscribe: present }
                }
                ("issue", "new-comment") => {
                    let baseline = get_comment_count(&repo, number)?;
                    GitHubCondition::IssueNewComment { repo, number, baseline }
                }
                ("ci", "success") => GitHubCondition::CiSuccess { repo, number },
                ("ci", "failure") => GitHubCondition::CiFailure { repo, number },
                ("ci", "completed") => GitHubCondition::CiCompleted { repo, number },
                _ => bail!(
                    "invalid github condition: kind={kind} event={event}. \
                     Valid: pr (merged|closed|ci-passed|new-comment|label:X), \
                     issue (closed|new-comment|label:X), ci (success|failure|completed)"
                ),
            };

            Ok(Condition::GitHub(inner))
        }
        "webhook" => {
            bail!("webhook source is not yet implemented");
        }
        _ => bail!("unknown source: {source}. Supported: github, timer, webhook"),
    }
}

fn check_label_present(repo: &str, _kind: &str, number: i64, label: &str) -> Result<bool> {
    // GitHub exposes labels on the /issues endpoint for both PRs and issues.
    let endpoint = format!("repos/{repo}/issues/{number}");
    let output = std::process::Command::new("gh")
        .args(["api", &endpoint, "--jq", "[.labels[].name]"])
        .output()?;
    if !output.status.success() {
        bail!("failed to check labels");
    }
    let labels_json = String::from_utf8_lossy(&output.stdout);
    let label_list: Vec<String> = serde_json::from_str(labels_json.trim()).unwrap_or_default();
    Ok(label_list.iter().any(|l| l == label))
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
    if num < 0 {
        bail!("duration must not be negative: {s}");
    }
    match unit {
        "s" => Ok(num),
        "m" => num.checked_mul(60).ok_or_else(|| anyhow::anyhow!("duration overflow: {s}")),
        "h" => num.checked_mul(3600).ok_or_else(|| anyhow::anyhow!("duration overflow: {s}")),
        "d" => num.checked_mul(86400).ok_or_else(|| anyhow::anyhow!("duration overflow: {s}")),
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
    fn test_parse_duration_negative() {
        assert!(parse_duration_secs("-5m").is_err());
    }

    #[test]
    fn test_parse_duration_overflow() {
        assert!(parse_duration_secs("9999999999999999999d").is_err());
        assert!(parse_duration_secs("9223372036854775807h").is_err());
    }

    #[test]
    fn test_parse_condition_timer() {
        let cond = parse_condition("timer", &["30m".into()], None).unwrap();
        match cond {
            Condition::Timer { duration, fire_at } => {
                assert_eq!(duration, "30m");
                assert!(fire_at > 0);
            }
            _ => panic!("expected Timer condition"),
        }
    }

    #[test]
    fn test_parse_condition_github() {
        let cond = parse_condition(
            "github",
            &["pr".into(), "42".into(), "merged".into()],
            Some("foo/bar"),
        ).unwrap();
        match cond {
            Condition::GitHub(GitHubCondition::PrMerged { repo, number }) => {
                assert_eq!(repo, "foo/bar");
                assert_eq!(number, 42);
            }
            _ => panic!("expected GitHub PrMerged condition"),
        }
    }

    #[test]
    fn test_parse_condition_github_ci() {
        let cond = parse_condition(
            "github",
            &["ci".into(), "999".into(), "success".into()],
            Some("foo/bar"),
        ).unwrap();
        match cond {
            Condition::GitHub(GitHubCondition::CiSuccess { repo, number }) => {
                assert_eq!(repo, "foo/bar");
                assert_eq!(number, 999);
            }
            _ => panic!("expected GitHub CiSuccess condition"),
        }
    }

    #[test]
    fn test_parse_condition_invalid_kind() {
        assert!(parse_condition(
            "github",
            &["bogus".into(), "1".into(), "merged".into()],
            Some("foo/bar"),
        ).is_err());
    }

    #[test]
    fn test_parse_condition_invalid_event() {
        assert!(parse_condition(
            "github",
            &["pr".into(), "1".into(), "bogus".into()],
            Some("foo/bar"),
        ).is_err());
    }

    #[test]
    fn test_condition_serde_roundtrip() {
        let conditions = vec![
            Condition::Timer { duration: "30m".into(), fire_at: 12345 },
            Condition::GitHub(GitHubCondition::PrMerged { repo: "foo/bar".into(), number: 42 }),
            Condition::GitHub(GitHubCondition::PrLabel {
                repo: "foo/bar".into(), number: 1, name: "ready".into(), present_at_subscribe: false,
            }),
            Condition::GitHub(GitHubCondition::IssueNewComment {
                repo: "a/b".into(), number: 10, baseline: 5,
            }),
            Condition::GitHub(GitHubCondition::CiFailure { repo: "x/y".into(), number: 100 }),
        ];
        for cond in conditions {
            let json = serde_json::to_string(&cond).unwrap();
            let parsed: Condition = serde_json::from_str(&json).unwrap();
            let json2 = serde_json::to_string(&parsed).unwrap();
            assert_eq!(json, json2, "roundtrip failed for: {json}");
        }
    }

    #[test]
    fn test_subscription_id_uniqueness() {
        use std::collections::HashSet;
        // T1: Generate 10,000 subscriptions and verify all IDs are unique
        let mut ids = HashSet::new();
        for _ in 0..10_000 {
            let id = uuid::Uuid::new_v4().to_string();
            assert!(ids.insert(id), "Duplicate subscription ID generated");
        }
        assert_eq!(ids.len(), 10_000);
    }

    #[test]
    fn test_condition_summary() {
        let sub = Subscription {
            id: "x".into(),
            source: "github".into(),
            condition: Condition::GitHub(GitHubCondition::PrMerged {
                repo: "foo/bar".into(), number: 42,
            }),
            mode: "wait".into(),
            callback: None,
            status: "active".into(),
            created_at: 0,
            expires_at: None,
            event_data: None,
        };
        assert_eq!(sub.condition_summary(), "foo/bar pr #42 merged");

        let sub2 = Subscription {
            condition: Condition::Timer { duration: "5m".into(), fire_at: 0 },
            source: "timer".into(),
            ..sub.clone()
        };
        assert_eq!(sub2.condition_summary(), "timer 5m");
    }
}

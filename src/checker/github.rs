use anyhow::Result;
use crate::subscription::GitHubCondition;

/// Trait for GitHub API access — enables testing with mock responses.
#[async_trait::async_trait]
pub trait GitHubClient: Send + Sync {
    async fn api(&self, endpoint: &str, jq: &str) -> Result<String>;
}

/// Production client that shells out to `gh` CLI.
pub struct GhCliClient;

#[async_trait::async_trait]
impl GitHubClient for GhCliClient {
    async fn api(&self, endpoint: &str, jq: &str) -> Result<String> {
        let output = tokio::process::Command::new("gh")
            .args(["api", endpoint, "--jq", jq])
            .output()
            .await?;
        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            anyhow::bail!("gh api failed: {stderr}");
        }
        Ok(String::from_utf8_lossy(&output.stdout).trim().to_string())
    }
}

/// Check a GitHub condition. Exhaustive match — compiler catches missing variants.
pub async fn check<C: GitHubClient>(condition: &GitHubCondition, client: &C) -> Result<Option<serde_json::Value>> {
    match condition {
        GitHubCondition::PrMerged { repo, number } => {
            check_pr_merged(client, repo, *number).await
        }
        GitHubCondition::PrClosed { repo, number } => {
            check_pr_closed(client, repo, *number).await
        }
        GitHubCondition::PrLabel { repo, number, name, present_at_subscribe } => {
            check_label(client, repo, *number, name, "pr", *present_at_subscribe).await
        }
        GitHubCondition::PrNewComment { repo, number, baseline } => {
            check_new_comment(client, repo, *number, *baseline).await
        }
        GitHubCondition::PrCiPassed { repo, number } => {
            check_ci(client, repo, *number, "success").await
        }
        GitHubCondition::IssueClosed { repo, number } => {
            check_issue_closed(client, repo, *number).await
        }
        GitHubCondition::IssueLabel { repo, number, name, present_at_subscribe } => {
            check_label(client, repo, *number, name, "issue", *present_at_subscribe).await
        }
        GitHubCondition::IssueNewComment { repo, number, baseline } => {
            check_new_comment(client, repo, *number, *baseline).await
        }
        GitHubCondition::CiSuccess { repo, number } => {
            check_ci(client, repo, *number, "success").await
        }
        GitHubCondition::CiFailure { repo, number } => {
            check_ci(client, repo, *number, "failure").await
        }
        GitHubCondition::CiCompleted { repo, number } => {
            check_ci(client, repo, *number, "completed").await
        }
    }
}

async fn check_pr_merged<C: GitHubClient>(client: &C, repo: &str, number: i64) -> Result<Option<serde_json::Value>> {
    let merged = client.api(
        &format!("repos/{repo}/pulls/{number}"),
        ".merged",
    ).await?;
    if merged == "true" {
        Ok(Some(serde_json::json!({
            "source": "github", "type": "pr_merged",
            "repo": repo, "pr": number,
        })))
    } else {
        Ok(None)
    }
}

async fn check_pr_closed<C: GitHubClient>(client: &C, repo: &str, number: i64) -> Result<Option<serde_json::Value>> {
    let state = client.api(
        &format!("repos/{repo}/pulls/{number}"),
        ".state",
    ).await?;
    if state == "closed" {
        Ok(Some(serde_json::json!({
            "source": "github", "type": "pr_closed",
            "repo": repo, "pr": number,
        })))
    } else {
        Ok(None)
    }
}

async fn check_label<C: GitHubClient>(
    client: &C, repo: &str, number: i64, label: &str, kind: &str, was_present: bool,
) -> Result<Option<serde_json::Value>> {
    let endpoint = match kind {
        "pr" => format!("repos/{repo}/pulls/{number}"),
        _ => format!("repos/{repo}/issues/{number}"),
    };
    let labels = client.api(&endpoint, "[.labels[].name] | join(\",\")").await?;
    let is_present = labels.split(',').any(|l| l.trim() == label);

    if is_present && !was_present {
        let mut event = serde_json::json!({
            "source": "github", "type": "label_added",
            "repo": repo, "label": label,
        });
        event[kind] = serde_json::json!(number);
        Ok(Some(event))
    } else {
        Ok(None)
    }
}

async fn check_issue_closed<C: GitHubClient>(client: &C, repo: &str, number: i64) -> Result<Option<serde_json::Value>> {
    let state = client.api(
        &format!("repos/{repo}/issues/{number}"),
        ".state",
    ).await?;
    if state == "closed" {
        Ok(Some(serde_json::json!({
            "source": "github", "type": "issue_closed",
            "repo": repo, "issue": number,
        })))
    } else {
        Ok(None)
    }
}

/// Check for new comments on an issue or PR.
/// Note: for PRs, this only counts issue-style comments (from the "Conversation" tab),
/// NOT PR review comments or inline code review comments.
async fn check_new_comment<C: GitHubClient>(
    client: &C, repo: &str, number: i64, baseline: i64,
) -> Result<Option<serde_json::Value>> {
    let count_str = client.api(
        &format!("repos/{repo}/issues/{number}"),
        ".comments",
    ).await?;
    let current_count: i64 = count_str.parse().unwrap_or(0);

    if current_count > baseline {
        Ok(Some(serde_json::json!({
            "source": "github", "type": "new_comment",
            "repo": repo, "number": number,
            "new_count": current_count, "baseline": baseline,
        })))
    } else {
        Ok(None)
    }
}

/// Fetch all check runs for a commit SHA, paginating through all pages.
async fn fetch_all_check_runs<C: GitHubClient>(client: &C, repo: &str, sha: &str) -> Result<Vec<serde_json::Value>> {
    let mut all_checks = Vec::new();
    let mut page = 1u32;
    let per_page = 100;

    loop {
        let endpoint = format!(
            "repos/{repo}/commits/{sha}/check-runs?per_page={per_page}&page={page}"
        );
        let body = client.api(&endpoint, ".").await?;
        let parsed: serde_json::Value = serde_json::from_str(&body).unwrap_or_default();
        let runs = parsed.get("check_runs")
            .and_then(|v| v.as_array())
            .cloned()
            .unwrap_or_default();

        let count = runs.len();
        for run in runs {
            all_checks.push(serde_json::json!({
                "name": run.get("name").and_then(|v| v.as_str()).unwrap_or(""),
                "status": run.get("status").and_then(|v| v.as_str()).unwrap_or(""),
                "conclusion": run.get("conclusion").and_then(|v| v.as_str()).unwrap_or(""),
            }));
        }

        if count < per_page as usize {
            break;
        }
        page += 1;
    }

    Ok(all_checks)
}

async fn check_ci<C: GitHubClient>(client: &C, repo: &str, number: i64, expected: &str) -> Result<Option<serde_json::Value>> {
    // Try actions/runs first (works for both run IDs and avoids 404 on pulls/ for run IDs)
    if let Ok(status) = client.api(
        &format!("repos/{repo}/actions/runs/{number}"),
        "{status: .status, conclusion: .conclusion}",
    ).await {
        let parsed: serde_json::Value = serde_json::from_str(&status).unwrap_or_default();
        let conclusion = parsed.get("conclusion").and_then(|v| v.as_str()).unwrap_or("");
        let run_status = parsed.get("status").and_then(|v| v.as_str()).unwrap_or("");

        if !run_status.is_empty() {
            let fired = match expected {
                "success" => conclusion == "success",
                "failure" => conclusion == "failure",
                "completed" => run_status == "completed",
                _ => false,
            };

            if fired {
                return Ok(Some(serde_json::json!({
                    "source": "github", "type": "ci_result",
                    "repo": repo, "run": number, "conclusion": conclusion,
                })));
            }
            return Ok(None);
        }
    }

    // Fall back to PR-based CI check: get head SHA from the pull request
    let sha = client.api(
        &format!("repos/{repo}/pulls/{number}"),
        ".head.sha",
    ).await?;

    if sha.is_empty() {
        return Ok(None);
    }

    let checks = fetch_all_check_runs(client, repo, &sha).await?;

    if checks.is_empty() {
        return Ok(None);
    }

    let all_completed = checks.iter().all(|c| c["status"] == "completed");
    if !all_completed {
        return Ok(None);
    }

    let overall = if checks.iter().all(|c| c["conclusion"] == "success") {
        "success"
    } else if checks.iter().any(|c| c["conclusion"] == "failure") {
        "failure"
    } else {
        "completed"
    };

    let fired = match expected {
        "success" => overall == "success",
        "failure" => overall == "failure",
        "completed" => true,
        _ => false,
    };

    if fired {
        Ok(Some(serde_json::json!({
            "source": "github", "type": "ci_result",
            "repo": repo, "pr": number, "conclusion": overall,
            "checks": checks,
        })))
    } else {
        Ok(None)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;

    struct MockGitHubClient {
        responses: HashMap<(String, String), String>,
    }

    impl MockGitHubClient {
        fn new() -> Self {
            Self { responses: HashMap::new() }
        }
        fn mock(&mut self, endpoint: &str, jq: &str, response: &str) {
            self.responses.insert((endpoint.to_string(), jq.to_string()), response.to_string());
        }
    }

    #[async_trait::async_trait]
    impl GitHubClient for MockGitHubClient {
        async fn api(&self, endpoint: &str, jq: &str) -> Result<String> {
            self.responses.get(&(endpoint.to_string(), jq.to_string()))
                .cloned()
                .ok_or_else(|| anyhow::anyhow!("no mock for endpoint={endpoint} jq={jq}"))
        }
    }

    #[tokio::test]
    async fn test_pr_merged_fires() {
        let mut client = MockGitHubClient::new();
        client.mock("repos/foo/bar/pulls/42", ".merged", "true");
        let cond = GitHubCondition::PrMerged { repo: "foo/bar".into(), number: 42 };
        let result = check(&cond, &client).await.unwrap();
        assert!(result.is_some());
        let event = result.unwrap();
        assert_eq!(event["type"], "pr_merged");
        assert_eq!(event["pr"], 42);
    }

    #[tokio::test]
    async fn test_pr_merged_not_yet() {
        let mut client = MockGitHubClient::new();
        client.mock("repos/foo/bar/pulls/42", ".merged", "false");
        let cond = GitHubCondition::PrMerged { repo: "foo/bar".into(), number: 42 };
        let result = check(&cond, &client).await.unwrap();
        assert!(result.is_none());
    }

    #[tokio::test]
    async fn test_pr_closed_fires() {
        let mut client = MockGitHubClient::new();
        client.mock("repos/foo/bar/pulls/10", ".state", "closed");
        let cond = GitHubCondition::PrClosed { repo: "foo/bar".into(), number: 10 };
        let result = check(&cond, &client).await.unwrap();
        assert!(result.is_some());
        assert_eq!(result.unwrap()["type"], "pr_closed");
    }

    #[tokio::test]
    async fn test_pr_closed_still_open() {
        let mut client = MockGitHubClient::new();
        client.mock("repos/foo/bar/pulls/10", ".state", "open");
        let cond = GitHubCondition::PrClosed { repo: "foo/bar".into(), number: 10 };
        assert!(check(&cond, &client).await.unwrap().is_none());
    }

    #[tokio::test]
    async fn test_label_fires_when_added() {
        let mut client = MockGitHubClient::new();
        client.mock("repos/foo/bar/pulls/42", "[.labels[].name] | join(\",\")", "ready,urgent");
        let cond = GitHubCondition::PrLabel {
            repo: "foo/bar".into(), number: 42,
            name: "ready".into(), present_at_subscribe: false,
        };
        let result = check(&cond, &client).await.unwrap();
        assert!(result.is_some());
        let event = result.unwrap();
        assert_eq!(event["type"], "label_added");
        assert_eq!(event["label"], "ready");
    }

    #[tokio::test]
    async fn test_label_does_not_fire_when_already_present() {
        let mut client = MockGitHubClient::new();
        client.mock("repos/foo/bar/pulls/42", "[.labels[].name] | join(\",\")", "ready");
        let cond = GitHubCondition::PrLabel {
            repo: "foo/bar".into(), number: 42,
            name: "ready".into(), present_at_subscribe: true,
        };
        assert!(check(&cond, &client).await.unwrap().is_none());
    }

    #[tokio::test]
    async fn test_label_not_present() {
        let mut client = MockGitHubClient::new();
        client.mock("repos/foo/bar/pulls/42", "[.labels[].name] | join(\",\")", "other");
        let cond = GitHubCondition::PrLabel {
            repo: "foo/bar".into(), number: 42,
            name: "ready".into(), present_at_subscribe: false,
        };
        assert!(check(&cond, &client).await.unwrap().is_none());
    }

    #[tokio::test]
    async fn test_issue_label_fires() {
        let mut client = MockGitHubClient::new();
        client.mock("repos/foo/bar/issues/5", "[.labels[].name] | join(\",\")", "bug");
        let cond = GitHubCondition::IssueLabel {
            repo: "foo/bar".into(), number: 5,
            name: "bug".into(), present_at_subscribe: false,
        };
        let result = check(&cond, &client).await.unwrap();
        assert!(result.is_some());
    }

    #[tokio::test]
    async fn test_new_comment_fires() {
        let mut client = MockGitHubClient::new();
        client.mock("repos/foo/bar/issues/42", ".comments", "5");
        let cond = GitHubCondition::PrNewComment {
            repo: "foo/bar".into(), number: 42, baseline: 3,
        };
        let result = check(&cond, &client).await.unwrap();
        assert!(result.is_some());
        let event = result.unwrap();
        assert_eq!(event["new_count"], 5);
        assert_eq!(event["baseline"], 3);
    }

    #[tokio::test]
    async fn test_new_comment_no_change() {
        let mut client = MockGitHubClient::new();
        client.mock("repos/foo/bar/issues/42", ".comments", "3");
        let cond = GitHubCondition::PrNewComment {
            repo: "foo/bar".into(), number: 42, baseline: 3,
        };
        assert!(check(&cond, &client).await.unwrap().is_none());
    }

    #[tokio::test]
    async fn test_issue_new_comment_fires() {
        let mut client = MockGitHubClient::new();
        client.mock("repos/foo/bar/issues/10", ".comments", "8");
        let cond = GitHubCondition::IssueNewComment {
            repo: "foo/bar".into(), number: 10, baseline: 5,
        };
        let result = check(&cond, &client).await.unwrap();
        assert!(result.is_some());
        assert_eq!(result.unwrap()["type"], "new_comment");
    }

    #[tokio::test]
    async fn test_issue_closed_fires() {
        let mut client = MockGitHubClient::new();
        client.mock("repos/foo/bar/issues/10", ".state", "closed");
        let cond = GitHubCondition::IssueClosed { repo: "foo/bar".into(), number: 10 };
        let result = check(&cond, &client).await.unwrap();
        assert!(result.is_some());
        assert_eq!(result.unwrap()["type"], "issue_closed");
    }

    #[tokio::test]
    async fn test_issue_closed_still_open() {
        let mut client = MockGitHubClient::new();
        client.mock("repos/foo/bar/issues/10", ".state", "open");
        let cond = GitHubCondition::IssueClosed { repo: "foo/bar".into(), number: 10 };
        assert!(check(&cond, &client).await.unwrap().is_none());
    }

    #[tokio::test]
    async fn test_ci_success_via_run_id() {
        let mut client = MockGitHubClient::new();
        client.mock(
            "repos/foo/bar/actions/runs/999",
            "{status: .status, conclusion: .conclusion}",
            r#"{"status": "completed", "conclusion": "success"}"#,
        );
        let cond = GitHubCondition::CiSuccess { repo: "foo/bar".into(), number: 999 };
        let result = check(&cond, &client).await.unwrap();
        assert!(result.is_some());
        assert_eq!(result.unwrap()["conclusion"], "success");
    }

    #[tokio::test]
    async fn test_ci_failure_via_run_id() {
        let mut client = MockGitHubClient::new();
        client.mock(
            "repos/foo/bar/actions/runs/999",
            "{status: .status, conclusion: .conclusion}",
            r#"{"status": "completed", "conclusion": "failure"}"#,
        );
        let cond = GitHubCondition::CiFailure { repo: "foo/bar".into(), number: 999 };
        let result = check(&cond, &client).await.unwrap();
        assert!(result.is_some());
    }

    #[tokio::test]
    async fn test_ci_not_yet_completed() {
        let mut client = MockGitHubClient::new();
        client.mock(
            "repos/foo/bar/actions/runs/999",
            "{status: .status, conclusion: .conclusion}",
            r#"{"status": "in_progress", "conclusion": ""}"#,
        );
        let cond = GitHubCondition::CiSuccess { repo: "foo/bar".into(), number: 999 };
        assert!(check(&cond, &client).await.unwrap().is_none());
    }

    #[tokio::test]
    async fn test_ci_pr_based_all_success() {
        let mut client = MockGitHubClient::new();
        // actions/runs fails (not a run ID) — triggers PR fallback
        client.mock("repos/foo/bar/pulls/42", ".head.sha", "abc123");
        client.mock(
            "repos/foo/bar/commits/abc123/check-runs?per_page=100&page=1",
            ".",
            r#"{"check_runs": [
                {"name": "build", "status": "completed", "conclusion": "success"},
                {"name": "test", "status": "completed", "conclusion": "success"}
            ]}"#,
        );
        let cond = GitHubCondition::PrCiPassed { repo: "foo/bar".into(), number: 42 };
        let result = check(&cond, &client).await.unwrap();
        assert!(result.is_some());
        assert_eq!(result.unwrap()["conclusion"], "success");
    }

    #[tokio::test]
    async fn test_ci_pr_based_has_failure() {
        let mut client = MockGitHubClient::new();
        client.mock("repos/foo/bar/pulls/42", ".head.sha", "abc123");
        client.mock(
            "repos/foo/bar/commits/abc123/check-runs?per_page=100&page=1",
            ".",
            r#"{"check_runs": [
                {"name": "build", "status": "completed", "conclusion": "success"},
                {"name": "test", "status": "completed", "conclusion": "failure"}
            ]}"#,
        );
        // CiSuccess should NOT fire when there's a failure
        let cond = GitHubCondition::PrCiPassed { repo: "foo/bar".into(), number: 42 };
        let result = check(&cond, &client).await.unwrap();
        assert!(result.is_none());
    }

    #[tokio::test]
    async fn test_api_error_propagates() {
        let client = MockGitHubClient::new(); // no mocks registered
        let cond = GitHubCondition::PrMerged { repo: "foo/bar".into(), number: 1 };
        let result = check(&cond, &client).await;
        assert!(result.is_err());
    }
}

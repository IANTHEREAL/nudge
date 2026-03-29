use anyhow::Result;
use crate::subscription::Subscription;

/// Check a GitHub subscription condition by calling `gh` CLI.
pub async fn check(sub: &Subscription) -> Result<Option<serde_json::Value>> {
    let kind = sub.condition.get("kind").and_then(|v| v.as_str()).unwrap_or("");
    let number = sub.condition.get("number").and_then(|v| v.as_i64()).unwrap_or(0);
    let event = sub.condition.get("event").and_then(|v| v.as_str()).unwrap_or("");
    let repo = sub.condition.get("repo").and_then(|v| v.as_str()).unwrap_or("");

    match (kind, event) {
        ("pr", "merged") => check_pr_merged(repo, number).await,
        ("pr", "closed") => check_pr_closed(repo, number).await,
        ("pr", e) if e.starts_with("label:") => {
            let label = &e["label:".len()..];
            let was_present = sub.condition.get("label_present_at_subscribe")
                .and_then(|v| v.as_bool()).unwrap_or(false);
            check_label(repo, number, label, "pr", was_present).await
        }
        ("pr", "new-comment") => check_new_comment(repo, number, sub).await,
        ("pr", "ci-passed") | ("ci", "success") => check_ci(repo, number, "success").await,
        ("ci", "failure") => check_ci(repo, number, "failure").await,
        ("ci", "completed") => check_ci(repo, number, "completed").await,
        ("issue", "new-comment") => check_new_comment(repo, number, sub).await,
        ("issue", "closed") => check_issue_closed(repo, number).await,
        ("issue", e) if e.starts_with("label:") => {
            let label = &e["label:".len()..];
            let was_present = sub.condition.get("label_present_at_subscribe")
                .and_then(|v| v.as_bool()).unwrap_or(false);
            check_label(repo, number, label, "issue", was_present).await
        }
        _ => {
            tracing::warn!(kind, event, "Unknown github condition");
            Ok(None)
        }
    }
}

async fn gh_api(endpoint: &str, jq: &str) -> Result<String> {
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

async fn check_pr_merged(repo: &str, number: i64) -> Result<Option<serde_json::Value>> {
    let merged = gh_api(
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

async fn check_pr_closed(repo: &str, number: i64) -> Result<Option<serde_json::Value>> {
    let state = gh_api(
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

async fn check_label(repo: &str, number: i64, label: &str, kind: &str, was_present: bool) -> Result<Option<serde_json::Value>> {
    let endpoint = match kind {
        "pr" => format!("repos/{repo}/pulls/{number}"),
        _ => format!("repos/{repo}/issues/{number}"),
    };
    let labels = gh_api(&endpoint, "[.labels[].name] | join(\",\")").await?;
    let is_present = labels.split(',').any(|l| l.trim() == label);

    // Only fire if label was NOT present at subscribe time and IS present now
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

async fn check_issue_closed(repo: &str, number: i64) -> Result<Option<serde_json::Value>> {
    let state = gh_api(
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
/// NOT PR review comments or inline code review comments. Those are a separate GitHub
/// concept tracked via /pulls/{number}/reviews and /pulls/{number}/comments endpoints.
async fn check_new_comment(repo: &str, number: i64, sub: &Subscription) -> Result<Option<serde_json::Value>> {
    let count_str = gh_api(
        &format!("repos/{repo}/issues/{number}"),
        ".comments",
    ).await?;
    let current_count: i64 = count_str.parse().unwrap_or(0);

    // The baseline count is stored in the condition at subscription time.
    let baseline = sub.condition.get("comment_count_at_subscribe")
        .and_then(|v| v.as_i64())
        .unwrap_or(current_count); // If no baseline, treat current as baseline (no new comment yet)

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
async fn fetch_all_check_runs(repo: &str, sha: &str) -> Result<Vec<serde_json::Value>> {
    let mut all_checks = Vec::new();
    let mut page = 1u32;
    let per_page = 100;

    loop {
        let endpoint = format!(
            "repos/{repo}/commits/{sha}/check-runs?per_page={per_page}&page={page}"
        );
        let body = gh_api(&endpoint, ".").await?;
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

async fn check_ci(repo: &str, number: i64, expected: &str) -> Result<Option<serde_json::Value>> {
    // Try actions/runs first (works for both run IDs and avoids 404 on pulls/ for run IDs)
    if let Ok(status) = gh_api(
        &format!("repos/{repo}/actions/runs/{number}"),
        "{status: .status, conclusion: .conclusion}",
    ).await {
        let parsed: serde_json::Value = serde_json::from_str(&status).unwrap_or_default();
        let conclusion = parsed.get("conclusion").and_then(|v| v.as_str()).unwrap_or("");
        let run_status = parsed.get("status").and_then(|v| v.as_str()).unwrap_or("");

        // If we got a valid response (status is non-empty), treat as a run ID
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
    let sha = gh_api(
        &format!("repos/{repo}/pulls/{number}"),
        ".head.sha",
    ).await?;

    if sha.is_empty() {
        return Ok(None);
    }

    // PR-based CI check — paginate to get all check runs
    let checks = fetch_all_check_runs(repo, &sha).await?;

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

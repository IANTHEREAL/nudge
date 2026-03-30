pub mod timer;
pub mod github;

use anyhow::Result;
use crate::subscription::{Condition, Subscription};
use crate::checker::github::GitHubClient;

/// Check if a subscription's condition has been met.
/// Returns Some(event_data) if fired, None if not yet.
pub async fn check<C: GitHubClient>(sub: &Subscription, client: &C) -> Result<Option<serde_json::Value>> {
    match &sub.condition {
        Condition::Timer { fire_at, duration } => timer::check(*fire_at, duration),
        Condition::GitHub(gh_cond) => github::check(gh_cond, client).await,
    }
}

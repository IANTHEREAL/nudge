pub mod timer;
pub mod github;

use anyhow::Result;
use crate::subscription::Subscription;

/// Check if a subscription's condition has been met.
/// Returns Some(event_data) if fired, None if not yet.
pub async fn check(sub: &Subscription) -> Result<Option<serde_json::Value>> {
    match sub.source.as_str() {
        "timer" => timer::check(sub),
        "github" => github::check(sub).await,
        other => {
            tracing::warn!(source = other, "Unknown source, skipping");
            Ok(None)
        }
    }
}

use anyhow::Result;
use chrono::Utc;

use crate::subscription::Subscription;

/// Check if a timer subscription has fired.
pub fn check(sub: &Subscription) -> Result<Option<serde_json::Value>> {
    let fire_at = sub.condition.get("fire_at")
        .and_then(|v| v.as_i64())
        .ok_or_else(|| anyhow::anyhow!("timer subscription missing fire_at"))?;

    if Utc::now().timestamp() >= fire_at {
        Ok(Some(serde_json::json!({
            "source": "timer",
            "type": "timer_expired",
            "duration": sub.condition.get("duration").and_then(|v| v.as_str()).unwrap_or("?"),
            "fired_at": Utc::now().timestamp(),
        })))
    } else {
        Ok(None)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_timer_not_yet() {
        let sub = Subscription {
            id: "t1".into(),
            source: "timer".into(),
            condition: serde_json::json!({"fire_at": Utc::now().timestamp() + 3600, "duration": "1h"}),
            mode: "wait".into(),
            callback: None,
            status: "active".into(),
            created_at: Utc::now().timestamp(),
            expires_at: None,
            event_data: None,
        };
        assert!(check(&sub).unwrap().is_none());
    }

    #[test]
    fn test_timer_fired() {
        let sub = Subscription {
            id: "t2".into(),
            source: "timer".into(),
            condition: serde_json::json!({"fire_at": Utc::now().timestamp() - 1, "duration": "0s"}),
            mode: "wait".into(),
            callback: None,
            status: "active".into(),
            created_at: Utc::now().timestamp(),
            expires_at: None,
            event_data: None,
        };
        let event = check(&sub).unwrap().unwrap();
        assert_eq!(event["type"], "timer_expired");
    }
}

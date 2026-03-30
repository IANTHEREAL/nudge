use anyhow::Result;
use chrono::Utc;

/// Check if a timer condition has fired.
pub fn check(fire_at: i64, duration: &str) -> Result<Option<serde_json::Value>> {
    if Utc::now().timestamp() >= fire_at {
        Ok(Some(serde_json::json!({
            "source": "timer",
            "type": "timer_expired",
            "duration": duration,
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
        let fire_at = Utc::now().timestamp() + 3600;
        assert!(check(fire_at, "1h").unwrap().is_none());
    }

    #[test]
    fn test_timer_fired() {
        let fire_at = Utc::now().timestamp() - 1;
        let event = check(fire_at, "0s").unwrap().unwrap();
        assert_eq!(event["type"], "timer_expired");
    }
}

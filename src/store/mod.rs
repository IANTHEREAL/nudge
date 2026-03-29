use anyhow::{bail, Result};
use rusqlite::{params, Connection};

use crate::subscription::Subscription;

pub struct StatusCounts {
    pub active: i64,
    pub fired: i64,
    pub expired: i64,
    pub cancelled: i64,
}

pub struct Store {
    conn: Connection,
}

impl Store {
    pub fn open(path: &str) -> Result<Self> {
        let conn = Connection::open(path)?;
        conn.execute_batch("PRAGMA journal_mode=WAL; PRAGMA busy_timeout=5000;")?;
        conn.execute_batch(
            "CREATE TABLE IF NOT EXISTS subscriptions (
                id          TEXT PRIMARY KEY,
                source      TEXT NOT NULL,
                condition   TEXT NOT NULL,
                mode        TEXT NOT NULL,
                callback    TEXT,
                status      TEXT NOT NULL DEFAULT 'active',
                created_at  INTEGER NOT NULL,
                expires_at  INTEGER,
                event_data  TEXT
            );
            CREATE INDEX IF NOT EXISTS idx_sub_status ON subscriptions(status);
            CREATE INDEX IF NOT EXISTS idx_sub_source ON subscriptions(source);",
        )?;
        Ok(Self { conn })
    }

    pub fn open_default() -> Result<Self> {
        let dir = dirs_next().join(".nudge");
        if !dir.exists() {
            std::fs::create_dir_all(&dir)?;
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o700))?;
            }
        }
        let path = dir.join("subscriptions.db");
        Self::open(path.to_str().unwrap())
    }

    pub fn insert(&self, sub: &Subscription) -> Result<String> {
        self.conn.execute(
            "INSERT INTO subscriptions (id, source, condition, mode, callback, status, created_at, expires_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
            params![
                sub.id,
                sub.source,
                serde_json::to_string(&sub.condition)?,
                sub.mode,
                sub.callback,
                sub.status,
                sub.created_at,
                sub.expires_at,
            ],
        )?;
        Ok(sub.id.clone())
    }

    pub fn get(&self, id: &str) -> Result<Option<Subscription>> {
        let mut stmt = self.conn.prepare(
            "SELECT id, source, condition, mode, callback, status, created_at, expires_at, event_data
             FROM subscriptions WHERE id = ?1",
        )?;
        let mut rows = stmt.query_map(params![id], |row| {
            Ok(row_to_sub(row))
        })?;
        match rows.next() {
            Some(Ok(sub)) => Ok(Some(sub)),
            Some(Err(e)) => Err(e.into()),
            None => Ok(None),
        }
    }

    pub fn list_active(&self) -> Result<Vec<Subscription>> {
        self.list(None, Some("active"))
    }

    pub fn list(&self, source: Option<&str>, status: Option<&str>) -> Result<Vec<Subscription>> {
        let mut sql = "SELECT id, source, condition, mode, callback, status, created_at, expires_at, event_data FROM subscriptions WHERE 1=1".to_string();
        let mut values: Vec<Box<dyn rusqlite::types::ToSql>> = vec![];

        if let Some(s) = source {
            sql.push_str(" AND source = ?");
            values.push(Box::new(s.to_string()));
        }
        if let Some(s) = status {
            sql.push_str(" AND status = ?");
            values.push(Box::new(s.to_string()));
        }
        sql.push_str(" ORDER BY created_at DESC");

        let mut stmt = self.conn.prepare(&sql)?;
        let params: Vec<&dyn rusqlite::types::ToSql> = values.iter().map(|v| v.as_ref()).collect();
        let rows = stmt.query_map(params.as_slice(), |row| Ok(row_to_sub(row)))?;

        let mut subs = vec![];
        for row in rows {
            subs.push(row?);
        }
        Ok(subs)
    }

    pub fn set_status(&self, id: &str, status: &str) -> Result<()> {
        let rows = self.conn.execute(
            "UPDATE subscriptions SET status = ?1 WHERE id = ?2 AND status = 'active'",
            params![status, id],
        )?;
        if rows == 0 {
            let exists: bool = self.conn.query_row(
                "SELECT COUNT(*) > 0 FROM subscriptions WHERE id = ?1",
                params![id],
                |row| row.get(0),
            )?;
            if exists {
                bail!("subscription {id} is not active");
            } else {
                bail!("subscription {id} not found");
            }
        }
        Ok(())
    }

    pub fn set_fired(&self, id: &str, event_data: &serde_json::Value) -> Result<bool> {
        let rows = self.conn.execute(
            "UPDATE subscriptions SET status = 'fired', event_data = ?1 WHERE id = ?2 AND status = 'active'",
            params![serde_json::to_string(event_data)?, id],
        )?;
        Ok(rows > 0)
    }

    pub fn expire_overdue(&self) -> Result<usize> {
        let now = chrono::Utc::now().timestamp();
        let rows = self.conn.execute(
            "UPDATE subscriptions SET status = 'expired'
             WHERE status = 'active' AND expires_at IS NOT NULL AND expires_at <= ?1",
            params![now],
        )?;
        Ok(rows)
    }

    pub fn status_counts(&self) -> Result<StatusCounts> {
        let mut stmt = self.conn.prepare(
            "SELECT status, COUNT(*) FROM subscriptions GROUP BY status",
        )?;
        let mut counts = StatusCounts { active: 0, fired: 0, expired: 0, cancelled: 0 };
        let rows = stmt.query_map([], |row| {
            Ok((row.get::<_, String>(0)?, row.get::<_, i64>(1)?))
        })?;
        for row in rows {
            let (status, count) = row?;
            match status.as_str() {
                "active" => counts.active = count,
                "fired" => counts.fired = count,
                "expired" => counts.expired = count,
                "cancelled" => counts.cancelled = count,
                _ => {}
            }
        }
        Ok(counts)
    }
}

fn row_to_sub(row: &rusqlite::Row) -> Subscription {
    let condition_str: String = row.get(2).unwrap_or_default();
    let event_str: Option<String> = row.get(8).unwrap_or(None);

    Subscription {
        id: row.get(0).unwrap_or_default(),
        source: row.get(1).unwrap_or_default(),
        condition: serde_json::from_str(&condition_str)
            .expect("invalid condition JSON in database; delete ~/.nudge/subscriptions.db to reset"),
        mode: row.get(3).unwrap_or_default(),
        callback: row.get(4).unwrap_or(None),
        status: row.get(5).unwrap_or_default(),
        created_at: row.get(6).unwrap_or(0),
        expires_at: row.get(7).unwrap_or(None),
        event_data: event_str.and_then(|s| serde_json::from_str(&s).ok()),
    }
}

fn dirs_next() -> std::path::PathBuf {
    if let Ok(home) = std::env::var("HOME") {
        std::path::PathBuf::from(home)
    } else {
        std::path::PathBuf::from(".")
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::subscription::{Condition, GitHubCondition};

    fn timer_sub(id: &str) -> Subscription {
        Subscription {
            id: id.into(),
            source: "timer".into(),
            condition: Condition::Timer { duration: "30m".into(), fire_at: 99999999999 },
            mode: "wait".into(),
            callback: None,
            status: "active".into(),
            created_at: 1000,
            expires_at: None,
            event_data: None,
        }
    }

    #[test]
    fn test_insert_and_get() {
        let store = Store::open(":memory:").unwrap();
        let sub = timer_sub("test-1");
        store.insert(&sub).unwrap();
        let got = store.get("test-1").unwrap().unwrap();
        assert_eq!(got.id, "test-1");
        assert_eq!(got.source, "timer");
        assert_eq!(got.status, "active");
    }

    #[test]
    fn test_insert_and_get_github() {
        let store = Store::open(":memory:").unwrap();
        let sub = Subscription {
            id: "gh-1".into(),
            source: "github".into(),
            condition: Condition::GitHub(GitHubCondition::PrMerged {
                repo: "foo/bar".into(), number: 42,
            }),
            mode: "wait".into(),
            callback: None,
            status: "active".into(),
            created_at: 1000,
            expires_at: None,
            event_data: None,
        };
        store.insert(&sub).unwrap();
        let got = store.get("gh-1").unwrap().unwrap();
        assert_eq!(got.source, "github");
        match got.condition {
            Condition::GitHub(GitHubCondition::PrMerged { repo, number }) => {
                assert_eq!(repo, "foo/bar");
                assert_eq!(number, 42);
            }
            _ => panic!("expected GitHub PrMerged"),
        }
    }

    #[test]
    fn test_set_fired() {
        let store = Store::open(":memory:").unwrap();
        let sub = timer_sub("test-2");
        store.insert(&sub).unwrap();
        store.set_fired("test-2", &serde_json::json!({"result": "done"})).unwrap();
        let got = store.get("test-2").unwrap().unwrap();
        assert_eq!(got.status, "fired");
        assert_eq!(got.event_data.unwrap()["result"], "done");
    }

    #[test]
    fn test_cancel_only_active() {
        let store = Store::open(":memory:").unwrap();
        let sub = timer_sub("test-cancel");
        store.insert(&sub).unwrap();
        store.set_fired("test-cancel", &serde_json::json!({"done": true})).unwrap();
        let err = store.set_status("test-cancel", "cancelled").unwrap_err();
        assert!(err.to_string().contains("not active"), "expected 'not active' error, got: {err}");
    }

    #[test]
    fn test_cancel_not_found() {
        let store = Store::open(":memory:").unwrap();
        let err = store.set_status("nonexistent", "cancelled").unwrap_err();
        assert!(err.to_string().contains("not found"), "expected 'not found' error, got: {err}");
    }

    #[test]
    fn test_list_active() {
        let store = Store::open(":memory:").unwrap();
        for i in 0..3 {
            let mut sub = timer_sub(&format!("s-{i}"));
            sub.created_at = 1000 + i as i64;
            if i == 1 {
                sub.status = "fired".into();
            }
            store.insert(&sub).unwrap();
        }
        let active = store.list_active().unwrap();
        assert_eq!(active.len(), 2);
    }

    #[test]
    fn test_set_fired_only_active() {
        let store = Store::open(":memory:").unwrap();
        let mut sub = timer_sub("test-fire-active");
        sub.expires_at = Some(0); // already expired by timestamp
        store.insert(&sub).unwrap();
        store.expire_overdue().unwrap();
        // After expiration, set_fired should return false
        let fired = store.set_fired("test-fire-active", &serde_json::json!({})).unwrap();
        assert!(!fired);
    }

    #[test]
    fn test_fire_before_expire_ordering() {
        let store = Store::open(":memory:").unwrap();
        let mut sub = timer_sub("test-order");
        sub.expires_at = Some(0); // expired by timestamp, but we fire first
        store.insert(&sub).unwrap();
        // Fire first
        assert!(store.set_fired("test-order", &serde_json::json!({"ok": true})).unwrap());
        // Now expire_overdue should not touch it (already fired)
        let expired = store.expire_overdue().unwrap();
        assert_eq!(expired, 0);
        let got = store.get("test-order").unwrap().unwrap();
        assert_eq!(got.status, "fired");
    }
}

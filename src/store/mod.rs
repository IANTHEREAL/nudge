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
        std::fs::create_dir_all(&dir)?;
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
            "UPDATE subscriptions SET status = ?1 WHERE id = ?2",
            params![status, id],
        )?;
        if rows == 0 {
            bail!("subscription {id} not found");
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
        condition: serde_json::from_str(&condition_str).unwrap_or(serde_json::Value::Null),
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

    #[test]
    fn test_insert_and_get() {
        let store = Store::open(":memory:").unwrap();
        let sub = Subscription {
            id: "test-1".into(),
            source: "timer".into(),
            condition: serde_json::json!({"duration": "30m"}),
            mode: "wait".into(),
            callback: None,
            status: "active".into(),
            created_at: 1000,
            expires_at: Some(2000),
            event_data: None,
        };
        store.insert(&sub).unwrap();
        let got = store.get("test-1").unwrap().unwrap();
        assert_eq!(got.id, "test-1");
        assert_eq!(got.source, "timer");
        assert_eq!(got.status, "active");
    }

    #[test]
    fn test_set_fired() {
        let store = Store::open(":memory:").unwrap();
        let sub = Subscription {
            id: "test-2".into(),
            source: "timer".into(),
            condition: serde_json::json!({}),
            mode: "wait".into(),
            callback: None,
            status: "active".into(),
            created_at: 1000,
            expires_at: None,
            event_data: None,
        };
        store.insert(&sub).unwrap();
        store.set_fired("test-2", &serde_json::json!({"result": "done"})).unwrap();
        let got = store.get("test-2").unwrap().unwrap();
        assert_eq!(got.status, "fired");
        assert_eq!(got.event_data.unwrap()["result"], "done");
    }

    #[test]
    fn test_list_active() {
        let store = Store::open(":memory:").unwrap();
        for i in 0..3 {
            let sub = Subscription {
                id: format!("s-{i}"),
                source: "timer".into(),
                condition: serde_json::json!({}),
                mode: "wait".into(),
                callback: None,
                status: if i == 1 { "fired".into() } else { "active".into() },
                created_at: 1000 + i,
                expires_at: None,
                event_data: None,
            };
            store.insert(&sub).unwrap();
        }
        let active = store.list_active().unwrap();
        assert_eq!(active.len(), 2);
    }
}

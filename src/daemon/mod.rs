use std::collections::HashMap;
use std::path::PathBuf;
use anyhow::Result;
use fs2::FileExt;
use tokio::time::{sleep, Duration, Instant};

use crate::checker;
use crate::cli::DaemonArgs;
use crate::store::Store;

const PID_FILE: &str = ".nudge/daemon.pid";
const TICK_INTERVAL_SECS: u64 = 2; // Base tick for timer responsiveness
const GITHUB_INTERVAL_SECS: u64 = 60; // GitHub API poll interval to avoid rate limits

/// Check if the daemon is already running.
pub fn is_running() -> bool {
    let pid_path = home_dir().join(PID_FILE);
    if let Ok(pid_str) = std::fs::read_to_string(&pid_path) {
        if let Ok(pid) = pid_str.trim().parse::<u32>() {
            // Check if process exists
            return std::path::Path::new(&format!("/proc/{pid}")).exists();
        }
    }
    false
}

/// Ensure daemon is running. Start it if not.
/// Uses file locking on the PID file to prevent TOCTOU races.
pub fn ensure_running() -> Result<()> {
    let pid_path = home_dir().join(PID_FILE);
    if let Some(parent) = pid_path.parent() {
        std::fs::create_dir_all(parent)?;
    }

    // Open-or-create the PID file and take an exclusive lock
    let lock_file = std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .open(&pid_path)?;
    lock_file.lock_exclusive()?;

    // Under the lock, check if daemon is already alive
    if let Ok(pid_str) = std::fs::read_to_string(&pid_path) {
        if let Ok(pid) = pid_str.trim().parse::<u32>() {
            if std::path::Path::new(&format!("/proc/{pid}")).exists() {
                // Already running; lock is released on drop
                return Ok(());
            }
        }
    }

    // Start daemon as a background process with log file
    let log_dir = home_dir().join(".nudge");
    let log_file = std::fs::File::create(log_dir.join("daemon.log"))?;
    let log_err = log_file.try_clone()?;

    let exe = std::env::current_exe()?;
    let child = std::process::Command::new(exe)
        .args(["daemon"])
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::from(log_file))
        .stderr(std::process::Stdio::from(log_err))
        .spawn()?;

    tracing::info!(pid = child.id(), "Started daemon");
    // Lock released on drop of lock_file
    Ok(())
}

/// Wait for a subscription to be fired. Polls the store.
pub async fn wait_for(id: &str) -> Result<serde_json::Value> {
    loop {
        let db = Store::open_default()?;
        if let Some(sub) = db.get(id)? {
            match sub.status.as_str() {
                "fired" => {
                    return Ok(sub.event_data.unwrap_or(serde_json::json!({"status": "fired"})));
                }
                "expired" => {
                    anyhow::bail!("Subscription expired");
                }
                "cancelled" => {
                    anyhow::bail!("Subscription cancelled");
                }
                _ => {} // still active, keep waiting
            }
        } else {
            anyhow::bail!("Subscription {id} not found");
        }
        drop(db);
        sleep(Duration::from_secs(1)).await;
    }
}

/// Run the daemon main loop.
pub async fn run(_args: DaemonArgs) -> Result<()> {
    // Write PID file
    let pid_path = home_dir().join(PID_FILE);
    if let Some(parent) = pid_path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    std::fs::write(&pid_path, std::process::id().to_string())?;

    // Acquire and hold flock for daemon lifetime (serializes startup)
    let _lock_file = std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .open(&pid_path)?;
    _lock_file.lock_exclusive()?;

    tracing::info!(pid = std::process::id(), "Daemon started");

    // Cleanup PID on exit
    let _guard = PidGuard(pid_path.clone());

    // Track last-check time per source type for rate limiting
    let mut last_check: HashMap<String, Instant> = HashMap::new();

    loop {
        if let Err(e) = poll_cycle(&mut last_check).await {
            tracing::error!(error = %e, "Poll cycle error");
        }
        sleep(Duration::from_secs(TICK_INTERVAL_SECS)).await;
    }
}

/// Return the minimum poll interval for a given source.
fn source_interval(source: &str) -> Duration {
    match source {
        "github" => Duration::from_secs(GITHUB_INTERVAL_SECS),
        _ => Duration::from_secs(TICK_INTERVAL_SECS), // timer, etc.
    }
}

async fn poll_cycle(last_check: &mut HashMap<String, Instant>) -> Result<()> {
    let db = Store::open_default()?;

    let now = Instant::now();

    // Determine which sources are due for checking this cycle
    let active = db.list_active()?;
    let mut sources_due: std::collections::HashSet<String> = std::collections::HashSet::new();
    for sub in &active {
        if sources_due.contains(&sub.source) {
            continue; // already determined this source is due
        }
        let interval = source_interval(&sub.source);
        let due = match last_check.get(&sub.source) {
            Some(&last) => now.duration_since(last) >= interval,
            None => true,
        };
        if due {
            sources_due.insert(sub.source.clone());
            last_check.insert(sub.source.clone(), now);
        }
    }

    // Check all active subscriptions whose source is due
    for sub in &active {
        if !sources_due.contains(&sub.source) {
            continue;
        }

        match checker::check(sub).await {
            Ok(Some(event_data)) => {
                // Only fire if still active (prevents double-fire)
                if db.set_fired(&sub.id, &event_data)? {
                    tracing::info!(id = %sub.id, source = %sub.source, "Condition met!");

                    // Dispatch callback for "on" mode (detached so it doesn't block the poll loop)
                    if sub.mode == "on" {
                        if let Some(callback) = &sub.callback {
                            let cmd = callback.clone();
                            let data = event_data.clone();
                            tokio::spawn(async move {
                                dispatch_callback(&cmd, &data).await;
                            });
                        }
                    }
                }
            }
            Ok(None) => {} // not yet
            Err(e) => {
                tracing::warn!(id = %sub.id, error = %e, "Check failed");
            }
        }
    }

    // Expire overdue subscriptions (after condition checks to avoid race at deadline boundary)
    let expired = db.expire_overdue()?;
    if expired > 0 {
        tracing::info!(count = expired, "Expired overdue subscriptions");
    }

    Ok(())
}

async fn dispatch_callback(command: &str, event_data: &serde_json::Value) {
    tracing::info!(command, "Dispatching callback");

    let event_json = serde_json::to_string(event_data).unwrap_or_default();

    let result = tokio::process::Command::new("sh")
        .args(["-c", command])
        .env("NUDGE_EVENT", &event_json)
        .stdin(std::process::Stdio::null())
        .output()
        .await;

    match result {
        Ok(output) => {
            if output.status.success() {
                tracing::info!(command, "Callback succeeded");
            } else {
                let stderr = String::from_utf8_lossy(&output.stderr);
                tracing::error!(command, stderr = %stderr, "Callback failed");
            }
        }
        Err(e) => {
            tracing::error!(command, error = %e, "Failed to spawn callback");
        }
    }
}

fn home_dir() -> PathBuf {
    std::env::var("HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|_| PathBuf::from("."))
}

struct PidGuard(PathBuf);

impl Drop for PidGuard {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.0);
    }
}

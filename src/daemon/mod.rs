use std::path::PathBuf;
use anyhow::Result;
use tokio::time::{sleep, Duration};

use crate::checker;
use crate::cli::DaemonArgs;
use crate::store::Store;

const PID_FILE: &str = ".nudge/daemon.pid";
const POLL_INTERVAL_SECS: u64 = 10;

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
pub fn ensure_running() -> Result<()> {
    if is_running() {
        return Ok(());
    }

    // Start daemon as a background process
    let exe = std::env::current_exe()?;
    let child = std::process::Command::new(exe)
        .args(["daemon"])
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()?;

    tracing::info!(pid = child.id(), "Started daemon");
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

    tracing::info!(pid = std::process::id(), "Daemon started");

    // Cleanup PID on exit
    let _guard = PidGuard(pid_path.clone());

    loop {
        if let Err(e) = poll_cycle().await {
            tracing::error!(error = %e, "Poll cycle error");
        }
        sleep(Duration::from_secs(POLL_INTERVAL_SECS)).await;
    }
}

async fn poll_cycle() -> Result<()> {
    let db = Store::open_default()?;

    // Expire overdue subscriptions
    let expired = db.expire_overdue()?;
    if expired > 0 {
        tracing::info!(count = expired, "Expired overdue subscriptions");
    }

    // Check all active subscriptions
    let active = db.list_active()?;
    for sub in &active {
        match checker::check(sub).await {
            Ok(Some(event_data)) => {
                tracing::info!(id = %sub.id, source = %sub.source, "Condition met!");
                db.set_fired(&sub.id, &event_data)?;

                // Dispatch callback for "on" mode
                if sub.mode == "on" {
                    if let Some(callback) = &sub.callback {
                        dispatch_callback(callback, &event_data).await;
                    }
                }
            }
            Ok(None) => {} // not yet
            Err(e) => {
                tracing::warn!(id = %sub.id, error = %e, "Check failed");
            }
        }
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

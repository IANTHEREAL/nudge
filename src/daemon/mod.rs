use std::collections::HashMap;
use std::path::PathBuf;
use anyhow::Result;
use tokio::time::{sleep, Duration, Instant};

use crate::checker;
use crate::checker::github::{GhCliClient, GitHubClient};
use crate::cli::DaemonArgs;
use crate::store::Store;

const SOCK_NAME: &str = "daemon.sock";
const TICK_INTERVAL_SECS: u64 = 2;
const GITHUB_INTERVAL_SECS: u64 = 60;

/// Check if the daemon is already running by trying to connect to the Unix socket.
pub fn is_running() -> bool {
    let sock_path = nudge_dir().join(SOCK_NAME);
    std::os::unix::net::UnixStream::connect(&sock_path).is_ok()
}

/// Ensure daemon is running. Start it if not.
/// Uses Unix domain socket connect() as health check — no TOCTOU race.
pub fn ensure_running() -> Result<()> {
    let sock_path = nudge_dir().join(SOCK_NAME);
    ensure_dir(&nudge_dir())?;

    // If we can connect, daemon is alive
    if std::os::unix::net::UnixStream::connect(&sock_path).is_ok() {
        return Ok(());
    }

    // Stale socket or no socket — clean up and spawn
    let _ = std::fs::remove_file(&sock_path);

    let log_dir = nudge_dir();
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

    // Poll for readiness — daemon binds socket on startup
    for _ in 0..20 {
        std::thread::sleep(std::time::Duration::from_millis(100));
        if std::os::unix::net::UnixStream::connect(&sock_path).is_ok() {
            return Ok(());
        }
    }

    anyhow::bail!("Daemon failed to start within 2s")
}

/// Wait for a subscription to be fired. Polls the store.
/// Also checks for local expiry and daemon liveness to avoid blocking indefinitely.
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
                _ => {
                    // Check if subscription has passed its expires_at locally
                    // (daemon may not have run expire_overdue yet, or may be dead)
                    if let Some(exp) = sub.expires_at {
                        if chrono::Utc::now().timestamp() >= exp {
                            anyhow::bail!("Subscription expired (timeout reached)");
                        }
                    }
                }
            }
        } else {
            anyhow::bail!("Subscription {id} not found");
        }
        drop(db);

        // Check if daemon is still alive — if not, no one is processing subscriptions
        if !is_running() {
            anyhow::bail!("Daemon is not running — subscription will not be processed");
        }

        sleep(Duration::from_secs(1)).await;
    }
}

/// Run the daemon main loop.
/// bind() on the Unix socket is atomic at the kernel level — two daemons cannot both succeed.
pub async fn run(_args: DaemonArgs) -> Result<()> {
    let sock_path = nudge_dir().join(SOCK_NAME);
    ensure_dir(&nudge_dir())?;

    // Atomic singleton: bind() fails if another daemon holds the socket
    let listener = match tokio::net::UnixListener::bind(&sock_path) {
        Ok(l) => l,
        Err(e) if e.kind() == std::io::ErrorKind::AddrInUse => {
            // Check if the other daemon is actually alive
            if std::os::unix::net::UnixStream::connect(&sock_path).is_ok() {
                tracing::info!("Another daemon is already running");
                return Ok(());
            }
            // Stale socket from a crashed daemon — remove and retry
            std::fs::remove_file(&sock_path)?;
            tokio::net::UnixListener::bind(&sock_path)?
        }
        Err(e) => return Err(e.into()),
    };

    // Clean up socket file on exit
    let _guard = SocketGuard(sock_path);

    tracing::info!(pid = std::process::id(), "Daemon started");

    // Accept connections for health checks (and future RPC)
    tokio::spawn(async move {
        loop {
            let _ = listener.accept().await;
        }
    });

    let client = GhCliClient;
    let mut last_check: HashMap<String, Instant> = HashMap::new();

    loop {
        if let Err(e) = poll_cycle(&mut last_check, &client).await {
            tracing::error!(error = %e, "Poll cycle error");
        }
        sleep(Duration::from_secs(TICK_INTERVAL_SECS)).await;
    }
}

/// Return the minimum poll interval for a given source.
fn source_interval(source: &str) -> Duration {
    match source {
        "github" => Duration::from_secs(GITHUB_INTERVAL_SECS),
        _ => Duration::from_secs(TICK_INTERVAL_SECS),
    }
}

async fn poll_cycle<C: GitHubClient>(last_check: &mut HashMap<String, Instant>, client: &C) -> Result<()> {
    let db = Store::open_default()?;

    // Expire overdue subscriptions FIRST so they aren't checked/fired below
    let expired = db.expire_overdue()?;
    if expired > 0 {
        tracing::info!(count = expired, "Expired overdue subscriptions");
    }

    let now = Instant::now();

    let active = db.list_active()?;
    let mut sources_due: std::collections::HashSet<String> = std::collections::HashSet::new();
    for sub in &active {
        if sources_due.contains(&sub.source) {
            continue;
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

    for sub in &active {
        if !sources_due.contains(&sub.source) {
            continue;
        }

        match checker::check(sub, client).await {
            Ok(Some(event_data)) => {
                if db.set_fired(&sub.id, &event_data)? {
                    tracing::info!(id = %sub.id, source = %sub.source, "Condition met!");

                    // "on" mode is one-shot by design: the callback fires once, then the
                    // subscription is marked "fired" and never re-checked. For recurring
                    // behavior, users re-subscribe inside the callback.
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
            Ok(None) => {}
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

    let mut child = match tokio::process::Command::new("sh")
        .args(["-c", command])
        .env("NUDGE_EVENT", &event_json)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
    {
        Ok(child) => child,
        Err(e) => {
            tracing::error!(command, error = %e, "Failed to spawn callback");
            return;
        }
    };

    let timeout_duration = std::time::Duration::from_secs(300);
    match tokio::time::timeout(timeout_duration, child.wait()).await {
        Ok(Ok(status)) => {
            if status.success() {
                tracing::info!(command, "Callback succeeded");
            } else {
                tracing::error!(command, ?status, "Callback failed");
            }
        }
        Ok(Err(e)) => {
            tracing::error!(command, error = %e, "Failed to wait on callback");
        }
        Err(_) => {
            tracing::error!(command, "Callback timed out after 300s, killing child process");
            let _ = child.kill().await;
        }
    }
}

fn home_dir() -> PathBuf {
    std::env::var("HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|_| PathBuf::from("."))
}

fn nudge_dir() -> PathBuf {
    home_dir().join(".nudge")
}

/// Create directory with mode 0700 (owner-only access).
fn ensure_dir(path: &std::path::Path) -> Result<()> {
    if !path.exists() {
        std::fs::create_dir_all(path)?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o700))?;
        }
    }
    Ok(())
}

/// RAII guard that removes the socket file on drop.
struct SocketGuard(PathBuf);

impl Drop for SocketGuard {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.0);
    }
}

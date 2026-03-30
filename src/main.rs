mod cli;
mod store;
mod checker;
mod daemon;
mod subscription;

use anyhow::Result;
use clap::Parser;
use cli::{Cli, Command};

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_writer(std::io::stderr)
        .with_env_filter(
            tracing_subscriber::EnvFilter::from_default_env()
                .add_directive("nudge=info".parse()?),
        )
        .init();

    let cli = Cli::parse();

    match cli.command {
        Command::Wait(args) => cmd_wait(args).await,
        Command::On(args) => cmd_on(args).await,
        Command::List(args) => cmd_list(args).await,
        Command::Cancel { id } => cmd_cancel(&id).await,
        Command::Status => cmd_status().await,
        Command::Daemon(args) => cmd_daemon(args).await,
    }
}

async fn cmd_wait(args: cli::WaitArgs) -> Result<()> {
    let sub = subscription::Subscription::from_wait_args(&args)?;

    // Ensure daemon is running BEFORE inserting — avoids orphan subscriptions if daemon fails
    daemon::ensure_running()?;

    let db = store::Store::open_default()?;
    let id = db.insert(&sub)?;
    tracing::info!(id = %id, source = %sub.source, "Watching...");

    // Block until fired or timeout
    let event = daemon::wait_for(&id).await?;
    println!("{}", serde_json::to_string_pretty(&event)?);
    Ok(())
}

async fn cmd_on(args: cli::OnArgs) -> Result<()> {
    let sub = subscription::Subscription::from_on_args(&args)?;

    // Ensure daemon is running BEFORE inserting — avoids orphan subscriptions if daemon fails
    daemon::ensure_running()?;

    let db = store::Store::open_default()?;
    let id = db.insert(&sub)?;
    tracing::info!(id = %id, source = %sub.source, callback = %sub.callback.as_deref().unwrap_or(""), "Registered");

    println!("{id}");
    Ok(())
}

async fn cmd_list(args: cli::ListArgs) -> Result<()> {
    let db = store::Store::open_default()?;
    let subs = db.list(args.source.as_deref(), args.status.as_deref())?;
    if subs.is_empty() {
        println!("No subscriptions.");
        return Ok(());
    }
    for sub in &subs {
        println!(
            "{} {} {} {} {}",
            sub.id,
            sub.status,
            sub.source,
            sub.mode,
            sub.condition_summary()
        );
    }
    Ok(())
}

async fn cmd_cancel(id: &str) -> Result<()> {
    let db = store::Store::open_default()?;
    db.set_status(id, "cancelled")?;
    println!("Cancelled {id}");
    Ok(())
}

async fn cmd_status() -> Result<()> {
    let db = store::Store::open_default()?;
    let counts = db.status_counts()?;
    let daemon_running = daemon::is_running();
    println!("Daemon: {}", if daemon_running { "running" } else { "stopped" });
    println!("Active:    {}", counts.active);
    println!("Fired:     {}", counts.fired);
    println!("Expired:   {}", counts.expired);
    println!("Cancelled: {}", counts.cancelled);
    Ok(())
}

async fn cmd_daemon(args: cli::DaemonArgs) -> Result<()> {
    daemon::run(args).await
}

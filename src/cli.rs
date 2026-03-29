use clap::{Parser, Subcommand, Args};

#[derive(Parser)]
#[command(name = "nudge", about = "Subscription system for AI agents")]
pub struct Cli {
    #[command(subcommand)]
    pub command: Command,
}

#[derive(Subcommand)]
pub enum Command {
    /// Block until condition is met, print event JSON to stdout
    Wait(WaitArgs),
    /// Register persistent subscription with callback, exit immediately
    On(OnArgs),
    /// List subscriptions
    List(ListArgs),
    /// Cancel a subscription
    Cancel {
        /// Subscription ID
        id: String,
    },
    /// Show daemon status
    Status,
    /// Start the daemon (usually auto-started)
    Daemon(DaemonArgs),
}

#[derive(Args)]
pub struct WaitArgs {
    /// Source: github, timer, webhook
    pub source: String,
    /// Source-specific arguments (e.g., "pr 42 merged", "30m")
    pub args: Vec<String>,
    /// Timeout duration (e.g., "1h", "30m")
    #[arg(long)]
    pub timeout: Option<String>,
    /// GitHub repo (owner/repo)
    #[arg(long)]
    pub repo: Option<String>,
}

#[derive(Args)]
pub struct OnArgs {
    /// Source: github, timer, webhook
    pub source: String,
    /// Source-specific arguments
    pub args: Vec<String>,
    /// Command to execute when event fires
    #[arg(long)]
    pub run: String,
    /// Timeout duration
    #[arg(long)]
    pub timeout: Option<String>,
    /// GitHub repo (owner/repo)
    #[arg(long)]
    pub repo: Option<String>,
}

#[derive(Args)]
pub struct ListArgs {
    /// Filter by source
    #[arg(long)]
    pub source: Option<String>,
    /// Filter by status
    #[arg(long)]
    pub status: Option<String>,
}

#[derive(Args)]
pub struct DaemonArgs {
    /// Enable webhook HTTP listener
    #[arg(long)]
    pub enable_webhooks: bool,
    /// Webhook listen port
    #[arg(long, default_value = "9876")]
    pub webhook_port: u16,
}

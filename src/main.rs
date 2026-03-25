//! Zestful CLI — agent notification tool.
//!
//! A single binary that provides both the CLI (`notify`, `watch`, `ssh`) and
//! the focus daemon (`daemon`). CLI commands are synchronous; the daemon starts
//! an async tokio/axum runtime for terminal focus switching.

mod cmd;
mod config;
mod focus;

use clap::{Parser, Subcommand};

#[derive(Parser)]
#[command(name = "zestful", version, about = "CLI for the Zestful agent notification app")]
struct Cli {
    #[command(subcommand)]
    command: Commands,
}

#[derive(Subcommand)]
enum Commands {
    /// Send a notification to the Zestful overlay
    Notify {
        /// Agent name (e.g. claude-code, cursor, aider)
        #[arg(long)]
        agent: String,

        /// Message to display
        #[arg(long)]
        message: String,

        /// Severity: info, warning (default), or urgent
        #[arg(long, default_value = "warning", value_parser = ["info", "warning", "urgent"])]
        severity: String,

        /// Terminal URI for click-to-focus (auto-detected if omitted)
        #[arg(long)]
        terminal_uri: Option<String>,

        /// Suppress push notification for this event
        #[arg(long)]
        no_push: bool,
    },

    /// Run a command and notify when it finishes
    Watch {
        /// Override agent name (default: watch)
        #[arg(long, default_value = "watch")]
        agent: String,

        /// Command and arguments to run
        #[arg(trailing_var_arg = true, required = true)]
        command: Vec<String>,
    },

    /// SSH with Zestful forwarding (port + token)
    Ssh {
        /// SSH arguments (destination + options)
        #[arg(trailing_var_arg = true, required = true)]
        args: Vec<String>,
    },

    /// Start the focus daemon (usually auto-started)
    Daemon,
}

fn main() -> anyhow::Result<()> {
    let cli = Cli::parse();

    // Auto-start daemon for commands that need it
    if !matches!(cli.command, Commands::Daemon) {
        config::ensure_daemon();
    }

    match cli.command {
        Commands::Notify {
            agent,
            message,
            severity,
            terminal_uri,
            no_push,
        } => cmd::notify::run(agent, message, severity, terminal_uri, no_push),

        Commands::Watch { agent, command } => cmd::watch::run(agent, command),

        Commands::Ssh { args } => cmd::ssh::run(args),

        Commands::Daemon => cmd::daemon::run(),
    }
}

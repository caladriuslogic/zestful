//! Zestful CLI — agent notification tool.
//!
//! A single binary that provides both the CLI (`notify`, `watch`, `ssh`) and
//! the focus daemon (`daemon`). CLI commands are synchronous; the daemon starts
//! an async tokio/axum runtime for terminal focus switching.

mod cmd;
mod config;
mod events;
pub mod hooks;
pub mod log;
pub mod workspace;

use clap::{Parser, Subcommand};

#[derive(Parser)]
#[command(
    name = "zestful",
    version,
    about = "CLI for the Zestful agent notification app"
)]
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

        /// Print the detected terminal URI to stderr
        #[arg(long)]
        debug: bool,
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

    /// Focus a terminal tab by URI or app name
    Focus {
        /// Terminal URI (e.g. workspace://iterm2/window:1/tab:2)
        #[arg(value_name = "URI")]
        terminal_uri: Option<String>,

        /// App name (alternative to URI)
        #[arg(long)]
        app: Option<String>,

        /// Window ID
        #[arg(long)]
        window_id: Option<String>,

        /// Tab ID
        #[arg(long)]
        tab_id: Option<String>,
    },

    /// Inspect running terminals, multiplexers, IDEs, and browsers
    Inspect {
        #[command(subcommand)]
        command: Option<cmd::inspect::InspectCommand>,

        /// Pretty-print human-readable output instead of JSON
        #[arg(long, global = true)]
        pretty: bool,
    },

    /// Cycle through all detected terminal tabs with focus (1s delay between each)
    TestFocus {
        /// App name to filter by (default: terminal)
        #[arg(long, default_value = "terminal")]
        app: String,
    },

    /// Universal agent hook entry. Reads a JSON payload on stdin, detects
    /// the invoking agent, and sends the appropriate Zestful notification.
    /// Designed as a drop-in hook command for Claude Code, Codex CLI, etc.
    Hook {
        /// Override detected agent kind (e.g. "claude-code", "codex").
        #[arg(long)]
        agent: Option<String>,
    },

    /// Query the local event store. Subcommands: list, tail, count.
    Events {
        #[command(subcommand)]
        command: cmd::events::EventsCommand,
    },

    /// Read the tiles projection from the local event store.
    Tiles {
        /// Filter to a single agent slug.
        #[arg(long)]
        agent: Option<String>,
        /// Override default 24h window (unix ms lower bound).
        #[arg(long)]
        since: Option<i64>,
        /// Print JSON instead of the human table.
        #[arg(long)]
        json: bool,
    },
}

fn main() -> anyhow::Result<()> {
    let cli = Cli::parse();

    // Auto-start daemon for commands that need it
    if !matches!(
        cli.command,
        Commands::Daemon
            | Commands::Inspect { .. }
            | Commands::Focus { .. }
            | Commands::TestFocus { .. }
            | Commands::Hook { .. }
            | Commands::Events { .. }
            | Commands::Tiles { .. }
    ) {
        config::ensure_daemon();
    }

    match cli.command {
        Commands::Notify {
            agent,
            message,
            severity,
            terminal_uri,
            no_push,
            debug,
        } => cmd::notify::run(agent, message, severity, terminal_uri, no_push, debug),

        Commands::Watch { agent, command } => cmd::watch::run(agent, command),

        Commands::Ssh { args } => cmd::ssh::run(args),

        Commands::Daemon => cmd::daemon::run(),

        Commands::Focus {
            terminal_uri,
            app,
            window_id,
            tab_id,
        } => cmd::focus::run(terminal_uri, app, window_id, tab_id),

        Commands::Inspect { command, pretty } => cmd::inspect::run(command, pretty),

        Commands::TestFocus { app } => cmd::test_focus::run(Some(app)),

        Commands::Hook { agent } => cmd::hook::run(agent),

        Commands::Events { command } => cmd::events::run(command),

        Commands::Tiles { agent, since, json } => cmd::tiles::run(agent, since, json),
    }
}

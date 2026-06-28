//! Native CLI (`zj-radar`): `notify` + `setup`. Host-only; gated behind the
//! `cli` feature so the wasm plugin build never pulls clap/toml_edit.

use clap::{Parser, Subcommand};

mod notify;
mod setup;

#[derive(Parser)]
#[command(
    name = "zj-radar",
    version,
    about = "Broadcast agent status to the zj-radar Zellij sidebar, and wire agents up."
)]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Broadcast one agent's status to the sidebar (called from an agent hook).
    Notify {
        /// Agent: claude | codex
        agent: String,
        /// For codex: the JSON the agent passes as a trailing argument.
        input: Option<String>,
        /// Explicit status (claude hooks pass this); bypasses event derivation.
        #[arg(long)]
        status: Option<String>,
        /// Print the payload instead of broadcasting.
        #[arg(long)]
        dry_run: bool,
    },
    /// Idempotently wire installed agents' configs to call `zj-radar notify`.
    Setup {
        /// Agents to set up (default: all detected). v1: codex.
        agents: Vec<String>,
        /// Remove our entries instead of adding them.
        #[arg(long)]
        uninstall: bool,
        /// Show what would change; write nothing.
        #[arg(long)]
        dry_run: bool,
        /// Skip the confirmation prompt.
        #[arg(long)]
        yes: bool,
        /// Overwrite a foreign notify entry (codex).
        #[arg(long)]
        force: bool,
    },
}

/// CLI entry point (called by `src/bin/cli.rs`).
pub fn run() {
    let cli = Cli::parse();
    match cli.command {
        Command::Notify {
            agent,
            input,
            status,
            dry_run,
        } => {
            notify::run(&agent, input.as_deref(), status.as_deref(), dry_run);
        }
        Command::Setup {
            agents,
            uninstall,
            dry_run,
            yes,
            force,
        } => {
            setup::run(&agents, uninstall, dry_run, yes, force);
        }
    }
}

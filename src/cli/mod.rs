//! Native CLI (`zj-radar`): `notify` + `setup`. Host-only; gated behind the
//! `cli` feature so the wasm plugin build never pulls clap/toml_edit.

use clap::{Parser, Subcommand};
use std::path::PathBuf;

mod agents;
mod notify;
mod run;
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
    /// Launch a turnkey Zellij session with the radar rail (owns its own config).
    Run {
        /// Session name (default: current directory's name).
        name: Option<String>,
        /// Print the zellij command instead of launching it.
        #[arg(long)]
        print_cmd: bool,
    },
    /// Idempotently wire installed agents and Zellij to use zj-radar.
    Setup {
        /// Targets to set up (default: detected agents only). Supported: codex, zellij.
        targets: Vec<String>,
        /// Wasm artifact to install when setting up Zellij.
        #[arg(long, value_name = "PATH")]
        wasm: Option<PathBuf>,
        /// Remove our entries instead of adding them.
        #[arg(long)]
        uninstall: bool,
        /// Show what would change; write nothing.
        #[arg(long)]
        dry_run: bool,
        /// Skip the confirmation prompt.
        #[arg(long)]
        yes: bool,
        /// Check setup status without writing files.
        #[arg(long)]
        check: bool,
        /// Use Codex's legacy single-slot notify config instead of hooks.json.
        #[arg(long)]
        legacy_notify: bool,
        /// Overwrite conflicting entries where supported.
        #[arg(long)]
        force: bool,
    },
}

/// CLI entry point (called by `src/bin/cli.rs`).
pub fn run() {
    let cli = Cli::parse();
    match cli.command {
        Command::Run { name, print_cmd } => {
            run::run(run::RunOptions { name, print_cmd });
        }
        Command::Notify {
            agent,
            input,
            status,
            dry_run,
        } => {
            notify::run(&agent, input.as_deref(), status.as_deref(), dry_run);
        }
        Command::Setup {
            targets,
            wasm,
            uninstall,
            dry_run,
            yes,
            check,
            legacy_notify,
            force,
        } => {
            setup::run(setup::SetupOptions {
                targets: &targets,
                wasm: wasm.as_deref(),
                uninstall,
                dry_run,
                yes,
                check,
                legacy_notify,
                force,
            });
        }
    }
}

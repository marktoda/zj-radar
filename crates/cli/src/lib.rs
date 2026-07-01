//! Native CLI (`zj-radar`): the host front door for the sidebar.
//!
//! Three subcommands, one per module:
//! - `notify <agent>` ([`notify`]) — the *pushed* information source. Reads an
//!   agent's hook payload and broadcasts a `zj_radar.status.v1` update. Each
//!   agent is a peer adapter behind the [`agents::Agent::derive`] seam, so
//!   `notify` stays agent-agnostic.
//! - `setup [codex|zellij]` ([`setup`]) — idempotent wiring: manage Codex
//!   notify/`hooks.json`, install the wasm plugin, and inject the rail into a
//!   Zellij layout ([`layout`]).
//! - `run` ([`run`]) — turnkey launch of a Zellij session that owns its own
//!   config with the rail preinstalled.

// Re-export the shared core so the CLI submodules keep addressing these as
// `crate::status`, `crate::payload`, … with no per-reference churn.
#[cfg_attr(not(test), allow(unused_imports))]
pub(crate) use zj_radar_core::{command, kind, payload, status};

use clap::{Parser, Subcommand};
use std::path::PathBuf;

mod agents;
mod fsutil;
pub(crate) mod layout;
mod notify;
mod run;
mod setup;

/// Process-wide failure flag. The setup/run orchestrators report refusals and
/// write failures by printing a diagnostic and returning early through several
/// layers; they mark the invocation failed here instead of threading a Result
/// through every signature. [`run`] maps it to the process exit code, so
/// `zj-radar setup … && next` composes correctly in scripts and installers.
/// A user *declining* a confirmation prompt is not a failure.
pub(crate) mod exit {
    use std::sync::atomic::{AtomicBool, Ordering};

    static FAILED: AtomicBool = AtomicBool::new(false);

    pub(crate) fn fail() {
        FAILED.store(true, Ordering::Relaxed);
    }

    pub(crate) fn failed() -> bool {
        FAILED.load(Ordering::Relaxed)
    }
}

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
        /// Which agent is reporting: `claude` or `codex`.
        agent: String,
        /// Hook payload as a trailing argument (codex). Claude passes it on stdin instead.
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
        /// Download the wasm matching this CLI's version instead of passing --wasm
        /// (set ZJ_RADAR_VERSION to pin a different release tag).
        #[arg(long)]
        download: bool,
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
        /// Inject the rail into the target layout without prompting (consent flag).
        #[arg(long)]
        inject: bool,
        /// Target layout name (default: the config's `default_layout`, else
        /// "default"). Looks up `<config_dir>/layouts/<name>.kdl`; honored by
        /// install, --uninstall, and --check alike.
        #[arg(long, value_name = "NAME")]
        layout: Option<String>,
        /// Open the plugin in a focused floating pane so Zellij can prompt for
        /// permissions (one-time grant). Exits after launching; does not run the
        /// wasm/alias/inject steps.
        #[arg(long, conflicts_with_all = ["wasm", "download", "inject", "layout", "uninstall"])]
        grant: bool,
    },
}

/// CLI entry point (called by `src/main.rs`). Returns the process exit code:
/// failure when any orchestrator flagged a refusal/error via [`exit::fail`].
pub fn run() -> std::process::ExitCode {
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
            download,
            uninstall,
            dry_run,
            yes,
            check,
            legacy_notify,
            force,
            inject,
            layout,
            grant,
        } => {
            setup::run(setup::SetupOptions {
                targets: &targets,
                wasm: wasm.as_deref(),
                download,
                uninstall,
                dry_run,
                yes,
                check,
                legacy_notify,
                force,
                inject,
                layout: layout.as_deref(),
                grant,
            });
        }
    }
    if exit::failed() {
        std::process::ExitCode::FAILURE
    } else {
        std::process::ExitCode::SUCCESS
    }
}

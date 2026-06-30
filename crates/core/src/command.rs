//! Per-pane command activity derived from Zellij's `CommandChanged` event.
//! No `zellij-tile` dependency — pure logic, host-testable.

use crate::kind::Kind;
use crate::observation::TrackedObservation;
use crate::payload::{sanitize, MAX_MSG_CHARS};
use crate::status::Status;
use std::collections::{HashMap, HashSet};

/// Debounce window: a pending fg command must survive this many ticks before
/// being promoted to Running.
pub const DEBOUNCE_TICKS: u64 = 1;

/// Shell/prompt programs that signal "back to the prompt" rather than a real
/// foreground command.
const IGNORE_NAMES: &[&str] = &["zsh", "bash", "fish", "sh", "dash", "starship"];

/// Binaries of the *push*-instrumented agents. These report via the
/// `zj_radar.status.v1` pipe from their hooks, so the command observer must
/// never apply its command lifecycle to them: treating an agent's foreground
/// process as an ordinary command flickers the row between Running and Done as
/// the agent spawns and reaps tool subprocesses (each fg transition is a
/// `CommandChanged` event).
///
/// This MUST equal the set of push adapters (`cli::agents::Agent`) — an exe
/// here with no adapter is suppressed *and* never pushed, so its pane goes dark
/// (the original Gemini bug). The `agent_names_match_push_adapter_sources` guard
/// pins the two sets. Agents without an adapter (e.g. Gemini today) are
/// deliberately absent: they fall through to ordinary command-tracking, which
/// still surfaces a Running/Done lifecycle under their own `Kind`.
pub const AGENT_NAMES: &[&str] = &["claude", "codex"];

/// A pending foreground command awaiting debounce promotion.
struct Pending {
    command: String,
    cwd: String,
    source: String,
    since_tick: u64,
}

/// Tracks per-pane command activity for terminal panes that have no agent
/// producer. The resolved display state is stored as `TrackedObservation` so it can be
/// consumed uniformly by the downstream aggregator.
#[derive(Default)]
pub struct CommandStore {
    /// Resolved displayable state, ready for aggregation.
    resolved: HashMap<u32, TrackedObservation>,
    /// Pending fg commands awaiting debounce promotion.
    pending: HashMap<u32, Pending>,
    /// Panes whose Running command has *left* the foreground, awaiting debounce
    /// confirmation before flipping to Done. Value = tick first observed leaving.
    /// This debounces the Running→Done edge symmetrically with promotion, so a
    /// brief foreground drop (a wrapper spawning a short-lived child) doesn't
    /// flash Done.
    pending_done: HashMap<u32, u64>,
    /// Exit-dedup: last-seen exit status per pane, to avoid re-applying
    /// identical exits.
    exited: HashMap<u32, Option<i32>>,
}

/// Extract the basename from a path-like string (split on `/`, take last
/// non-empty segment; empty string if input is empty).
fn basename(s: &str) -> &str {
    s.rsplit('/').find(|seg| !seg.is_empty()).unwrap_or("")
}

fn is_option_arg(s: &str) -> bool {
    s.starts_with('-') && s != "-"
}

fn first_non_option(args: &[String], start: usize) -> Option<(usize, &str)> {
    args.iter()
        .enumerate()
        .skip(start)
        .find_map(|(idx, arg)| (!is_option_arg(arg)).then_some((idx, arg.as_str())))
}

fn known_subcommand<'a>(args: &'a [String], known: &[&str]) -> Option<(usize, &'a str)> {
    args.iter().enumerate().find_map(|(idx, arg)| {
        (!is_option_arg(arg) && known.contains(&arg.as_str())).then_some((idx, arg.as_str()))
    })
}

fn target_after(args: &[String], start: usize) -> Option<&str> {
    first_non_option(args, start).map(|(_, arg)| arg)
}

fn raw_display(parts: &[&str]) -> String {
    parts
        .iter()
        .filter(|part| !part.is_empty())
        .copied()
        .collect::<Vec<_>>()
        .join(" ")
}

/// Launcher exes that prefix the *real* command (`sudo make`, `time cargo
/// build`, `env FOO=1 pytest`). Classification should see through them.
const WRAPPERS: &[&str] = &["sudo", "doas", "env", "time", "nice", "command", "exec"];

/// Is `s` a leading `KEY=VAL` environment assignment (e.g. `RUST_LOG=debug`)?
/// Requires a non-empty key of `[A-Za-z0-9_]` before the `=`, so flags like
/// `--features=cli` (key contains `-`) and paths are not mistaken for one.
fn is_env_assignment(s: &str) -> bool {
    s.split_once('=').is_some_and(|(key, _)| {
        !key.is_empty() && key.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'_')
    })
}

/// Strip leading `KEY=VAL` assignments and known wrapper exes so the rest of the
/// pipeline classifies the real command: `RUST_LOG=debug cargo test` → `cargo
/// test`, `sudo cargo build` → `cargo build`. Conservative: if peeling would
/// land on an option (a wrapper with a value-taking flag we don't model, e.g.
/// `sudo -u user make`) the original argv is returned unchanged — never worse
/// than not peeling.
fn effective_command(command: &[String]) -> &[String] {
    let mut i = 0;
    loop {
        while command.get(i).is_some_and(|t| is_env_assignment(t)) {
            i += 1;
        }
        match command.get(i) {
            Some(tok) if WRAPPERS.contains(&basename(tok)) => i += 1,
            _ => break,
        }
    }
    match command.get(i) {
        Some(tok) if i > 0 && !is_option_arg(tok) && !is_env_assignment(tok) => &command[i..],
        _ => command,
    }
}

/// Declarative compaction rule for one family of tools. The per-exe special
/// cases all reduce to two questions — *which token is the operative verb* and
/// *which verbs carry a trailing target* — so adding a tool is a one-line table
/// entry rather than another nested match arm.
struct ToolRule {
    /// Exe basenames this rule matches.
    exes: &'static [&'static str],
    /// Known subcommands. `Some` → only a matching arg is the operative verb
    /// (`cargo`, `go`), and an unrecognized argv collapses to the bare exe.
    /// `None` → the first non-option arg is the verb (`npm`, `make`, …).
    subcommands: Option<&'static [&'static str]>,
    /// Verbs after which the next non-option arg is appended as a target
    /// (`cargo test <name>`, `npm run <script>`).
    target_verbs: &'static [&'static str],
}

/// Tools with structured argv. Anything not listed (and not an agent or
/// `python`) falls through to [`FIRST_ARG_RULE`], which keeps the first
/// non-option arg — covering `pytest`, `ruff`, `make`, `just`, `sleep`, etc.
const TOOL_RULES: &[ToolRule] = &[
    ToolRule {
        exes: &["cargo"],
        subcommands: Some(&[
            "test", "build", "check", "clippy", "fmt", "run", "bench", "doc", "clean", "install",
            "publish", "update", "nextest",
        ]),
        target_verbs: &["test", "bench", "run", "nextest"],
    },
    ToolRule {
        exes: &["go"],
        subcommands: Some(&["test", "build", "run", "fmt", "vet", "mod"]),
        target_verbs: &["test", "run"],
    },
    ToolRule {
        exes: &["npm", "pnpm", "yarn", "bun"],
        subcommands: None,
        target_verbs: &["run"],
    },
];

/// The fallback rule: keep the exe plus its first non-option arg.
const FIRST_ARG_RULE: ToolRule = ToolRule {
    exes: &[],
    subcommands: None,
    target_verbs: &[],
};

/// Render `exe` plus its operative verb (and optional target) per `rule`.
fn apply_tool_rule(exe: &str, args: &[String], rule: &ToolRule) -> String {
    let verb = match rule.subcommands {
        Some(subs) => known_subcommand(args, subs),
        None => first_non_option(args, 0),
    };
    let Some((idx, verb)) = verb else {
        return exe.to_string();
    };
    if rule.target_verbs.contains(&verb) {
        if let Some(target) = target_after(args, idx + 1).filter(|t| !t.starts_with('-')) {
            return raw_display(&[exe, verb, target]);
        }
    }
    raw_display(&[exe, verb])
}

/// `python -m <module>` has its own shape (the `-m` flag, plus a `pytest`
/// target), so it stays a dedicated path rather than bending the table.
fn display_python(exe: &str, args: &[String]) -> String {
    if let Some(idx) = args.iter().position(|arg| arg == "-m") {
        match args.get(idx + 1).map(String::as_str) {
            Some("pytest") => match target_after(args, idx + 2) {
                Some(target) => raw_display(&[exe, "-m", "pytest", target]),
                None => raw_display(&[exe, "-m", "pytest"]),
            },
            Some(module) => raw_display(&[exe, "-m", module]),
            None => exe.to_string(),
        }
    } else if let Some((_, script)) = first_non_option(args, 0) {
        raw_display(&[exe, basename(script)])
    } else {
        exe.to_string()
    }
}

/// Compact foreground argv into a rail-friendly activity string. Keep useful
/// verbs/subcommands (`cargo test`, `npm run build`) while dropping noisy flags
/// (`--dangerously-bypass-...`, `--features`, `--watch`) that make the sidebar
/// read like shell history instead of intent.
fn display_command(command: &[String]) -> String {
    let Some(first) = command.first() else {
        return String::new();
    };
    let exe = basename(first);
    let args = &command[1..];
    let raw = match exe {
        // Agents own their pane via the push pipe; show only the bare name.
        "codex" | "claude" | "gemini" => exe.to_string(),
        "python" | "python3" => display_python(exe, args),
        _ => {
            let rule = TOOL_RULES
                .iter()
                .find(|r| r.exes.contains(&exe))
                .unwrap_or(&FIRST_ARG_RULE);
            apply_tool_rule(exe, args, rule)
        }
    };
    sanitize(&raw, MAX_MSG_CHARS)
}

/// Classify a foreground command into the `Kind` that owns its pane. The
/// resulting kind's `as_source()` token is what gets stored as the
/// observation's `source` and later round-tripped back through
/// `Kind::from_source` at roll-up — so classification flows through the `Kind`
/// seam as a type, never a loose string.
fn command_kind(command: &[String], display: &str) -> Kind {
    let exe = command.first().map(|s| basename(s)).unwrap_or("");
    match exe {
        "claude" => Kind::Claude,
        "codex" => Kind::Codex,
        "gemini" => Kind::Gemini,
        "pytest" => Kind::Test,
        "cargo" if display.starts_with("cargo test") || display.starts_with("cargo nextest") => {
            Kind::Test
        }
        "cargo" if display.starts_with("cargo build") || display.starts_with("cargo check") => {
            Kind::Build
        }
        "npm" | "pnpm" | "yarn" | "bun" if display.contains(" test") => Kind::Test,
        "npm" | "pnpm" | "yarn" | "bun" if display.contains(" build") => Kind::Build,
        "npm" | "pnpm" | "yarn" | "bun"
            if display.contains(" dev")
                || display.contains(" start")
                || display.contains(" serve") =>
        {
            Kind::Server
        }
        "go" if display.starts_with("go test") => Kind::Test,
        "go" if display.starts_with("go build") => Kind::Build,
        "make" | "just" | "ruff" => {
            if display.contains("test") {
                Kind::Test
            } else if display.contains("build") {
                Kind::Build
            } else if display.contains("deploy") || display.contains("push") {
                Kind::Deploy
            } else if display.contains("serve")
                || display.contains("server")
                || display.contains("dev")
            {
                Kind::Server
            } else {
                Kind::Command
            }
        }
        _ => Kind::Command,
    }
}

impl CommandStore {
    /// Handle a `CommandChanged` event for a terminal pane.
    ///
    /// `command` is the argv reported by Zellij. `cwd` is the pane's
    /// last-known cwd (None if unknown). `tick` is the current tick.
    pub fn on_command_changed(
        &mut self,
        pane_id: u32,
        command: &[String],
        is_foreground: bool,
        cwd: Option<&str>,
        tick: u64,
    ) {
        // Peel env-prefixes/wrappers once at intake so the ignore check, the
        // display string, and the Kind all classify the real command.
        let command = effective_command(command);
        let name = command.first().map(|s| basename(s)).unwrap_or("");
        // Shells and agents are both "not a real command we track here": shells
        // mean back-to-the-prompt; agents are owned by the push pipe (see
        // AGENT_NAMES). Either way we never open a command lifecycle for them.
        let in_ignore_set = IGNORE_NAMES.contains(&name) || AGENT_NAMES.contains(&name);

        if !is_foreground || in_ignore_set {
            // The foreground command (if any) has ended: clear pending and, if a
            // command was Running, mark it *tentatively* done. `on_timer`
            // confirms the Done after the debounce window — so a momentary
            // foreground drop (a child subprocess, a TUI handoff) doesn't flip
            // the row to Done and straight back.
            self.pending.remove(&pane_id);
            if self
                .resolved
                .get(&pane_id)
                .is_some_and(|s| s.status == Status::Running)
            {
                self.pending_done.entry(pane_id).or_insert(tick);
            }
            // Otherwise leave resolved unchanged (idle stays idle).
        } else {
            // A real foreground command is running: it is no longer leaving, so
            // cancel any tentative-done.
            self.pending_done.remove(&pane_id);
            // Build the cleaned command string.
            let cmd_string = display_command(command);
            if cmd_string.is_empty() {
                // Unknown/empty argv — never surface a blank Running row.
                return;
            }
            let source = command_kind(command, &cmd_string).as_source().to_string();

            let cwd_str = cwd.unwrap_or("").to_string();
            self.pending.insert(
                pane_id,
                Pending {
                    command: cmd_string,
                    cwd: cwd_str,
                    source,
                    since_tick: tick,
                },
            );
        }
    }

    /// Timer tick: promote any pending fg command that has survived the
    /// debounce window to Running.
    pub fn on_timer(&mut self, tick: u64) {
        let to_promote: Vec<u32> = self
            .pending
            .iter()
            .filter(|(_, p)| tick.saturating_sub(p.since_tick) >= DEBOUNCE_TICKS)
            .map(|(&id, _)| id)
            .collect();

        for pane_id in to_promote {
            if let Some(p) = self.pending.remove(&pane_id) {
                let repo = sanitize(basename(&p.cwd), 40).to_string();
                self.resolved.insert(
                    pane_id,
                    TrackedObservation::command(Status::Running, repo, p.command, p.source, tick),
                );
            }
        }

        // Confirm tentative-done transitions that survived the debounce window:
        // a Running command that left the foreground and never came back flips
        // to Done (symmetric debounce with promotion above).
        let to_finish: Vec<u32> = self
            .pending_done
            .iter()
            .filter(|(_, &since)| tick.saturating_sub(since) >= DEBOUNCE_TICKS)
            .map(|(&id, _)| id)
            .collect();
        for pane_id in to_finish {
            self.pending_done.remove(&pane_id);
            if let Some(s) = self.resolved.get_mut(&pane_id) {
                if s.status == Status::Running {
                    s.status = Status::Done;
                    s.on_focus = Some(Status::Idle);
                    s.last_change_tick = tick;
                }
            }
        }
    }

    /// Apply a pane's exit status. Deduped: a repeated identical
    /// `(pane, exit_status)` is a no-op.
    /// `Some(0)` → Done, `Some(n != 0)` → Error.
    /// `None` → Done (pane exited without a recorded exit code, e.g. killed by
    /// a signal). We show it as Done rather than Error because we have no
    /// evidence of failure — the user can see the pane's scrollback if they
    /// care about the cause.
    pub fn on_exit(&mut self, pane_id: u32, exit_status: Option<i32>, tick: u64) {
        // Dedupe: if we've already applied the same exit status, skip.
        if self.exited.get(&pane_id) == Some(&exit_status) {
            return;
        }
        self.exited.insert(pane_id, exit_status);
        // Clear any pending / tentative-done entry for this pane — the exit is
        // authoritative.
        self.pending.remove(&pane_id);
        self.pending_done.remove(&pane_id);

        let new_status = match exit_status {
            Some(0) => Status::Done,
            Some(_) => Status::Error,
            None => Status::Done,
        };

        if let Some(s) = self.resolved.get_mut(&pane_id) {
            s.status = new_status;
            s.on_focus = Some(Status::Idle);
            s.last_change_tick = tick;
            s.exit_code = exit_status;
        } else {
            self.resolved.insert(
                pane_id,
                TrackedObservation {
                    on_focus: Some(Status::Idle),
                    exit_code: exit_status,
                    ..TrackedObservation::command(
                        new_status,
                        String::new(),
                        String::new(),
                        "command".into(),
                        tick,
                    )
                },
            );
        }
    }

    /// Clear-on-focus: apply a pending `on_focus` transition for this pane via
    /// the shared `TrackedObservation::apply_on_focus` (same semantics as `StatusStore`).
    pub fn on_pane_focused(&mut self, pane_id: u32, tick: u64) {
        if let Some(s) = self.resolved.get_mut(&pane_id) {
            s.apply_on_focus(tick);
        }
    }

    /// Recede this pane's completion the instant it finishes under focus (Done
    /// only — see `TrackedObservation::recede_on_focus`). Twin of `StatusStore`'s
    /// method: the caller passes the focused pane id, the store forwards. Lets a
    /// `cargo build` that finishes in the focused pane recede without waiting for
    /// a leave-and-return.
    pub fn recede_if_focused(&mut self, pane_id: u32, tick: u64) {
        if let Some(s) = self.resolved.get_mut(&pane_id) {
            s.recede_on_focus(tick);
        }
    }

    /// Drop entries (resolved + pending + exit-dedup) for panes not in `live`.
    pub fn prune(&mut self, live: &HashSet<u32>) {
        self.resolved.retain(|id, _| live.contains(id));
        self.pending.retain(|id, _| live.contains(id));
        self.pending_done.retain(|id, _| live.contains(id));
        self.exited.retain(|id, _| live.contains(id));
    }

    /// Resolved displayable state for a pane, or None.
    pub fn get(&self, pane_id: u32) -> Option<&TrackedObservation> {
        self.resolved.get(&pane_id)
    }

    pub fn observations(&self) -> impl Iterator<Item = (u32, &TrackedObservation)> {
        self.resolved
            .iter()
            .map(|(&pane_id, observation)| (pane_id, observation))
    }

    /// Insert a snapshot-loaded observation. The caller (`RadarState::load_snapshot`)
    /// owns origin routing — it `match`es on `observation.origin` to pick the store
    /// — so this trusts what it's handed rather than re-checking the origin.
    pub fn insert_snapshot_observation(
        &mut self,
        pane_id: u32,
        observation: TrackedObservation,
    ) {
        self.resolved.insert(pane_id, observation);
    }

    /// True if any pane is Running or has a pending fg command. Used by the wasm
    /// glue to keep the timer armed. Deliberately narrower than
    /// `StatusStore::any_active` (which counts any non-idle, Done included): a
    /// finished command is terminal and needs no further ticking, so only
    /// `Running` (plus a not-yet-promoted pending command) counts as live here.
    pub fn has_pending_or_active(&self) -> bool {
        !self.pending.is_empty() || self.resolved.values().any(|s| s.status == Status::Running)
    }
}

#[cfg(test)]
mod tests;

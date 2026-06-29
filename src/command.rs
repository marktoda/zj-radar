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
pub(crate) const AGENT_NAMES: &[&str] = &["claude", "codex"];

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

    pub(crate) fn observations(&self) -> impl Iterator<Item = (u32, &TrackedObservation)> {
        self.resolved
            .iter()
            .map(|(&pane_id, observation)| (pane_id, observation))
    }

    /// Insert a snapshot-loaded observation. The caller (`RadarState::load_snapshot`)
    /// owns origin routing — it `match`es on `observation.origin` to pick the store
    /// — so this trusts what it's handed rather than re-checking the origin.
    pub(crate) fn insert_snapshot_observation(
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
mod tests {
    use super::*;

    fn argv(parts: &[&str]) -> Vec<String> {
        parts.iter().map(|part| part.to_string()).collect()
    }

    #[test]
    fn display_command_keeps_useful_subcommands_and_drops_flags() {
        assert_eq!(
            display_command(&argv(&[
                "cargo",
                "test",
                "render::tests",
                "--features",
                "cli",
                "--",
                "--nocapture"
            ])),
            "cargo test render::tests"
        );
        assert_eq!(
            display_command(&argv(&[
                "codex",
                "--dangerously-bypass-approvals-and-sandbox",
                "--model",
                "gpt-5"
            ])),
            "codex"
        );
        assert_eq!(
            display_command(&argv(&["npm", "run", "build", "--", "--watch"])),
            "npm run build"
        );
        assert_eq!(
            display_command(&argv(&["python", "-m", "pytest", "-q", "tests/render.rs"])),
            "python -m pytest tests/render.rs"
        );
        assert_eq!(display_command(&argv(&["sleep", "5"])), "sleep 5");
    }

    /// Representative `(argv, display, expected Kind)` covering every `Kind`
    /// that `command_kind` can emit. Shared by the classification test and the
    /// Kind-round-trip guard so both exercise exactly the same set.
    fn kind_classification_cases() -> Vec<(Vec<String>, &'static str, Kind)> {
        use crate::kind::Kind;
        vec![
            // Agents, by basename.
            (argv(&["claude"]), "claude", Kind::Claude),
            (argv(&["codex", "--dangerously-bypass-sandbox"]), "codex", Kind::Codex),
            (argv(&["gemini"]), "gemini", Kind::Gemini),
            // Test runners across ecosystems.
            (argv(&["cargo", "test", "--features", "cli"]), "cargo test", Kind::Test),
            (argv(&["pytest"]), "pytest", Kind::Test),
            (argv(&["go", "test", "./..."]), "go test ./...", Kind::Test),
            (argv(&["npm", "run", "test"]), "npm run test", Kind::Test),
            // Build.
            (argv(&["cargo", "build"]), "cargo build", Kind::Build),
            (argv(&["npm", "run", "build"]), "npm run build", Kind::Build),
            // Server and deploy (npm dev-server; make/just verb routing).
            (argv(&["npm", "run", "dev"]), "npm run dev", Kind::Server),
            (argv(&["just", "serve"]), "just serve", Kind::Server),
            (argv(&["make", "deploy"]), "make deploy", Kind::Deploy),
            // Anything unrecognized is a plain command.
            (argv(&["sleep", "5"]), "sleep 5", Kind::Command),
        ]
    }

    #[test]
    fn wrappers_and_env_prefixes_are_peeled_before_classification() {
        // Real Zellij argv routinely carries env assignments and launcher
        // wrappers. The observer must classify the *wrapped* command, not the
        // wrapper, for both the display string and the Kind.
        let cases: &[(&[&str], &str, Kind)] = &[
            (&["RUST_LOG=debug", "cargo", "test", "render"], "cargo test render", Kind::Test),
            (&["sudo", "cargo", "build"], "cargo build", Kind::Build),
            (&["env", "FOO=1", "BAR=2", "pytest"], "pytest", Kind::Test),
            (&["time", "npm", "run", "build"], "npm run build", Kind::Build),
        ];
        for (args, want_msg, want_kind) in cases {
            let mut store = CommandStore::default();
            store.on_command_changed(1, &argv(args), true, Some("/work/repo"), 1);
            store.on_timer(2);
            let s = store
                .get(1)
                .unwrap_or_else(|| panic!("{args:?} should be tracked"));
            assert_eq!(&s.msg, want_msg, "display for {args:?}");
            assert_eq!(s.source, want_kind.as_source(), "kind for {args:?}");
        }
    }

    #[test]
    fn a_wrapped_agent_is_still_suppressed() {
        // `sudo claude` is still claude — a push-owned agent — so it must not
        // open a command lifecycle even behind a wrapper.
        let mut store = CommandStore::default();
        store.on_command_changed(1, &argv(&["sudo", "claude"]), true, Some("/work/repo"), 1);
        store.on_timer(2);
        assert!(store.get(1).is_none(), "wrapped agent must stay suppressed");
    }

    #[test]
    fn unknown_wrapper_options_are_left_alone() {
        // `sudo -u user make` carries a value-taking option we don't model;
        // rather than mis-parse it, peeling bails and leaves the command as-is
        // (no regression vs. not peeling). It still tracks as a generic command.
        let mut store = CommandStore::default();
        store.on_command_changed(1, &argv(&["sudo", "-u", "user", "make"]), true, Some("/r"), 1);
        store.on_timer(2);
        let s = store.get(1).expect("should still be tracked");
        assert_eq!(s.source, Kind::Command.as_source());
    }

    #[test]
    fn command_kind_classifies_every_emitted_kind() {
        for (cmd, display, expected) in kind_classification_cases() {
            assert_eq!(command_kind(&cmd, display), expected, "classify {display:?}");
        }
    }

    #[test]
    fn command_source_round_trips_through_kind() {
        // Twin of the agent-side `source_round_trips_through_kind` (see
        // CONTEXT.md "Information source"). The command path stores
        // `command_kind(..).as_source()` and the roll-up reads it back via
        // `Kind::from_source`, so every classified command must survive that
        // round-trip to the SAME kind — and never degrade to `Kind::Other`,
        // the reserved sentinel for a genuinely-unknown source. (Kind's own
        // universal round-trip is guarded in `kind.rs`; this pins that the
        // command boundary actually rides that seam.)
        use crate::kind::Kind;
        for (cmd, display, _) in kind_classification_cases() {
            let kind = command_kind(&cmd, display);
            assert_ne!(kind, Kind::Other, "{display:?} classified as the Other sentinel");
            assert_eq!(
                Kind::from_source(kind.as_source()),
                kind,
                "{display:?} source {:?} must round-trip to its kind",
                kind.as_source(),
            );
        }
    }

    #[test]
    fn resolved_command_source_round_trips_through_kind() {
        // End-to-end twin: drive a command through the store and confirm the
        // *persisted* observation `source` (not just the classifier output)
        // round-trips to the kind the classifier picked. Guards the wiring in
        // `on_command_changed` → `on_timer`, not only `command_kind` in
        // isolation.
        use crate::kind::Kind;
        let mut store = CommandStore::default();
        let cmd = argv(&["cargo", "test", "--features", "cli"]);
        store.on_command_changed(1, &cmd, true, Some("/home/u/repo"), 1);
        store.on_timer(1 + DEBOUNCE_TICKS);
        let obs = store.get(1).expect("fg command promoted to resolved");
        assert_eq!(Kind::from_source(&obs.source), Kind::Test);
    }

    // ── Test 1: fg real command → pending, NOT Running until on_timer past DEBOUNCE_TICKS

    #[test]
    fn fg_command_stays_pending_until_debounce() {
        let mut store = CommandStore::default();
        let cmd = vec!["sleep".to_string(), "5".to_string()];

        // t=1: fg command arrives → pending, not yet Running
        store.on_command_changed(1, &cmd, true, Some("/home/user/myrepo"), 1);
        assert!(
            store.get(1).is_none(),
            "must not be Running yet — still pending"
        );
        assert!(store.pending.contains_key(&1), "must be in pending");

        // t=1: timer fires at same tick → not past debounce (0 < 1)
        store.on_timer(1);
        assert!(store.get(1).is_none(), "still pending at same tick");

        // t=2: timer fires past debounce (2 - 1 = 1 >= DEBOUNCE_TICKS) → promote
        store.on_timer(2);
        let s = store.get(1).expect("must be Running after debounce");
        assert_eq!(s.status, Status::Running);
        assert_eq!(s.msg, "sleep 5");
        assert_eq!(s.source, "command");
        assert_eq!(s.repo, "myrepo");
        assert!(
            !store.pending.contains_key(&1),
            "pending cleared after promotion"
        );
    }

    // ── Test 2: fg blip filtered (real command then is_foreground=false before timer)

    #[test]
    fn fg_blip_cleared_before_timer_never_becomes_running() {
        let mut store = CommandStore::default();
        let cmd = vec!["cargo".to_string(), "build".to_string()];

        // t=1: fg real command → pending
        store.on_command_changed(1, &cmd, true, None, 1);
        assert!(store.pending.contains_key(&1));

        // t=1: is_foreground=false (e.g. zellij reports bg) → clear pending
        store.on_command_changed(1, &[], false, None, 1);
        assert!(
            !store.pending.contains_key(&1),
            "pending cleared on return-to-shell"
        );

        // t=5: timer fires — nothing to promote
        store.on_timer(5);
        assert!(store.get(1).is_none(), "must never become Running");
    }

    // ── Test 3: starship ignore-set: stays Idle, no pending, no Done

    #[test]
    fn starship_on_idle_pane_leaves_no_trace() {
        let mut store = CommandStore::default();
        let cmd = vec!["starship".to_string()];

        store.on_command_changed(1, &cmd, true, None, 1);
        assert!(
            !store.pending.contains_key(&1),
            "starship must not enter pending"
        );
        assert!(store.get(1).is_none(), "no resolved state expected");
    }

    // ── Test 4: Running → return-to-shell → Done with on_focus; on_pane_focused → Idle

    #[test]
    fn running_to_return_to_shell_sets_done_then_focused_sets_idle() {
        let mut store = CommandStore::default();
        let cmd = vec!["make".to_string()];

        // t=1: fg real command
        store.on_command_changed(1, &cmd, true, Some("/repo"), 1);
        // t=2: promote to Running
        store.on_timer(2);
        assert_eq!(store.get(1).unwrap().status, Status::Running);

        // t=3: return-to-shell (is_foreground=false) → tentative, still Running
        store.on_command_changed(1, &[], false, None, 3);
        assert_eq!(store.get(1).unwrap().status, Status::Running);
        // t=4: timer past debounce → Done with on_focus=Some(Idle)
        store.on_timer(4);
        let s = store.get(1).unwrap();
        assert_eq!(s.status, Status::Done);
        assert_eq!(s.on_focus, Some(Status::Idle));
        assert_eq!(s.last_change_tick, 4);

        // t=5: pane focused → Idle, on_focus cleared
        store.on_pane_focused(1, 5);
        let s = store.get(1).unwrap();
        assert_eq!(s.status, Status::Idle);
        assert_eq!(s.on_focus, None);
        assert_eq!(s.last_change_tick, 5);
    }

    // ── Test 5: on_exit(Some(0)) → Done; on_exit(Some(3)) → Error; dedupe

    #[test]
    fn on_exit_sets_status_and_dedupes() {
        let mut store = CommandStore::default();

        // Exit 0 → Done with on_focus=Some(Idle)
        store.on_exit(1, Some(0), 5);
        let s = store.get(1).unwrap();
        assert_eq!(s.status, Status::Done);
        assert_eq!(s.on_focus, Some(Status::Idle));

        // Repeated identical exit → no-op (on_focus unchanged, tick unchanged)
        store.on_exit(1, Some(0), 10);
        let s = store.get(1).unwrap();
        assert_eq!(
            s.last_change_tick, 5,
            "repeated identical exit must be a no-op"
        );

        // Pane 2: nonzero exit → Error
        store.on_exit(2, Some(3), 6);
        let s = store.get(2).unwrap();
        assert_eq!(s.status, Status::Error);
        assert_eq!(s.on_focus, Some(Status::Idle));

        // Repeated identical exit for pane 2 → no-op
        store.on_exit(2, Some(3), 99);
        assert_eq!(
            store.get(2).unwrap().last_change_tick,
            6,
            "repeated identical exit must be a no-op"
        );
    }

    // ── Test 6: basename of an absolute argv[0] path

    #[test]
    fn absolute_argv0_path_basename_used_for_command_and_repo() {
        let mut store = CommandStore::default();
        // Nix store path for cargo
        let cmd = vec![
            "/nix/store/abc123-cargo-1.0/bin/cargo".to_string(),
            "build".to_string(),
        ];

        store.on_command_changed(1, &cmd, true, Some("/home/user/myproject"), 1);
        store.on_timer(2);
        let s = store.get(1).expect("must be Running");
        assert_eq!(s.msg, "cargo build", "basename of nix path must be used");
        assert_eq!(s.source, "build");
        assert_eq!(s.repo, "myproject", "repo must be basename of cwd");
    }

    // ── Test 7: prune drops dead panes from all maps

    #[test]
    fn prune_drops_dead_panes_from_all_maps() {
        let mut store = CommandStore::default();

        // Set up pane 1: pending
        store.on_command_changed(1, &["vim".to_string()], true, None, 1);
        // Set up pane 2: resolved Running
        store.on_command_changed(2, &["cargo".to_string()], true, None, 1);
        store.on_timer(2);
        // Set up pane 3: has exit record
        store.on_exit(3, Some(0), 1);

        // Keep only pane 2
        let live: HashSet<u32> = [2].into_iter().collect();
        store.prune(&live);

        assert!(store.get(1).is_none(), "pane 1 resolved must be pruned");
        assert!(
            !store.pending.contains_key(&1),
            "pane 1 pending must be pruned"
        );
        assert!(store.get(2).is_some(), "pane 2 must survive");
        assert!(store.get(3).is_none(), "pane 3 resolved must be pruned");
        assert!(
            !store.exited.contains_key(&3),
            "pane 3 exited must be pruned"
        );
    }

    // ── Test 8: has_pending_or_active

    #[test]
    fn has_pending_or_active_reflects_state() {
        let mut store = CommandStore::default();
        assert!(!store.has_pending_or_active(), "empty store → false");

        // Add a pending entry
        store.on_command_changed(1, &["vim".to_string()], true, None, 1);
        assert!(store.has_pending_or_active(), "true while pending");

        // Promote to Running
        store.on_timer(2);
        assert!(store.has_pending_or_active(), "true while Running");

        // Return to shell → tentative; still active (Running) until debounce.
        store.on_command_changed(1, &[], false, None, 3);
        assert!(
            store.has_pending_or_active(),
            "still active until the debounce window flips it to Done"
        );

        // Timer past debounce → Done (no pending, no Running).
        store.on_timer(4);
        assert!(
            !store.has_pending_or_active(),
            "false once Done (no pending, no Running)"
        );

        // Focus to clear to Idle
        store.on_pane_focused(1, 5);
        assert!(!store.has_pending_or_active(), "false when Idle");
    }

    // ── Additional edge cases ──

    #[test]
    fn return_to_shell_on_idle_pane_leaves_no_done() {
        // A starship blip on an idle prompt must NOT create a Done entry.
        let mut store = CommandStore::default();

        // Pane is idle (no resolved entry yet); return-to-shell arrives
        store.on_command_changed(1, &[], false, None, 1);
        assert!(
            store.get(1).is_none(),
            "idle + return-to-shell must not create Done"
        );
    }

    #[test]
    fn ignore_set_covers_all_shells() {
        let mut store = CommandStore::default();
        // All shell/prompt names in IGNORE_NAMES must be filtered. "starship" is
        // included because it fires a CommandChanged event before the real shell
        // prompt reappears — treating it as a command would cause a spurious Done.
        for shell in &["zsh", "bash", "fish", "sh", "dash", "starship"] {
            let cmd = vec![shell.to_string()];
            store.on_command_changed(1, &cmd, true, None, 1);
            assert!(
                !store.pending.contains_key(&1),
                "{} must not enter pending",
                shell
            );
            assert!(
                store.get(1).is_none(),
                "{} must leave no resolved state",
                shell
            );
        }
    }

    // ── Test: on_exit(None) → Done with on_focus=Some(Idle), ever_active=true

    #[test]
    fn on_exit_none_yields_done_and_ever_active() {
        let mut store = CommandStore::default();

        // A pane that exited without a recorded code (e.g. killed by signal)
        // → Done (not Error), with on_focus=Some(Idle) so it clears when focused.
        store.on_exit(1, None, 5);
        let s = store
            .get(1)
            .expect("must have a resolved entry after on_exit(None)");
        assert_eq!(s.status, Status::Done, "None exit_status must yield Done");
        assert_eq!(
            s.on_focus,
            Some(Status::Idle),
            "on_focus must be set to Idle"
        );
        // A fast `zellij run -- false` that never reached Running must still
        // render as active (✗), so ever_active must be true even for a pane
        // with no prior resolved entry.
        assert!(
            s.ever_active,
            "ever_active must be true for a pane with no prior resolved entry"
        );
    }

    #[test]
    fn on_exit_preserves_existing_repo_and_msg() {
        let mut store = CommandStore::default();
        // Set up Running state
        store.on_command_changed(
            1,
            &["cargo".to_string(), "test".to_string()],
            true,
            Some("/work/pinky"),
            1,
        );
        store.on_timer(2);
        assert_eq!(store.get(1).unwrap().status, Status::Running);
        assert_eq!(store.get(1).unwrap().repo, "pinky");
        assert_eq!(store.get(1).unwrap().msg, "cargo test");
        assert_eq!(store.get(1).unwrap().source, "test");

        // Exit 0 → Done, but repo and msg preserved
        store.on_exit(1, Some(0), 3);
        let s = store.get(1).unwrap();
        assert_eq!(s.status, Status::Done);
        assert_eq!(s.repo, "pinky", "repo must be preserved");
        assert_eq!(s.msg, "cargo test", "msg must be preserved");
    }

    // ── A: agent binaries are push-tracked, never command-tracked ──

    #[test]
    fn agent_foreground_commands_are_not_tracked() {
        // Push-instrumented agents report their status via the push pipe. Their
        // foreground command must leave NO command-store trace — otherwise
        // Zellij's CommandChanged churn (agent → tool subprocess → agent)
        // flickers the row between Running and Done and rewrites its message.
        // The set of suppressed agents is exactly the push adapters (see the
        // `agent_names_match_push_adapter_sources` guard); Gemini is NOT one —
        // see `gemini_foreground_command_is_tracked`.
        for agent in &["claude", "codex"] {
            let mut store = CommandStore::default();
            store.on_command_changed(1, &[agent.to_string()], true, Some("/work/repo"), 1);
            assert!(
                !store.pending.contains_key(&1),
                "{agent} must not enter pending"
            );
            store.on_timer(2);
            assert!(
                store.get(1).is_none(),
                "{agent} must leave no resolved command state"
            );
        }
    }

    #[test]
    fn gemini_foreground_command_is_tracked() {
        // Gemini has no push adapter (the shipped scope is Claude + Codex), so
        // unlike them it is *observed* via command-tracking rather than
        // suppressed — otherwise its panes would show nothing at all. It carries
        // its own `Kind::Gemini` source so it renders with the gemini mark.
        let mut store = CommandStore::default();
        store.on_command_changed(1, &["gemini".to_string()], true, Some("/work/repo"), 1);
        store.on_timer(2);
        let s = store
            .get(1)
            .expect("gemini must leave a resolved command observation");
        assert_eq!(s.status, Status::Running);
        assert_eq!(s.source, Kind::Gemini.as_source());
    }

    // ── B: leaving the foreground is debounced before flipping to Done ──

    #[test]
    fn leaving_foreground_debounces_before_marking_done() {
        let mut store = CommandStore::default();
        store.on_command_changed(1, &["make".to_string()], true, Some("/repo"), 1);
        store.on_timer(2);
        assert_eq!(store.get(1).unwrap().status, Status::Running);

        // Return-to-shell: tentative — must still read Running this instant.
        store.on_command_changed(1, &[], false, None, 3);
        assert_eq!(
            store.get(1).unwrap().status,
            Status::Running,
            "leaving the foreground must not flip to Done instantly"
        );

        // Timer past the debounce window → now Done.
        store.on_timer(4);
        let s = store.get(1).unwrap();
        assert_eq!(s.status, Status::Done);
        assert_eq!(s.on_focus, Some(Status::Idle));
        assert_eq!(s.last_change_tick, 4);
    }

    #[test]
    fn brief_foreground_drop_replaced_by_command_never_shows_done() {
        // A pane that briefly drops out of the foreground then immediately runs
        // another real command (e.g. a wrapper spawning a child) must never show
        // a spurious Done in between.
        let mut store = CommandStore::default();
        store.on_command_changed(1, &["make".to_string()], true, Some("/repo"), 1);
        store.on_timer(2);
        assert_eq!(store.get(1).unwrap().status, Status::Running);

        store.on_command_changed(1, &[], false, None, 3);
        store.on_command_changed(1, &["rg".to_string(), "needle".to_string()], true, Some("/repo"), 3);

        store.on_timer(4);
        assert_eq!(
            store.get(1).unwrap().status,
            Status::Running,
            "a brief fg drop replaced by a new command must never surface Done"
        );
    }

    // ── C: an empty/unknown foreground command never becomes a blank row ──

    #[test]
    fn empty_foreground_command_is_never_promoted() {
        let mut store = CommandStore::default();
        store.on_command_changed(1, &[], true, Some("/repo"), 1);
        assert!(
            !store.pending.contains_key(&1),
            "empty fg argv must not enter pending"
        );
        store.on_timer(2);
        assert!(
            store.get(1).is_none(),
            "empty fg command must leave no resolved state (no blank Running row)"
        );
    }

    #[test]
    fn on_pane_focused_same_status_does_not_update_tick() {
        let mut store = CommandStore::default();
        // Place pane in Done with on_focus=Some(Done) (same status → no tick update)
        store.on_exit(1, Some(0), 5);
        // Manually set on_focus to Done (same as current status) to test tick stability
        store.resolved.get_mut(&1).unwrap().on_focus = Some(Status::Done);
        store.on_pane_focused(1, 10);
        assert_eq!(store.get(1).unwrap().status, Status::Done);
        // last_change_tick should NOT be updated (status did not change)
        assert_eq!(store.get(1).unwrap().last_change_tick, 5);
    }
}

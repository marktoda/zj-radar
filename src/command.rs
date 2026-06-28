//! Per-pane command activity derived from Zellij's `CommandChanged` event.
//! No `zellij-tile` dependency — pure logic, host-testable.

use crate::payload::{sanitize, MAX_MSG_CHARS};
use crate::state::AgentState;
use crate::status::Status;
use std::collections::{HashMap, HashSet};

/// Debounce window: a pending fg command must survive this many ticks before
/// being promoted to Running.
pub const DEBOUNCE_TICKS: u64 = 1;

/// Shell/prompt programs that signal "back to the prompt" rather than a real
/// foreground command.
const IGNORE_NAMES: &[&str] = &["zsh", "bash", "fish", "sh", "dash", "starship"];

/// A pending foreground command awaiting debounce promotion.
struct Pending {
    command: String,
    cwd: String,
    since_tick: u64,
}

/// Tracks per-pane command activity for terminal panes that have no agent
/// producer. The resolved display state is stored as `AgentState` so it can be
/// consumed uniformly by the downstream aggregator.
#[derive(Default)]
pub struct CommandStore {
    /// Resolved displayable state, ready for aggregation.
    resolved: HashMap<u32, AgentState>,
    /// Pending fg commands awaiting debounce promotion.
    pending: HashMap<u32, Pending>,
    /// Exit-dedup: last-seen exit status per pane, to avoid re-applying
    /// identical exits.
    exited: HashMap<u32, Option<i32>>,
}

/// Extract the basename from a path-like string (split on `/`, take last
/// non-empty segment; empty string if input is empty).
fn basename(s: &str) -> &str {
    s.rsplit('/').find(|seg| !seg.is_empty()).unwrap_or("")
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
        let name = command.first().map(|s| basename(s)).unwrap_or("");
        let in_ignore_set = IGNORE_NAMES.contains(&name);

        if !is_foreground || in_ignore_set {
            // The foreground command (if any) has ended: clear pending and
            // possibly transition Running → Done.
            self.pending.remove(&pane_id);
            if let Some(s) = self.resolved.get_mut(&pane_id) {
                if s.status == Status::Running {
                    s.status = Status::Done;
                    s.on_focus = Some(Status::Idle);
                    s.last_change_tick = tick;
                }
                // Otherwise leave resolved unchanged (idle stays idle).
            }
        } else {
            // Real foreground command: build the cleaned command string.
            let cmd_string = if command.is_empty() {
                String::new()
            } else {
                let base = basename(&command[0]);
                let rest = command[1..].join(" ");
                let raw = if rest.is_empty() {
                    base.to_string()
                } else {
                    format!("{} {}", base, rest)
                };
                sanitize(&raw, MAX_MSG_CHARS)
            };

            let cwd_str = cwd.unwrap_or("").to_string();
            self.pending.insert(
                pane_id,
                Pending {
                    command: cmd_string,
                    cwd: cwd_str,
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
                    AgentState {
                        status: Status::Running,
                        repo,
                        branch: String::new(),
                        msg: p.command,
                        last_change_tick: tick,
                        seq: None,
                        on_focus: None,
                        ever_active: true,
                        source: "command".into(),
                    },
                );
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
        // Clear any pending entry for this pane.
        self.pending.remove(&pane_id);

        let new_status = match exit_status {
            Some(0) => Status::Done,
            Some(_) => Status::Error,
            None => Status::Done,
        };

        if let Some(s) = self.resolved.get_mut(&pane_id) {
            s.status = new_status;
            s.on_focus = Some(Status::Idle);
            s.last_change_tick = tick;
        } else {
            self.resolved.insert(
                pane_id,
                AgentState {
                    status: new_status,
                    repo: String::new(),
                    branch: String::new(),
                    msg: String::new(),
                    last_change_tick: tick,
                    seq: None,
                    on_focus: Some(Status::Idle),
                    ever_active: true,
                    source: "command".into(),
                },
            );
        }
    }

    /// Clear-on-focus: apply a pending `on_focus` transition for this pane via
    /// the shared `AgentState::apply_on_focus` (same semantics as `StateStore`).
    pub fn on_pane_focused(&mut self, pane_id: u32, tick: u64) {
        if let Some(s) = self.resolved.get_mut(&pane_id) {
            s.apply_on_focus(tick);
        }
    }

    /// Drop entries (resolved + pending + exit-dedup) for panes not in `live`.
    pub fn prune(&mut self, live: &HashSet<u32>) {
        self.resolved.retain(|id, _| live.contains(id));
        self.pending.retain(|id, _| live.contains(id));
        self.exited.retain(|id, _| live.contains(id));
    }

    /// Resolved displayable state for a pane, or None.
    pub fn get(&self, pane_id: u32) -> Option<&AgentState> {
        self.resolved.get(&pane_id)
    }

    /// True if any pane is Running or has a pending fg command.
    /// Used by the wasm glue to keep the timer armed.
    pub fn has_pending_or_active(&self) -> bool {
        !self.pending.is_empty() || self.resolved.values().any(|s| s.status == Status::Running)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

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

        // t=3: return-to-shell (is_foreground=false) → Done with on_focus=Some(Idle)
        store.on_command_changed(1, &[], false, None, 3);
        let s = store.get(1).unwrap();
        assert_eq!(s.status, Status::Done);
        assert_eq!(s.on_focus, Some(Status::Idle));
        assert_eq!(s.last_change_tick, 3);

        // t=4: pane focused → Idle, on_focus cleared
        store.on_pane_focused(1, 4);
        let s = store.get(1).unwrap();
        assert_eq!(s.status, Status::Idle);
        assert_eq!(s.on_focus, None);
        assert_eq!(s.last_change_tick, 4);
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

        // Return to shell → Done
        store.on_command_changed(1, &[], false, None, 3);
        assert!(
            !store.has_pending_or_active(),
            "false once Done (no pending, no Running)"
        );

        // Focus to clear to Idle
        store.on_pane_focused(1, 4);
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

        // Exit 0 → Done, but repo and msg preserved
        store.on_exit(1, Some(0), 3);
        let s = store.get(1).unwrap();
        assert_eq!(s.status, Status::Done);
        assert_eq!(s.repo, "pinky", "repo must be preserved");
        assert_eq!(s.msg, "cargo test", "msg must be preserved");
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

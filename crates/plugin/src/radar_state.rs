//! Session radar state: live tabs/panes plus source-specific observations.
//! No zellij-tile dependency.

use crate::command::CommandStore;
use crate::config;
use crate::observation::{ObservationOrigin, TrackedObservation};
use crate::payload;
use crate::render::TabRow;
use crate::rollup::{self, TabDisplay};
use crate::status::Status;
use crate::status_store::StatusStore;
use crate::tab_namer::{PaneFacts, TabFacts, TabNamer, TabRename};
use crate::theme;
use std::collections::{HashMap, HashSet};

/// Direction for attention-tab cycling.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Direction {
    Next,
    Prev,
}

/// Pick the next/previous tab position that needs attention, relative to the
/// active tab, wrapping at the ends. Pure over `(position, status)` pairs so it
/// is trivially testable and deterministic — every per-tab plugin instance that
/// receives the same broadcast computes the identical target (idempotent switch).
///
/// Returns `None` when no tab needs attention, or when the only attention tab is
/// already active (a no-op).
fn cycle_attention(
    tabs: &[(usize, Status)],
    active: Option<usize>,
    dir: Direction,
) -> Option<usize> {
    let mut members: Vec<usize> = tabs
        .iter()
        .filter(|(_, s)| s.needs_attention())
        .map(|(p, _)| *p)
        .collect();
    members.sort_unstable();
    members.dedup();
    if members.is_empty() {
        return None;
    }
    let target = match (dir, active) {
        (Direction::Next, Some(a)) => members
            .iter()
            .copied()
            .find(|&p| p > a)
            .or_else(|| members.first().copied()),
        (Direction::Next, None) => members.first().copied(),
        (Direction::Prev, Some(a)) => members
            .iter()
            .rev()
            .copied()
            .find(|&p| p < a)
            .or_else(|| members.last().copied()),
        (Direction::Prev, None) => members.last().copied(),
    };
    match target {
        Some(t) if Some(t) != active => Some(t),
        _ => None,
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub(crate) struct TabId(usize);

impl TabId {
    pub(crate) fn new(raw: usize) -> Self {
        Self(raw)
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct RadarTab {
    pub id: TabId,
    pub position: usize,
    pub name: String,
    pub active: bool,
    pub has_bell: bool,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct TerminalPane {
    pub id: u32,
    pub title: String,
    pub focused_in_tab: bool,
}

#[derive(Debug)]
pub(crate) struct PaneUpdate {
    pub tab_panes: HashMap<usize, Vec<TerminalPane>>,
    pub live: HashSet<u32>,
    pub theme: Option<theme::DerivedColors>,
    pub exits: Vec<(u32, Option<i32>)>,
}

/// One terminal pane as the Zellij host reports it — the few `PaneInfo` fields
/// the radar consumes, copied into plain owned data. The wasm adapter (`lib.rs`)
/// does nothing but this field copy; every bit of *policy* lives in
/// [`PaneUpdate::from_raw`], so it stays host-testable.
pub(crate) struct RawPane {
    pub tab_pos: usize,
    pub id: u32,
    pub title: String,
    pub is_plugin: bool,
    pub is_focused: bool,
    pub default_bg: Option<String>,
    pub default_fg: Option<String>,
    pub exited: bool,
    pub exit_status: Option<i32>,
}

impl PaneUpdate {
    /// Fold the host's panes into a `PaneUpdate`: drop plugin panes (the rail
    /// itself), sanitize titles, collect live ids and exits, and derive the
    /// terminal theme from the focused pane's reported bg/fg — falling back to
    /// the first terminal pane that reports both. Pure over `RawPane`, so the
    /// color precedence and plugin-skip are unit-testable without a live Zellij.
    pub(crate) fn from_raw(panes: Vec<RawPane>) -> PaneUpdate {
        let mut tab_panes: HashMap<usize, Vec<TerminalPane>> = HashMap::new();
        let mut live: HashSet<u32> = HashSet::new();
        let mut exits: Vec<(u32, Option<i32>)> = Vec::new();
        let mut focused_colors: Option<(theme::Rgb, theme::Rgb)> = None;
        let mut any_colors: Option<(theme::Rgb, theme::Rgb)> = None;
        for p in panes {
            if p.is_plugin {
                continue;
            }
            let colors = match (
                p.default_bg.as_deref().and_then(theme::parse_hex),
                p.default_fg.as_deref().and_then(theme::parse_hex),
            ) {
                (Some(bg), Some(fg)) => Some((bg, fg)),
                _ => None,
            };
            if let Some(c) = colors {
                any_colors.get_or_insert(c);
                if p.is_focused {
                    focused_colors = Some(c);
                }
            }
            tab_panes.entry(p.tab_pos).or_default().push(TerminalPane {
                id: p.id,
                title: payload::sanitize(&p.title, 40),
                focused_in_tab: p.is_focused,
            });
            live.insert(p.id);
            if p.exited {
                exits.push((p.id, p.exit_status));
            }
        }
        let theme = focused_colors
            .or(any_colors)
            .map(|(bg, fg)| theme::DerivedColors::from_bg_fg(bg, fg));
        PaneUpdate {
            tab_panes,
            live,
            theme,
            exits,
        }
    }
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct RadarChange {
    pub render: bool,
    pub persist_snapshot: bool,
    pub renames: Vec<TabRename>,
    /// Terminal panes whose working directory should be read once (via a
    /// blocking `get_pane_cwd` host call in the wasm glue) to bootstrap a name
    /// for a freshly-opened tab that has not emitted a `CwdChanged` yet.
    pub cwd_bootstrap: Vec<u32>,
    /// Whether this event's focus is trustworthy enough to fire notifications now
    /// (the notifier suppresses the focused pane) — see `CONTEXT.md`'s `## Settle`
    /// entry. `false` defers notification to the timer. (It no longer gates any
    /// rail-state change; focus stopped driving state in the drop-focus refactor.)
    pub settle: bool,
}

/// Upper bound on the number of one-shot `get_pane_cwd` reads requested per
/// `PaneUpdate`. Each read is a blocking host round-trip, so we cap a single
/// update's fan-out (e.g. a session restore surfacing many panes at once),
/// resolving focused panes first. The overflow is picked up on the next
/// `PaneUpdate` that occurs — so in the rare case of a large burst followed by
/// total inactivity, the tail stays unnamed until the next interaction. This is
/// the postmortem's "cap concurrent in-flight host calls" rule; the cap is
/// generous enough that opening tabs one at a time never hits it.
const MAX_CWD_BOOTSTRAP_PER_UPDATE: usize = 8;

#[derive(Default)]
pub(crate) struct RadarState {
    status: StatusStore,
    command: CommandStore,
    tabs: Vec<RadarTab>,
    tab_panes: HashMap<usize, Vec<TerminalPane>>,
    pane_cwd: HashMap<u32, String>,
    namer: TabNamer,
    last_focused: Option<u32>,
    live_panes: Option<HashSet<u32>>,
    /// Pane ids we have already requested an initial `get_pane_cwd` read for.
    /// Tracks *attempts*, not successes, so a pane that has no cwd yet is never
    /// re-polled; pruned with `pane_cwd` so a recycled id can bootstrap again.
    cwd_bootstrap_attempted: HashSet<u32>,
}

impl RadarState {
    pub(crate) fn load_snapshot(&mut self, raw: &str) -> Option<u64> {
        let (observations, tick) = snapshot::load(raw)?;
        self.status = StatusStore::default();
        self.command = CommandStore::default();
        // This match is the SINGLE origin→store guard: each entry's intrinsic
        // origin (strict on deserialize) routes it to exactly one store, so the
        // stores trust what they're handed and don't re-check. Deserialize already
        // rejects unknown origins, dropping the whole snapshot.
        for (pane_id, observation) in observations {
            match observation.origin {
                ObservationOrigin::StatusPipe => self
                    .status
                    .insert_snapshot_observation(pane_id, observation),
                ObservationOrigin::Command => self
                    .command
                    .insert_snapshot_observation(pane_id, observation),
            }
        }
        Some(tick)
    }

    pub(crate) fn snapshot_json(&self, existing: Option<&str>, tick: u64) -> String {
        // Both stores' observations carry their own `origin`, so the snapshot
        // module keys the merge on it — no need to tag the two iterators here.
        let current = self.status.observations().chain(self.command.observations());
        snapshot::to_json(current, self.live_panes.as_ref(), existing, tick)
    }

    /// Target tab position for an `attention-next`/`attention-prev` command, or
    /// `None` for a no-op. Reads the live active tab and per-tab rollup; the
    /// pure `cycle_attention` owns the ordering/wrap logic.
    pub(crate) fn next_attention_tab(&self, dir: Direction) -> Option<usize> {
        let active = self.tabs.iter().find(|t| t.active).map(|t| t.position);
        // Order is irrelevant here: `cycle_attention` sorts the attention
        // members itself, so we gather `(position, status)` pairs as-is.
        let pairs: Vec<(usize, Status)> = self
            .tabs
            .iter()
            .map(|t| (t.position, self.tab_display_for(t.position).status))
            .collect();
        cycle_attention(&pairs, active, dir)
    }

    pub(crate) fn rows(&self) -> Vec<TabRow> {
        let mut rows = Vec::new();
        let mut sorted = self.tabs.clone();
        sorted.sort_by_key(|t| t.position);
        for t in &sorted {
            rows.push(TabRow {
                number: t.position as u32 + 1,
                name: t.name.clone(),
                active: t.active,
                has_bell: t.has_bell,
                display: self.tab_display_for(t.position),
            });
        }
        rows
    }

    pub(crate) fn tabs_changed(&mut self, tabs: Vec<RadarTab>) -> RadarChange {
        self.tabs = tabs;
        // Drop naming state for tabs that closed (the update carries the full
        // current set), so `applied` doesn't accrete gone tabs.
        let live: HashSet<TabId> = self.tabs.iter().map(|t| t.id).collect();
        self.namer.retain_tabs(&live);
        RadarChange {
            render: true,
            settle: false,
            ..RadarChange::default()
        }
    }

    pub(crate) fn panes_changed(
        &mut self,
        update: PaneUpdate,
        tick: u64,
        now_epoch_s: u64,
        naming: config::NamingMode,
    ) -> RadarChange {
        for (pane_id, exit_status) in update.exits {
            // Displaced completion ignored here (a prior Done/Error still on
            // the pane when a fresh exit lands); Task 11 ledgers it.
            let _ = self.command.on_exit(pane_id, exit_status, tick, now_epoch_s);
        }
        self.live_panes = Some(update.live.clone());
        self.tab_panes = update.tab_panes;
        // Dropped completions ignored here; Task 11 ledgers a pane closing with
        // an unreceded Done/Error still on it.
        let _ = self.status.prune(&update.live);
        // Dropped completions ignored here; Task 11 ledgers a pane closing with
        // an unreceded Done/Error still on it.
        let _ = self.command.prune(&update.live);
        self.pane_cwd.retain(|id, _| update.live.contains(id));
        self.cwd_bootstrap_attempted
            .retain(|id| update.live.contains(id));
        // Track this update's fresh focus for notification suppression. It no
        // longer drives rail state (no focus recede) — see `note_focus`. `settle`
        // still gates the notifier: `panes_changed` carries trustworthy focus, so
        // it fires notifications now rather than deferring to the timer.
        self.note_focus(self.focused_terminal_in_active_tab());

        RadarChange {
            render: true,
            persist_snapshot: true,
            renames: self.rename_tabs(naming),
            cwd_bootstrap: self.cwd_bootstrap_targets(naming),
            settle: true,
        }
    }

    /// Timer tick. Returns whether an observation changed (a debounced
    /// promotion or Done-flip) — the runtime persists the snapshot on it so
    /// timer-driven mutations reach late-spawned instances too.
    ///
    /// TODO(Task 11): consume `TimerReport::receded` into the ledger.
    pub(crate) fn timer(&mut self, tick: u64, now_epoch_s: u64) -> bool {
        self.command.on_timer(tick, now_epoch_s).changed
    }

    pub(crate) fn cwd_changed(
        &mut self,
        pane_id: u32,
        path: String,
        naming: config::NamingMode,
    ) -> RadarChange {
        self.pane_cwd.insert(pane_id, path);
        RadarChange {
            render: true,
            renames: self.rename_tabs(naming),
            settle: false,
            ..RadarChange::default()
        }
    }

    pub(crate) fn command_changed(
        &mut self,
        pane_id: u32,
        command: &[String],
        is_foreground: bool,
        tick: u64,
        now_epoch_s: u64,
    ) -> RadarChange {
        let cwd = self.pane_cwd.get(&pane_id).map(String::as_str);
        self.command
            .on_command_changed(pane_id, command, is_foreground, cwd, tick);
        // A pane back at its shell prompt means the agent that was pushing status
        // has exited (no producer hook fires on quit), so clear the now-stale
        // pushed status → idle. This rides the shared `CommandChanged` signal, so
        // every tab's instance clears in lockstep. `clear_on_prompt_return`
        // ignores a Running status, so a mid-turn foreground flicker to a shell
        // can't be mistaken for the agent exiting.
        //
        // `now_epoch_s` is threaded through for signature symmetry with the
        // other three mutating entry points, but unused today:
        // `clear_on_prompt_return` recedes to Idle and stamps no completion
        // epoch. TODO(Task 11): the ledger may want the wall-clock moment of
        // this recede.
        let _ = now_epoch_s;
        let cleared = crate::command::is_shell_prompt(command, is_foreground)
            && self.status.clear_on_prompt_return(pane_id, tick).is_some();
        RadarChange {
            render: true,
            settle: false,
            // Persist only when we actually cleared, so a newly-opened tab
            // rehydrates the idle from the snapshot rather than the stale status.
            persist_snapshot: cleared,
            ..RadarChange::default()
        }
    }

    pub(crate) fn status_pipe(
        &mut self,
        raw: &str,
        tick: u64,
        now_epoch_s: u64,
        naming: config::NamingMode,
    ) -> Option<RadarChange> {
        let p = payload::parse(raw)?;
        // Displaced completion ignored here; Task 11 ledgers a Done/Error that
        // recedes on overwrite.
        let _ = self.status.apply(p, tick, now_epoch_s);
        // NOTE: we deliberately do NOT settle here. A pushed status is shown as-is;
        // focus no longer recedes or clears it. A completion clears only via a new
        // broadcast for the pane, the return-to-shell exit-clear
        // (`command_changed` → `clear_on_prompt_return`), or a prune.
        Some(RadarChange {
            render: true,
            persist_snapshot: true,
            renames: self.rename_tabs(naming),
            cwd_bootstrap: Vec::new(),
            settle: false,
        })
    }

    /// True while any tracked pane is actively *working* — a status-pipe agent
    /// reporting `Running`, or an observed foreground command still live. This is
    /// the animated set (the spinner), so it wants a per-tick repaint. Deliberately
    /// narrow: a finished `Done`/`Error` or a waiting `Pending` is not "animating"
    /// work, mirroring `CommandStore::has_pending_or_active`.
    ///
    /// This does NOT mean a finished `Done` never needs another tick — a command
    /// `Done` sits on a `DONE_TTL_TICKS` clock before it recedes to Idle (spec
    /// §3.1's cadence design), and that recede has to land on schedule even
    /// though the row itself is static. That's a *separate* arming reason,
    /// deliberately kept out of this predicate: `command_awaiting_recede` carries
    /// the TTL window, so `PluginRuntime::timer_should_continue` ORs the two
    /// rather than broadening `has_running_work` to cover a case it was never
    /// meant to (animation vs. a scheduled one-shot are different reasons to
    /// tick, and conflating them would blur what each predicate promises).
    pub(crate) fn has_running_work(&self) -> bool {
        self.status.any_running() || self.command.has_pending_or_active()
    }

    /// True while a command-origin `Done` is still inside its `DONE_TTL_TICKS`
    /// window, awaiting the recede to Idle. Delegates to
    /// `CommandStore::has_done_awaiting_recede` — see `has_running_work`'s doc
    /// for why this is a distinct arming reason rather than folded into it.
    pub(crate) fn command_awaiting_recede(&self) -> bool {
        self.command.has_done_awaiting_recede()
    }

    pub(crate) fn recompute_renames(&mut self, naming: config::NamingMode) -> Vec<TabRename> {
        self.rename_tabs(naming)
    }

    /// Track the focused terminal pane, for the notifier's "don't ding the pane
    /// you're looking at" suppression only. Focus **no longer drives any rail
    /// state**: `done`/`error`/`pending` rows clear only via a new broadcast, the
    /// return-to-shell exit-clear (`command_changed`), or prune — all *shared*
    /// inputs Zellij delivers to every tab's instance. So the rail renders
    /// identically on every tab regardless of which pane is focused (focus is
    /// per-client and is NOT delivered to background instances — deriving rail
    /// state from it was the source of the cross-tab desync).
    ///
    /// A `None` reading carries no focus information (the active tab's focused
    /// pane is a plugin/float, or topology is mid-churn), so preserve the last
    /// known focus rather than clobbering it.
    pub(crate) fn note_focus(&mut self, focused: Option<u32>) {
        if let Some(id) = focused {
            self.last_focused = Some(id);
        }
    }

    pub(crate) fn last_focused(&self) -> Option<u32> {
        self.last_focused
    }

    /// THE precedence between observation sources: a status-pipe observation wins
    /// over a command one for the same pane id. This is the single definition of
    /// the rule — the one place that knows there is more than one store. Both
    /// consumers (`tab_display`'s roll-up and `notify_views`) read through it, so
    /// the precedence can never be encoded two different ways and silently drift.
    fn resolve(&self, pane_id: u32) -> Option<&TrackedObservation> {
        self.status.get(pane_id).or_else(|| self.command.get(pane_id))
    }

    /// Union of both stores' observations, keyed by pane id, for the notifier.
    /// A pane CAN appear in both stores; `resolve` applies the system-wide
    /// "status wins over command" precedence, so a shared id maps to the status
    /// observation.
    pub(crate) fn notify_views(
        &self,
    ) -> std::collections::BTreeMap<u32, &crate::observation::TrackedObservation> {
        let ids: std::collections::BTreeSet<u32> = self
            .command
            .observations()
            .map(|(id, _)| id)
            .chain(self.status.observations().map(|(id, _)| id))
            .collect();
        ids.into_iter()
            .filter_map(|id| self.resolve(id).map(|o| (id, o)))
            .collect()
    }

    #[cfg(test)]
    pub(crate) fn status(&self, pane_id: u32) -> Option<&crate::observation::TrackedObservation> {
        self.status.get(pane_id)
    }

    #[cfg(test)]
    pub(crate) fn command(&self, pane_id: u32) -> Option<&crate::observation::TrackedObservation> {
        self.command.get(pane_id)
    }

    #[cfg(test)]
    pub(crate) fn status_mut(&mut self) -> &mut StatusStore {
        &mut self.status
    }

    #[cfg(test)]
    pub(crate) fn status_store(&self) -> &StatusStore {
        &self.status
    }

    #[cfg(test)]
    pub(crate) fn command_mut(&mut self) -> &mut CommandStore {
        &mut self.command
    }

    #[cfg(test)]
    pub(crate) fn command_store(&self) -> &CommandStore {
        &self.command
    }

    #[cfg(test)]
    pub(crate) fn set_tab_panes_for_position(&mut self, position: usize, panes: Vec<TerminalPane>) {
        self.tab_panes.insert(position, panes);
    }

    #[cfg(test)]
    pub(crate) fn applied_name(&self, tab_id: TabId) -> Option<&str> {
        self.namer.applied_name(tab_id)
    }

    fn focused_terminal_in_active_tab(&self) -> Option<u32> {
        let active = self.tabs.iter().find(|tab| tab.active)?;
        let panes = self.tab_panes.get(&active.position)?;
        panes
            .iter()
            .find(|pane| pane.focused_in_tab)
            .map(|pane| pane.id)
    }

    /// Live terminal panes whose cwd we have neither learned (via `CwdChanged`)
    /// nor yet attempted to read. Focused panes come first — they name their tab
    /// — and the result is capped at `MAX_CWD_BOOTSTRAP_PER_UPDATE` so one update
    /// never fans out an unbounded number of blocking host calls. Every returned
    /// id is recorded as attempted, so it is requested at most once per lifetime.
    ///
    /// Bootstrap exists only to name tabs, so it is a no-op (and pays for no
    /// blocking reads) when naming is `Off` — mirroring `rename_tabs`.
    fn cwd_bootstrap_targets(&mut self, naming_mode: config::NamingMode) -> Vec<u32> {
        if naming_mode == config::NamingMode::Off {
            return Vec::new();
        }
        let mut focused = Vec::new();
        let mut others = Vec::new();
        for panes in self.tab_panes.values() {
            for p in panes {
                if self.pane_cwd.contains_key(&p.id)
                    || self.cwd_bootstrap_attempted.contains(&p.id)
                {
                    continue;
                }
                if p.focused_in_tab {
                    focused.push(p.id);
                } else {
                    others.push(p.id);
                }
            }
        }
        // Deterministic order regardless of HashMap iteration; focused first.
        focused.sort_unstable();
        others.sort_unstable();
        let targets: Vec<u32> = focused
            .into_iter()
            .chain(others)
            .take(MAX_CWD_BOOTSTRAP_PER_UPDATE)
            .collect();
        for id in &targets {
            self.cwd_bootstrap_attempted.insert(*id);
        }
        targets
    }

    /// Delegate naming to the [`TabNamer`]: assemble resolved facts, then let the
    /// naming module pick and remember names. `Off` short-circuits before any
    /// fact-building (the namer also no-ops on `Off`).
    fn rename_tabs(&mut self, naming_mode: config::NamingMode) -> Vec<TabRename> {
        if naming_mode == config::NamingMode::Off {
            return Vec::new();
        }
        let facts = self.name_facts();
        self.namer.rename(&facts, naming_mode)
    }

    /// Join this state's tabs, pane topology, status observations, and known
    /// cwds into the resolved [`TabFacts`] the [`TabNamer`] consumes. `repo` is
    /// sourced from the *status* store only (commands carry no repo), matching
    /// the pre-extraction behavior; the raw `cwd`/`title` are processed inside
    /// the namer. Iterates `self.tabs` in stored order, as the old renamer did.
    fn name_facts(&self) -> Vec<TabFacts> {
        self.tabs
            .iter()
            .map(|tab| {
                let empty = Vec::new();
                let panes = self.tab_panes.get(&tab.position).unwrap_or(&empty);
                TabFacts {
                    id: tab.id,
                    name: tab.name.clone(),
                    position: tab.position,
                    panes: panes
                        .iter()
                        .map(|p| PaneFacts {
                            repo: self.status.get(p.id).map(|s| s.repo.clone()),
                            cwd: self.pane_cwd.get(&p.id).cloned(),
                            title: p.title.clone(),
                            focused: p.focused_in_tab,
                        })
                        .collect(),
                }
            })
            .collect()
    }

    /// Roll this tab's panes up into a `TabDisplay`. The "status wins over
    /// command" precedence across observation sources lives in `resolve`, with
    /// the stores; `rollup::roll_up` only sees "is there an observation for this
    /// pane?" — keeping the aggregation rules behind the Tab Roll-Up seam.
    fn tab_display(&self, panes: &[TerminalPane]) -> TabDisplay {
        rollup::roll_up(panes, |id| self.resolve(id))
    }

    /// Roll a tab up by position, treating an absent pane list as no panes.
    /// The per-position lookup is shared by `rows` and `next_attention_tab`.
    fn tab_display_for(&self, position: usize) -> TabDisplay {
        let empty = Vec::new();
        let panes = self.tab_panes.get(&position).unwrap_or(&empty);
        self.tab_display(panes)
    }
}

mod snapshot;

#[cfg(test)]
mod tests;

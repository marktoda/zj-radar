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
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, HashMap, HashSet};

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

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct RadarChange {
    pub render: bool,
    pub persist_snapshot: bool,
    pub renames: Vec<TabRename>,
    /// Terminal panes whose working directory should be read once (via a
    /// blocking `get_pane_cwd` host call in the wasm glue) to bootstrap a name
    /// for a freshly-opened tab that has not emitted a `CwdChanged` yet.
    pub cwd_bootstrap: Vec<u32>,
}

/// One v2 snapshot record: a `pane_id` key plus the pane's `TrackedObservation`
/// flattened inline. `TrackedObservation` serializes itself (enum fields as wire
/// tokens, optional fields defaulted), so this wrapper is the *only* snapshot
/// glue — there is no field-by-field mirror struct or mapper.
#[derive(Serialize, Deserialize)]
struct SnapshotEntry {
    pane_id: u32,
    #[serde(flatten)]
    obs: TrackedObservation,
}

#[derive(Serialize, Deserialize)]
struct RadarSnapshot {
    v: u32,
    tick: u64,
    observations: Vec<SnapshotEntry>,
}

#[derive(Serialize, Deserialize)]
struct LegacyStatusSnapshot {
    v: u32,
    tick: u64,
    panes: Vec<LegacyPaneSnapshot>,
}

#[derive(Serialize, Deserialize)]
struct LegacyPaneSnapshot {
    pane_id: u32,
    status: String,
    repo: String,
    branch: String,
    msg: String,
    source: String,
    last_change_tick: u64,
    #[serde(default)]
    on_focus: Option<String>,
    ever_active: bool,
}

const RADAR_SNAPSHOT_V: u32 = 2;
const LEGACY_STATUS_SNAPSHOT_V: u32 = 1;

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
        let (observations, tick) = parse_snapshot(raw)?;
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
        let mut snapshot_tick = tick;
        let mut observations: BTreeMap<(u32, ObservationOrigin), TrackedObservation> =
            BTreeMap::new();

        if let Some(raw) = existing {
            if let Some((existing_observations, existing_tick)) = parse_snapshot(raw) {
                snapshot_tick = snapshot_tick.max(existing_tick);
                for (pane_id, observation) in existing_observations {
                    if self
                        .live_panes
                        .as_ref()
                        .is_some_and(|live| !live.contains(&pane_id))
                    {
                        continue;
                    }
                    observations.insert((pane_id, observation.origin), observation);
                }
            }
        }

        for (pane_id, observation) in self.status.observations() {
            observations.insert(
                (pane_id, ObservationOrigin::StatusPipe),
                observation.clone(),
            );
        }
        for (pane_id, observation) in self.command.observations() {
            observations.insert((pane_id, ObservationOrigin::Command), observation.clone());
        }

        let snapshot = RadarSnapshot {
            v: RADAR_SNAPSHOT_V,
            tick: snapshot_tick,
            observations: observations
                .into_iter()
                .map(|((pane_id, _), obs)| SnapshotEntry { pane_id, obs })
                .collect(),
        };
        serde_json::to_string(&snapshot).unwrap_or_default()
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
        RadarChange {
            render: true,
            ..RadarChange::default()
        }
    }

    pub(crate) fn panes_changed(
        &mut self,
        update: PaneUpdate,
        tick: u64,
        naming: config::NamingMode,
    ) -> RadarChange {
        for (pane_id, exit_status) in update.exits {
            self.command.on_exit(pane_id, exit_status, tick);
        }
        self.live_panes = Some(update.live.clone());
        self.tab_panes = update.tab_panes;
        self.status.prune(&update.live);
        self.command.prune(&update.live);
        self.pane_cwd.retain(|id, _| update.live.contains(id));
        self.cwd_bootstrap_attempted
            .retain(|id| update.live.contains(id));
        // Reconcile against this update's fresh focus: an entry visit-clears the
        // entered pane, or — if focus stayed put — a command that just exited in
        // it recedes. One call; see `reconcile_focus`.
        self.reconcile_focus(self.focused_terminal_in_active_tab(), tick);

        RadarChange {
            render: true,
            persist_snapshot: true,
            renames: self.rename_tabs(naming),
            cwd_bootstrap: self.cwd_bootstrap_targets(naming),
        }
    }

    pub(crate) fn timer(&mut self, tick: u64) {
        self.command.on_timer(tick);
        // Reconcile against the (unchanged) focus on the cadence tick. This is the
        // recede path for a *watched* agent turn (whose Done arrived on the pipe,
        // which deliberately does not reconcile) and for a return-to-shell command
        // confirmed Done this tick. By the time a tick fires, any focus `PaneUpdate`
        // has been processed, so `last_focused` is settled — passing it means
        // `changed == false`, i.e. the recede branch. See `reconcile_focus`.
        self.reconcile_focus(self.last_focused, tick);
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
            ..RadarChange::default()
        }
    }

    pub(crate) fn command_changed(
        &mut self,
        pane_id: u32,
        command: &[String],
        is_foreground: bool,
        tick: u64,
    ) -> RadarChange {
        let cwd = self.pane_cwd.get(&pane_id).map(String::as_str);
        self.command
            .on_command_changed(pane_id, command, is_foreground, cwd, tick);
        RadarChange {
            render: true,
            ..RadarChange::default()
        }
    }

    pub(crate) fn status_pipe(
        &mut self,
        raw: &str,
        tick: u64,
        naming: config::NamingMode,
    ) -> Option<RadarChange> {
        let p = payload::parse(raw)?;
        self.status.apply(p, tick);
        // NOTE: we deliberately do NOT settle here. A status-pipe payload is a raw
        // completion edge that can arrive in the gap between the user switching
        // away from this pane and the focus `PaneUpdate` landing — so `last_focused`
        // may still name this pane even though the user has already left. Receding
        // on that stale focus would silently drop a completion the user *should*
        // see. Instead the recede rides the timer (armed by the runtime on this
        // event), which fires on a cadence by which point focus has settled — so a
        // genuinely-watched agent turn still recedes within a tick, while one you
        // navigated away from stays lit. See `reconcile_focus`.
        Some(RadarChange {
            render: true,
            persist_snapshot: true,
            renames: self.rename_tabs(naming),
            cwd_bootstrap: Vec::new(),
        })
    }

    pub(crate) fn has_active_or_pending_work(&self) -> bool {
        self.status.any_active() || self.command.has_pending_or_active()
    }

    pub(crate) fn recompute_renames(&mut self, naming: config::NamingMode) -> Vec<TabRename> {
        self.rename_tabs(naming)
    }

    /// Reconcile a pane gaining or holding focus against its queued completion.
    /// One operation, two cases derived from whether focus actually moved:
    ///
    /// - **focus CHANGED** (an entry / "visit") → clear the entered pane's queued
    ///   state entirely, `Done` *or* `Error`: entering a pane acknowledges whatever
    ///   it shows ("seen, even errors").
    /// - **focus UNCHANGED** (you are sitting on it) → recede a *fresh `Done`* only:
    ///   you watched it finish, so it should not light the rail; an `Error` or a
    ///   "needs you" `Pending` stays lit even while watched (`recede_if_focused`
    ///   skips them).
    ///
    /// Background panes are never touched here — their completion persists until a
    /// later focus entry recurs through the CHANGED branch. Recede is monotonic
    /// (`Done → Idle` once, `on_focus` then `None`), so calling this on every event
    /// and timer tick cannot oscillate — that is what keeps it free of the
    /// direction-dependent flicker an earlier "clear on every update" version had.
    ///
    /// Callers pass whatever focus they can trust: `panes_changed` passes this
    /// update's fresh focus; `timer` passes the (settled) `last_focused`, which
    /// makes `changed == false` → the recede branch. `status_pipe` deliberately
    /// does NOT call this — its focus could be stale; see the note there. Returns
    /// whether focus changed.
    pub(crate) fn reconcile_focus(&mut self, focused: Option<u32>, tick: u64) -> bool {
        let changed = focused != self.last_focused;
        self.last_focused = focused;
        if let Some(id) = focused {
            if changed {
                self.status.on_pane_focused(id, tick);
                self.command.on_pane_focused(id, tick);
            } else {
                self.status.recede_if_focused(id, tick);
                self.command.recede_if_focused(id, tick);
            }
        }
        changed
    }

    pub(crate) fn last_focused(&self) -> Option<u32> {
        self.last_focused
    }

    /// Union of both stores' observations, keyed by pane id, for the notifier.
    /// A pane CAN appear in both stores; status takes precedence (matching the
    /// system-wide rule), so command is inserted first and status second so that
    /// status overwrites command on collision.
    pub(crate) fn notify_views(
        &self,
    ) -> std::collections::BTreeMap<u32, &crate::observation::TrackedObservation> {
        let mut m = std::collections::BTreeMap::new();
        for (id, o) in self.command.observations() {
            m.insert(id, o);
        }
        for (id, o) in self.status.observations() {
            m.insert(id, o);
        }
        m
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
    pub(crate) fn set_last_focused(&mut self, pane_id: Option<u32>) {
        self.last_focused = pane_id;
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
    /// command" precedence across observation sources lives here, with the
    /// stores; `rollup::roll_up` only sees "is there an observation for this
    /// pane?" — keeping the aggregation rules behind the Tab Roll-Up seam.
    fn tab_display(&self, panes: &[TerminalPane]) -> TabDisplay {
        rollup::roll_up(panes, |id| {
            self.status.get(id).or_else(|| self.command.get(id))
        })
    }

    /// Roll a tab up by position, treating an absent pane list as no panes.
    /// The per-position lookup is shared by `rows` and `next_attention_tab`.
    fn tab_display_for(&self, position: usize) -> TabDisplay {
        let empty = Vec::new();
        let panes = self.tab_panes.get(&position).unwrap_or(&empty);
        self.tab_display(panes)
    }
}

fn parse_snapshot(raw: &str) -> Option<(Vec<(u32, TrackedObservation)>, u64)> {
    let value: serde_json::Value = serde_json::from_str(raw).ok()?;
    match value.get("v").and_then(serde_json::Value::as_u64)? as u32 {
        RADAR_SNAPSHOT_V => parse_v2_snapshot(value),
        LEGACY_STATUS_SNAPSHOT_V => parse_legacy_status_snapshot(value),
        _ => None,
    }
}

fn parse_v2_snapshot(value: serde_json::Value) -> Option<(Vec<(u32, TrackedObservation)>, u64)> {
    // `TrackedObservation` deserializes itself; an entry with an unknown origin
    // fails deserialization, which drops the whole snapshot (`.ok()?`).
    let snapshot: RadarSnapshot = serde_json::from_value(value).ok()?;
    let observations = snapshot
        .observations
        .into_iter()
        .map(|entry| (entry.pane_id, entry.obs))
        .collect();
    Some((observations, snapshot.tick))
}

fn parse_legacy_status_snapshot(
    value: serde_json::Value,
) -> Option<(Vec<(u32, TrackedObservation)>, u64)> {
    let snapshot: LegacyStatusSnapshot = serde_json::from_value(value).ok()?;
    let observations = snapshot
        .panes
        .into_iter()
        .map(|pane| {
            (
                pane.pane_id,
                TrackedObservation {
                    origin: ObservationOrigin::StatusPipe,
                    status: Status::from_wire(&pane.status),
                    repo: pane.repo,
                    branch: pane.branch,
                    msg: pane.msg,
                    source: pane.source,
                    last_change_tick: pane.last_change_tick,
                    on_focus: pane.on_focus.as_deref().map(Status::from_wire),
                    ever_active: pane.ever_active,
                    exit_code: None,
                },
            )
        })
        .collect();
    Some((observations, snapshot.tick))
}

#[cfg(test)]
mod tests;

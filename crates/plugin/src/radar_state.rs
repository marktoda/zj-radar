//! Session radar state: live tabs/panes plus source-specific observations.
//! No zellij-tile dependency.

use crate::command::CommandStore;
use crate::config;
use crate::ledger::{Ledger, LedgerEntry, LedgerOutcome};
use crate::observation::{ObservationOrigin, TrackedObservation};
use crate::payload;
use crate::render::TabRow;
use crate::rollup::{self, TabDisplay};
use crate::status::Status;
use crate::status_store::StatusStore;
use crate::tab_namer::{PaneFacts, TabFacts, TabNamer, TabRename};
use crate::theme;
use serde::{Deserialize, Serialize};
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

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
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

/// A ledger entry, resolved for rendering: the live tab position (or `None`
/// once that tab is gone, making the row click-inert) looked up fresh on every
/// call, rather than cached — the ledger itself only ever remembers the
/// `TabId` it happened in.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct LedgerLine {
    pub at_epoch_s: u64,
    pub error: bool,
    pub tab_name: String,
    pub label: String,
    pub tab_position: Option<usize>,
}

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
    /// Completions that have receded off the rail — every edge that removes a
    /// Done/Error from a card (TTL recede, prompt-return clear, an overwrite,
    /// or a prune) hands its observation here (spec §4.2).
    ledger: Ledger,
    /// Tab ids currently mid-flash, mapped to the tick their flash expires at
    /// (exclusive — `rows` reads `now_tick < expiry`). Set ONLY by `status_pipe`
    /// on a live not-Pending → Pending edge for one of the tab's panes (checked
    /// BEFORE `StatusStore::apply` runs, so a re-broadcast of an already-Pending
    /// status never re-flashes); a snapshot load never flashes (spec §8) since
    /// `load_snapshot` never touches this map. Expired entries are pruned
    /// lazily by `timer` — `rows` stays `&self` so it can be called freely from
    /// render paths without needing `&mut`.
    flash_until: HashMap<TabId, u64>,
}

impl RadarState {
    pub(crate) fn load_snapshot(&mut self, raw: &str) -> Option<u64> {
        let (observations, tick, ledger) = snapshot::load(raw)?;
        self.status = StatusStore::default();
        self.command = CommandStore::default();
        // This match is the SINGLE origin→store guard: each entry's intrinsic
        // origin (strict on deserialize) routes it to exactly one store, so the
        // stores trust what they're handed and don't re-check. Deserialize already
        // rejects unknown origins, dropping the whole snapshot.
        for (pane_id, mut observation) in observations {
            // TTL re-base: a loaded command-origin `Done` keeps ticking down its
            // `DONE_TTL_TICKS` window against whichever tick domain wrote it —
            // possibly a different instance's tick count entirely. Stamping
            // `last_change_tick` to the tick this snapshot is adopting restarts
            // that window fresh on load, so a foreign tick domain can't make it
            // recede instantly (huge apparent tick delta) or never recede
            // (`tick` moving backwards relative to a `last_change_tick` far in
            // its "future"). `Error` is exempt — it has no TTL to re-base.
            if observation.origin == ObservationOrigin::Command && observation.status == Status::Done {
                observation.last_change_tick = tick;
            }
            match observation.origin {
                ObservationOrigin::StatusPipe => self
                    .status
                    .insert_snapshot_observation(pane_id, observation),
                ObservationOrigin::Command => self
                    .command
                    .insert_snapshot_observation(pane_id, observation),
            }
        }
        self.ledger.replace(ledger);
        Some(tick)
    }

    pub(crate) fn snapshot_json(&self, existing: Option<&str>, tick: u64) -> String {
        // Both stores' observations carry their own `origin`, so the snapshot
        // module keys the merge on it — no need to tag the two iterators here.
        let current = self.status.observations().chain(self.command.observations());
        snapshot::to_json(current, self.live_panes.as_ref(), existing, tick, self.ledger.to_vec())
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

    pub(crate) fn rows(&self, now_tick: u64) -> Vec<TabRow> {
        let mut rows = Vec::new();
        let mut sorted = self.tabs.clone();
        sorted.sort_by_key(|t| t.position);
        for t in &sorted {
            rows.push(TabRow {
                number: t.position as u32 + 1,
                name: t.name.clone(),
                active: t.active,
                has_bell: t.has_bell,
                flash: self.flash_until.get(&t.id).is_some_and(|&u| now_tick < u),
                display: self.tab_display_for(t.position),
            });
        }
        rows
    }

    /// True while any tab is still mid-flash at `now_tick` — ORed into
    /// `PluginRuntime::timer_should_continue` so the ping's full window renders
    /// even though nothing else may be animating (see `flash_until`'s doc).
    pub(crate) fn has_active_flash(&self, now_tick: u64) -> bool {
        self.flash_until.values().any(|&u| now_tick < u)
    }

    pub(crate) fn tabs_changed(&mut self, mut tabs: Vec<RadarTab>) -> RadarChange {
        // Tab names are externally writable (`zellij rename-tab`, layouts) and
        // reach the render grid verbatim, so they get the same intake sanitize
        // as pane titles: a raw ESC would inject ANSI into the rail, and a
        // newline would desync the line↔click-target lockstep.
        for t in &mut tabs {
            t.name = payload::sanitize(&t.name, 40);
        }
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
        // Captured BEFORE `self.tab_panes` is overwritten below: every ledger
        // edge in this method (the exit-displace and both stores' prunes)
        // reports a pane that is either still on its pre-close topology or is
        // *about* to leave it, so the tab name it ledgers under must be the
        // last known one, not whatever (if anything) replaces it.
        let old_index = self.pane_tab_index();
        // Captured BEFORE `self.status.prune` runs below — mirrors `resolve`'s
        // status-wins-over-command precedence. A command-origin recede for a
        // pane the status store was tracking at THIS moment was never actually
        // shown on the card, so `ledger_receded` must not ghost it in.
        let status_tracked = self.status_tracked_pane_ids();

        for (pane_id, exit_status) in update.exits {
            let displaced = self.command.on_exit(pane_id, exit_status, tick, now_epoch_s);
            self.ledger_receded(displaced, &old_index, &status_tracked);
        }
        self.live_panes = Some(update.live.clone());
        self.tab_panes = update.tab_panes;
        // A pane closing with an unreceded Done/Error still on it is a recede
        // edge for both stores; both prunes ledger against the pre-close
        // `old_index` and `status_tracked` captured above.
        let dropped_status = self.status.prune(&update.live);
        let dropped_command = self.command.prune(&update.live);
        self.ledger_receded(dropped_status, &old_index, &status_tracked);
        self.ledger_receded(dropped_command, &old_index, &status_tracked);
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
    /// timer-driven mutations reach late-spawned instances too. Every
    /// completion the tick receded (TTL recede, or a promotion displacing a
    /// still-lit Done/Error) hands off to the ledger.
    pub(crate) fn timer(&mut self, tick: u64, now_epoch_s: u64) -> bool {
        // Lazy expiry: `rows`/`has_active_flash` stay `&self` (read paths), so
        // the map itself is only ever pruned here, on the one `&mut self` tick
        // that already runs regardless of flash state.
        self.flash_until.retain(|_, &mut u| tick < u);
        let report = self.command.on_timer(tick, now_epoch_s);
        let index = self.pane_tab_index();
        // All-command-origin recedes here; no pruning is in flight on this
        // edge, so the live status store IS the "at this moment" set — see
        // `resolve`'s precedence and `status_tracked_pane_ids`'s doc.
        let status_tracked = self.status_tracked_pane_ids();
        self.ledger_receded(report.receded, &index, &status_tracked);
        // Stale-Running expiry: an agent killed mid-turn sends no clearing
        // broadcast; its prompt-return grace clock (see `clear_on_prompt_return`)
        // runs out here. Running is not a completion — nothing to ledger — but
        // the clear must render and persist like any other store change.
        let stale_cleared = !self.status.expire_stale_running(tick).is_empty();
        report.changed || stale_cleared
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
        // every tab's instance clears in lockstep. A Running status is not
        // cleared immediately — `clear_on_prompt_return` starts a grace clock
        // instead, so a mid-turn foreground flicker to a shell can't be
        // mistaken for the agent exiting, while an agent killed mid-turn still
        // expires to idle on the timer (`expire_stale_running`).
        //
        // `now_epoch_s` stays unused here (kept for signature symmetry with the
        // other three mutating entry points): the
        // displaced observation `clear_on_prompt_return` hands back already
        // carries its own `completed_epoch_s` stamp from when it first
        // completed, so `LedgerEntry::from_observation` needs no fresh epoch
        // from this call site.
        let _ = now_epoch_s;
        let cleared = if crate::command::is_shell_prompt(command, is_foreground) {
            match self.status.clear_on_prompt_return(pane_id, tick) {
                Some(receded) => {
                    let index = self.pane_tab_index();
                    // Status-origin recede: `status_tracked` never suppresses
                    // it — the shadow filter only ever applies to
                    // Command-origin observations.
                    let status_tracked = self.status_tracked_pane_ids();
                    self.ledger_receded(vec![(pane_id, receded)], &index, &status_tracked);
                    true
                }
                None => false,
            }
        } else {
            // The agent's exe back in the foreground resolves a mid-turn
            // flicker: cancel any stale-Running grace clock the shell blip
            // started. Other foregrounds don't vouch — a command run in the
            // shell an agent died in must not keep its ghost alive.
            if crate::command::is_agent_foreground(command, is_foreground) {
                self.status.cancel_running_suspect(pane_id);
            }
            false
        };
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
        let pane_id = p.pane_id;
        // Captured BEFORE `apply` overwrites the store: the ping flash fires
        // only on a LIVE not-Pending → Pending edge, never on a re-broadcast of
        // an already-Pending status (spec's "flip", not "is"). Snapshot load
        // never touches this map at all, so a restored Pending never flashes.
        let was_pending = self.status.get(pane_id).map(|o| o.status) == Some(Status::Pending);
        let flips_to_pending = p.status == Status::Pending && !was_pending;
        // A Done/Error that recedes on overwrite (a new broadcast for the same
        // pane, INCLUDING the `/clear` idle-overwrite edge) hands off here.
        if let Some(displaced) = self.status.apply(p, tick, now_epoch_s) {
            let index = self.pane_tab_index();
            // Status-origin recede: never suppressed (see `command_changed`'s
            // matching call site).
            let status_tracked = self.status_tracked_pane_ids();
            self.ledger_receded(vec![(pane_id, displaced)], &index, &status_tracked);
        }
        if flips_to_pending {
            if let Some((tab_id, _)) = self.pane_tab_index().get(&pane_id) {
                self.flash_until.insert(*tab_id, tick + 2);
            }
        }
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

    /// Prepared ledger rows for rendering, newest first. Each row's tab
    /// position is a *live* lookup of the stored `tab_id` against `self.tabs`
    /// — `None` once that tab has closed, which the renderer reads as
    /// click-inert (the ledger itself never forgets an entry just because its
    /// tab went away).
    pub(crate) fn ledger_lines(&self) -> Vec<LedgerLine> {
        self.ledger
            .entries()
            .map(|e| LedgerLine {
                at_epoch_s: e.at_epoch_s,
                error: e.outcome == LedgerOutcome::Error,
                tab_name: e.tab_name.clone(),
                label: e.label.clone(),
                tab_position: self.tabs.iter().find(|t| t.id == e.tab_id).map(|t| t.position),
            })
            .collect()
    }

    /// Any ledger entry still younger than the saturate window — drives the
    /// Slow cadence (spec §4.4/§10) so the timer stays armed only while a row's
    /// displayed age can still change. Consumed by `PluginRuntime::desired_cadence`.
    pub(crate) fn ledger_any_unsaturated(&self, now_epoch_s: u64) -> bool {
        self.ledger.any_unsaturated(now_epoch_s)
    }

    /// Any pushed `Pending` row whose `· Nm` wait tag is still counting (age
    /// under the ledger's saturate window)? The pending twin of
    /// [`ledger_any_unsaturated`](Self::ledger_any_unsaturated): both feed the
    /// Slow cadence, and both freeze at `1h+` so the timer can disarm.
    pub(crate) fn pending_wait_unsaturated(&self, now_epoch_s: u64) -> bool {
        self.status.observations().any(|(_, o)| {
            o.status == Status::Pending
                && o.pending_epoch_s
                    .is_some_and(|t| now_epoch_s.saturating_sub(t) < crate::ledger::SATURATE_S)
        })
    }

    /// The zero-state routing gate: `PluginRuntime::render` shows the minimal
    /// scanning face only when there are no tracked tabs AND no completion
    /// history — a session with zero live tabs but a non-empty ledger still
    /// renders `render_rail`'s header + bottom region (spec §9's floor).
    /// Unlike `ledger_lines`, this asks `RadarState` directly rather than the
    /// prepared `Vec<LedgerLine>` — the routing decision happens before
    /// `RenderOpts` is built.
    pub(crate) fn ledger_is_empty(&self) -> bool {
        self.ledger.is_empty()
    }

    /// Pane → (tab id, tab name) for every pane currently in `self.tab_panes`,
    /// joined against `self.tabs` for the name. Callers that need the topology
    /// as of *before* a mutation (`panes_changed`'s prune edges) must capture
    /// this BEFORE applying that mutation — the index itself is always just a
    /// snapshot of current `self` state.
    fn pane_tab_index(&self) -> HashMap<u32, (TabId, String)> {
        let mut index = HashMap::new();
        for tab in &self.tabs {
            if let Some(panes) = self.tab_panes.get(&tab.position) {
                for pane in panes {
                    index.insert(pane.id, (tab.id, tab.name.clone()));
                }
            }
        }
        index
    }

    /// Pane ids the status store currently holds an observation for,
    /// regardless of its status value — mirrors `resolve`'s precedence check
    /// (`status.get(...).or_else(command.get(...))`: mere PRESENCE in the
    /// status store shadows a command observation, whatever its status is).
    /// `ledger_receded` uses this to recognize a command-origin recede that
    /// was never actually shown on the card.
    fn status_tracked_pane_ids(&self) -> HashSet<u32> {
        self.status.observations().map(|(id, _)| id).collect()
    }

    /// Push every receded observation that resolves to a completion into the
    /// ledger, via `index`. A pane absent from `index` (its tab/topology is
    /// unknown to this call) is silently dropped — there is no tab name to
    /// ledger it under.
    ///
    /// A command-origin observation for a pane in `status_tracked` is also
    /// dropped, unconditionally: `resolve`'s status-wins-over-command
    /// precedence means that observation was never a card fact, so its
    /// recede — TTL or prune — must not ghost into the ledger (spec §4.2's
    /// "an observation at the edge where it stops being shown as a card
    /// fact"). A status-origin observation is never filtered here; only
    /// `resolve` and this check know there are two stores at all.
    fn ledger_receded(
        &mut self,
        receded: Vec<(u32, TrackedObservation)>,
        index: &HashMap<u32, (TabId, String)>,
        status_tracked: &HashSet<u32>,
    ) {
        for (pane_id, obs) in receded {
            if obs.origin == ObservationOrigin::Command && status_tracked.contains(&pane_id) {
                continue;
            }
            let Some((tab_id, tab_name)) = index.get(&pane_id) else {
                continue;
            };
            if let Some(entry) = LedgerEntry::from_observation(pane_id, &obs, *tab_id, tab_name) {
                self.ledger.push(entry);
            }
        }
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
    pub(crate) fn ledger(&self) -> &Ledger {
        &self.ledger
    }

    /// Test-only: build a ledger entry directly (bypassing the recede path) so
    /// cadence tests can pin an exact `at_epoch_s` without driving a whole
    /// pane lifecycle.
    #[cfg(test)]
    pub(crate) fn ledger_mut(&mut self) -> &mut Ledger {
        &mut self.ledger
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

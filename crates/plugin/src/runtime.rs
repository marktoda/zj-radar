//! Deep runtime module: repo-owned events in, ordered host effects out.
//! No zellij-tile dependency.
//!
//! [`PluginRuntime`] is the pure state machine behind the wasm glue in
//! `lib.rs`. The glue translates each Zellij host event into one method call
//! here, and every mutating call returns an [`Outcome`] (a render flag plus an
//! ordered list of [`Effect`]s the glue replays against the host). Because the
//! mapping is total and side-effect-free, host tests drive these entry points
//! directly and assert on the returned `Outcome` — no Zellij needed.
//!
//! # Event entry points
//! - [`load`](PluginRuntime::load) — first run: seed config/snapshot, begin the
//!   permission flow.
//! - [`tabs_changed`](PluginRuntime::tabs_changed) /
//!   [`panes_changed`](PluginRuntime::panes_changed) /
//!   [`cwd_changed`](PluginRuntime::cwd_changed) — Zellij topology updates.
//! - [`command_changed`](PluginRuntime::command_changed) — a pane's foreground
//!   process changed (the *observed* information source).
//! - [`status_pipe`](PluginRuntime::status_pipe) — a `zj_radar.status.v1`
//!   broadcast from an agent hook (the *pushed* information source).
//! - [`config_pipe`](PluginRuntime::config_pipe) /
//!   [`control`](PluginRuntime::control) /
//!   [`control_pipe`](PluginRuntime::control_pipe) — runtime config + remote
//!   commands.
//! - [`session_update`](PluginRuntime::session_update) — Zellij's live
//!   session list (also how the runtime learns its own session's name).
//! - [`presences_changed`](PluginRuntime::presences_changed) — peer presence
//!   files, freshly read from disk.
//! - [`timer`](PluginRuntime::timer) — periodic tick (animation +
//!   permission-flow coordination + cross-session cycle commit).
//! - [`mouse_click`](PluginRuntime::mouse_click) — resolved against the cached
//!   [`RenderedRail`] for click-to-switch.
//! - [`permission_result`](PluginRuntime::permission_result) — Zellij's grant /
//!   deny verdict.

use crate::control::Verb;
use crate::config;
use crate::permission::{PermissionMarker, PermissionPolicy, PermissionProbe, PermissionState, Transition};
use crate::presence::Presence;
use crate::radar_state::{Direction, PaneUpdate, RadarChange, RadarState, RadarTab};
use crate::render::{self, RenderedRail};
use crate::rollup::TabRow;
use crate::sessions::{CommitTarget, SessionLite, Sessions};
use crate::status::Status;
use crate::tab_namer::TabRename;
use crate::theme;
use std::collections::BTreeMap;

/// How urgently the one-shot timer should re-fire. `Fast` is the 1 Hz tick
/// that drives animation and debounce/TTL bookkeeping; `Slow` backs off to a
/// once-a-minute heartbeat when nothing needs per-second resolution but a
/// ledger age is still changing. `desired_cadence` selects between the two
/// (or `None` to fully disarm).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Cadence {
    Fast,
    Slow,
}

impl Cadence {
    pub(crate) fn seconds(self) -> f64 {
        match self {
            Cadence::Fast => 1.0,
            Cadence::Slow => 60.0,
        }
    }
}

/// A `Timer` fire whose reported elapsed exceeds this came from a Slow (60s)
/// arm. Fast fires report ~1s and Slow ~60s, so any threshold safely between
/// the two works; 5s tolerates heavy scheduler delay on a fast fire (a fast
/// fire arriving >5s late is pathological) while never mistaking a slow fire
/// for fast. Used only to decide which of two in-flight fires is the stale
/// one — see [`PluginRuntime::timer`].
const STALE_FIRE_ELAPSED_S: f64 = 5.0;

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum Effect {
    RequestPermission,
    SetSelectable(bool),
    SetTimeout(Cadence),
    PersistSnapshot,
    PersistPermissionMarker(PermissionMarker),
    RenameTab { position: usize, name: String },
    SwitchTab { position: usize },
    ShowPane { pane_id: u32 },
    /// Read these panes' working directories once (blocking `get_pane_cwd`) to
    /// bootstrap a name for a freshly-opened tab before it emits `CwdChanged`.
    ///
    /// Unlike the other (fire-and-forget) effects, this one is a *request*: the
    /// glue feeds each result back through `cwd_changed`, which re-enters the
    /// runtime and may itself emit `RenameTab`. The recursion is bounded —
    /// `cwd_changed` never emits another `ResolveCwd` — but note that this
    /// effect's full consequences are realized in that second pass, not in the
    /// `Outcome` that carried it.
    ResolveCwd { pane_ids: Vec<u32> },
    /// Close this plugin's own pane. Emitted by the onboarding floating pane
    /// after permission is granted — it has served its purpose. Needs no Zellij
    /// permission (`close_self` is always allowed).
    CloseSelf,
    /// Refresh the shared permission lock's mtime. Emitted each tick while this
    /// instance's own request is in-flight (`Requesting`), so waiting peers
    /// never see the lock go stale while a user is still looking at a live
    /// prompt — `reclaim_if_stale` (and with it the deferring rails' patience
    /// escalation) only ever fires once the prompt-owner is actually gone.
    HeartbeatPermissionLock,
    /// Show a desktop notification. `key` is the event's cross-instance
    /// identity (`notify_rules::claim_key`): every per-tab instance computes
    /// the same edge and emits this same effect, and the host layer uses the
    /// key to elect exactly one dispatcher (`SessionFiles::claim_notification`)
    /// so N visited tabs don't produce N identical toasts.
    Notify { key: String, title: String, body: String },
    /// Publish this session's own [`Presence`] for peer rails to read.
    /// Content-compared at the edge in `project` — lib.rs does
    /// `files.persist_presence(&runtime.presence_json())`.
    PersistPresence,
    /// Re-read every peer session's presence file and feed the result back
    /// through `presences_changed` — mirrors `ResolveCwd`'s
    /// request/read-back pattern, except the read is gated on cadence
    /// (Fast fires only — see `timer`) rather than on a fresh set of pane
    /// ids: one directory scan per second, only while Fast is armed, never
    /// on the Slow heartbeat.
    ReadPresences,
    /// Commit a cross-session cycle selection: switch to `name` and, once
    /// there, jump straight to the tab that needs attention (if any).
    /// Emitted by `timer` when `Sessions::tick` reports an idle commit.
    SwitchSession { name: String, tab_position: Option<usize> },
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct Outcome {
    pub render: bool,
    pub effects: Vec<Effect>,
}

impl Outcome {
    fn none() -> Self {
        Self::default()
    }

    fn with_effects(render: bool, effects: Vec<Effect>) -> Self {
        Self { render, effects }
    }
}

/// The one-shot timer chain. Zellij's `set_timeout` is non-cancellable, so a
/// cadence change can leave TWO fires in flight; the pairing "arming grows the
/// in-flight count exactly where a `SetTimeout` is emitted, a fire retires
/// exactly one, a stale fire is swallowed whole" is a single invariant — held
/// structurally: the fields are private, so [`TimerChain::arm`] and
/// [`TimerChain::on_fire`] are the only ways to move them.
#[derive(Default)]
struct TimerChain {
    /// The cadence the last-scheduled `SetTimeout` was armed with, or `None`
    /// if the timer is fully disarmed.
    armed: Option<Cadence>,
    /// How many `SetTimeout` fires are still in flight. The Slow→Fast top-up
    /// leaves two; the slow-armed one is stale and must be swallowed by
    /// `on_fire` — ticking on it would re-arm a second persistent chain
    /// (N chains → N Hz, every tick-window elapsing N× too fast).
    pending_fires: u32,
}

/// What a `Timer` event means for the runtime — see [`TimerChain::on_fire`].
enum Fire {
    /// The live chain: process the tick and re-arm.
    Live,
    /// A stale leftover from before a cadence top-up: swallow it whole —
    /// no tick, no re-arm, the live arm untouched.
    Stale,
}

impl TimerChain {
    /// Arm (or re-arm) toward `desired`. `Some(c)` means the caller MUST emit
    /// `Effect::SetTimeout(c)` — returning the cadence from the same place
    /// the count grows is what keeps them paired. Compares the *cadence* the
    /// previous arm used, not just "is anything armed": a Slow-armed timer
    /// that should now be Fast gets a fresh fast arm immediately, rather than
    /// waiting for the (harmless, spurious) slow fire to notice. Every other
    /// transition — first arm, already-correct cadence, nothing desired — is
    /// a no-op.
    fn arm(&mut self, desired: Option<Cadence>) -> Option<Cadence> {
        let arm = match (self.armed, desired) {
            (Some(Cadence::Slow), Some(Cadence::Fast)) => Some(Cadence::Fast),
            (None, Some(cadence)) => Some(cadence),
            _ => None,
        };
        if let Some(cadence) = arm {
            self.armed = Some(cadence);
            self.pending_fires += 1;
        }
        arm
    }

    /// Retire one in-flight fire, pairing the count with the fire's reported
    /// `elapsed_s` to identify a stale one: swallow ([`Fire::Stale`]) only
    /// when BOTH hold — its elapsed marks it slow-armed
    /// (> `STALE_FIRE_ELAPSED_S`) and another fire is still out
    /// (post-decrement count > 0). A fast fire always processes: swallowing
    /// by count alone would freeze the tick clock in the common order, where
    /// the live fast fire lands first and the stale slow fire lands up to 59s
    /// later. A slow fire with nothing else in flight IS the live chain.
    ///
    /// Convergence, common order: fast fire lands (2→1), processes, re-arms
    /// (→2); each following fast fire repeats that; the stale slow finally
    /// lands (2→1), swallowed → steady single chain. Rare order (a top-up in
    /// the slow window's final second): the stale slow lands first (2→1),
    /// swallowed; the fast fire then processes (1→0) and re-arms (→1). The
    /// Fast→Slow wind-down converges too: an older slow always lands before a
    /// newer slow, so the older one still sees a fire in flight and is the one
    /// swallowed. `saturating_sub` keeps a direct `timer()` call with nothing
    /// armed (tests drive the entry point that way) counting as a live fire.
    fn on_fire(&mut self, elapsed_s: f64) -> Fire {
        self.pending_fires = self.pending_fires.saturating_sub(1);
        if elapsed_s > STALE_FIRE_ELAPSED_S && self.pending_fires > 0 {
            return Fire::Stale;
        }
        self.armed = None;
        Fire::Live
    }

    /// The currently armed cadence (`None` = fully disarmed). Read-only —
    /// the battery-property tests assert arm/disarm through this.
    #[cfg(test)]
    fn armed(&self) -> Option<Cadence> {
        self.armed
    }

    /// Test-only escape hatch: force the disarmed state so a test can watch
    /// an arm happen in isolation. Explicit and greppable, unlike the raw
    /// field write it replaced — production code has no way to do this.
    #[cfg(test)]
    fn disarm_for_test(&mut self) {
        self.armed = None;
    }
}

#[derive(Default)]
pub(crate) struct PluginRuntime {
    pub(crate) radar: RadarState,
    pub(crate) tick: u64,
    /// The one-shot `SetTimeout` chain — see [`TimerChain`]. Every arm goes
    /// through `arm_timer_if_needed` (`begin_permission_flow` and `project`
    /// both arm through it); every fire retires through `timer`.
    timer_chain: TimerChain,
    pub(crate) last_render_height: usize,
    pub(crate) config: config::Config,
    pub(crate) permission: PermissionState,
    pub(crate) theme: theme::DerivedColors,
    last_rendered: RenderedRail,
    notify_prev: BTreeMap<u32, crate::status::Status>,
    /// Cross-session peer state + the Alt+[/] cycle selection machine.
    sessions: Sessions,
    /// This session's own name, learned from `session_update` (the
    /// `SessionLite` with `is_current == true`). Empty until Zellij's first
    /// session-list event lands — `project` withholds `Effect::PersistPresence`
    /// while empty, since an unnamed presence file is useless to peers.
    own_session_name: String,
    /// The last own-`Presence` actually published, canonicalized with
    /// `updated_epoch_s` zeroed before compare-and-cache — so `project` can
    /// content-compare and emit `Effect::PersistPresence` only on a real
    /// *content* edge, the same "write on edges only" rule `PersistSnapshot`
    /// follows. Comparing the raw JSON (epoch included) would fire on every
    /// Fast tick even with unchanged counts, since `last_now_epoch_s` moves
    /// every second — mirrors `sessions.rs::set_own`, whose badge-derived
    /// change check already excludes `updated_epoch_s` the same way. The
    /// real epoch still reaches peers: it's stamped fresh into whatever
    /// `presence_json()` returns when the host actually handles the effect,
    /// so it reads as "epoch of the last content edge", not "epoch of the
    /// last tick".
    last_presence: Option<Presence>,
    /// The most recent `now_epoch_s` any entry point has captured. Reused
    /// (never re-read from the clock) by call paths that have no epoch of
    /// their own to work with — `presence_json`, `session_update`,
    /// `presences_changed` — so a single event's "now" never forks in two
    /// directions.
    last_now_epoch_s: u64,
}

impl PluginRuntime {
    pub(crate) fn load(
        &mut self,
        config: config::Config,
        snapshot: Option<&str>,
        permission: PermissionProbe,
    ) -> Outcome {
        self.config = config;
        if let Some(raw) = snapshot {
            if let Some(tick) = self.radar.load_snapshot(raw) {
                self.tick = tick;
            }
        }
        // Seed the notification baseline from the restored snapshot so that
        // pre-existing completions never fire a spurious Notify effect.
        self.notify_prev = crate::notify_rules::status_map(&self.radar.notify_views());
        self.begin_permission_flow(permission)
    }

    pub(crate) fn build_rows(&self) -> Vec<TabRow> {
        self.radar.rows(self.tick)
    }

    pub(crate) fn tabs_changed(&mut self, tabs: Vec<RadarTab>) -> Outcome {
        let change = self.radar.tabs_changed(tabs);
        self.project(vec![], change, crate::clock::now_epoch_s())
    }

    pub(crate) fn panes_changed(&mut self, update: PaneUpdate) -> Outcome {
        let now = crate::clock::now_epoch_s();
        if let Some(theme) = update.theme.clone() {
            self.theme = theme;
        }
        let change = self.radar.panes_changed(update, self.tick, now, self.config.naming);
        self.project(vec![], change, now)
    }

    /// `elapsed_s` is the duration Zellij reports on `Event::Timer` — the
    /// seconds the fired `set_timeout` was armed with, i.e. which cadence
    /// scheduled this fire.
    pub(crate) fn timer(&mut self, permission: PermissionProbe, elapsed_s: f64) -> Outcome {
        // Retire one in-flight fire; a stale one (see `TimerChain::on_fire`)
        // is swallowed whole. A swallowed fire skips
        // `check_deferred_permission_request`: safe, because an overlap only
        // exists while a newer chain is live, whose next fire runs it within
        // ~1s.
        if let Fire::Stale = self.timer_chain.on_fire(elapsed_s) {
            return Outcome::none();
        }
        // One clock capture per event: every consumer below (store timer,
        // cadence decision, re-arm via project) sees the same "now".
        let now = crate::clock::now_epoch_s();
        let mut effects = Vec::new();
        let permission_changed =
            self.check_deferred_permission_request(permission, &mut effects);
        // Our own request is in-flight (including one this very tick just
        // fired): keep the shared lock fresh so no waiting peer reclaims it
        // out from under a live prompt.
        if self.permission.selectable() {
            effects.push(Effect::HeartbeatPermissionLock);
        }
        self.tick += 1;
        // A tick can mutate the command store (debounced promotion to Running,
        // Running→Done confirm). Persist the snapshot when it does, or a tab
        // opened in that window would seed a rail missing the change — the same
        // cross-instance convergence pushed statuses get from `status_pipe`.
        let store_changed = self.radar.timer(self.tick, now);
        // Cross-session peers: re-read the directory bound to Fast fires only
        // — "one directory scan per second, only while Fast is armed", never
        // on the Slow heartbeat (which exists solely to repaint ledger ages
        // and has no business paying for a peer scan). `elapsed_s` is also
        // how `TimerChain::on_fire` above tells a stale fire from a live one:
        // Fast fires report ~1s, Slow ~60s, and `STALE_FIRE_ELAPSED_S` sits
        // safely between the two, so reusing it here (rather than inventing
        // parallel state) is the same discrimination, applied to cadence
        // instead of staleness.
        if elapsed_s <= STALE_FIRE_ELAPSED_S {
            effects.push(Effect::ReadPresences);
        }
        // BEFORE re-arming below, commit an idle cycle selection if one is
        // pending. Committing here — not after `project` re-arms — matters:
        // a commit clears `Sessions::wants_fast_cadence`, and the chain must
        // be free to decay to Slow on this same pass rather than one fire
        // late.
        let session_commit = self.sessions.tick(self.tick);
        if let Some(CommitTarget { name, attention_tab_position }) = session_commit {
            effects.push(Effect::SwitchSession { name, tab_position: attention_tab_position });
        }
        // Capture before re-arming: an in-flight permission request must repaint
        // the needs_permission screen each tick until the user answers.
        let awaiting_permission = self.sidebar_should_be_selectable();
        let render = permission_changed
            || awaiting_permission
            || store_changed
            || self.timer_should_continue()
            // A Slow tick exists precisely to repaint ledger ages — even
            // when nothing else changed, `format_age` output may have moved.
            || self.desired_cadence(now) == Some(Cadence::Slow);
        let change = RadarChange {
            render,
            settle: true,
            persist_snapshot: store_changed,
            renames: vec![],
            cwd_bootstrap: vec![],
        };
        self.project(effects, change, now)
    }

    pub(crate) fn mouse_click(&self, line: isize) -> Outcome {
        if !self.permission.granted() {
            return Outcome::none();
        }
        let Some(target) = self.last_rendered.target_at_line(line) else {
            return Outcome::none();
        };
        let effect = match target.pane_id {
            Some(pane_id) => Effect::ShowPane { pane_id },
            None => Effect::SwitchTab {
                position: target.tab_position,
            },
        };
        Outcome::with_effects(false, vec![effect])
    }

    /// Run an imperative command verb. `AttentionNext/Prev` resolve a
    /// deterministic target tab and emit `SwitchTab`; `SessionNext/Prev`
    /// advance the cross-session cycle selection (`Sessions::cycle`) and
    /// render the highlight move — the actual switch is a *later* idle-commit
    /// (see `timer`), not an immediate effect here. Inert until permission is
    /// granted, mirroring `mouse_click`: session switching is
    /// `ChangeApplicationState` territory, same as `SwitchTab`.
    pub(crate) fn control(&mut self, verb: Verb) -> Outcome {
        if !self.permission.granted() {
            return Outcome::none();
        }
        match verb {
            Verb::AttentionNext | Verb::AttentionPrev => {
                let dir = if verb == Verb::AttentionNext { Direction::Next } else { Direction::Prev };
                match self.radar.next_attention_tab(dir) {
                    Some(position) => Outcome::with_effects(false, vec![Effect::SwitchTab { position }]),
                    None => Outcome::none(),
                }
            }
            Verb::SessionNext | Verb::SessionPrev => {
                let dir = if verb == Verb::SessionNext { Direction::Next } else { Direction::Prev };
                let render = self.sessions.cycle(dir, self.tick);
                // A fresh tap must arm Fast immediately (not wait for the next
                // domain change to pass through `project`), so the idle-commit
                // in `timer` fires promptly rather than stalling behind a Slow
                // or fully-disarmed chain.
                let mut effects = Vec::new();
                self.arm_timer_if_needed(self.last_now_epoch_s, &mut effects);
                Outcome::with_effects(render, effects)
            }
        }
    }

    /// Parse a `cmd.v1` payload and dispatch it. Unknown verbs are a no-op.
    pub(crate) fn control_pipe(&mut self, payload: &str) -> Outcome {
        match crate::control::parse(payload) {
            Some(verb) => self.control(verb),
            None => Outcome::none(),
        }
    }

    /// Zellij's live session list. Also how the runtime learns its own
    /// session's name (the entry with `is_current == true`) — needed by
    /// `presence_json` and gating `Effect::PersistPresence` in `project`.
    pub(crate) fn session_update(&mut self, live: Vec<SessionLite>) -> Outcome {
        if let Some(me) = live.iter().find(|s| s.is_current) {
            self.own_session_name = me.name.clone();
        }
        let render = self.sessions.update_live(live);
        let change = RadarChange { render, settle: false, persist_snapshot: false, renames: vec![], cwd_bootstrap: vec![] };
        self.project(vec![], change, self.last_now_epoch_s)
    }

    /// A fresh read of every peer session's presence file
    /// (`Effect::ReadPresences`'s read-back). Renders only when the derived
    /// badge actually changed.
    pub(crate) fn presences_changed(&mut self, raw: Vec<String>) -> Outcome {
        let render = self.sessions.update_presences(raw);
        let change = RadarChange { render, settle: false, persist_snapshot: false, renames: vec![], cwd_bootstrap: vec![] };
        self.project(vec![], change, self.last_now_epoch_s)
    }

    /// This session's own `Presence`, derived from the same rows the rail
    /// renders (`radar.rows`) — never a separately-tracked count that could
    /// drift from what's on screen. `attention_tab_position` is the first
    /// (lowest-position) row that `needs_you()`, matching the position
    /// `Sessions`/`BadgeEntry` expect for a same-repo jump-on-arrival.
    fn own_presence(&self) -> Presence {
        let rows = self.radar.rows(self.tick);
        let running = rows.iter().filter(|r| r.display.status == Status::Running).count();
        let mut attention = 0usize;
        let mut attention_tab_position = None;
        for r in &rows {
            if r.display.status.needs_you() {
                attention += 1;
                if attention_tab_position.is_none() {
                    attention_tab_position = Some(r.number as usize - 1);
                }
            }
        }
        Presence {
            session_name: self.own_session_name.clone(),
            running,
            attention,
            attention_tab_position,
            updated_epoch_s: self.last_now_epoch_s,
        }
    }

    /// JSON the host actually writes to disk on `Effect::PersistPresence` —
    /// see `own_presence` for the field derivation. Only the wasm glue
    /// (Task 6) calls this in production; on a host build nothing here
    /// invokes it (that wiring is still `project`'s `own_presence`
    /// comparison), so it would otherwise read as dead code.
    #[cfg_attr(not(target_arch = "wasm32"), allow(dead_code))]
    pub(crate) fn presence_json(&self) -> String {
        self.own_presence().to_json()
    }

    /// True while [`timer`](Self::timer) still consumes a fresh
    /// [`PermissionProbe`] — i.e. the permission machine is waiting on a peer's
    /// marker/lock. Once a request is in-flight or the state is resolved,
    /// `on_timer` ignores the probe entirely, so the host glue can skip the
    /// per-tick marker read + lock attempt (N tabs were stat-reading N files
    /// per second forever) and pass `PermissionProbe::default()` instead.
    pub(crate) fn wants_permission_probe(&self) -> bool {
        self.permission.is_waiting()
    }

    pub(crate) fn permission_result(&mut self, granted: bool) -> Outcome {
        self.record_permission_result(granted);
        let mut effects = vec![
            Effect::PersistPermissionMarker(if granted {
                PermissionMarker::Granted
            } else {
                PermissionMarker::Denied
            }),
            Effect::SetSelectable(self.sidebar_should_be_selectable()),
        ];
        // The onboarding pane exists only to host the grant prompt. Once granted
        // — and the grant is cached by plugin URL, so the rail inherits it — it
        // removes itself, leaving the user with just the rail.
        if granted && self.config.role == config::Role::Onboarding {
            effects.push(Effect::CloseSelf);
        }
        Outcome::with_effects(true, effects)
    }

    pub(crate) fn cwd_changed(&mut self, pane_id: u32, path: String) -> Outcome {
        let change = self.radar.cwd_changed(pane_id, path, self.config.naming);
        self.project(vec![], change, crate::clock::now_epoch_s())
    }

    pub(crate) fn command_changed(
        &mut self,
        pane_id: u32,
        command: &[String],
        is_foreground: bool,
    ) -> Outcome {
        let change = self.radar.command_changed(pane_id, command, is_foreground, self.tick);
        self.project(vec![], change, crate::clock::now_epoch_s())
    }

    pub(crate) fn status_pipe(&mut self, raw: &str) -> Outcome {
        let now = crate::clock::now_epoch_s();
        let Some(change) = self.radar.status_pipe(raw, self.tick, now, self.config.naming) else {
            return Outcome::none();
        };
        self.project(vec![], change, now)
    }

    pub(crate) fn snapshot_json(&self, existing: Option<&str>) -> String {
        self.radar.snapshot_json(existing, self.tick)
    }

    pub(crate) fn config_pipe(&mut self, raw: &str) -> Outcome {
        let Some(kv) = crate::config::overrides_from_json(raw) else {
            return Outcome::none();
        };
        self.config.apply_overrides(&kv);
        let renames = self.radar.recompute_renames(self.config.naming);
        let change = RadarChange {
            render: true,
            renames,
            settle: false,
            persist_snapshot: false,
            cwd_bootstrap: vec![],
        };
        self.project(vec![], change, crate::clock::now_epoch_s())
    }

    pub(crate) fn render(&mut self, rows: usize, cols: usize) -> String {
        self.last_render_height = rows;
        let tabrows = self.build_rows();
        let opts = render::RenderOpts {
            width: cols.max(1),
            height: rows,
            now_tick: self.tick,
            glyphs: self.config.glyphs,
            header: self.config.header,
            density: self.config.density,
            theme: self.theme.clone(),
            now_epoch_s: crate::clock::now_epoch_s(),
            jump_hint: self.config.jump_hint.shows(),
        };
        let ledger = self.radar.ledger_lines();
        let rail = if !self.permission.granted() {
            render::needs_permission(&opts, self.config.grant_hint)
        } else if tabrows.is_empty() && self.radar.ledger_is_empty() {
            render::onboarding(&opts)
        } else {
            render::render_rail(&tabrows, &ledger, &opts)
        };
        let ansi = rail.ansi.clone();
        self.last_rendered = rail;
        ansi
    }

    /// Test-only: this session's natural content height at `cols` — enough to
    /// show every row with no overflow folding and no bottom-region padding
    /// (spec §9). Click-mapping tests want a "big enough, no overflow" height
    /// without hard-coding one; passing a merely-large sentinel (the old
    /// `usize::MAX / 2` convention) now lands in the bottom region's
    /// unbounded-filler branch, so this asks `render::body_line_count` for the
    /// real number instead.
    #[cfg(test)]
    pub(crate) fn natural_height(&self, cols: usize) -> usize {
        let tabrows = self.build_rows();
        let opts = render::RenderOpts {
            width: cols.max(1),
            height: usize::MAX / 2,
            now_tick: self.tick,
            glyphs: self.config.glyphs,
            header: self.config.header,
            density: self.config.density,
            theme: self.theme.clone(),
            now_epoch_s: crate::clock::now_epoch_s(),
            jump_hint: self.config.jump_hint.shows(),
        };
        render::body_line_count(&tabrows, &self.radar.ledger_lines(), &opts)
    }

    pub(crate) fn sidebar_should_be_selectable(&self) -> bool {
        self.permission.selectable()
    }

    /// Test-only: force the in-flight `Requesting` state without driving the
    /// full probe flow. Production reaches `Requesting` via `on_load`/`on_timer`.
    #[cfg(test)]
    pub(crate) fn record_permission_request_started(&mut self) {
        self.permission = PermissionState::Requesting;
    }

    pub(crate) fn record_permission_result(&mut self, granted: bool) {
        self.permission.on_result(granted);
    }

    #[cfg(test)]
    pub(crate) fn target_at_line(&self, line: isize) -> Option<(usize, Option<u32>)> {
        let t = self.last_rendered.target_at_line(line)?;
        Some((t.tab_position, t.pane_id))
    }

    #[cfg(test)]
    pub(crate) fn tab_position_at_line(&self, line: isize) -> Option<usize> {
        self.target_at_line(line).map(|(pos, _)| pos)
    }

    /// Collapse the two config flags into the permission module's policy. The
    /// precedence (onboarding wins, then defer, else the lock dance) lives here,
    /// so `permission.rs` never imports `Config` and the dead
    /// `Onboarding && defer` combination is unrepresentable downstream.
    fn permission_policy(&self) -> PermissionPolicy {
        if self.config.role == config::Role::Onboarding {
            PermissionPolicy::OnboardingPane
        } else if self.config.defer_permission {
            PermissionPolicy::Deferring
        } else {
            PermissionPolicy::LockCoordinated
        }
    }

    fn begin_permission_flow(&mut self, probe: PermissionProbe) -> Outcome {
        let policy = self.permission_policy();
        let mut effects = Vec::new();
        // Only a fresh request needs the host's permission prompt; a marker-driven
        // resolution or a wait emit no permission effect here.
        if self.permission.on_load(&probe, policy) == Transition::Requested {
            effects.push(Effect::RequestPermission);
        }
        // Arm a timer whenever a decision is still outstanding — either we're
        // waiting on a peer's marker, or our own request is in-flight. Pre-grant
        // Zellij withholds the state events that would otherwise trigger a paint
        // (they need ReadApplicationState), so this timer is the only thing that
        // gets the needs_permission screen onto the rail.
        self.arm_timer_if_needed(crate::clock::now_epoch_s(), &mut effects);
        // Load always initializes the sidebar's selectability, every arm.
        effects.push(Effect::SetSelectable(self.permission.selectable()));
        Outcome::with_effects(false, effects)
    }

    fn check_deferred_permission_request(
        &mut self,
        probe: PermissionProbe,
        effects: &mut Vec<Effect>,
    ) -> bool {
        let policy = self.permission_policy();
        // `on_timer` is inert unless we're waiting on a peer; a decision landing
        // (marker arrived, or we reclaimed a stale lock — see session_files)
        // refreshes selectability. `Requested` additionally fires the prompt.
        match self.permission.on_timer(&probe, policy) {
            Transition::Requested => effects.push(Effect::RequestPermission),
            Transition::Resolved { .. } => {}
            Transition::NoChange | Transition::StillWaiting => return false,
        }
        effects.push(Effect::SetSelectable(self.permission.selectable()));
        true
    }

    /// Spec §10 cadence function. Fast (1s) while anything tick-windowed is
    /// live; Slow (60s) while ledger ages are still changing; None once every
    /// age is saturated ("1h+") — the battery property's full-disarm state.
    /// A *denied* rail disarms unconditionally: without `ReadApplicationState`
    /// none of the events that clear domain work ever arrive, so a stale
    /// `Running` loaded from a snapshot would otherwise pin Fast ticks and
    /// repaints forever behind a static needs-permission face.
    /// `now_epoch_s` is the event's single clock capture (the stores already
    /// take epochs as arguments; this extends that discipline up through the
    /// runtime so one event never sees two different "now"s).
    fn desired_cadence(&self, now_epoch_s: u64) -> Option<Cadence> {
        if self.permission.denied() {
            return None;
        }
        if self.permission.is_waiting()
            || self.permission.selectable()
            || self.timer_should_continue()
            // A pending cross-session cycle selection needs the idle-commit
            // in `timer` to fire promptly, not wait out a Slow (or fully
            // disarmed) chain.
            || self.sessions.wants_fast_cadence()
        {
            Some(Cadence::Fast)
        } else if self.radar.ledger_any_unsaturated(now_epoch_s)
            || self.radar.pending_wait_unsaturated(now_epoch_s)
        {
            // Slow ticks exist to advance minute-granular ages: ledger rows'
            // relative ages and pending rows' `· Nm` wait tags. Both freeze at
            // 1h+ (saturation), which is what lets the timer disarm fully.
            Some(Cadence::Slow)
        } else {
            None
        }
    }

    /// Arm (or re-arm) the one-shot timer at `desired_cadence()` — a thin
    /// bridge from [`TimerChain::arm`]'s decision to the effect vec, so the
    /// "arm returned ⇒ SetTimeout emitted" pairing has exactly one home.
    fn arm_timer_if_needed(&mut self, now_epoch_s: u64, effects: &mut Vec<Effect>) {
        if let Some(cadence) = self.timer_chain.arm(self.desired_cadence(now_epoch_s)) {
            effects.push(Effect::SetTimeout(cadence));
        }
    }

    /// Whether the one-shot timer should (re-)arm for *domain* reasons — the
    /// "tick only while there's something to do" rule that keeps an idle rail from
    /// waking every second. Four triggers:
    ///
    /// - **animating work** — a `Running` agent/command whose glyph spins each
    ///   tick (`RadarState::has_running_work`);
    /// - **an un-carried completion edge** — a `status_pipe` payload defers its
    ///   recede + notification to the timer (it can't trust its own focus, see
    ///   `RadarState::status_pipe`), so we must keep ticking until the settle has
    ///   run. [`has_unsettled_notifications`] goes false the moment that settle
    ///   advances the baseline, so a *backgrounded* `Done`/`Error`/`Pending` stops
    ///   pinning the timer awake once notified — a later focus change or broadcast
    ///   re-arms it. (The pre-settle baseline read costs at most one extra tick.)
    /// - **a command `Done` awaiting its TTL recede** (`RadarState::command_awaiting_recede`)
    ///   — the row itself is static (it doesn't animate and its notify already
    ///   fired), but the ledger handoff at `DONE_TTL_TICKS` still needs a tick to
    ///   land on schedule, so it keeps this armed even though `has_running_work`
    ///   stays narrow (see that method's doc for the arming split).
    /// - **an active ping flash** (`RadarState::has_active_flash`) — the
    ///   flip-to-pending glance-catcher is a two-tick visual, not a card fact,
    ///   so nothing else here would otherwise keep the timer awake for it.
    ///
    /// [`has_unsettled_notifications`]: Self::has_unsettled_notifications
    fn timer_should_continue(&self) -> bool {
        self.radar.has_running_work()
            || self.has_unsettled_notifications()
            || self.radar.command_awaiting_recede()
            || self.radar.has_active_flash(self.tick)
    }

    /// True while the notification baseline lags the live per-pane statuses — i.e.
    /// an attention edge has landed that a settle tick hasn't carried yet. Reads
    /// the exact baseline [`notify_effects`](Self::notify_effects) advances, so it
    /// goes quiet precisely when the deferred recede + notify are done.
    fn has_unsettled_notifications(&self) -> bool {
        crate::notify_rules::status_map(&self.radar.notify_views()) != self.notify_prev
    }

    /// Diff observable pane statuses against `notify_prev` and emit `Effect::Notify`
    /// for each attention-status transition.
    ///
    /// Intentionally runs regardless of `permission_granted`. Without the
    /// `RunCommands` grant, `run_command` is a silent host no-op, so notifications
    /// are harmlessly dropped. More importantly, gating this on `permission_granted`
    /// would skip advancing `notify_prev` during the ungranted window, which risks a
    /// burst of stale notifications the moment the grant arrives. The ungranted window
    /// is startup-only and brief, so the no-op cost is negligible.
    fn notify_effects(&mut self) -> Vec<Effect> {
        let views = self.radar.notify_views();
        let focused = self.radar.last_focused();
        let notes = crate::notify_rules::diff(&self.notify_prev, &views, focused, &self.config);
        self.notify_prev = crate::notify_rules::status_map(&views);
        notes
            .into_iter()
            .map(|n| Effect::Notify { key: crate::notify_rules::claim_key(&n), title: n.title, body: n.body })
            .collect()
    }

    fn effects_from_renames(&self, renames: Vec<TabRename>) -> Vec<Effect> {
        renames
            .into_iter()
            .map(|TabRename { position, name }| Effect::RenameTab { position, name })
            .collect()
    }

    /// The sole projection from a domain [`RadarChange`] to host [`Effect`]s.
    /// `fx` is a caller-supplied seed so the `timer` handler's permission
    /// effects come first, without a post-hoc splice. Canonical order: renames
    /// → snapshot → presence → cwd → `SetTimeout` → notify — identical to
    /// today's `panes_changed` apart from the presence edge, so that handler
    /// is otherwise byte-for-byte unchanged. `settle` is the per-handler stamp
    /// described in `## Settle` (`CONTEXT.md`): this is the sole caller of
    /// `notify_effects`, and the only *domain-change* path that arms the timer
    /// (the permission flow arms it separately in `begin_permission_flow`).
    /// `TimerChain::arm` self-guards on the armed cadence and on whether
    /// there's anything to arm for, so calling it unconditionally here is a
    /// no-op wherever a handler has no pending work to arm for.
    ///
    /// Also the single point deciding `Effect::PersistPresence`: every
    /// domain-change entry point funnels through here with its `now_epoch_s`,
    /// which this stores (`last_now_epoch_s`) for the call paths that have no
    /// epoch of their own — `presence_json`, `session_update`,
    /// `presences_changed`. The freshly computed own-`Presence` is
    /// content-compared against the last one published, EXCLUDING
    /// `updated_epoch_s` (zeroed on both sides before the compare/cache) —
    /// see `last_presence`'s doc for why a raw compare would defeat the edge
    /// gate on Fast cadence. A real content edge pushes the effect and
    /// updates the cache, mirroring `PersistSnapshot`'s "write on edges only"
    /// rule. Withheld while `own_session_name` is empty — a presence file
    /// with no name is useless to peers — so it stays quiet until
    /// `session_update` (also routed through here) learns it.
    fn project(&mut self, mut fx: Vec<Effect>, c: RadarChange, now_epoch_s: u64) -> Outcome {
        self.last_now_epoch_s = now_epoch_s;
        fx.extend(self.effects_from_renames(c.renames));
        if c.persist_snapshot {
            fx.push(Effect::PersistSnapshot);
        }
        if !self.own_session_name.is_empty() {
            let mut fresh = self.own_presence();
            fresh.updated_epoch_s = 0;
            if self.last_presence.as_ref() != Some(&fresh) {
                self.last_presence = Some(fresh);
                fx.push(Effect::PersistPresence);
            }
        }
        if !c.cwd_bootstrap.is_empty() {
            fx.push(Effect::ResolveCwd { pane_ids: c.cwd_bootstrap });
        }
        self.arm_timer_if_needed(now_epoch_s, &mut fx);
        if c.settle {
            fx.extend(self.notify_effects());
        }
        Outcome::with_effects(c.render, fx)
    }
}

#[cfg(test)]
mod tests;

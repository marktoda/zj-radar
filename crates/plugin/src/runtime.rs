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
//!   [`command`](PluginRuntime::command) /
//!   [`command_pipe`](PluginRuntime::command_pipe) — runtime config + remote
//!   commands.
//! - [`timer`](PluginRuntime::timer) — periodic tick (animation +
//!   permission-flow coordination).
//! - [`mouse_click`](PluginRuntime::mouse_click) — resolved against the cached
//!   [`RenderedRail`] for click-to-switch.
//! - [`permission_result`](PluginRuntime::permission_result) — Zellij's grant /
//!   deny verdict.

use crate::control::Command;
use crate::config;
use crate::permission::{PermissionMarker, PermissionPolicy, PermissionProbe, PermissionState, Transition};
use crate::radar_state::{Direction, PaneUpdate, RadarChange, RadarState, RadarTab};
use crate::render::{self, RenderedRail};
use crate::rollup::TabRow;
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
        self.project(vec![], change)
    }

    pub(crate) fn panes_changed(&mut self, update: PaneUpdate) -> Outcome {
        if let Some(theme) = update.theme.clone() {
            self.theme = theme;
        }
        let change = self.radar.panes_changed(
            update,
            self.tick,
            crate::clock::now_epoch_s(),
            self.config.naming,
        );
        self.project(vec![], change)
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
        let store_changed = self.radar.timer(self.tick, crate::clock::now_epoch_s());
        // Capture before re-arming: an in-flight permission request must repaint
        // the needs_permission screen each tick until the user answers.
        let awaiting_permission = self.sidebar_should_be_selectable();
        let render = permission_changed
            || awaiting_permission
            || store_changed
            || self.timer_should_continue()
            // A Slow tick exists precisely to repaint ledger ages — even
            // when nothing else changed, `format_age` output may have moved.
            || self.desired_cadence() == Some(Cadence::Slow);
        let change = RadarChange {
            render,
            settle: true,
            persist_snapshot: store_changed,
            renames: vec![],
            cwd_bootstrap: vec![],
        };
        self.project(effects, change)
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

    /// Run an imperative command verb. Read-only navigation today: resolves a
    /// deterministic target tab and emits `SwitchTab`. Inert until permission is
    /// granted, mirroring `mouse_click`.
    pub(crate) fn command(&self, cmd: Command) -> Outcome {
        if !self.permission.granted() {
            return Outcome::none();
        }
        let dir = match cmd {
            Command::AttentionNext => Direction::Next,
            Command::AttentionPrev => Direction::Prev,
        };
        match self.radar.next_attention_tab(dir) {
            Some(position) => Outcome::with_effects(false, vec![Effect::SwitchTab { position }]),
            None => Outcome::none(),
        }
    }

    /// Parse a `cmd.v1` payload and dispatch it. Unknown verbs are a no-op.
    pub(crate) fn command_pipe(&self, payload: &str) -> Outcome {
        match crate::control::parse(payload) {
            Some(cmd) => self.command(cmd),
            None => Outcome::none(),
        }
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
        self.project(vec![], change)
    }

    pub(crate) fn command_changed(
        &mut self,
        pane_id: u32,
        command: &[String],
        is_foreground: bool,
    ) -> Outcome {
        let change = self.radar.command_changed(
            pane_id,
            command,
            is_foreground,
            self.tick,
            crate::clock::now_epoch_s(),
        );
        self.project(vec![], change)
    }

    pub(crate) fn status_pipe(&mut self, raw: &str) -> Outcome {
        let Some(change) = self.radar.status_pipe(
            raw,
            self.tick,
            crate::clock::now_epoch_s(),
            self.config.naming,
        ) else {
            return Outcome::none();
        };
        self.project(vec![], change)
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
        self.project(vec![], change)
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
        self.arm_timer_if_needed(&mut effects);
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
    fn desired_cadence(&self) -> Option<Cadence> {
        if self.permission.is_waiting() || self.permission.selectable() || self.timer_should_continue() {
            Some(Cadence::Fast)
        } else if self.radar.ledger_any_unsaturated(crate::clock::now_epoch_s())
            || self.radar.pending_wait_unsaturated(crate::clock::now_epoch_s())
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
    fn arm_timer_if_needed(&mut self, effects: &mut Vec<Effect>) {
        if let Some(cadence) = self.timer_chain.arm(self.desired_cadence()) {
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
    /// → snapshot → cwd → `SetTimeout` → notify — identical to today's
    /// `panes_changed`, so that handler is byte-for-byte unchanged. `settle`
    /// is the per-handler stamp described in `## Settle` (`CONTEXT.md`): this
    /// is the sole caller of `notify_effects`, and the only *domain-change* path
    /// that arms the timer (the permission flow arms it separately in
    /// `begin_permission_flow`). `TimerChain::arm` self-guards on the armed cadence and on
    /// whether there's anything to arm for, so calling it unconditionally
    /// here is a no-op wherever a handler has no pending work to arm for.
    fn project(&mut self, mut fx: Vec<Effect>, c: RadarChange) -> Outcome {
        fx.extend(self.effects_from_renames(c.renames));
        if c.persist_snapshot {
            fx.push(Effect::PersistSnapshot);
        }
        if !c.cwd_bootstrap.is_empty() {
            fx.push(Effect::ResolveCwd { pane_ids: c.cwd_bootstrap });
        }
        self.arm_timer_if_needed(&mut fx);
        if c.settle {
            fx.extend(self.notify_effects());
        }
        Outcome::with_effects(c.render, fx)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::command::DEBOUNCE_TICKS;
    use crate::config::{Density, NamingMode};
    use crate::payload::{self, StatusPayload};
    use crate::radar_state::TabId;
    use crate::rollup::TerminalPane;
    use crate::status::{GlyphSet, Status};
    use std::collections::{HashMap, HashSet};

    fn tab(position: usize, name: &str, active: bool) -> RadarTab {
        RadarTab {
            id: TabId::new(position + 1),
            position,
            name: name.into(),
            active,
            has_bell: false,
        }
    }

    fn pane(id: u32) -> TerminalPane {
        TerminalPane {
            id,
            ..Default::default()
        }
    }

    fn payload_for(pane_id: u32, status: Status) -> StatusPayload {
        StatusPayload {
            pane_id,
            status,
            repo: "repo".into(),
            branch: "main".into(),
            msg: "working".into(),
            task: String::new(),
            source: "claude".into(),
        }
    }

    fn config() -> config::Config {
        config::Config {
            naming: NamingMode::Off,
            density: Density::Compact,
            ..config::Config::default()
        }
    }

    fn runtime_with_config(config: config::Config) -> PluginRuntime {
        PluginRuntime {
            config,
            ..Default::default()
        }
    }

    impl PluginRuntime {
        /// Test shorthand: deliver a live Fast fire (elapsed ~1s) — how every
        /// test that isn't about the stale-fire dedup drives the tick entry
        /// point. Dedup tests pass explicit elapsed values to `timer` instead.
        fn timer_fast(&mut self, permission: PermissionProbe) -> Outcome {
            self.timer(permission, Cadence::Fast.seconds())
        }
    }

    #[test]
    fn load_rehydrates_snapshot_and_requests_permission_for_owner() {
        let mut seeded = RadarState::default();
        seeded
            .status_mut()
            .apply(payload_for(9, Status::Running), 7, 0);
        let snapshot = seeded.snapshot_json(None, 7);

        let mut runtime = PluginRuntime::default();
        let outcome = runtime.load(
            config(),
            Some(&snapshot),
            PermissionProbe {
                marker: None,
                lock_acquired: true,
            },
        );

        assert_eq!(runtime.tick, 7);
        assert_eq!(
            runtime.radar.status_store().get(9).unwrap().status,
            Status::Running
        );
        assert_eq!(runtime.permission, PermissionState::Requesting);
        assert_eq!(
            outcome,
            Outcome {
                render: false,
                // SetTimeout keeps a paint trigger alive so the needs_permission
                // screen reaches the rail before the user grants (pre-grant
                // Zellij sends no state events to trigger a render).
                effects: vec![
                    Effect::RequestPermission,
                    Effect::SetTimeout(Cadence::Fast),
                    Effect::SetSelectable(true),
                ],
            }
        );
    }

    #[test]
    fn load_denied_marker_records_denial_without_requesting_permission() {
        let mut runtime = PluginRuntime::default();
        let outcome = runtime.load(
            config(),
            None,
            PermissionProbe {
                marker: Some(PermissionMarker::Denied),
                lock_acquired: false,
            },
        );

        assert!(!runtime.permission.granted());
        assert!(matches!(runtime.permission, PermissionState::Resolved { .. }));
        assert_eq!(
            outcome,
            Outcome {
                render: false,
                effects: vec![Effect::SetSelectable(false)],
            }
        );
    }

    // The exhaustive probe→decision/state truth table now lives in
    // `permission.rs` (`on_load_truth_table` et al.), tested directly against
    // the state machine. Runtime tests below assert only on the derived effects.

    #[test]
    fn onboarding_pane_requests_even_without_lock_and_closes_on_grant() {
        // The onboarding floating pane is the dedicated, legible prompt host. It
        // must request permission regardless of the session lock (a sidebar peer
        // may hold it), so Zellij renders its grant prompt on the focused float.
        let onboarding = config::Config { role: config::Role::Onboarding, ..config() };
        let mut runtime = PluginRuntime::default();
        let load = runtime.load(
            onboarding,
            None,
            PermissionProbe { marker: None, lock_acquired: false },
        );
        assert_eq!(runtime.permission, PermissionState::Requesting);
        assert!(load.effects.contains(&Effect::RequestPermission));

        // Once the user grants via that prompt, the onboarding pane removes itself.
        let granted = runtime.permission_result(true);
        assert!(granted.effects.contains(&Effect::CloseSelf));
    }

    #[test]
    fn sidebar_grant_does_not_close_the_pane() {
        let mut runtime = runtime_with_config(config());
        runtime.record_permission_request_started();
        let granted = runtime.permission_result(true);
        assert!(!granted.effects.contains(&Effect::CloseSelf));
    }

    #[test]
    fn deferring_rail_never_requests_until_marker_lands() {
        // In the onboarding layout the rail defers: it must NOT fire its own
        // request even though it could own the lock — that would steal Zellij's
        // prompt binding from the floating onboarding pane.
        let deferring = config::Config { defer_permission: true, ..config() };
        let mut runtime = PluginRuntime::default();
        let load = runtime.load(
            deferring,
            None,
            // Even WITH the lock available, a deferring rail must wait.
            PermissionProbe { marker: None, lock_acquired: true },
        );
        assert!(!load.effects.contains(&Effect::RequestPermission));
        assert_eq!(runtime.permission, PermissionState::WaitingForPeer { ticks: 0 });

        // A later tick that (re)acquires the lock still must not request —
        // only a landed Granted marker may unblock it.
        let tick = runtime.timer_fast(PermissionProbe { marker: None, lock_acquired: true });
        assert!(!tick.effects.contains(&Effect::RequestPermission));

        // The float's granted marker finally lets it request (auto-resolves).
        let granted_tick = runtime.timer_fast(PermissionProbe {
            marker: Some(PermissionMarker::Granted),
            lock_acquired: false,
        });
        assert!(granted_tick.effects.contains(&Effect::RequestPermission));
    }

    #[test]
    fn peer_waits_then_requests_after_granted_marker() {
        let mut runtime = PluginRuntime::default();
        let load = runtime.load(
            config(),
            None,
            PermissionProbe {
                marker: None,
                lock_acquired: false,
            },
        );
        assert_eq!(runtime.permission, PermissionState::WaitingForPeer { ticks: 0 });
        assert_eq!(
            load.effects,
            vec![Effect::SetTimeout(Cadence::Fast), Effect::SetSelectable(false)]
        );

        let timer = runtime.timer_fast(PermissionProbe {
            marker: Some(PermissionMarker::Granted),
            lock_acquired: false,
        });

        assert!(timer.render);
        assert_eq!(runtime.permission, PermissionState::Requesting);
        assert!(!runtime.permission.is_waiting());
        // The promoted peer is now an owner with an in-flight request, so it also
        // arms the needs_permission heartbeat until the user answers — and
        // immediately starts heartbeating the lock it now effectively owns.
        assert_eq!(
            timer.effects,
            vec![
                Effect::RequestPermission,
                Effect::SetSelectable(true),
                Effect::HeartbeatPermissionLock,
                Effect::SetTimeout(Cadence::Fast),
            ]
        );
    }

    #[test]
    fn requesting_instance_heartbeats_the_lock_each_tick_and_stops_when_answered() {
        // In-flight request: every tick refreshes the shared lock so waiting
        // peers can't reclaim it from under the live prompt.
        let mut runtime = PluginRuntime::default();
        let load = runtime.load(
            config(),
            None,
            PermissionProbe { marker: None, lock_acquired: true },
        );
        assert!(load.effects.contains(&Effect::RequestPermission));
        let tick = runtime.timer_fast(PermissionProbe { marker: None, lock_acquired: false });
        assert!(
            tick.effects.contains(&Effect::HeartbeatPermissionLock),
            "an in-flight request must heartbeat the lock; effects = {:?}", tick.effects,
        );

        // A merely WAITING peer never heartbeats — a stale lock is exactly the
        // signal its patience escalation relies on.
        let mut waiting = PluginRuntime::default();
        let deferring = config::Config { defer_permission: true, ..config() };
        let _ = waiting.load(deferring, None, PermissionProbe { marker: None, lock_acquired: false });
        let waiting_tick = waiting.timer_fast(PermissionProbe { marker: None, lock_acquired: false });
        assert!(!waiting_tick.effects.contains(&Effect::HeartbeatPermissionLock));

        // Answered: the heartbeat stops with the request.
        let _ = runtime.permission_result(true);
        let after = runtime.timer_fast(PermissionProbe { marker: None, lock_acquired: false });
        assert!(!after.effects.contains(&Effect::HeartbeatPermissionLock));
    }

    #[test]
    fn stranded_deferring_rail_escalates_and_requests_after_patience() {
        // The resurrect deadlock: a session rebuilt from a cached onboarding
        // layout has defer_permission rails but no float — no marker will ever
        // land. Once patience runs out AND the (stale) lock is reclaimed, the
        // rail must fire its own request instead of waiting forever.
        let deferring = config::Config { defer_permission: true, ..config() };
        let mut runtime = PluginRuntime::default();
        let _ = runtime.load(deferring, None, PermissionProbe { marker: None, lock_acquired: true });
        runtime.permission = PermissionState::WaitingForPeer {
            ticks: crate::permission::DEFER_PATIENCE_TICKS - 1,
        };
        let tick = runtime.timer_fast(PermissionProbe { marker: None, lock_acquired: true });
        assert!(
            tick.effects.contains(&Effect::RequestPermission),
            "patience exhausted + reclaimed lock must self-elect; effects = {:?}", tick.effects,
        );
        assert_eq!(runtime.permission, PermissionState::Requesting);
    }

    #[test]
    fn owner_paints_needs_permission_while_request_in_flight() {
        // Fresh first-run owner: it requests permission and must keep a paint
        // trigger alive until the user answers. Pre-grant, Zellij delivers no
        // state events (they need ReadApplicationState), so without this the
        // needs_permission screen never gets a render trigger and the rail sits
        // blank — the bug this guards.
        let mut runtime = PluginRuntime::default();
        let load = runtime.load(
            config(),
            None,
            PermissionProbe {
                marker: None,
                lock_acquired: true,
            },
        );
        assert!(
            load.effects.contains(&Effect::SetTimeout(Cadence::Fast)),
            "owner must arm a timer so the needs_permission screen gets a paint trigger",
        );

        // The tick repaints while still awaiting the user's y/n — even with no
        // marker, no reclaimed lock, and no agent work to report.
        let tick = runtime.timer_fast(PermissionProbe {
            marker: None,
            lock_acquired: false,
        });
        assert!(
            tick.render,
            "owner repaints needs_permission while its request is in-flight",
        );
        assert!(!runtime.permission.granted());

        // Once the user answers, the heartbeat stops: a granted, idle rail must
        // not spin a timer forever.
        let _ = runtime.permission_result(true);
        let after = runtime.timer_fast(PermissionProbe {
            marker: None,
            lock_acquired: false,
        });
        assert!(!after.render, "granted idle rail must not keep repainting");
        assert!(!after.effects.contains(&Effect::SetTimeout(Cadence::Fast)));
    }

    #[test]
    fn waiting_peer_self_promotes_when_it_reclaims_the_lock() {
        // A peer waiting on the owner's marker re-probes each timer. If the
        // owner died and the peer reclaimed the now-stale lock, the refreshed
        // probe reports lock_acquired with no marker — the peer must take over
        // the prompt rather than wait forever.
        let mut runtime = PluginRuntime::default();
        let _ = runtime.load(
            config(),
            None,
            PermissionProbe {
                marker: None,
                lock_acquired: false,
            },
        );
        assert_eq!(runtime.permission, PermissionState::WaitingForPeer { ticks: 0 });

        let timer = runtime.timer_fast(PermissionProbe {
            marker: None,
            lock_acquired: true,
        });

        assert_eq!(runtime.permission, PermissionState::Requesting);
        assert!(!runtime.permission.is_waiting());
        assert!(timer.effects.contains(&Effect::RequestPermission));
    }

    #[test]
    fn permission_result_persists_marker_and_updates_selectability() {
        let mut runtime = PluginRuntime::default();
        runtime.record_permission_request_started();

        let outcome = runtime.permission_result(true);

        assert!(runtime.permission.granted());
        assert!(matches!(runtime.permission, PermissionState::Resolved { .. }));
        assert_eq!(
            outcome,
            Outcome {
                render: true,
                effects: vec![
                    Effect::PersistPermissionMarker(PermissionMarker::Granted),
                    Effect::SetSelectable(false),
                ],
            }
        );
    }

    #[test]
    fn timer_promotion_persists_snapshot_for_late_spawned_instances() {
        // A tick that promotes a debounced command to Running (or confirms a
        // Done) MUTATES the command store, so it must persist the shared
        // snapshot exactly like `status_pipe` does — otherwise a tab opened in
        // that window seeds a rail missing the command and diverges until the
        // command's next lifecycle event.
        let mut runtime = runtime_with_config(config());
        let argv: Vec<String> = vec!["cargo".into(), "test".into()];
        runtime.command_changed(7, &argv, true);

        // Ticks short of the debounce window are quiet (no store mutation yet).
        for _ in 1..DEBOUNCE_TICKS {
            let quiet = runtime.timer_fast(PermissionProbe::default());
            assert!(
                !quiet.effects.iter().any(|e| matches!(e, Effect::PersistSnapshot)),
                "a tick short of the debounce window must not persist, got {:?}",
                quiet.effects
            );
        }

        // The tick that reaches the debounce window promotes → must persist.
        let promoted = runtime.timer_fast(PermissionProbe::default());
        assert!(
            promoted.effects.iter().any(|e| matches!(e, Effect::PersistSnapshot)),
            "promotion tick must persist the snapshot, got {:?}",
            promoted.effects
        );
        let json = runtime.snapshot_json(None);
        let mut restored = RadarState::default();
        restored.load_snapshot(&json).expect("valid snapshot");
        assert_eq!(
            restored.command_store().get(7).unwrap().status,
            Status::Running,
            "a late-spawned instance must see the promoted command"
        );

        // A quiet tick (no store mutation) must NOT persist.
        let quiet = runtime.timer_fast(PermissionProbe::default());
        assert!(
            !quiet.effects.iter().any(|e| matches!(e, Effect::PersistSnapshot)),
            "a no-change tick must not churn the snapshot, got {:?}",
            quiet.effects
        );
    }

    #[test]
    fn status_pipe_mutates_store_arms_timer_and_persists_snapshot() {
        let mut runtime = runtime_with_config(config());
        let raw = payload::to_wire(&StatusPayload {
            msg: "cargo test".into(),
            ..payload_for(5, Status::Running)
        });

        let outcome = runtime.status_pipe(&raw);

        assert!(outcome.render);
        assert!(runtime.radar.status_store().any_running());
        // Canonical `project` order is renames → snapshot → cwd → SetTimeout →
        // notify, so PersistSnapshot now precedes SetTimeout. Assert membership,
        // not position — the order contract has its own dedicated test.
        assert_eq!(outcome.effects.len(), 2);
        assert!(outcome.effects.contains(&Effect::SetTimeout(Cadence::Fast)));
        assert!(outcome
            .effects
            .iter()
            .any(|effect| matches!(effect, Effect::PersistSnapshot)));
        let json = runtime.snapshot_json(None);
        let mut restored = RadarState::default();
        let tick = restored.load_snapshot(&json).expect("valid snapshot");
        assert_eq!(tick, 0);
        assert_eq!(
            restored.status_store().get(5).unwrap().status,
            Status::Running
        );
    }

    #[test]
    fn panes_changed_prunes_focuses_and_persists_snapshot() {
        let mut runtime = runtime_with_config(config());
        runtime.tabs_changed(vec![tab(0, "work", true)]);
        runtime
            .radar
            .status_mut()
            .apply(payload_for(10, Status::Running), 1, 0);
        runtime
            .radar
            .status_mut()
            .apply(payload_for(11, Status::Running), 1, 0);
        runtime.radar.command_mut().on_exit(12, Some(0), 1, 0);

        let mut live = HashSet::new();
        live.insert(10);
        let mut tab_panes = HashMap::new();
        tab_panes.insert(
            0,
            vec![TerminalPane {
                focused_in_tab: true,
                ..pane(10)
            }],
        );

        let outcome = runtime.panes_changed(PaneUpdate {
            tab_panes,
            live,
            theme: Some(theme::DerivedColors::default()),
            exits: vec![(10, Some(0))],
        });

        assert!(outcome.render);
        assert_eq!(runtime.radar.last_focused(), Some(10));
        assert!(runtime.radar.status_store().get(11).is_none());
        assert!(runtime.radar.command_store().get(12).is_none());
        assert!(outcome
            .effects
            .iter()
            .any(|effect| matches!(effect, Effect::PersistSnapshot)));
    }

    #[test]
    fn panes_changed_emits_resolve_cwd_effect_for_new_panes() {
        let mut runtime = runtime_with_config(config::Config {
            naming: NamingMode::Managed,
            density: Density::Compact,
            ..config::Config::default()
        });
        runtime.tabs_changed(vec![tab(0, "Tab #1", true)]);

        let mut focused = pane(7);
        focused.focused_in_tab = true;
        let outcome = runtime.panes_changed(PaneUpdate {
            tab_panes: HashMap::from([(0, vec![focused])]),
            live: HashSet::from([7]),
            theme: None,
            exits: Vec::new(),
        });

        assert!(outcome
            .effects
            .iter()
            .any(|e| matches!(e, Effect::ResolveCwd { pane_ids } if pane_ids == &vec![7])));
    }

    #[test]
    fn cwd_change_renames_default_named_tab_and_command_uses_cwd() {
        let mut runtime = runtime_with_config(config::Config {
            naming: NamingMode::Managed,
            density: Density::Compact,
            ..config::Config::default()
        });
        runtime.tabs_changed(vec![tab(0, "Tab #1", true)]);
        runtime.radar.set_tab_panes_for_position(0, vec![pane(7)]);

        let rename = runtime.cwd_changed(7, "/work/myrepo".into());

        assert_eq!(
            rename.effects,
            vec![Effect::RenameTab {
                position: 0,
                name: "myrepo".into(),
            }]
        );
        assert_eq!(runtime.radar.applied_name(TabId::new(1)), Some("myrepo"));

        let command = vec!["cargo".to_string(), "test".to_string()];
        let command_outcome = runtime.command_changed(7, &command, true);
        assert_eq!(command_outcome.effects, vec![Effect::SetTimeout(Cadence::Fast)]);

        for _ in 1..DEBOUNCE_TICKS {
            let quiet = runtime.timer_fast(PermissionProbe::default());
            assert_eq!(
                quiet.effects,
                vec![Effect::SetTimeout(Cadence::Fast)],
                "still pending short of the debounce window"
            );
        }

        let timer = runtime.timer_fast(PermissionProbe::default());
        assert!(timer.render);
        // The promotion mutates the command store, so this tick persists the
        // snapshot too (late-spawned instances must see the Running command).
        assert_eq!(timer.effects, vec![Effect::PersistSnapshot, Effect::SetTimeout(Cadence::Fast)]);
        let state = runtime
            .radar
            .command_store()
            .get(7)
            .expect("promoted command");
        assert_eq!(state.status, Status::Running);
        assert_eq!(state.repo, "myrepo");
    }

    #[test]
    fn config_pipe_accepts_json_scalars() {
        let mut runtime = PluginRuntime::default();

        let outcome = runtime
            .config_pipe(r#"{"header":false,"density":"compact","glyphs":"nerd","naming":"off"}"#);

        assert!(outcome.render);
        assert_eq!(runtime.config.naming, NamingMode::Off);
        assert_eq!(runtime.config.density, Density::Compact);
        assert_eq!(runtime.config.glyphs, GlyphSet::Nerd);
        assert!(!runtime.config.header);
    }

    #[test]
    fn render_records_targets_and_mouse_click_returns_host_effect() {
        // 3 tracked panes → multi-pane mode (line-per-pane).
        // Line 2 = tab header, line 3 = pane 20, line 4 = pane 21, line 5 = pane 22.
        let mut runtime = PluginRuntime {
            permission: PermissionState::Resolved { granted: true },
            config: config(),
            ..Default::default()
        };
        runtime.tabs_changed(vec![tab(0, "team", false), tab(1, "plain", false)]);
        runtime
            .radar
            .set_tab_panes_for_position(0, vec![pane(20), pane(21), pane(22)]);
        runtime
            .radar
            .status_mut()
            .apply(payload_for(20, Status::Pending), 1, 0);
        runtime
            .radar
            .status_mut()
            .apply(payload_for(21, Status::Running), 1, 0);
        runtime
            .radar
            .status_mut()
            .apply(payload_for(22, Status::Running), 1, 0);

        let ansi = runtime.render(100, 80);
        assert!(ansi.contains("team"));

        let tab_click = runtime.mouse_click(2);
        let pane20_click = runtime.mouse_click(3);
        let pane21_click = runtime.mouse_click(4);

        assert_eq!(tab_click.effects, vec![Effect::SwitchTab { position: 0 }]);
        assert_eq!(pane20_click.effects, vec![Effect::ShowPane { pane_id: 20 }]);
        assert_eq!(pane21_click.effects, vec![Effect::ShowPane { pane_id: 21 }]);
    }

    #[test]
    fn single_pane_detail_line_click_shows_the_pane() {
        // One tab, one tracked pane with a msg → single-pane path: header
        // (line 2) + detail line (line 3). The detail line describes that one
        // pane, so it must click-target the pane (ShowPane), not the tab
        // (SwitchTab) — mirroring the multi-pane tree rows.
        let mut runtime = PluginRuntime {
            permission: PermissionState::Resolved { granted: true },
            config: config(),
            ..Default::default()
        };
        runtime.tabs_changed(vec![tab(0, "team", false)]);
        runtime
            .radar
            .set_tab_panes_for_position(0, vec![pane(30)]);
        runtime
            .radar
            .status_mut()
            .apply(payload_for(30, Status::Running), 1, 0);

        let ansi = runtime.render(100, 80);
        assert!(ansi.contains("team"));

        let header_click = runtime.mouse_click(2);
        let detail_click = runtime.mouse_click(3);

        assert_eq!(header_click.effects, vec![Effect::SwitchTab { position: 0 }]);
        assert_eq!(detail_click.effects, vec![Effect::ShowPane { pane_id: 30 }]);
    }

    #[test]
    fn mouse_click_is_ignored_until_permission_granted() {
        let mut runtime = runtime_with_config(config());
        runtime.tabs_changed(vec![tab(0, "team", false)]);
        runtime.render(100, 80);

        assert_eq!(runtime.mouse_click(2), Outcome::default());
    }

    #[test]
    fn no_tabs_with_history_renders_ledger_not_scanning() {
        // Zero tracked tabs alone isn't the onboarding trigger — a
        // session with completion history still has something to show. Seed a
        // Done pane, let it recede into the ledger as its tab closes, then
        // close every tab and confirm `render` picks `render_rail` (header +
        // ledger + footer) over the minimal scanning face.
        let mut runtime = PluginRuntime {
            permission: PermissionState::Resolved { granted: true },
            // `jump_hint` opted in: this test also pins the config → render
            // plumbing for the footer's alt-[n] line (hidden by default —
            // only run-owned configs, which bind the chord, may claim it).
            config: config::Config { jump_hint: config::JumpHint::AltN, ..config() },
            ..Default::default()
        };
        runtime.tabs_changed(vec![tab(0, "web", true)]);
        runtime.radar.set_tab_panes_for_position(0, vec![pane(5)]);
        runtime
            .radar
            .status_mut()
            .apply(payload_for(5, Status::Done), 1, 1_000);

        // The pane closes with a still-lit Done: pruning hands it to the
        // ledger (spec §4.2).
        runtime.panes_changed(PaneUpdate {
            tab_panes: HashMap::new(),
            live: HashSet::new(),
            theme: None,
            exits: Vec::new(),
        });
        assert!(!runtime.radar.ledger_is_empty(), "setup: ledger must be seeded");

        // The tab itself closes too — zero tabs, but history remains.
        runtime.tabs_changed(vec![]);

        let ansi = runtime.render(24, 40);
        assert!(ansi.contains("earlier"), "ledger renders even with no tabs: {ansi:?}");
        assert!(ansi.contains("alt-[n] jump"), "footer still pins to the floor: {ansi:?}");
        assert!(
            !ansi.to_lowercase().contains("scanning"),
            "must not fall back to the onboarding scanning face: {ansi:?}"
        );
    }

    #[test]
    fn command_attention_next_emits_switch_tab() {
        let mut runtime = PluginRuntime {
            permission: PermissionState::Resolved { granted: true },
            config: config(),
            ..Default::default()
        };
        // tab 0 active (running), tab 1 pending → attention.
        runtime.tabs_changed(vec![tab(0, "a", true), tab(1, "b", false)]);
        runtime.radar.set_tab_panes_for_position(0, vec![pane(10)]);
        runtime.radar.set_tab_panes_for_position(1, vec![pane(11)]);
        runtime.radar.status_mut().apply(payload_for(10, Status::Running), 1, 0);
        runtime.radar.status_mut().apply(payload_for(11, Status::Pending), 1, 0);

        let out = runtime.command(Command::AttentionNext);
        assert_eq!(out.effects, vec![Effect::SwitchTab { position: 1 }]);
    }

    #[test]
    fn command_is_inert_without_permission() {
        let mut runtime = PluginRuntime { config: config(), ..Default::default() };
        runtime.tabs_changed(vec![tab(0, "a", true), tab(1, "b", false)]);
        runtime.radar.set_tab_panes_for_position(1, vec![pane(11)]);
        runtime.radar.status_mut().apply(payload_for(11, Status::Pending), 1, 0);

        assert_eq!(runtime.command(Command::AttentionNext), Outcome::default());
    }

    #[test]
    fn command_no_op_when_no_attention() {
        let mut runtime = PluginRuntime {
            permission: PermissionState::Resolved { granted: true },
            config: config(),
            ..Default::default()
        };
        runtime.tabs_changed(vec![tab(0, "a", true)]);
        assert_eq!(runtime.command(Command::AttentionNext), Outcome::default());
    }

    #[test]
    fn command_pipe_unknown_verb_is_no_op() {
        let runtime = PluginRuntime {
            permission: PermissionState::Resolved { granted: true },
            config: config(),
            ..Default::default()
        };
        assert_eq!(runtime.command_pipe("attention-top"), Outcome::default());
        assert_eq!(runtime.command_pipe(""), Outcome::default());
    }

    // ── Effect::Notify integration ─────────────────────────────────────────────

    /// Helper: two tabs; pane 5 focused in active tab 0, pane 7 in background
    /// tab 1. Both panes have a Running command promoted via a prior timer tick.
    fn two_tab_runtime_with_running_commands() -> PluginRuntime {
        let mut rt = runtime_with_config(config());
        rt.tabs_changed(vec![tab(0, "active", true), tab(1, "bg", false)]);
        // Place panes in their tabs.
        rt.radar.set_tab_panes_for_position(0, vec![TerminalPane {
            id: 5,
            focused_in_tab: true,
            ..Default::default()
        }]);
        rt.radar.set_tab_panes_for_position(1, vec![pane(7)]);
        // Register foreground commands on both panes.
        rt.command_changed(5, &["make".into()], true);
        rt.command_changed(7, &["cargo".into(), "test".into()], true);
        // Promote pending → Running via a timer tick.
        rt.timer_fast(PermissionProbe::default());
        // The timer tick above also advances notify_prev to a Running baseline via
        // notify_effects, so subsequent tests start from Running rather than the
        // Idle default. In production the same happens on every timer fire; here
        // it means test assertions only see the transition edge under test.
        rt
    }

    #[test]
    fn project_emits_effects_in_canonical_order() {
        // Sole home of the order contract: renames → snapshot → cwd →
        // SetTimeout → notify. Seed a background Done so `settle` actually
        // produces a Notify, exercising all five effect kinds in one change.
        let mut rt = two_tab_runtime_with_running_commands();
        rt.radar.command_mut().on_exit(7, Some(0), rt.tick, 0);
        // `TimerChain::arm` self-guards on the armed cadence; the setup helper's
        // timer tick already armed it, so force the disarmed state to let
        // `project`'s unconditional arm call actually produce a `SetTimeout`.
        rt.timer_chain.disarm_for_test();

        let change = RadarChange {
            render: true,
            persist_snapshot: true,
            renames: vec![TabRename { position: 0, name: "renamed".into() }],
            cwd_bootstrap: vec![7],
            settle: true,
        };
        let outcome = rt.project(vec![], change);

        let kind = |e: &Effect| match e {
            Effect::RenameTab { .. } => 0,
            Effect::PersistSnapshot => 1,
            Effect::ResolveCwd { .. } => 2,
            Effect::SetTimeout(_) => 3,
            Effect::Notify { .. } => 4,
            other => panic!("unexpected effect in canonical-order test: {other:?}"),
        };
        let kinds: Vec<i32> = outcome.effects.iter().map(kind).collect();
        let mut sorted = kinds.clone();
        sorted.sort_unstable();
        assert_eq!(
            kinds, sorted,
            "effects must appear in canonical order (renames < snapshot < cwd < timer < notify); got {:?}",
            outcome.effects
        );
        // All five kinds must actually be present, otherwise the ordering
        // assertion above is vacuous.
        for expected in 0..=4 {
            assert!(
                kinds.contains(&expected),
                "expected effect kind {expected} to be present; got {:?}",
                outcome.effects
            );
        }
    }

    #[test]
    fn cwd_changed_never_bootstraps_cwd() {
        // Guards the bound documented on `Effect::ResolveCwd`: `cwd_changed`'s
        // `RadarChange` must never carry a `cwd_bootstrap`, or the
        // `ResolveCwd` → `cwd_changed` re-entry could recurse.
        let mut runtime = runtime_with_config(config::Config {
            naming: NamingMode::Managed,
            density: Density::Compact,
            ..config::Config::default()
        });
        runtime.tabs_changed(vec![tab(0, "Tab #1", true)]);
        runtime.radar.set_tab_panes_for_position(0, vec![pane(7)]);

        let change = runtime.radar.cwd_changed(7, "/work/myrepo".into(), NamingMode::Managed);

        assert!(change.cwd_bootstrap.is_empty());
    }

    #[test]
    fn backgrounded_done_emits_notify_effect() {
        let mut rt = two_tab_runtime_with_running_commands();
        // Pane 7 is in the background tab. Pane 5 stays focused in the active tab.
        let out = rt.panes_changed(PaneUpdate {
            tab_panes: HashMap::from([
                (0, vec![TerminalPane { id: 5, focused_in_tab: true, ..Default::default() }]),
                (1, vec![pane(7)]),
            ]),
            live: HashSet::from([5, 7]),
            theme: None,
            exits: vec![(7, Some(0))], // pane 7 exits 0 → Done in background
        });
        assert!(
            out.effects.iter().any(|e| matches!(e, Effect::Notify { .. })),
            "a background Done should emit Effect::Notify; effects = {:?}", out.effects
        );
    }

    #[test]
    fn focused_done_emits_no_notify_effect() {
        let mut rt = two_tab_runtime_with_running_commands();
        // Pane 5 is focused and exits 0. panes_changed records last_focused=Some(5)
        // via note_focus; the notifier then suppresses a Notify for the focused
        // pane. The Done stays lit on the rail (focus no longer recedes it), but
        // no notification must be emitted for the pane the user is watching.
        let out = rt.panes_changed(PaneUpdate {
            tab_panes: HashMap::from([
                (0, vec![TerminalPane { id: 5, focused_in_tab: true, ..Default::default() }]),
                (1, vec![pane(7)]),
            ]),
            live: HashSet::from([5, 7]),
            theme: None,
            exits: vec![(5, Some(0))], // pane 5 exits 0 while focused
        });
        assert!(
            !out.effects.iter().any(|e| matches!(e, Effect::Notify { .. })),
            "a focused Done must not emit Effect::Notify (the user is watching it); effects = {:?}",
            out.effects
        );
    }

    #[test]
    fn restored_snapshot_does_not_notify() {
        // Build a snapshot containing an already-Done command pane.
        let mut seeded = crate::radar_state::RadarState::default();
        seeded.command_mut().on_exit(7, Some(0), 1, 0);
        // Confirm the observation is present as Done.
        assert_eq!(seeded.command(7).unwrap().status, Status::Done);
        let snapshot = seeded.snapshot_json(None, 2);

        // Restore the snapshot via load; the seed must silence the pre-existing Done.
        let mut rt = runtime_with_config(config());
        rt.load(config(), Some(&snapshot), PermissionProbe::default());

        // A subsequent timer tick must not emit any Notify for the pre-existing pane.
        let out = rt.timer_fast(PermissionProbe::default());
        assert!(
            !out.effects.iter().any(|e| matches!(e, Effect::Notify { .. })),
            "a pre-existing Done loaded from snapshot must not fire a notification; \
             effects = {:?}", out.effects
        );
    }

    #[test]
    fn backgrounded_done_via_status_pipe_notifies_once_then_timer_quiesces() {
        // The headline of the timer-arming rule: a finished agent in a background
        // tab must NOT keep the 1 Hz timer alive forever. The Done arrives on the
        // non-settling status pipe, so the runtime arms the timer once to carry the
        // deferred notify/recede — then quiesces.
        let mut rt = runtime_with_config(config());
        let raw = payload::to_wire(&StatusPayload {
            msg: "shipped".into(),
            ..payload_for(7, Status::Done)
        });

        // The edge arms the timer but does not itself settle (focus could be stale).
        let edge = rt.status_pipe(&raw);
        assert!(edge.effects.contains(&Effect::SetTimeout(Cadence::Fast)), "status-pipe edge arms the timer");
        assert!(
            !edge.effects.iter().any(|e| matches!(e, Effect::Notify { .. })),
            "the edge itself does not notify (settle is deferred to the timer)"
        );

        // The first tick carries the deferred completion notification exactly once.
        let tick1 = rt.timer_fast(PermissionProbe::default());
        assert_eq!(
            tick1.effects.iter().filter(|e| matches!(e, Effect::Notify { .. })).count(),
            1,
            "the settle tick fires the done notification once; effects = {:?}", tick1.effects,
        );

        // Then the timer quiesces within a bounded number of ticks — a backgrounded
        // Done no longer pins it awake, and no further notifications fire.
        let mut extra = 0;
        while rt.timer_chain.armed().is_some() {
            let t = rt.timer_fast(PermissionProbe::default());
            assert!(
                !t.effects.iter().any(|e| matches!(e, Effect::Notify { .. })),
                "no repeat notification after the first settle",
            );
            extra += 1;
            assert!(extra < 4, "timer must quiesce for a backgrounded Done, not tick forever");
        }
        assert!(!rt.timer_should_continue(), "quiesced: nothing left to tick for");

        // The Done stays lit (it recedes only when focused, via a later PaneUpdate).
        assert_eq!(rt.radar.status_store().get(7).unwrap().status, Status::Done);
    }

    #[test]
    fn flash_keeps_fast_timer_until_cleared() {
        // A flip-to-pending pipe edge arms a two-tick ping flash — even once the
        // deferred notify settle has fired and nothing else is running, the
        // timer must keep ticking Fast until the flash itself clears. Mirrors
        // `backgrounded_done_via_status_pipe_notifies_once_then_timer_quiesces`,
        // which quiesces right after its one settle tick; the flash pins the
        // timer open for its own extra window on top of that.
        let mut rt = runtime_with_config(config());
        rt.tabs_changed(vec![tab(0, "work", true)]);
        rt.radar.set_tab_panes_for_position(0, vec![pane(7)]);

        let raw = payload::to_wire(&StatusPayload {
            msg: "approve?".into(),
            ..payload_for(7, Status::Pending)
        });
        let edge = rt.status_pipe(&raw);
        assert!(
            edge.effects.contains(&Effect::SetTimeout(Cadence::Fast)),
            "the flip-to-pending edge arms the timer"
        );

        // Tick 1 carries the deferred notify settle; the flash (armed through
        // tick 2) is still active, so the timer must not disarm yet.
        rt.timer_fast(PermissionProbe::default());
        assert_eq!(rt.tick, 1);
        assert!(
            rt.timer_chain.armed().is_some(),
            "flash still active at tick 1 — timer must stay armed"
        );

        // Tick 2: the flash window has just elapsed (`now_tick < flash_until`,
        // and `flash_until == 2`).
        rt.timer_fast(PermissionProbe::default());
        assert_eq!(rt.tick, 2);
        assert!(
            !rt.radar.has_active_flash(rt.tick),
            "flash window has elapsed by tick 2"
        );

        // With nothing running, the Fast loop has nothing left — but the
        // pending row's `· Nm` wait tag is still counting, so the timer
        // settles to the Slow heartbeat (the same 1h-saturating cadence the
        // ledger uses) rather than disarming outright.
        for _ in 0..3 {
            rt.timer_fast(PermissionProbe::default());
        }
        assert!(!rt.timer_should_continue(), "nothing needs the Fast loop");
        assert_eq!(
            rt.timer_chain.armed(),
            Some(Cadence::Slow),
            "an unsaturated pending wait keeps the Slow heartbeat armed"
        );
    }

    #[test]
    fn command_attention_prev_emits_switch_tab() {
        let mut runtime = PluginRuntime {
            permission: PermissionState::Resolved { granted: true },
            config: config(),
            ..Default::default()
        };
        // tab 0 active (running); tabs 1 and 2 pending → attention.
        // From active 0: Next steps forward to 1, Prev wraps backward to 2.
        runtime.tabs_changed(vec![tab(0, "a", true), tab(1, "b", false), tab(2, "c", false)]);
        runtime.radar.set_tab_panes_for_position(0, vec![pane(10)]);
        runtime.radar.set_tab_panes_for_position(1, vec![pane(11)]);
        runtime.radar.set_tab_panes_for_position(2, vec![pane(12)]);
        runtime.radar.status_mut().apply(payload_for(10, Status::Running), 1, 0);
        runtime.radar.status_mut().apply(payload_for(11, Status::Pending), 1, 0);
        runtime.radar.status_mut().apply(payload_for(12, Status::Pending), 1, 0);

        let out = runtime.command(Command::AttentionPrev);
        assert_eq!(out.effects, vec![Effect::SwitchTab { position: 2 }]);
    }

    #[test]
    fn command_pipe_dispatches_known_verb() {
        let mut runtime = PluginRuntime {
            permission: PermissionState::Resolved { granted: true },
            config: config(),
            ..Default::default()
        };
        // tab 0 active (running), tab 1 pending → attention.
        runtime.tabs_changed(vec![tab(0, "a", true), tab(1, "b", false)]);
        runtime.radar.set_tab_panes_for_position(0, vec![pane(10)]);
        runtime.radar.set_tab_panes_for_position(1, vec![pane(11)]);
        runtime.radar.status_mut().apply(payload_for(10, Status::Running), 1, 0);
        runtime.radar.status_mut().apply(payload_for(11, Status::Pending), 1, 0);

        // Exercises the full parse → command → effect path through the pipe entry.
        let out = runtime.command_pipe("attention-next");
        assert_eq!(out.effects, vec![Effect::SwitchTab { position: 1 }]);
    }

    #[test]
    fn cadence_seconds_maps_fast_and_slow() {
        // Both cadences are exercised here (rather than only via the wasm-only
        // glue that replays `SetTimeout`) so this pure mapping is host-testable
        // and neither variant reads as dead code under `cargo test`.
        assert_eq!(Cadence::Fast.seconds(), 1.0);
        assert_eq!(Cadence::Slow.seconds(), 60.0);
    }

    #[test]
    fn command_done_keeps_fast_timer_armed_until_ttl_recede() {
        let mut rt = runtime_with_config(config());
        rt.command_changed(7, &["make".into()], true);
        rt.timer_fast(PermissionProbe::default()); // debounce tick 1
        rt.timer_fast(PermissionProbe::default()); // promote (DEBOUNCE_TICKS=2)
        // Command leaves the foreground → tentative done → confirmed next tick.
        rt.command_changed(7, &["zsh".into()], true);
        rt.timer_fast(PermissionProbe::default());
        rt.timer_fast(PermissionProbe::default());
        assert_eq!(rt.radar.command_store().get(7).unwrap().status, Status::Done);
        assert!(rt.timer_chain.armed().is_some(), "a Done awaiting TTL must keep the timer armed");
        // Tick past the TTL: the Done recedes and the timer quiesces. No tab
        // topology is registered for pane 7, so the recede has no tab to
        // ledger under and is silently dropped (`ledger_receded`) — the
        // ledger stays empty and cadence fully disarms.
        for _ in 0..=crate::command::DONE_TTL_TICKS {
            rt.timer_fast(PermissionProbe::default());
        }
        assert_eq!(rt.radar.command_store().get(7).unwrap().status, Status::Idle);
        assert!(rt.radar.ledger_is_empty(), "setup: no tab topology, so the recede has nowhere to ledger");
        assert!(rt.timer_chain.armed().is_none(), "receded: nothing left to tick for");
    }

    #[test]
    fn command_ttl_recede_rearms_slow_not_fast_when_ledgered() {
        // The subtle Fast→Slow handoff: when the LAST fast-worthy signal (a
        // Done awaiting its TTL) finally recedes, `arm_timer_if_needed`
        // re-arms from scratch on that very tick's `project` call. This time
        // the pane has real tab topology, so the recede lands a fresh entry
        // in the ledger — the freshly re-armed cadence must be Slow (there's
        // an age to repaint), not None (nothing left) and not Fast (nothing
        // tick-windowed remains).
        let mut rt = runtime_with_config(config());
        rt.tabs_changed(vec![tab(0, "work", true)]);
        rt.radar.set_tab_panes_for_position(0, vec![pane(7)]);
        rt.command_changed(7, &["make".into()], true);
        rt.timer_fast(PermissionProbe::default()); // debounce tick 1
        rt.timer_fast(PermissionProbe::default()); // promote (DEBOUNCE_TICKS=2)
        rt.command_changed(7, &["zsh".into()], true);
        rt.timer_fast(PermissionProbe::default());
        rt.timer_fast(PermissionProbe::default());
        assert_eq!(rt.radar.command_store().get(7).unwrap().status, Status::Done);
        assert_eq!(
            rt.timer_chain.armed(),
            Some(Cadence::Fast),
            "a Done awaiting TTL needs Fast resolution"
        );

        for _ in 0..=crate::command::DONE_TTL_TICKS {
            rt.timer_fast(PermissionProbe::default());
        }

        assert_eq!(rt.radar.command_store().get(7).unwrap().status, Status::Idle);
        assert!(!rt.radar.ledger_is_empty(), "the TTL recede must hand the completion to the ledger");
        assert_eq!(
            rt.timer_chain.armed(),
            Some(Cadence::Slow),
            "receded: nothing fast-worthy remains, but the fresh ledger entry keeps a Slow heartbeat armed"
        );
    }

    #[test]
    fn idle_with_fresh_history_arms_slow_and_repaints() {
        let mut rt = PluginRuntime {
            permission: PermissionState::Resolved { granted: true },
            config: config(),
            ..Default::default()
        };
        let now = crate::clock::now_epoch_s();
        rt.radar.ledger_mut().push(crate::ledger::LedgerEntry {
            at_epoch_s: now,
            outcome: crate::ledger::LedgerOutcome::Done,
            tab_id: TabId::new(1),
            tab_name: "work".into(),
            label: "cargo test".into(),
            pane_id: 5,
        });
        assert!(rt.timer_chain.armed().is_none(), "setup: nothing has armed a timer yet");

        // Any event that runs `project` (here, a no-op topology update) must
        // arm the Slow heartbeat — nothing is tick-windowed, but the ledger
        // age is still changing.
        let outcome = rt.tabs_changed(vec![]);
        assert!(
            outcome.effects.contains(&Effect::SetTimeout(Cadence::Slow)),
            "idle with unsaturated history must arm Slow, got {:?}",
            outcome.effects
        );
        assert_eq!(rt.timer_chain.armed(), Some(Cadence::Slow));

        // The slow tick itself must render — it exists precisely to repaint
        // the ledger's ages.
        let tick = rt.timer_fast(PermissionProbe::default());
        assert!(tick.render, "a slow tick renders to repaint ledger ages");
    }

    #[test]
    fn saturated_history_fully_disarms() {
        let mut rt = PluginRuntime {
            permission: PermissionState::Resolved { granted: true },
            config: config(),
            ..Default::default()
        };
        // Any epoch older than SATURATE_S relative to the real wall clock —
        // 0 trivially qualifies.
        rt.radar.ledger_mut().push(crate::ledger::LedgerEntry {
            at_epoch_s: 0,
            outcome: crate::ledger::LedgerOutcome::Done,
            tab_id: TabId::new(1),
            tab_name: "work".into(),
            label: "cargo test".into(),
            pane_id: 5,
        });
        assert_eq!(rt.desired_cadence(), None, "a saturated ledger has nothing left worth ticking for");

        let outcome = rt.tabs_changed(vec![]);
        assert!(
            !outcome.effects.iter().any(|e| matches!(e, Effect::SetTimeout(_))),
            "a fully-saturated idle rail must not arm any timer, got {:?}",
            outcome.effects
        );
        assert!(rt.timer_chain.armed().is_none());
    }

    #[test]
    fn fast_work_arriving_during_slow_rearms_fast() {
        let mut rt = PluginRuntime {
            permission: PermissionState::Resolved { granted: true },
            config: config(),
            ..Default::default()
        };
        let now = crate::clock::now_epoch_s();
        rt.radar.ledger_mut().push(crate::ledger::LedgerEntry {
            at_epoch_s: now,
            outcome: crate::ledger::LedgerOutcome::Done,
            tab_id: TabId::new(1),
            tab_name: "work".into(),
            label: "cargo test".into(),
            pane_id: 5,
        });
        rt.tabs_changed(vec![]);
        assert_eq!(rt.timer_chain.armed(), Some(Cadence::Slow), "setup: slow-armed on fresh history");

        // New fast-worthy work (a Running status) arrives while Slow-armed.
        // The earlier-scheduled slow fire is a harmless spurious tick, but a
        // fresh `SetTimeout(Fast)` must also be pushed so the 1s resolution
        // returns promptly.
        let raw = payload::to_wire(&payload_for(5, Status::Running));
        let outcome = rt.status_pipe(&raw);
        assert!(
            outcome.effects.contains(&Effect::SetTimeout(Cadence::Fast)),
            "fast work arriving during a slow arm must re-arm Fast, got {:?}",
            outcome.effects
        );
        assert_eq!(rt.timer_chain.armed(), Some(Cadence::Fast));
    }

    /// Shared setup for the stale-fire dedup tests: Slow-armed on fresh
    /// history (one fire in flight), then a Running broadcast tops up Fast
    /// (a second, non-cancellable fire in flight).
    fn slow_armed_then_fast_topup() -> PluginRuntime {
        let mut rt = PluginRuntime {
            permission: PermissionState::Resolved { granted: true },
            config: config(),
            ..Default::default()
        };
        let now = crate::clock::now_epoch_s();
        rt.radar.ledger_mut().push(crate::ledger::LedgerEntry {
            at_epoch_s: now,
            outcome: crate::ledger::LedgerOutcome::Done,
            tab_id: TabId::new(1),
            tab_name: "work".into(),
            label: "cargo test".into(),
            pane_id: 5,
        });
        rt.tabs_changed(vec![]); // arms Slow: one fire in flight
        assert_eq!(rt.timer_chain.armed(), Some(Cadence::Slow), "setup: slow-armed on fresh history");
        let raw = payload::to_wire(&payload_for(5, Status::Running));
        let outcome = rt.status_pipe(&raw);
        assert!(
            outcome.effects.contains(&Effect::SetTimeout(Cadence::Fast)),
            "setup: the top-up must arm Fast, got {:?}",
            outcome.effects
        );
        rt
    }

    #[test]
    fn live_fast_fire_processes_then_stale_slow_fire_is_swallowed() {
        // The COMMON arrival order after a Slow→Fast top-up: the fast fire
        // (armed for 1s) lands first; the stale slow fire lands up to 59s
        // later. The fast fire must process normally — swallowing it by count
        // alone would freeze the tick clock (spinner, debounce, TTL, flash)
        // until the slow fire finally landed, while Fast-worthy work runs.
        let mut rt = slow_armed_then_fast_topup();

        // The live fast fire (elapsed ~1s) ticks and re-arms exactly once.
        let tick_before = rt.tick;
        let live = rt.timer(PermissionProbe::default(), 1.0);
        assert_eq!(rt.tick, tick_before + 1, "the live fast fire ticks");
        let rearms = live
            .effects
            .iter()
            .filter(|e| matches!(e, Effect::SetTimeout(_)))
            .count();
        assert_eq!(rearms, 1, "the live fire re-arms exactly once, got {:?}", live.effects);
        assert!(
            live.effects.contains(&Effect::SetTimeout(Cadence::Fast)),
            "running work keeps the Fast cadence"
        );

        // The STALE slow fire (elapsed ~60s) lands second, with the re-armed
        // fast fire still in flight: swallowed whole — no tick advance, no
        // effects, the live arm untouched. Ticking it would re-arm a
        // second persistent chain.
        let tick_before = rt.tick;
        let stale = rt.timer(PermissionProbe::default(), 60.0);
        assert_eq!(stale, Outcome::none(), "a stale slow fire must be swallowed whole");
        assert_eq!(rt.tick, tick_before, "a swallowed fire must not advance the tick");
        assert_eq!(rt.timer_chain.armed(), Some(Cadence::Fast), "a swallowed fire must not disturb the live arm");

        // Steady state: exactly one chain remains and keeps ticking.
        let next = rt.timer(PermissionProbe::default(), 1.0);
        assert_eq!(rt.tick, tick_before + 1, "the surviving chain keeps ticking");
        assert!(
            next.effects.contains(&Effect::SetTimeout(Cadence::Fast)),
            "the surviving chain re-arms, got {:?}",
            next.effects
        );
    }

    #[test]
    fn stale_slow_fire_landing_first_is_swallowed() {
        // The RARE arrival order (top-up in the slow window's final second):
        // the stale slow fire lands before the fast one. It must be swallowed
        // — another fire is in flight and its elapsed marks it slow-armed —
        // and the fast fire then processes normally.
        let mut rt = slow_armed_then_fast_topup();

        let tick_before = rt.tick;
        let stale = rt.timer(PermissionProbe::default(), 60.0);
        assert_eq!(stale, Outcome::none(), "a stale slow fire must be swallowed whole");
        assert_eq!(rt.tick, tick_before, "a swallowed fire must not advance the tick");
        assert_eq!(rt.timer_chain.armed(), Some(Cadence::Fast), "a swallowed fire must not disturb the live arm");

        // The surviving fast fire ticks normally and re-arms exactly once.
        let live = rt.timer(PermissionProbe::default(), 1.0);
        assert_eq!(rt.tick, tick_before + 1, "the live fire ticks");
        let rearms = live
            .effects
            .iter()
            .filter(|e| matches!(e, Effect::SetTimeout(_)))
            .count();
        assert_eq!(rearms, 1, "the live fire re-arms exactly once, got {:?}", live.effects);
        assert!(
            live.effects.contains(&Effect::SetTimeout(Cadence::Fast)),
            "running work keeps the Fast cadence"
        );
    }

    #[test]
    fn lone_slow_fire_processes_as_the_live_chain() {
        // A slow fire with no other fire in flight IS the live chain — its
        // 60s elapsed must not get it swallowed, or the idle heartbeat (and
        // the ledger-age repaint it exists for) would die.
        let mut rt = PluginRuntime {
            permission: PermissionState::Resolved { granted: true },
            config: config(),
            ..Default::default()
        };
        let now = crate::clock::now_epoch_s();
        rt.radar.ledger_mut().push(crate::ledger::LedgerEntry {
            at_epoch_s: now,
            outcome: crate::ledger::LedgerOutcome::Done,
            tab_id: TabId::new(1),
            tab_name: "work".into(),
            label: "cargo test".into(),
            pane_id: 5,
        });
        rt.tabs_changed(vec![]); // arms Slow: the only fire in flight
        assert_eq!(rt.timer_chain.armed(), Some(Cadence::Slow));

        let tick = rt.timer(PermissionProbe::default(), 60.0);
        assert!(tick.render, "the lone slow fire processes and repaints ledger ages");
        assert!(
            tick.effects.contains(&Effect::SetTimeout(Cadence::Slow)),
            "the lone slow chain re-arms itself, got {:?}",
            tick.effects
        );
    }
}

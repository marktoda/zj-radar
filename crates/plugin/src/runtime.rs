//! Deep runtime module: repo-owned events in, ordered host effects out.
//! No zellij-tile dependency.

use crate::control::Command;
use crate::config;
use crate::radar_state::{Direction, PaneUpdate, RadarState, RadarTab};
use crate::render::{self, RenderedRail, TabRow};
use crate::tab_namer::TabRename;
use crate::theme;
use std::collections::BTreeMap;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum PermissionMarker {
    Granted,
    Denied,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) struct PermissionProbe {
    pub marker: Option<PermissionMarker>,
    pub lock_acquired: bool,
}

/// What a `PermissionProbe` dictates this sidebar do, independent of whether it
/// arrived at load or on a deferred timer tick. `None` from
/// [`PluginRuntime::permission_decision`] means "no decision yet — keep waiting
/// on a peer." See that function for the single mapping both entry points share.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum PermissionDecision {
    /// Become the prompt-shower: request permission from Zellij.
    Request,
    /// Permission was denied; record the terminal result.
    Deny,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum Effect {
    RequestPermission,
    SetSelectable(bool),
    SetTimeout,
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
    Notify { title: String, body: String },
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

#[derive(Default)]
pub(crate) struct PluginRuntime {
    pub(crate) radar: RadarState,
    pub(crate) tick: u64,
    pub(crate) timer_armed: bool,
    pub(crate) last_render_height: usize,
    pub(crate) config: config::Config,
    pub(crate) permission_granted: bool,
    pub(crate) permission_response_received: bool,
    pub(crate) permission_request_started: bool,
    pub(crate) permission_waiting_for_peer: bool,
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
        self.radar.rows()
    }

    pub(crate) fn tabs_changed(&mut self, tabs: Vec<RadarTab>) -> Outcome {
        let change = self.radar.tabs_changed(tabs);
        Outcome::with_effects(change.render, Vec::new())
    }

    pub(crate) fn panes_changed(&mut self, update: PaneUpdate) -> Outcome {
        if let Some(theme) = update.theme.clone() {
            self.theme = theme;
        }
        let change = self
            .radar
            .panes_changed(update, self.tick, self.config.naming);
        let mut effects = self.effects_from_renames(change.renames);
        if change.persist_snapshot {
            effects.push(Effect::PersistSnapshot);
        }
        if !change.cwd_bootstrap.is_empty() {
            effects.push(Effect::ResolveCwd {
                pane_ids: change.cwd_bootstrap,
            });
        }
        effects.extend(self.notify_effects());
        Outcome::with_effects(true, effects)
    }

    pub(crate) fn timer(&mut self, permission: PermissionProbe) -> Outcome {
        self.timer_armed = false;
        let mut effects = Vec::new();
        let permission_changed =
            self.check_deferred_permission_request(permission, &mut effects);
        self.tick += 1;
        self.radar.timer(self.tick);
        // Capture before re-arming: an in-flight permission request must repaint
        // the needs_permission screen each tick until the user answers.
        let awaiting_permission = self.sidebar_should_be_selectable();
        self.arm_timer_if_needed(&mut effects);
        effects.extend(self.notify_effects());
        Outcome::with_effects(
            permission_changed || awaiting_permission || self.radar.has_active_or_pending_work(),
            effects,
        )
    }

    pub(crate) fn mouse_click(&self, line: isize) -> Outcome {
        if !self.permission_granted {
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
        if !self.permission_granted {
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
        Outcome::with_effects(change.render, self.effects_from_renames(change.renames))
    }

    pub(crate) fn command_changed(
        &mut self,
        pane_id: u32,
        command: &[String],
        is_foreground: bool,
    ) -> Outcome {
        let change = self
            .radar
            .command_changed(pane_id, command, is_foreground, self.tick);
        let mut effects = Vec::new();
        self.arm_timer_if_needed(&mut effects);
        Outcome::with_effects(change.render, effects)
    }

    pub(crate) fn status_pipe(&mut self, raw: &str) -> Outcome {
        let Some(change) = self.radar.status_pipe(raw, self.tick, self.config.naming) else {
            return Outcome::none();
        };
        let mut effects = self.effects_from_renames(change.renames);
        self.arm_timer_if_needed(&mut effects);
        if change.persist_snapshot {
            effects.push(Effect::PersistSnapshot);
        }
        Outcome::with_effects(change.render, effects)
    }

    pub(crate) fn snapshot_json(&self, existing: Option<&str>) -> String {
        self.radar.snapshot_json(existing, self.tick)
    }

    pub(crate) fn config_pipe(&mut self, raw: &str) -> Outcome {
        let Ok(val) = serde_json::from_str::<serde_json::Value>(raw) else {
            return Outcome::none();
        };
        let Some(obj) = val.as_object() else {
            return Outcome::none();
        };
        let kv: BTreeMap<String, String> = obj
            .iter()
            .filter_map(|(k, v)| {
                let s = match v {
                    serde_json::Value::String(s) => Some(s.clone()),
                    serde_json::Value::Bool(b) => {
                        Some(if *b { "true" } else { "false" }.to_string())
                    }
                    serde_json::Value::Number(n) => Some(n.to_string()),
                    _ => None,
                };
                s.map(|s| (k.clone(), s))
            })
            .collect();
        self.config.apply_overrides(&kv);
        let renames = self.radar.recompute_renames(self.config.naming);
        Outcome::with_effects(true, self.effects_from_renames(renames))
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
        };
        let rail = if !self.permission_granted {
            render::needs_permission(&opts)
        } else if tabrows.is_empty() {
            render::onboarding(&opts)
        } else {
            render::render_rail(&tabrows, &opts)
        };
        let ansi = rail.ansi.clone();
        self.last_rendered = rail;
        ansi
    }

    pub(crate) fn sidebar_should_be_selectable(&self) -> bool {
        self.permission_request_started && !self.permission_response_received
    }

    pub(crate) fn record_permission_request_started(&mut self) {
        self.permission_request_started = true;
    }

    pub(crate) fn record_permission_result(&mut self, granted: bool) {
        self.permission_granted = granted;
        self.permission_response_received = true;
        self.permission_request_started = false;
        self.permission_waiting_for_peer = false;
    }

    #[cfg(test)]
    pub(crate) fn reconcile_focus(&mut self, focused: Option<u32>, tick: u64) -> bool {
        self.radar.reconcile_focus(focused, tick)
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

    /// The single source of truth for what a probe means — shared by the load
    /// path (`begin_permission_flow`) and the deferred timer path
    /// (`check_deferred_permission_request`) so the two can never disagree.
    ///
    /// `Granted` and "no marker but we hold the lock" both mean *request now*:
    /// either permission is already granted (the request will auto-resolve) or
    /// we just won/reclaimed the first-run lock and own the prompt. A bare
    /// `None` (no marker, no lock) is no decision yet — keep waiting on a peer.
    fn permission_decision(probe: &PermissionProbe) -> Option<PermissionDecision> {
        match probe.marker {
            Some(PermissionMarker::Granted) => Some(PermissionDecision::Request),
            Some(PermissionMarker::Denied) => Some(PermissionDecision::Deny),
            None if probe.lock_acquired => Some(PermissionDecision::Request),
            None => None,
        }
    }

    /// Role/defer-aware decision, used by BOTH the load and timer paths so they
    /// can't diverge:
    /// - the onboarding float always owns the prompt (it's the only legible
    ///   surface), regardless of the lock;
    /// - a deferring rail acts ONLY on a landed marker — it never self-owns via
    ///   the lock, which would steal Zellij's prompt binding from the float;
    /// - everyone else uses the plain lock-coordinated decision.
    fn decide(&self, probe: &PermissionProbe) -> Option<PermissionDecision> {
        if self.config.role == config::Role::Onboarding {
            return Some(PermissionDecision::Request);
        }
        if self.config.defer_permission {
            return match probe.marker {
                Some(PermissionMarker::Granted) => Some(PermissionDecision::Request),
                Some(PermissionMarker::Denied) => Some(PermissionDecision::Deny),
                None => None,
            };
        }
        Self::permission_decision(probe)
    }

    /// Apply a resolved decision: mutate permission state and push the
    /// request/record effect. Always clears `permission_waiting_for_peer` (a
    /// decision ends the wait). The caller owns the trailing `SetSelectable`,
    /// since the two entry points emit it differently (always vs. only on a
    /// decision).
    fn apply_permission_decision(&mut self, decision: PermissionDecision, effects: &mut Vec<Effect>) {
        self.permission_waiting_for_peer = false;
        match decision {
            PermissionDecision::Request => {
                self.record_permission_request_started();
                effects.push(Effect::RequestPermission);
            }
            PermissionDecision::Deny => self.record_permission_result(false),
        }
    }

    fn begin_permission_flow(&mut self, permission: PermissionProbe) -> Outcome {
        let mut effects = Vec::new();
        match self.decide(&permission) {
            Some(decision) => self.apply_permission_decision(decision, &mut effects),
            None => self.permission_waiting_for_peer = true,
        }
        // Arm a timer whenever a decision is still outstanding — either we're
        // waiting on a peer's marker, or our own request is in-flight. Pre-grant
        // Zellij withholds the state events that would otherwise trigger a paint
        // (they need ReadApplicationState), so this timer is the only thing that
        // gets the needs_permission screen onto the rail.
        self.arm_timer_if_needed(&mut effects);
        // Load always initializes the sidebar's selectability, every arm.
        effects.push(Effect::SetSelectable(self.sidebar_should_be_selectable()));
        Outcome::with_effects(false, effects)
    }

    fn check_deferred_permission_request(
        &mut self,
        probe: PermissionProbe,
        effects: &mut Vec<Effect>,
    ) -> bool {
        if !self.permission_waiting_for_peer {
            return false;
        }
        match self.decide(&probe) {
            // A decision landed (marker arrived, or we reclaimed a stale lock —
            // see session_files): apply it and refresh selectability.
            Some(decision) => {
                self.apply_permission_decision(decision, effects);
                effects.push(Effect::SetSelectable(self.sidebar_should_be_selectable()));
                true
            }
            // Still no marker and no lock: keep waiting (no effect, no change).
            None => false,
        }
    }

    fn arm_timer_if_needed(&mut self, effects: &mut Vec<Effect>) {
        if !self.timer_armed
            && (self.permission_waiting_for_peer
                || self.sidebar_should_be_selectable()
                || self.radar.has_active_or_pending_work())
        {
            self.timer_armed = true;
            effects.push(Effect::SetTimeout);
        }
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
            .map(|n| Effect::Notify { title: n.title, body: n.body })
            .collect()
    }

    fn effects_from_renames(&self, renames: Vec<TabRename>) -> Vec<Effect> {
        renames
            .into_iter()
            .map(|TabRename { position, name }| Effect::RenameTab { position, name })
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::{Density, NamingMode};
    use crate::payload::{self, StatusPayload};
    use crate::radar_state::{TabId, TerminalPane};
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
            on_focus: None,
            seq: None,
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

    #[test]
    fn load_rehydrates_snapshot_and_requests_permission_for_owner() {
        let mut seeded = RadarState::default();
        seeded
            .status_mut()
            .apply(payload_for(9, Status::Running), 7);
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
        assert!(runtime.permission_request_started);
        assert_eq!(
            outcome,
            Outcome {
                render: false,
                // SetTimeout keeps a paint trigger alive so the needs_permission
                // screen reaches the rail before the user grants (pre-grant
                // Zellij sends no state events to trigger a render).
                effects: vec![
                    Effect::RequestPermission,
                    Effect::SetTimeout,
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

        assert!(!runtime.permission_granted);
        assert!(runtime.permission_response_received);
        assert_eq!(
            outcome,
            Outcome {
                render: false,
                effects: vec![Effect::SetSelectable(false)],
            }
        );
    }

    #[test]
    fn permission_decision_maps_every_probe() {
        // Lock the shared probe→decision table directly (all marker × lock
        // combinations), so the mapping both entry points ride is guarded
        // independently of the flow tests below.
        use PermissionDecision::{Deny, Request};
        use PermissionMarker::{Denied, Granted};
        let cases = [
            // marker,          lock,  expected
            (Some(Granted), false, Some(Request)),
            (Some(Granted), true, Some(Request)),
            (Some(Denied), false, Some(Deny)),
            (Some(Denied), true, Some(Deny)),
            (None, true, Some(Request)), // reclaimed/own the lock → request
            (None, false, None),         // no marker, no lock → keep waiting
        ];
        for (marker, lock_acquired, expected) in cases {
            let probe = PermissionProbe {
                marker,
                lock_acquired,
            };
            assert_eq!(
                PluginRuntime::permission_decision(&probe),
                expected,
                "probe {probe:?}",
            );
        }
    }

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
        assert!(runtime.permission_request_started);
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
        assert!(runtime.permission_waiting_for_peer);

        // A later tick that (re)acquires the lock still must not request —
        // only a landed Granted marker may unblock it.
        let tick = runtime.timer(PermissionProbe { marker: None, lock_acquired: true });
        assert!(!tick.effects.contains(&Effect::RequestPermission));

        // The float's granted marker finally lets it request (auto-resolves).
        let granted_tick = runtime.timer(PermissionProbe {
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
        assert!(runtime.permission_waiting_for_peer);
        assert_eq!(
            load.effects,
            vec![Effect::SetTimeout, Effect::SetSelectable(false)]
        );

        let timer = runtime.timer(PermissionProbe {
            marker: Some(PermissionMarker::Granted),
            lock_acquired: false,
        });

        assert!(timer.render);
        assert!(runtime.permission_request_started);
        assert!(!runtime.permission_waiting_for_peer);
        // The promoted peer is now an owner with an in-flight request, so it also
        // arms the needs_permission heartbeat until the user answers.
        assert_eq!(
            timer.effects,
            vec![
                Effect::RequestPermission,
                Effect::SetSelectable(true),
                Effect::SetTimeout,
            ]
        );
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
            load.effects.contains(&Effect::SetTimeout),
            "owner must arm a timer so the needs_permission screen gets a paint trigger",
        );

        // The tick repaints while still awaiting the user's y/n — even with no
        // marker, no reclaimed lock, and no agent work to report.
        let tick = runtime.timer(PermissionProbe {
            marker: None,
            lock_acquired: false,
        });
        assert!(
            tick.render,
            "owner repaints needs_permission while its request is in-flight",
        );
        assert!(!runtime.permission_granted);

        // Once the user answers, the heartbeat stops: a granted, idle rail must
        // not spin a timer forever.
        let _ = runtime.permission_result(true);
        let after = runtime.timer(PermissionProbe {
            marker: None,
            lock_acquired: false,
        });
        assert!(!after.render, "granted idle rail must not keep repainting");
        assert!(!after.effects.contains(&Effect::SetTimeout));
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
        assert!(runtime.permission_waiting_for_peer);

        let timer = runtime.timer(PermissionProbe {
            marker: None,
            lock_acquired: true,
        });

        assert!(runtime.permission_request_started);
        assert!(!runtime.permission_waiting_for_peer);
        assert!(timer.effects.contains(&Effect::RequestPermission));
    }

    #[test]
    fn permission_result_persists_marker_and_updates_selectability() {
        let mut runtime = PluginRuntime::default();
        runtime.record_permission_request_started();

        let outcome = runtime.permission_result(true);

        assert!(runtime.permission_granted);
        assert!(runtime.permission_response_received);
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
    fn status_pipe_mutates_store_arms_timer_and_persists_snapshot() {
        let mut runtime = runtime_with_config(config());
        let raw = payload::to_wire(
            5,
            Status::Running,
            "repo",
            "main",
            "cargo test",
            Some(Status::Idle),
            "claude",
        );

        let outcome = runtime.status_pipe(&raw);

        assert!(outcome.render);
        assert!(runtime.radar.status_store().any_active());
        assert_eq!(outcome.effects.len(), 2);
        assert_eq!(outcome.effects[0], Effect::SetTimeout);
        let Effect::PersistSnapshot = &outcome.effects[1] else {
            panic!("expected persisted snapshot");
        };
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
            .apply(payload_for(10, Status::Running), 1);
        runtime
            .radar
            .status_mut()
            .apply(payload_for(11, Status::Running), 1);
        runtime.radar.command_mut().on_exit(12, Some(0), 1);

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
        assert_eq!(command_outcome.effects, vec![Effect::SetTimeout]);

        let timer = runtime.timer(PermissionProbe::default());
        assert!(timer.render);
        assert_eq!(timer.effects, vec![Effect::SetTimeout]);
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
            permission_granted: true,
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
            .apply(payload_for(20, Status::Pending), 1);
        runtime
            .radar
            .status_mut()
            .apply(payload_for(21, Status::Running), 1);
        runtime
            .radar
            .status_mut()
            .apply(payload_for(22, Status::Running), 1);

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
    fn mouse_click_is_ignored_until_permission_granted() {
        let mut runtime = runtime_with_config(config());
        runtime.tabs_changed(vec![tab(0, "team", false)]);
        runtime.render(100, 80);

        assert_eq!(runtime.mouse_click(2), Outcome::default());
    }

    #[test]
    fn command_attention_next_emits_switch_tab() {
        let mut runtime = PluginRuntime {
            permission_granted: true,
            config: config(),
            ..Default::default()
        };
        // tab 0 active (running), tab 1 pending → attention.
        runtime.tabs_changed(vec![tab(0, "a", true), tab(1, "b", false)]);
        runtime.radar.set_tab_panes_for_position(0, vec![pane(10)]);
        runtime.radar.set_tab_panes_for_position(1, vec![pane(11)]);
        runtime.radar.status_mut().apply(payload_for(10, Status::Running), 1);
        runtime.radar.status_mut().apply(payload_for(11, Status::Pending), 1);

        let out = runtime.command(Command::AttentionNext);
        assert_eq!(out.effects, vec![Effect::SwitchTab { position: 1 }]);
    }

    #[test]
    fn command_is_inert_without_permission() {
        let mut runtime = PluginRuntime { config: config(), ..Default::default() };
        runtime.tabs_changed(vec![tab(0, "a", true), tab(1, "b", false)]);
        runtime.radar.set_tab_panes_for_position(1, vec![pane(11)]);
        runtime.radar.status_mut().apply(payload_for(11, Status::Pending), 1);

        assert_eq!(runtime.command(Command::AttentionNext), Outcome::default());
    }

    #[test]
    fn command_no_op_when_no_attention() {
        let mut runtime = PluginRuntime {
            permission_granted: true,
            config: config(),
            ..Default::default()
        };
        runtime.tabs_changed(vec![tab(0, "a", true)]);
        assert_eq!(runtime.command(Command::AttentionNext), Outcome::default());
    }

    #[test]
    fn command_pipe_unknown_verb_is_no_op() {
        let runtime = PluginRuntime {
            permission_granted: true,
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
        rt.timer(PermissionProbe::default());
        // The timer tick above also advances notify_prev to a Running baseline via
        // notify_effects, so subsequent tests start from Running rather than the
        // Idle default. In production the same happens on every timer fire; here
        // it means test assertions only see the transition edge under test.
        rt
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
        // Pane 5 is focused and exits 0. The helper never calls panes_changed, so
        // last_focused is None; panes_changed here transitions focus None→Some(5),
        // which is a change, so reconcile_focus takes the visit-clear branch
        // (on_pane_focused) and clears the Done before notify_effects runs.
        // No notification must be emitted.
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
            "a focused Done recedes to Idle and must not emit Effect::Notify; effects = {:?}",
            out.effects
        );
    }

    #[test]
    fn restored_snapshot_does_not_notify() {
        // Build a snapshot containing an already-Done command pane.
        let mut seeded = crate::radar_state::RadarState::default();
        seeded.command_mut().on_exit(7, Some(0), 1);
        // Confirm the observation is present as Done.
        assert_eq!(seeded.command(7).unwrap().status, Status::Done);
        let snapshot = seeded.snapshot_json(None, 2);

        // Restore the snapshot via load; the seed must silence the pre-existing Done.
        let mut rt = runtime_with_config(config());
        rt.load(config(), Some(&snapshot), PermissionProbe::default());

        // A subsequent timer tick must not emit any Notify for the pre-existing pane.
        let out = rt.timer(PermissionProbe::default());
        assert!(
            !out.effects.iter().any(|e| matches!(e, Effect::Notify { .. })),
            "a pre-existing Done loaded from snapshot must not fire a notification; \
             effects = {:?}", out.effects
        );
    }

    #[test]
    fn command_attention_prev_emits_switch_tab() {
        let mut runtime = PluginRuntime {
            permission_granted: true,
            config: config(),
            ..Default::default()
        };
        // tab 0 active (running); tabs 1 and 2 pending → attention.
        // From active 0: Next steps forward to 1, Prev wraps backward to 2.
        runtime.tabs_changed(vec![tab(0, "a", true), tab(1, "b", false), tab(2, "c", false)]);
        runtime.radar.set_tab_panes_for_position(0, vec![pane(10)]);
        runtime.radar.set_tab_panes_for_position(1, vec![pane(11)]);
        runtime.radar.set_tab_panes_for_position(2, vec![pane(12)]);
        runtime.radar.status_mut().apply(payload_for(10, Status::Running), 1);
        runtime.radar.status_mut().apply(payload_for(11, Status::Pending), 1);
        runtime.radar.status_mut().apply(payload_for(12, Status::Pending), 1);

        let out = runtime.command(Command::AttentionPrev);
        assert_eq!(out.effects, vec![Effect::SwitchTab { position: 2 }]);
    }

    #[test]
    fn command_pipe_dispatches_known_verb() {
        let mut runtime = PluginRuntime {
            permission_granted: true,
            config: config(),
            ..Default::default()
        };
        // tab 0 active (running), tab 1 pending → attention.
        runtime.tabs_changed(vec![tab(0, "a", true), tab(1, "b", false)]);
        runtime.radar.set_tab_panes_for_position(0, vec![pane(10)]);
        runtime.radar.set_tab_panes_for_position(1, vec![pane(11)]);
        runtime.radar.status_mut().apply(payload_for(10, Status::Running), 1);
        runtime.radar.status_mut().apply(payload_for(11, Status::Pending), 1);

        // Exercises the full parse → command → effect path through the pipe entry.
        let out = runtime.command_pipe("attention-next");
        assert_eq!(out.effects, vec![Effect::SwitchTab { position: 1 }]);
    }
}

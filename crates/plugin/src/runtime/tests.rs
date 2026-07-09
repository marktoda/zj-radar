use super::*;
use crate::command::{DEBOUNCE_TICKS, EpochSecs, Tick};
use crate::config::{Density, NamingMode};
use crate::payload::{self, StatusPayload};
use crate::radar_state::TabId;
use crate::rollup::TerminalPane;
use crate::status::{GlyphSet, Status};
use crate::test_fixtures::{pane, payload_for, tab};
use std::collections::{HashMap, HashSet};

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

#[test]
fn denied_rail_with_running_snapshot_never_arms_the_timer() {
    // A denied rail receives none of the state events that clear domain work
    // (they need ReadApplicationState), so a stale `Running` rehydrated from a
    // snapshot can never finish — arming Fast for it would pin 1 Hz ticks and
    // repaints forever behind the static needs-permission face.
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
            marker: Some(PermissionMarker::Denied),
            lock_acquired: false,
        },
    );

    assert!(runtime.permission.denied());
    assert!(runtime.radar.has_running_work());
    assert!(
        !outcome.effects.iter().any(|e| matches!(e, Effect::SetTimeout(_))),
        "denied rail must not tick for work it can never observe finishing"
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
            Effect::ReadPresences,
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
fn probe_is_wanted_only_while_waiting_on_a_peer() {
    // The wasm glue stat-reads the marker file (and attempts the lock) each
    // tick ONLY while the machine can still consume the probe. Once a request
    // is in-flight or the state resolves, `on_timer` ignores it — so the
    // per-tick disk probe must stop (N tabs were reading N files/sec forever).
    let mut runtime = PluginRuntime::default();
    let _ = runtime.load(
        config(),
        None,
        PermissionProbe { marker: None, lock_acquired: false },
    );
    assert_eq!(runtime.permission, PermissionState::WaitingForPeer { ticks: 0 });
    assert!(runtime.wants_permission_probe(), "a waiting peer must keep probing");

    // Still waiting after an undecided tick: the probe stays wanted — this is
    // what lets a peer eventually reclaim a dead owner's stale lock.
    let _ = runtime.timer_fast(PermissionProbe { marker: None, lock_acquired: false });
    assert!(runtime.wants_permission_probe(), "an undecided tick must keep probing");

    // Promoted to an in-flight request: settled from the probe's point of view.
    let _ = runtime.timer_fast(PermissionProbe { marker: None, lock_acquired: true });
    assert_eq!(runtime.permission, PermissionState::Requesting);
    assert!(!runtime.wants_permission_probe(), "an in-flight request ignores the probe");

    // Resolved: terminal — the probe is never wanted again.
    let _ = runtime.permission_result(true);
    assert!(!runtime.wants_permission_probe(), "a resolved state ignores the probe");
}

#[test]
fn probe_stays_wanted_for_a_deferring_rail_past_its_patience() {
    // The stranded deferring rail relies on the per-tick re-probe to notice a
    // reclaimable stale lock long after DEFER_PATIENCE_TICKS. Gating the disk
    // probe must not starve that escalation path.
    let deferring = config::Config { defer_permission: true, ..config() };
    let mut runtime = PluginRuntime::default();
    let _ = runtime.load(deferring, None, PermissionProbe { marker: None, lock_acquired: false });
    runtime.permission = PermissionState::WaitingForPeer {
        ticks: crate::permission::DEFER_PATIENCE_TICKS + 5,
    };
    assert!(
        runtime.wants_permission_probe(),
        "an impatient deferring waiter still needs fresh probes to escalate",
    );
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
    runtime.radar.command_mut().on_exit(12, Some(0), Tick(1), EpochSecs(0));

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

    let first = runtime.panes_changed(PaneUpdate {
        tab_panes: tab_panes.clone(),
        live: live.clone(),
        theme: Some(theme::DerivedColors::default()),
        exits: vec![(10, Some(0))],
    });
    assert!(first.render);
    assert_eq!(runtime.radar.last_focused(), Some(10));
    // Panes 11/12 are absent for the first time — the break-pane grace keeps
    // their observations for one manifest; the second absence prunes.
    let outcome = runtime.panes_changed(PaneUpdate {
        tab_panes,
        live,
        theme: Some(theme::DerivedColors::default()),
        exits: Vec::new(),
    });

    assert!(outcome.render);
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
            vec![Effect::ReadPresences, Effect::SetTimeout(Cadence::Fast)],
            "still pending short of the debounce window"
        );
    }

    let timer = runtime.timer_fast(PermissionProbe::default());
    assert!(timer.render);
    // The promotion mutates the command store, so this tick persists the
    // snapshot too (late-spawned instances must see the Running command).
    assert_eq!(
        timer.effects,
        vec![Effect::ReadPresences, Effect::PersistSnapshot, Effect::SetTimeout(Cadence::Fast)]
    );
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

    // The pane closes with a still-lit Done: the second absence confirms the
    // close (the first is the break-pane grace) and pruning hands it to the
    // ledger (spec §4.2).
    for _ in 0..2 {
        runtime.panes_changed(PaneUpdate {
            tab_panes: HashMap::new(),
            live: HashSet::new(),
            theme: None,
            exits: Vec::new(),
        });
    }
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

    let out = runtime.control(Verb::AttentionNext);
    assert_eq!(out.effects, vec![Effect::SwitchTab { position: 1 }]);
}

#[test]
fn command_is_inert_without_permission() {
    let mut runtime = PluginRuntime { config: config(), ..Default::default() };
    runtime.tabs_changed(vec![tab(0, "a", true), tab(1, "b", false)]);
    runtime.radar.set_tab_panes_for_position(1, vec![pane(11)]);
    runtime.radar.status_mut().apply(payload_for(11, Status::Pending), 1, 0);

    assert_eq!(runtime.control(Verb::AttentionNext), Outcome::default());
}

#[test]
fn command_no_op_when_no_attention() {
    let mut runtime = PluginRuntime {
        permission: PermissionState::Resolved { granted: true },
        config: config(),
        ..Default::default()
    };
    runtime.tabs_changed(vec![tab(0, "a", true)]);
    assert_eq!(runtime.control(Verb::AttentionNext), Outcome::default());
}

#[test]
fn control_pipe_unknown_verb_is_no_op() {
    let mut runtime = PluginRuntime {
        permission: PermissionState::Resolved { granted: true },
        config: config(),
        ..Default::default()
    };
    assert_eq!(runtime.control_pipe("attention-top"), Outcome::default());
    assert_eq!(runtime.control_pipe(""), Outcome::default());
}

// ── Cross-session presence + cycling ───────────────────────────────────────

/// A granted runtime whose own session name is already known (as it would be
/// in production once Zellij's first `ModeUpdate` lands) — needed for the
/// presence edge in `project` to ever fire.
fn runtime_with_granted_permission() -> PluginRuntime {
    let mut rt = PluginRuntime {
        permission: PermissionState::Resolved { granted: true },
        config: config(),
        ..Default::default()
    };
    rt.session_name_changed(Some("work".into()));
    rt
}

/// One tab (position 0), one tracked pane (id 7) — the minimal topology a
/// status edge needs to land on a row `presence_json` can see.
fn drive_tabs_and_panes(rt: &mut PluginRuntime) {
    rt.tabs_changed(vec![tab(0, "a", true)]);
    rt.radar.set_tab_panes_for_position(0, vec![pane(7)]);
}

/// A `zj_radar.status.v1` wire payload for `pane_id` at `status` ("running" /
/// "pending" / etc.) — `payload_for` takes the typed `Status`, so this bridges
/// from the wire vocabulary the way a real producer would send it.
fn payload_json(pane_id: u32, status: &str) -> String {
    payload::to_wire(&payload_for(pane_id, Status::from_wire(status)))
}

/// A peer presence JSON literal paired with a fresh (age-0) mtime — the
/// shape `presences_changed` now takes (`session_files::read_peer_presences`'s
/// `(json, age_secs)` pairing). Most tests want a live-looking peer; use the
/// tuple literal directly when a test needs to exercise staleness.
fn fresh(json: &str) -> (String, u64) {
    (json.to_string(), 0)
}

#[test]
fn status_edge_persists_presence_once_and_not_on_identical_state() {
    let mut rt = runtime_with_granted_permission();
    drive_tabs_and_panes(&mut rt);

    let out = rt.status_pipe(&payload_json(7, "running"));
    assert!(out.effects.contains(&Effect::PersistPresence), "running-count edge publishes presence, got {:?}", out.effects);

    let again = rt.status_pipe(&payload_json(7, "running"));
    assert!(!again.effects.contains(&Effect::PersistPresence), "identical state does not re-publish, got {:?}", again.effects);
}

#[test]
fn presence_edge_ignores_timestamp_but_still_reacts_to_content() {
    // Finding 1 pin: `presence_json` embeds `updated_epoch_s`, which on Fast
    // cadence moves every tick. If `project`'s change check compared the raw
    // JSON (epoch included), advancing the epoch alone — with unchanged
    // counts — would look like an edge and re-fire `PersistPresence` every
    // second. It must not: only a real content change (running/attention/
    // attention_tab_position) counts, mirroring `sessions.rs::set_own`'s
    // badge-derived check, which already excludes `updated_epoch_s`.
    let mut rt = runtime_with_granted_permission();
    drive_tabs_and_panes(&mut rt);

    let first = rt.status_pipe(&payload_json(7, "running"));
    assert!(first.effects.contains(&Effect::PersistPresence), "setup: first edge publishes, got {:?}", first.effects);

    // Drive `project` directly (as `project_emits_effects_in_canonical_order`
    // does) with a no-op domain change but a strictly advancing epoch each
    // call — the seam that lets this test move "now" without sleeping.
    let noop = RadarChange::default();
    let same_epoch_advanced_once = rt.project(vec![], noop.clone(), 1_000);
    assert!(
        !same_epoch_advanced_once.effects.contains(&Effect::PersistPresence),
        "epoch-only change (unchanged counts) must not republish, got {:?}",
        same_epoch_advanced_once.effects
    );
    let same_epoch_advanced_again = rt.project(vec![], noop, 2_000);
    assert!(
        !same_epoch_advanced_again.effects.contains(&Effect::PersistPresence),
        "epoch-only change (unchanged counts, again) must not republish, got {:?}",
        same_epoch_advanced_again.effects
    );

    // A real content edge (attention 0 -> 1) evaluated at the SAME epoch as
    // the call just above still publishes — the exclusion is timestamp-only,
    // not a blanket suppression of `PersistPresence`.
    rt.radar.status_mut().apply(payload_for(7, Status::Pending), rt.tick, 2_000);
    let content_edge = rt.project(vec![], RadarChange::default(), 2_000);
    assert!(
        content_edge.effects.contains(&Effect::PersistPresence),
        "a counts change at an unchanged epoch must still publish, got {:?}",
        content_edge.effects
    );
}

#[test]
fn presence_withheld_until_own_session_name_is_known() {
    // Same status edge as above, but WITHOUT `session_name_changed` ever landing —
    // an unnamed presence file is useless to peers, so `project` must not
    // emit `PersistPresence` no matter what the own counts do.
    let mut rt = PluginRuntime {
        permission: PermissionState::Resolved { granted: true },
        config: config(),
        ..Default::default()
    };
    drive_tabs_and_panes(&mut rt);

    let out = rt.status_pipe(&payload_json(7, "running"));
    assert!(!out.effects.contains(&Effect::PersistPresence), "no session name yet, got {:?}", out.effects);
}

#[test]
fn session_cycle_commits_via_switch_session_effect_on_idle_tick() {
    let mut rt = runtime_with_granted_permission(); // own session name is "work"
    // "alpha" needs no separate liveness registration anymore — its presence
    // report IS its liveness (task-8b-brief.md: no more `SessionUpdate` peer
    // list to cross-check against).
    rt.presences_changed(vec![
        fresh(r#"{"session_name":"alpha","running":0,"attention":1,"attention_tab_position":1}"#),
    ]);

    let out = rt.control_pipe("session-next");
    assert!(out.render, "selection highlight renders");

    // The Fast fire covering the tap itself must not commit (task-14: the
    // taps-since-last-fire flag resets the deadline instead) — only the
    // NEXT, fully quiet fire may.
    let covering = rt.timer_fast(PermissionProbe::default());
    assert!(
        !covering.effects.iter().any(|e| matches!(e, Effect::SwitchSession { .. })),
        "the fire covering the tap must not commit, got {:?}",
        covering.effects
    );
    let tick = rt.timer_fast(PermissionProbe::default()); // the next, quiet Fast tick
    assert!(
        tick.effects.iter().any(|e| matches!(
            e, Effect::SwitchSession { name, tab_position: Some(1) } if name == "alpha"
        )),
        "idle tick commits the pending selection, got {:?}",
        tick.effects
    );
}

#[test]
fn session_cycle_is_inert_without_permission() {
    let mut rt = PluginRuntime::default();
    rt.session_name_changed(Some("work".into()));
    rt.presences_changed(vec![fresh(r#"{"session_name":"alpha","running":0,"attention":0}"#)]);
    assert_eq!(rt.control_pipe("session-next"), Outcome::default());
}

#[test]
fn session_cycle_arms_fast_cadence_for_the_idle_commit() {
    let mut rt = runtime_with_granted_permission();
    rt.presences_changed(vec![fresh(r#"{"session_name":"alpha","running":0,"attention":0}"#)]);
    let out = rt.control_pipe("session-next");
    assert!(
        out.effects.contains(&Effect::SetTimeout(Cadence::Fast)),
        "a pending cycle selection must arm Fast so the idle-commit fires promptly, got {:?}",
        out.effects
    );
}

#[test]
fn clicking_a_session_line_emits_switch_session() {
    // Two live sessions (own "work" + peer "alpha") put two badge lines
    // between the header and the first tab card (`render::render_session_badge`,
    // wired into the body in Task 7). Mirrors
    // `render_records_targets_and_mouse_click_returns_host_effect`'s line-index
    // bookkeeping: Compact density + header:true is a 2-line header (title,
    // rule — "line 2 = tab header" there), so with the badge inserted here:
    // line 0 = title, 1 = rule, 2 = own "work" badge line (click-inert, no
    // cross-session target), 3 = peer "alpha" badge line (clickable).
    let mut rt = runtime_with_granted_permission(); // own session name "work"
    rt.tabs_changed(vec![tab(0, "team", false)]);
    rt.presences_changed(vec![
        fresh(r#"{"session_name":"alpha","running":0,"attention":1,"attention_tab_position":2}"#),
    ]);

    let ansi = rt.render(100, 80);
    assert!(ansi.contains("alpha"), "setup: the peer's badge line must actually render");

    let own_click = rt.mouse_click(2);
    assert_eq!(
        own_click,
        Outcome::default(),
        "the own-session badge line has no click target, got {:?}",
        own_click
    );

    let peer_click = rt.mouse_click(3);
    assert_eq!(
        peer_click.effects,
        vec![Effect::SwitchSession { name: "alpha".into(), tab_position: Some(2) }]
    );
}

#[test]
fn own_badge_row_updates_live_as_running_and_attention_move() {
    // Task-6 flagged `Sessions::set_own` as dead code: nothing called it, so
    // the own row of the cross-session badge never reflected the running/
    // attention counts actually moving underneath it — it would render
    // whatever it started at (0/0) forever. task-8b-brief.md revives it by
    // having `project` call it every pass once the name is known; this pins
    // that the own row's rendered counts actually track a later status edge,
    // not just its state at the moment the name became known.
    let mut rt = runtime_with_granted_permission(); // own session name "work"
    // A second session must exist for the badge to render at all
    // (`render_session_badge` renders zero lines for `entries.len() <= 1`).
    rt.presences_changed(vec![fresh(r#"{"session_name":"alpha","running":0,"attention":0}"#)]);
    drive_tabs_and_panes(&mut rt); // tab 0, pane 7

    let before = rt.render(100, 80);
    assert!(!before.contains("work 1"), "setup: own row must start at zero running, got {before:?}");

    rt.status_pipe(&payload_json(7, "running"));
    let after = rt.render(100, 80);
    let running_glyph = Status::Running.glyph_for(GlyphSet::Plain);
    assert!(
        after.contains(&format!("work 1{running_glyph}")),
        "own badge row must show the fresh running count, got {after:?}"
    );
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
    rt.radar.command_mut().on_exit(7, Some(0), Tick(rt.tick), EpochSecs(0));
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
    let outcome = rt.project(vec![], change, 0);

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
    seeded.command_mut().on_exit(7, Some(0), Tick(1), EpochSecs(0));
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

    let out = runtime.control(Verb::AttentionPrev);
    assert_eq!(out.effects, vec![Effect::SwitchTab { position: 2 }]);
}

#[test]
fn control_pipe_dispatches_known_verb() {
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
    let out = runtime.control_pipe("attention-next");
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
    // The battery property's full-disarm pin — deliberately PRE-NAME (no
    // `session_name_changed` ever lands, so `own_session_name` stays at its
    // Default empty). Once a name is known the presence-liveness heartbeat
    // keeps Slow armed instead — see the sibling test below.
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
    assert_eq!(
        rt.desired_cadence(crate::clock::now_epoch_s()),
        None,
        "a saturated ledger has nothing left worth ticking for"
    );

    let outcome = rt.tabs_changed(vec![]);
    assert!(
        !outcome.effects.iter().any(|e| matches!(e, Effect::SetTimeout(_))),
        "a fully-saturated idle rail must not arm any timer, got {:?}",
        outcome.effects
    );
    assert!(rt.timer_chain.armed().is_none());
}

#[test]
fn saturated_history_with_known_name_keeps_slow_armed_for_the_heartbeat() {
    // Sibling of `saturated_history_fully_disarms`, adding the one
    // ingredient that test leaves out: a learned own-session name. Once the
    // name is known this session owns a presence file, and that file's
    // mtime is the signal peers read to tell fresh from stale
    // (`sessions::STALE_AFTER_SECS`) — refreshed solely by `timer`'s
    // Slow-fire heartbeat. Were the chain to fully disarm on a saturated
    // idle rail, that heartbeat would never fire again, the mtime would
    // freeze, and after 90s every peer would dim this still-alive session's
    // badge to stale (never drop it — task-14 — but still a needless false
    // alarm) — exactly the idle-but-visible case the feature exists for.
    // So: saturation may step the cadence down to Slow, but never to None
    // while the name is known.
    let mut rt = PluginRuntime {
        permission: PermissionState::Resolved { granted: true },
        config: config(),
        ..Default::default()
    };
    rt.radar.ledger_mut().push(crate::ledger::LedgerEntry {
        at_epoch_s: 0,
        outcome: crate::ledger::LedgerOutcome::Done,
        tab_id: TabId::new(1),
        tab_name: "work".into(),
        label: "cargo test".into(),
        pane_id: 5,
    });
    // Seed `last_now_epoch_s` with a real wall-clock capture (any entry
    // point that owns an epoch does) BEFORE the name lands, so the arm
    // decision inside `session_name_changed`'s project pass evaluates at
    // true "now" — where the 0-stamped ledger entry above reads as
    // saturated — and not at the Default epoch 0, where that entry would
    // still look fresh and arm Slow for the wrong reason.
    let nameless = rt.tabs_changed(vec![]);
    assert!(
        !nameless.effects.iter().any(|e| matches!(e, Effect::SetTimeout(_))),
        "setup: nameless + saturated must stay fully disarmed, got {:?}",
        nameless.effects
    );

    let named = rt.session_name_changed(Some("work".into()));
    assert!(
        named.effects.contains(&Effect::SetTimeout(Cadence::Slow)),
        "learning the name must arm the Slow heartbeat, got {:?}",
        named.effects
    );
    assert_eq!(rt.timer_chain.armed(), Some(Cadence::Slow));

    // The Slow fire must refresh the presence mtime AND re-arm itself —
    // the self-sustaining loop that keeps an idle session inside the TTL.
    let slow = rt.timer(PermissionProbe::default(), Cadence::Slow.seconds());
    assert!(
        slow.effects.contains(&Effect::PersistPresence),
        "the heartbeat refreshes the presence file, got {:?}", slow.effects
    );
    assert!(
        slow.effects.contains(&Effect::SetTimeout(Cadence::Slow)),
        "the heartbeat chain re-arms itself, got {:?}", slow.effects
    );
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

#[test]
fn idle_alive_session_heartbeats_presence_unconditionally_on_slow_fires_only() {
    // An idle-but-alive session (nothing fast-worthy happening) has no
    // content edge to trigger `project`'s compare-and-cache `PersistPresence`
    // — but its presence file's mtime is the signal peers use to tell fresh
    // from stale (`sessions::STALE_AFTER_SECS`; liveness is no longer
    // `SessionUpdate`-derived at all, and past task-14 it's never a hard
    // drop either — just a dim). The Slow (60s) heartbeat must therefore
    // emit `PersistPresence` unconditionally, bypassing the content-compare
    // gate; Fast (1s) fires must NOT — that would be needless per-second
    // churn for a signal that only needs to beat a 90s staleness window.
    let mut rt = runtime_with_granted_permission(); // own session name is "work"

    // A Fast fire with nothing changed must stay quiet — the edge gate
    // already published once, inside `runtime_with_granted_permission`'s
    // `session_name_changed` call.
    let fast = rt.timer(PermissionProbe::default(), Cadence::Fast.seconds());
    assert!(
        !fast.effects.contains(&Effect::PersistPresence),
        "a fast fire with unchanged content must not republish, got {:?}", fast.effects
    );

    // A Slow fire, even with identical content, must heartbeat anyway —
    // exactly once, not doubled up with `project`'s own (correctly
    // no-op, since content is unchanged) edge-gated push.
    let slow = rt.timer(PermissionProbe::default(), Cadence::Slow.seconds());
    let persists = slow.effects.iter().filter(|e| matches!(e, Effect::PersistPresence)).count();
    assert_eq!(
        persists, 1,
        "an idle session's slow heartbeat must refresh its presence file's \
         mtime unconditionally, exactly once, got {:?}", slow.effects
    );
}

#[test]
fn slow_heartbeat_coincident_with_a_genuine_presence_edge_persists_exactly_once() {
    // Sibling of `idle_alive_session_heartbeats_presence_unconditionally_
    // on_slow_fires_only`, but for the case that test's own comment waves
    // off as "correctly no-op": here the Slow fire's OWN tick is what
    // produces the content edge, not an unrelated prior call. A live Slow
    // fire that promotes a debounced command to Running crosses `project`'s
    // edge gate (running 0 -> 1) on the exact same pass where `timer`'s
    // unconditional Slow heartbeat has already seeded a `PersistPresence`
    // — two independently-correct pushes landing in the same `fx`, which
    // must still collapse to one effect.
    let mut rt = runtime_with_granted_permission(); // own session name is "work"
    drive_tabs_and_panes(&mut rt); // tab 0 / pane 7, the row `own_presence` reads

    rt.command_changed(7, &["cargo".into(), "test".into()], true); // pending, not yet Running

    // The named-idle heartbeat pre-armed Slow inside the helper's
    // `session_name_changed`, so `command_changed`'s Fast arm above was a
    // Slow→Fast top-up: TWO fires are now in flight, and the probe below
    // needs its Slow fire to land as the LIVE chain. Retire the stale
    // slow-armed fire first (the "rare order" of
    // `stale_slow_fire_landing_first_is_swallowed`): swallowed whole, so it
    // doesn't advance the debounce tick count either.
    let stale = rt.timer(PermissionProbe::default(), Cadence::Slow.seconds());
    assert_eq!(stale, Outcome::none(), "setup: the pre-armed slow fire is stale and swallowed");

    // Ticks short of the debounce window: quiet, and don't disturb the fire
    // count `timer`'s final Slow fire below needs to land as the live chain.
    for _ in 1..DEBOUNCE_TICKS {
        rt.timer_fast(PermissionProbe::default());
    }

    // The debounce-completing tick, reported as a Slow (60s) fire — the
    // reviewer's exact probe: a live Slow fire whose own tick promotes
    // pending -> Running, landing a genuine content edge.
    let tick = rt.timer(PermissionProbe::default(), Cadence::Slow.seconds());
    let persists = tick.effects.iter().filter(|e| matches!(e, Effect::PersistPresence)).count();
    assert_eq!(
        persists, 1,
        "a Slow heartbeat coinciding with a real content edge must still \
         publish exactly once, got {:?}", tick.effects
    );
}

#[test]
fn read_presences_is_bound_to_fast_fires_only() {
    // Finding 2 pin: the brief bounds `Effect::ReadPresences` to "one
    // directory scan per second, only while Fast is armed" — it must not
    // ride along on the Slow (60s) heartbeat, which exists purely to repaint
    // ledger ages. `timer` tells Fast from Slow the same way `TimerChain::
    // on_fire` tells live from stale: by `elapsed_s` against
    // `STALE_FIRE_ELAPSED_S`.
    let mut rt = PluginRuntime {
        permission: PermissionState::Resolved { granted: true },
        config: config(),
        ..Default::default()
    };

    let fast = rt.timer(PermissionProbe::default(), Cadence::Fast.seconds());
    assert!(fast.effects.contains(&Effect::ReadPresences), "a Fast fire must scan peers, got {:?}", fast.effects);

    // A lone Slow fire (nothing else in flight, so it's the live chain, not
    // a stale leftover — see `lone_slow_fire_processes_as_the_live_chain`)
    // must not.
    let mut rt = PluginRuntime {
        permission: PermissionState::Resolved { granted: true },
        config: config(),
        ..Default::default()
    };
    let slow = rt.timer(PermissionProbe::default(), Cadence::Slow.seconds());
    assert!(!slow.effects.contains(&Effect::ReadPresences), "a Slow fire must not scan peers, got {:?}", slow.effects);
}

// ── Fast-decay heartbeat survival (task-13 investigation) ───────────────
//
// Field reports: sessions that had Fast-cadence activity and then decayed
// to idle stopped heartbeating (their presence file's mtime froze), while
// a session idle from birth (never armed Fast) kept heartbeating forever.
// Suspected mechanism: the Fast→Slow decay stranding `TimerChain` — the
// original stale Slow arm from before the top-up lands, gets swallowed as
// stale (`TimerChain::on_fire`'s elapsed+pending check), and the chain
// never re-arms. The tests below drive `PluginRuntime::timer` through a
// real fire-order simulation (`FireSim`, not hand-picked `elapsed_s`
// values) across the exact interleaving, a long sustained-busy variant, a
// flapping-activity variant, and a property-fuzzed search over thousands
// of random event/fire orderings (`timer_chain_fuzz` below) — all confirm
// the *current* `arm`/`on_fire` bookkeeping self-corrects: whichever fire
// is chronologically last to land is always the one treated as live, and
// it always re-arms (see task-13-report.md for the full trace). Kept as
// permanent regression coverage for this exact hypothesis, not as a
// reproduction of a confirmed bug.

/// A one-shot, non-cancellable `set_timeout` queue, exactly like Zellij's:
/// every `Effect::SetTimeout` schedules a real future fire at `now + dur`,
/// and fires are delivered in absolute-time order (never reordered) —
/// mirroring lib.rs's `set_timeout(cadence.seconds())` and Zellij's own
/// single-threaded event loop. Used to drive `PluginRuntime::timer` the way
/// production actually does, instead of hand-picking `elapsed_s` values.
struct FireSim {
    now_ms: u64,
    queue: std::collections::BinaryHeap<std::cmp::Reverse<(u64, u64)>>,
}
impl FireSim {
    fn new() -> Self {
        Self { now_ms: 0, queue: Default::default() }
    }
    fn schedule_from(&mut self, effects: &[Effect]) {
        for e in effects {
            if let Effect::SetTimeout(c) = e {
                let dur_ms = (c.seconds() * 1000.0).round() as u64;
                self.queue.push(std::cmp::Reverse((self.now_ms + dur_ms, dur_ms)));
            }
        }
    }
    /// Pop the earliest scheduled fire and report it as `timer()` wants:
    /// `elapsed_s` = the duration it was armed with (matches lib.rs's
    /// documented approximation).
    fn pop(&mut self) -> Option<f64> {
        let std::cmp::Reverse((at, dur_ms)) = self.queue.pop()?;
        self.now_ms = at;
        Some(dur_ms as f64 / 1000.0)
    }
}

#[test]
fn slow_heartbeat_survives_a_fast_decay_indefinitely() {
    // The exact suspected interleaving: Slow-armed idle, a Fast top-up,
    // several Fast fires driving real work, activity ends, and the
    // ORIGINAL stale Slow arm lands somewhere in the stream. Run it out
    // past 15 minutes of virtual time and assert every live Slow fire
    // still re-arms and still heartbeats.
    let mut rt = PluginRuntime {
        permission: PermissionState::Resolved { granted: true },
        config: config(),
        ..Default::default()
    };
    let mut sim = FireSim::new();

    // name-known: arms Slow immediately (idle from birth so far).
    let named = rt.session_name_changed(Some("work".into()));
    sim.schedule_from(&named.effects);
    drive_tabs_and_panes(&mut rt);

    // busy: a running command tops up Fast on top of the still-outstanding
    // Slow arm — exactly the "TWO in flight" case 24968b1's tests pinned.
    let busy = rt.status_pipe(&payload_json(7, "running"));
    sim.schedule_from(&busy.effects);
    assert_eq!(rt.timer_chain.armed(), Some(Cadence::Fast), "setup: busy tops up Fast");

    // Drive real fires for a few seconds of Fast-cadence "work", then end
    // the activity (status -> done, which settles and lets the row go
    // idle), continuing to drain the FireSim exactly as Zellij would
    // deliver it — including the original stale Slow fire, whenever in
    // this stream it actually lands.
    let mut ended_activity = false;
    let mut stale_swallows = 0;
    for i in 0.. {
        let Some(elapsed) = sim.pop() else { break };
        let out = rt.timer(PermissionProbe::default(), elapsed);
        if out == Outcome::none() {
            stale_swallows += 1;
        }
        sim.schedule_from(&out.effects);

        if i == 3 && !ended_activity {
            ended_activity = true;
            let done = rt.status_pipe(&payload_json(7, "done"));
            sim.schedule_from(&done.effects);
        }
        // Stop once we've drained past the point business has settled AND
        // the chain has decayed back to Slow-only (no Fast desire left).
        if ended_activity && rt.timer_chain.armed() == Some(Cadence::Slow) && sim.queue.len() <= 1 {
            break;
        }
        if i > 500 {
            panic!("setup: never settled back to a single Slow chain");
        }
    }
    assert!(
        stale_swallows >= 1,
        "setup: the original Slow arm must actually land and get swallowed as \
         stale during this drain — otherwise this test never exercises the \
         'two in flight' interleaving at all"
    );

    // Now simulate 15 minutes of virtual time purely via the FireSim queue
    // — no more domain events, exactly an idle-but-alive session. Every
    // live fire must be a Slow fire that (a) re-arms and (b) heartbeats.
    let mut live_fires = 0;
    while sim.now_ms < 15 * 60 * 1000 {
        let Some(elapsed) = sim.pop() else {
            panic!(
                "TimerChain starved: nothing scheduled at virtual t={:.1}s \
                 (armed={:?}) — the chain believes itself armed with no \
                 timeout outstanding",
                sim.now_ms as f64 / 1000.0,
                rt.timer_chain.armed(),
            );
        };
        let out = rt.timer(PermissionProbe::default(), elapsed);
        sim.schedule_from(&out.effects);

        if out == Outcome::none() {
            continue; // correctly-swallowed stale fire
        }
        live_fires += 1;
        assert!(
            elapsed > STALE_FIRE_ELAPSED_S,
            "expected only Slow fires once idle, got elapsed={elapsed} at t={:.1}s",
            sim.now_ms as f64 / 1000.0
        );
        assert!(
            out.effects.contains(&Effect::PersistPresence),
            "live Slow fire at t={:.1}s must heartbeat, got {:?}",
            sim.now_ms as f64 / 1000.0,
            out.effects
        );
        assert!(
            out.effects.contains(&Effect::SetTimeout(Cadence::Slow)),
            "live Slow fire at t={:.1}s must re-arm, got {:?}",
            sim.now_ms as f64 / 1000.0,
            out.effects
        );
    }
    assert!(
        live_fires >= 10,
        "expected at least 10 live Slow heartbeats over 15 virtual minutes, got {live_fires}"
    );
}

/// Drain `sim`/`rt` until the chain is Slow-only-armed with nothing else
/// outstanding, feeding every fire (live or stale) back through `timer`.
/// Panics past a generous bound instead of looping forever on a genuine
/// stall, so a hang shows up as a normal test failure.
fn drain_to_settled_slow(rt: &mut PluginRuntime, sim: &mut FireSim) {
    for _ in 0..2000 {
        if rt.timer_chain.armed() == Some(Cadence::Slow) && sim.queue.len() <= 1 {
            return;
        }
        let Some(elapsed) = sim.pop() else { panic!("starved before settling to Slow") };
        let out = rt.timer(PermissionProbe::default(), elapsed);
        sim.schedule_from(&out.effects);
    }
    panic!("never settled back to a single Slow chain");
}

/// Run `sim`/`rt` forward for `minutes` of pure idle virtual time (no more
/// domain events), asserting every live fire is a heartbeating Slow fire.
/// Returns the live-fire count.
fn assert_heartbeats_for(rt: &mut PluginRuntime, sim: &mut FireSim, minutes: u64) -> u64 {
    let deadline_ms = sim.now_ms + minutes * 60_000;
    let mut live_fires = 0;
    while sim.now_ms < deadline_ms {
        let Some(elapsed) = sim.pop() else {
            panic!(
                "TimerChain starved at virtual t={:.1}s (armed={:?})",
                sim.now_ms as f64 / 1000.0,
                rt.timer_chain.armed(),
            );
        };
        let out = rt.timer(PermissionProbe::default(), elapsed);
        sim.schedule_from(&out.effects);
        if out == Outcome::none() {
            continue;
        }
        live_fires += 1;
        assert!(
            out.effects.contains(&Effect::PersistPresence),
            "live fire at t={:.1}s must heartbeat, got {:?}",
            sim.now_ms as f64 / 1000.0,
            out.effects
        );
    }
    live_fires
}

#[test]
fn slow_heartbeat_survives_a_long_sustained_busy_period() {
    // Variant where the original stale Slow fire's 60s deadline actually
    // elapses WHILE Fast activity is still ongoing (not right at the
    // boundary), so it gets swallowed mid-burst rather than right at the
    // busy/idle seam.
    let mut rt = PluginRuntime { permission: PermissionState::Resolved { granted: true }, config: config(), ..Default::default() };
    let mut sim = FireSim::new();
    let named = rt.session_name_changed(Some("work".into()));
    sim.schedule_from(&named.effects);
    drive_tabs_and_panes(&mut rt);

    let busy = rt.status_pipe(&payload_json(7, "running"));
    sim.schedule_from(&busy.effects);

    // Sustain Fast for 200 virtual seconds (>> the 60s Slow deadline) of
    // pure timer-driven work — no further domain events.
    for _ in 0..200 {
        let Some(elapsed) = sim.pop() else { panic!("starved mid-burst") };
        let out = rt.timer(PermissionProbe::default(), elapsed);
        sim.schedule_from(&out.effects);
    }
    assert_eq!(rt.timer_chain.armed(), Some(Cadence::Fast), "setup: still busy at t=200s");

    let done = rt.status_pipe(&payload_json(7, "done"));
    sim.schedule_from(&done.effects);
    drain_to_settled_slow(&mut rt, &mut sim);

    let live_fires = assert_heartbeats_for(&mut rt, &mut sim, 15);
    assert!(live_fires >= 10, "expected >=10 heartbeats, got {live_fires}");
}

#[test]
fn slow_heartbeat_survives_flapping_activity_then_settling() {
    // The "actively-used agent" shape: many short bursts with brief idle
    // gaps in between (too short for even one Slow fire to land), each one
    // a fresh Slow->Fast top-up stacking a fresh stale-Slow-to-be behind
    // the last — then a final, permanent idle settle.
    let mut rt = PluginRuntime { permission: PermissionState::Resolved { granted: true }, config: config(), ..Default::default() };
    let mut sim = FireSim::new();
    let named = rt.session_name_changed(Some("work".into()));
    sim.schedule_from(&named.effects);
    drive_tabs_and_panes(&mut rt);

    for cycle in 0..40 {
        let busy = rt.status_pipe(&payload_json(7, "running"));
        sim.schedule_from(&busy.effects);
        for _ in 0..3 {
            let Some(elapsed) = sim.pop() else { panic!("starved in flap cycle {cycle}") };
            let out = rt.timer(PermissionProbe::default(), elapsed);
            sim.schedule_from(&out.effects);
        }
        let done = rt.status_pipe(&payload_json(7, "done"));
        sim.schedule_from(&done.effects);
        // A couple settle ticks, then straight into the next burst — no
        // time for the chain to fully decay to a lone Slow.
        for _ in 0..2 {
            let Some(elapsed) = sim.pop() else { panic!("starved settling flap cycle {cycle}") };
            let out = rt.timer(PermissionProbe::default(), elapsed);
            sim.schedule_from(&out.effects);
        }
    }

    drain_to_settled_slow(&mut rt, &mut sim);
    let live_fires = assert_heartbeats_for(&mut rt, &mut sim, 15);
    assert!(live_fires >= 10, "expected >=10 heartbeats after flapping settled, got {live_fires}");
}

mod timer_chain_fuzz {
    use super::*;
    use proptest::prelude::*;

    #[derive(Clone, Debug)]
    enum Step {
        /// Pop and deliver the earliest scheduled fire (real FIFO order).
        Fire,
        /// A domain edge that can flip `has_running_work` — the real trigger
        /// for a Slow<->Fast cadence transition.
        SetRunning(bool),
        /// The cross-session cycle's own direct `arm_timer_if_needed` call
        /// site (`control()`), which bypasses `project()` entirely.
        SessionCycle,
    }

    fn arb_step() -> impl Strategy<Value = Step> {
        prop_oneof![
            4 => Just(Step::Fire),
            1 => any::<bool>().prop_map(Step::SetRunning),
            1 => Just(Step::SessionCycle),
        ]
    }

    proptest! {
        #![proptest_config(ProptestConfig::with_cases(2000))]
        #[test]
        fn timer_chain_never_starves_a_named_granted_session(
            steps in proptest::collection::vec(arb_step(), 0..300)
        ) {
            let mut rt = PluginRuntime {
                permission: PermissionState::Resolved { granted: true },
                config: config(),
                ..Default::default()
            };
            let mut sim = FireSim::new();
            let named = rt.session_name_changed(Some("work".into()));
            sim.schedule_from(&named.effects);
            drive_tabs_and_panes(&mut rt);

            for step in steps {
                match step {
                    Step::Fire => {
                        if let Some(elapsed) = sim.pop() {
                            let out = rt.timer(PermissionProbe::default(), elapsed);
                            sim.schedule_from(&out.effects);
                        }
                    }
                    Step::SetRunning(running) => {
                        let status = if running { "running" } else { "done" };
                        let out = rt.status_pipe(&payload_json(7, status));
                        sim.schedule_from(&out.effects);
                    }
                    Step::SessionCycle => {
                        let out = rt.control(Verb::SessionNext);
                        sim.schedule_from(&out.effects);
                    }
                }
                // The invariant: a named, permission-granted session's
                // `desired_cadence` is NEVER `None` (barring denial, which
                // never happens on this path) — so the moment nothing is
                // physically outstanding, the chain has permanently
                // stranded itself; no future event will ever revive it,
                // because nothing left to arm/on_fire from.
                prop_assert!(
                    !sim.queue.is_empty(),
                    "TimerChain starved: no physical timer outstanding \
                     (armed={:?}) after step, session still named+granted",
                    rt.timer_chain.armed(),
                );
            }

            // Stronger post-condition, independent of the starve check
            // above: after the random churn stops, force any dangling
            // `status_pipe`-origin busy edge closed (a *still-running*
            // agent legitimately keeps Fast armed forever — that's correct,
            // not a bug, so it must not be mistaken for one here) and let
            // no more domain events land. A generous drain (well past
            // DONE_TTL_TICKS=60 and any plausible stacked-stale-fire
            // backlog) must still reach a genuine Slow heartbeat —
            // catching a chain that's stuck "armed" on paper but only ever
            // fires (or only ever gets swallowed as stale) without ever
            // landing a live, presence-persisting Slow fire again.
            let close = rt.status_pipe(&payload_json(7, "done"));
            sim.schedule_from(&close.effects);
            let mut settled = false;
            for _ in 0..5000 {
                let Some(elapsed) = sim.pop() else { break };
                let out = rt.timer(PermissionProbe::default(), elapsed);
                sim.schedule_from(&out.effects);
                if elapsed > STALE_FIRE_ELAPSED_S && out.effects.contains(&Effect::PersistPresence) {
                    settled = true;
                    break;
                }
            }
            prop_assert!(
                settled,
                "no live Slow heartbeat landed within 5000 drained fires after churn stopped \
                 (armed={:?}, queue_len={})",
                rt.timer_chain.armed(),
                sim.queue.len(),
            );
        }
    }
}

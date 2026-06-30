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

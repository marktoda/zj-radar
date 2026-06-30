use super::*;
use crate::payload::StatusPayload;
use crate::rollup::Outcome;

fn tab(id: usize, position: usize, name: &str, active: bool) -> RadarTab {
    RadarTab {
        id: TabId::new(id),
        position,
        name: name.into(),
        active,
        has_bell: false,
    }
}

fn pane(id: u32) -> TerminalPane {
    TerminalPane {
        id,
        ..TerminalPane::default()
    }
}

fn focused_pane(id: u32) -> TerminalPane {
    TerminalPane {
        id,
        focused_in_tab: true,
        ..TerminalPane::default()
    }
}

fn payload_for(pane_id: u32, status: Status, repo: &str) -> StatusPayload {
    StatusPayload {
        pane_id,
        status,
        repo: repo.into(),
        branch: "main".into(),
        msg: "working".into(),
        on_focus: None,
        source: "claude".into(),
    }
}

#[test]
fn rows_sort_tabs_by_position_and_aggregate_panes() {
    let mut radar = RadarState::default();
    radar.tabs_changed(vec![
        tab(30, 2, "c", false),
        tab(10, 0, "a", true),
        tab(20, 1, "b", false),
    ]);
    radar.set_tab_panes_for_position(0, vec![pane(42)]);
    radar
        .status_mut()
        .apply(payload_for(42, Status::Running, "repo"), 1);

    let rows = radar.rows();

    assert_eq!(
        rows.iter().map(|r| r.name.as_str()).collect::<Vec<_>>(),
        vec!["a", "b", "c"]
    );
    assert_eq!(rows[0].display.status, Status::Running);
}

#[test]
fn active_tab_focus_is_the_only_global_focus_transition() {
    let mut radar = RadarState::default();
    radar.tabs_changed(vec![tab(10, 0, "left", false), tab(20, 1, "right", true)]);
    let update = PaneUpdate {
        tab_panes: HashMap::from([(0, vec![focused_pane(1)]), (1, vec![focused_pane(2)])]),
        live: HashSet::from([1, 2]),
        theme: None,
        exits: Vec::new(),
    };

    radar.panes_changed(update, 7, config::NamingMode::Off);

    assert_eq!(radar.last_focused(), Some(2));
}

#[test]
fn rename_ownership_follows_stable_tab_id_across_reorder() {
    let mut radar = RadarState::default();
    radar.tabs_changed(vec![
        tab(10, 0, "Tab #1", true),
        tab(20, 1, "custom", false),
    ]);
    radar.set_tab_panes_for_position(0, vec![focused_pane(1)]);
    let rename = radar.cwd_changed(1, "/work/alpha".into(), config::NamingMode::Managed);
    assert_eq!(
        rename.renames,
        vec![TabRename {
            position: 0,
            name: "alpha".into(),
        }]
    );
    assert_eq!(radar.applied_name(TabId::new(10)), Some("alpha"));

    radar.tabs_changed(vec![tab(20, 0, "custom", false), tab(10, 1, "alpha", true)]);
    radar.set_tab_panes_for_position(1, vec![focused_pane(1)]);
    let rename = radar.cwd_changed(1, "/work/beta".into(), config::NamingMode::Managed);

    assert_eq!(
        rename.renames,
        vec![TabRename {
            position: 1,
            name: "beta".into(),
        }]
    );
    assert_eq!(radar.applied_name(TabId::new(10)), Some("beta"));
}

#[test]
fn observation_origin_is_source_specific() {
    let mut radar = RadarState::default();
    radar
        .status_mut()
        .apply(payload_for(1, Status::Running, "status"), 1);
    radar.command_changed(2, &["cargo".into(), "test".into()], true, 1);
    radar.timer(2);

    assert_eq!(
        radar.status(1).unwrap().origin,
        crate::observation::ObservationOrigin::StatusPipe
    );
    assert_eq!(
        radar.command(2).unwrap().origin,
        crate::observation::ObservationOrigin::Command
    );
}

#[test]
fn managed_naming_skips_manual_names_but_force_overrides() {
    let mut radar = RadarState::default();
    radar.tabs_changed(vec![tab(10, 0, "manual", true)]);
    radar.set_tab_panes_for_position(0, vec![focused_pane(1)]);
    radar.pane_cwd.insert(1, "/work/repo".into());

    let managed = radar.recompute_renames(config::NamingMode::Managed);
    assert!(managed.is_empty());

    let forced = radar.recompute_renames(config::NamingMode::Force);
    assert_eq!(
        forced,
        vec![TabRename {
            position: 0,
            name: "repo".into(),
        }]
    );
}

fn pane_update(tab_panes: HashMap<usize, Vec<TerminalPane>>) -> PaneUpdate {
    let live = tab_panes
        .values()
        .flat_map(|panes| panes.iter().map(|p| p.id))
        .collect();
    PaneUpdate {
        tab_panes,
        live,
        theme: None,
        exits: Vec::new(),
    }
}

#[test]
fn panes_changed_requests_cwd_bootstrap_for_new_pane_without_cwd() {
    let mut radar = RadarState::default();
    radar.tabs_changed(vec![tab(10, 0, "Tab #1", true)]);

    let change = radar.panes_changed(
        pane_update(HashMap::from([(0, vec![focused_pane(7)])])),
        1,
        config::NamingMode::Managed,
    );

    assert_eq!(change.cwd_bootstrap, vec![7]);
}

#[test]
fn panes_changed_requests_no_cwd_bootstrap_when_naming_off() {
    let mut radar = RadarState::default();
    radar.tabs_changed(vec![tab(10, 0, "Tab #1", true)]);

    let change = radar.panes_changed(
        pane_update(HashMap::from([(0, vec![focused_pane(7)])])),
        1,
        config::NamingMode::Off,
    );

    assert!(change.cwd_bootstrap.is_empty());
}

#[test]
fn panes_changed_skips_cwd_bootstrap_when_cwd_already_known() {
    let mut radar = RadarState::default();
    radar.tabs_changed(vec![tab(10, 0, "Tab #1", true)]);
    radar.cwd_changed(7, "/work/repo".into(), config::NamingMode::Managed);

    let change = radar.panes_changed(
        pane_update(HashMap::from([(0, vec![focused_pane(7)])])),
        1,
        config::NamingMode::Managed,
    );

    assert!(change.cwd_bootstrap.is_empty());
}

#[test]
fn panes_changed_requests_each_pane_cwd_only_once_even_if_unresolved() {
    // The host call may come back empty (a fresh pane with no cwd yet); we
    // must never re-issue it, or we rebuild the meltdown re-poll loop.
    let mut radar = RadarState::default();
    radar.tabs_changed(vec![tab(10, 0, "Tab #1", true)]);
    let update = || pane_update(HashMap::from([(0, vec![focused_pane(7)])]));

    let first = radar.panes_changed(update(), 1, config::NamingMode::Managed);
    let second = radar.panes_changed(update(), 2, config::NamingMode::Managed);

    assert_eq!(first.cwd_bootstrap, vec![7]);
    assert!(second.cwd_bootstrap.is_empty());
}

#[test]
fn cwd_bootstrap_attempt_resets_when_pane_id_is_recycled() {
    let mut radar = RadarState::default();
    radar.tabs_changed(vec![tab(10, 0, "Tab #1", true)]);

    let first = radar.panes_changed(
        pane_update(HashMap::from([(0, vec![focused_pane(7)])])),
        1,
        config::NamingMode::Managed,
    );
    assert_eq!(first.cwd_bootstrap, vec![7]);

    // Pane 7 closes (no longer live), then a new pane reuses id 7.
    radar.panes_changed(pane_update(HashMap::new()), 2, config::NamingMode::Managed);
    let reborn = radar.panes_changed(
        pane_update(HashMap::from([(0, vec![focused_pane(7)])])),
        3,
        config::NamingMode::Managed,
    );

    assert_eq!(reborn.cwd_bootstrap, vec![7]);
}

#[test]
fn cwd_bootstrap_prioritizes_focused_panes_and_caps_volume_per_update() {
    let mut radar = RadarState::default();
    radar.tabs_changed(vec![tab(10, 0, "Tab #1", true)]);
    // Eight unfocused panes (ids 1..=8) plus one focused pane (id 9): more
    // candidates than the per-update cap.
    let mut panes: Vec<TerminalPane> = (1..=MAX_CWD_BOOTSTRAP_PER_UPDATE as u32)
        .map(pane)
        .collect();
    panes.push(focused_pane(9));

    let first = radar.panes_changed(
        pane_update(HashMap::from([(0, panes)])),
        1,
        config::NamingMode::Managed,
    );

    assert_eq!(first.cwd_bootstrap.len(), MAX_CWD_BOOTSTRAP_PER_UPDATE);
    // The focused pane is resolved this round even though its id sorts last;
    // the lowest-id unfocused pane (8) spills to the next round.
    assert!(first.cwd_bootstrap.contains(&9));
    assert!(!first.cwd_bootstrap.contains(&8));

    let second = radar.panes_changed(
        pane_update(HashMap::from([(
            0,
            (1..=MAX_CWD_BOOTSTRAP_PER_UPDATE as u32)
                .map(pane)
                .chain(std::iter::once(focused_pane(9)))
                .collect(),
        )])),
        2,
        config::NamingMode::Managed,
    );
    assert_eq!(second.cwd_bootstrap, vec![8]);
}

#[test]
fn bootstrapped_cwd_names_the_tab_and_later_cd_still_renames() {
    let mut radar = RadarState::default();
    radar.tabs_changed(vec![tab(10, 0, "Tab #1", true)]);

    // 1. New tab → bootstrap requests pane 7's cwd.
    let opened = radar.panes_changed(
        pane_update(HashMap::from([(0, vec![focused_pane(7)])])),
        1,
        config::NamingMode::Managed,
    );
    assert_eq!(opened.cwd_bootstrap, vec![7]);

    // 2. Host resolves it; the tab is named from the spawn directory.
    let bootstrapped = radar.cwd_changed(7, "/work/alpha".into(), config::NamingMode::Managed);
    assert_eq!(
        bootstrapped.renames,
        vec![TabRename {
            position: 0,
            name: "alpha".into(),
        }]
    );

    // 3. A later PaneUpdate does not re-request the cwd (we already have it).
    let refreshed = radar.panes_changed(
        pane_update(HashMap::from([(0, vec![focused_pane(7)])])),
        2,
        config::NamingMode::Managed,
    );
    assert!(refreshed.cwd_bootstrap.is_empty());

    // 4. A real `cd` (CwdChanged) renames the tab as before.
    let moved = radar.cwd_changed(7, "/work/beta".into(), config::NamingMode::Managed);
    assert_eq!(
        moved.renames,
        vec![TabRename {
            position: 0,
            name: "beta".into(),
        }]
    );
}

// The pure roll-up behaviours — untracked panes shown-but-not-counted,
// severity precedence, tie-break, done/total/pending counts, and the
// command-only outcome mapping — are tested directly against `roll_up` in
// `rollup.rs`. The tests below cover what the roll-up seam can't see: the
// status>command resolution precedence and the full command-pipeline path.

#[test]
fn same_pane_status_observation_wins_over_command() {
    // The status>command precedence lives in `tab_display`'s resolve closure
    // (`status.get().or_else(command.get())`) — `roll_up` only ever sees the
    // one observation the closure returns, so this is the seam's own contract
    // and is invisible to rollup's tests. A pane carrying BOTH a status-pipe
    // and a command observation must resolve to the status one.
    let mut radar = RadarState::default();
    radar.tabs_changed(vec![tab(10, 0, "work", true)]);
    radar.set_tab_panes_for_position(0, vec![focused_pane(5)]);

    // A command observation on pane 5 (foreground command, promoted to Running).
    radar.command_changed(5, &["cargo".into(), "build".into()], true, 1);
    radar.timer(2);
    assert_eq!(radar.command(5).unwrap().status, Status::Running);

    // A status-pipe observation on the SAME pane 5, with a distinct repo.
    radar
        .status_mut()
        .apply(payload_for(5, Status::Running, "from-status"), 3);

    let row = radar.rows().remove(0);
    let detail = row.display.detail.as_ref().expect("active pane sets detail");
    assert_eq!(
        detail.repo, "from-status",
        "status pipe wins over command for the same pane id"
    );
}

#[test]
fn notify_views_status_wins_over_command_for_same_pane() {
    // Guards the status>command precedence in `notify_views`: when the same pane
    // id appears in both the status store and the command store, the returned
    // observation must be the STATUS one (mirrors the system-wide rule and the
    // precedence tested for `rows()` in `same_pane_status_observation_wins_over_command`).
    let mut radar = RadarState::default();

    // A command observation on pane 5 (foreground command, promoted to Running).
    radar.command_changed(5, &["cargo".into(), "build".into()], true, 1);
    radar.timer(2);
    assert_eq!(radar.command(5).unwrap().status, Status::Running);

    // A status-pipe observation on the SAME pane 5, with a distinct repo so
    // the two sources are distinguishable.
    radar
        .status_mut()
        .apply(payload_for(5, Status::Done, "from-status"), 3);

    let views = radar.notify_views();
    let obs = views.get(&5).expect("pane 5 must appear in notify_views");
    assert_eq!(
        obs.repo, "from-status",
        "notify_views must return the status-store observation, not the command-store one"
    );
}

// ── End-result outcome (full pipeline) ──

#[test]
fn finished_command_pane_carries_outcome_through_rows() {
    // Full path: a foreground command runs, then exits nonzero → the tab's
    // detail carries Failed(code) and msg stays the pure command string.
    let mut radar = RadarState::default();
    radar.tabs_changed(vec![tab(10, 0, "work", true)]);
    radar.set_tab_panes_for_position(0, vec![focused_pane(1)]);
    radar.command_changed(1, &["cargo".into(), "build".into()], true, 1);
    radar.timer(2); // promote pending → Running
    assert_eq!(radar.command(1).unwrap().status, Status::Running);

    radar.command_mut().on_exit(1, Some(2), 3);

    let row = radar.rows().remove(0);
    assert_eq!(row.display.status, Status::Error);
    let detail = row.display.detail.as_ref().unwrap();
    assert_eq!(detail.msg, "cargo build", "msg stays pure (tag is structural)");
    assert_eq!(detail.outcome, Some(Outcome::Failed(Some(2))));
}

#[test]
fn snapshot_round_trip_preserves_command_exit_code() {
    let mut radar = RadarState::default();
    radar.command_changed(7, &["cargo".into(), "test".into()], true, 1);
    radar.timer(2);
    radar.command_mut().on_exit(7, Some(3), 3);
    assert_eq!(radar.command(7).unwrap().exit_code, Some(3));

    let json = radar.snapshot_json(None, 3);
    let mut restored = RadarState::default();
    restored.load_snapshot(&json).expect("valid snapshot");
    let cmd = restored.command(7).expect("command restored");
    assert_eq!(cmd.status, Status::Error);
    assert_eq!(cmd.exit_code, Some(3));
}

#[test]
fn snapshot_round_trip_preserves_status_observations_and_tick() {
    let mut radar = RadarState::default();
    radar
        .status_mut()
        .apply(payload_for(1, Status::Running, "repo"), 3);
    let mut done = payload_for(2, Status::Done, "pinky");
    done.on_focus = Some(Status::Idle);
    done.branch = "fix/x".into();
    done.msg = "shipped it".into();
    done.source = "codex".into();
    radar.status_mut().apply(done, 5);

    let json = radar.snapshot_json(None, 42);
    let mut restored = RadarState::default();
    let tick = restored.load_snapshot(&json).expect("valid snapshot");

    assert_eq!(tick, 42);
    assert_eq!(restored.status(1).unwrap().status, Status::Running);
    let pane = restored.status(2).expect("pane 2 restored");
    assert_eq!(pane.status, Status::Done);
    assert_eq!(pane.repo, "pinky");
    assert_eq!(pane.branch, "fix/x");
    assert_eq!(pane.msg, "shipped it");
    assert_eq!(pane.source, "codex");
    assert_eq!(pane.on_focus, Some(Status::Idle));
}

#[test]
fn snapshot_round_trip_preserves_command_observations() {
    let mut radar = RadarState::default();
    radar.command_changed(7, &["cargo".into(), "test".into()], true, 1);
    radar.timer(2);

    let json = radar.snapshot_json(None, 2);
    let mut restored = RadarState::default();
    restored.load_snapshot(&json).expect("valid snapshot");

    let command = restored.command(7).expect("command restored");
    assert_eq!(command.origin, ObservationOrigin::Command);
    assert_eq!(command.status, Status::Running);
    assert_eq!(command.msg, "cargo test");
}

#[test]
fn snapshot_load_migrates_legacy_status_snapshot() {
    // The legacy record still carries a `"seq":3` — now an unknown field that
    // must be ignored, not rejected, so old snapshots keep loading.
    let legacy = r#"{"v":1,"tick":7,"panes":[{"pane_id":9,"status":"running","repo":"repo","branch":"main","msg":"work","source":"claude","last_change_tick":6,"seq":3,"on_focus":"idle","ever_active":true}]}"#;
    let mut radar = RadarState::default();

    let tick = radar.load_snapshot(legacy).expect("legacy snapshot loads");

    assert_eq!(tick, 7);
    let pane = radar.status(9).expect("legacy pane restored");
    assert_eq!(pane.origin, ObservationOrigin::StatusPipe);
    assert_eq!(pane.status, Status::Running);
    assert_eq!(pane.on_focus, Some(Status::Idle));
}

#[test]
fn snapshot_rejects_garbage_and_unknown_versions() {
    let mut radar = RadarState::default();
    assert!(radar.load_snapshot("not json").is_none());
    assert!(radar
        .load_snapshot(r#"{"v":999,"tick":1,"observations":[]}"#)
        .is_none());
}

#[test]
fn snapshot_with_one_unknown_origin_is_rejected_whole() {
    // A corrupt origin fails `TrackedObservation` deserialization, and the
    // loader drops the entire snapshot rather than silently keeping the rest
    // — so a partially-corrupt file can't load as a partial radar.
    let json = r#"{"v":2,"tick":4,"observations":[
        {"pane_id":1,"origin":"status_pipe","status":"running","repo":"r","branch":"b","msg":"m","source":"claude","last_change_tick":1,"ever_active":true},
        {"pane_id":2,"origin":"???","status":"done","repo":"r","branch":"b","msg":"m","source":"build","last_change_tick":2,"ever_active":true}
    ]}"#;
    let mut radar = RadarState::default();
    assert!(radar.load_snapshot(json).is_none());
}

#[test]
fn snapshot_merge_preserves_existing_when_live_panes_are_unknown() {
    let mut existing = RadarState::default();
    existing
        .status_mut()
        .apply(payload_for(1, Status::Running, "old"), 1);
    let existing_json = existing.snapshot_json(None, 5);

    let mut current = RadarState::default();
    current
        .status_mut()
        .apply(payload_for(2, Status::Running, "new"), 2);
    let merged = current.snapshot_json(Some(&existing_json), 3);

    let mut restored = RadarState::default();
    let tick = restored.load_snapshot(&merged).expect("merged snapshot");
    assert_eq!(tick, 5, "merge keeps the higher existing tick");
    assert_eq!(restored.status(1).unwrap().repo, "old");
    assert_eq!(restored.status(2).unwrap().repo, "new");
}

#[test]
fn snapshot_merge_drops_existing_dead_panes_after_live_update() {
    let mut existing = RadarState::default();
    existing
        .status_mut()
        .apply(payload_for(1, Status::Running, "dead"), 1);
    let existing_json = existing.snapshot_json(None, 5);

    let mut current = RadarState::default();
    current.tabs_changed(vec![tab(10, 0, "work", true)]);
    current
        .status_mut()
        .apply(payload_for(2, Status::Running, "live"), 2);
    current.panes_changed(
        PaneUpdate {
            tab_panes: HashMap::from([(0, vec![focused_pane(2)])]),
            live: HashSet::from([2]),
            theme: None,
            exits: Vec::new(),
        },
        3,
        config::NamingMode::Off,
    );

    let merged = current.snapshot_json(Some(&existing_json), 3);
    let mut restored = RadarState::default();
    restored.load_snapshot(&merged).expect("merged snapshot");

    assert!(restored.status(1).is_none(), "known-dead pane is pruned");
    assert_eq!(restored.status(2).unwrap().repo, "live");
}

#[test]
fn snapshot_seeded_done_pane_still_clears_on_focus() {
    let mut radar = RadarState::default();
    let mut done = payload_for(5, Status::Done, "repo");
    done.on_focus = Some(Status::Idle);
    radar.status_mut().apply(done, 1);
    let json = radar.snapshot_json(None, 2);

    let mut restored = RadarState::default();
    restored.load_snapshot(&json).unwrap();
    restored.reconcile_focus(Some(5), 9);

    assert_eq!(restored.status(5).unwrap().status, Status::Idle);
    assert_eq!(restored.status(5).unwrap().on_focus, None);
}

#[test]
fn applied_tab_name_sticks_when_focus_moves_to_a_different_repo_pane() {
    let mut radar = RadarState::default();
    radar.tabs_changed(vec![tab(10, 0, "Tab #1", true)]);
    radar.set_tab_panes_for_position(0, vec![focused_pane(1), pane(2)]);
    radar.cwd_changed(1, "/work/alpha".into(), config::NamingMode::Managed);
    radar.cwd_changed(2, "/work/beta".into(), config::NamingMode::Managed);
    // Focused pane 1 named the tab "alpha".
    assert_eq!(radar.applied_name(TabId::new(10)), Some("alpha"));
    // Zellij echoes our rename back as a TabUpdate.
    radar.tabs_changed(vec![tab(10, 0, "alpha", true)]);

    // Focus shifts to pane 2 (a different repo). "alpha" is still justified by
    // pane 1, so the tab name must NOT flip — no rename emitted.
    let change = radar.panes_changed(
        pane_update(HashMap::from([(0, vec![pane(1), focused_pane(2)])])),
        2,
        config::NamingMode::Managed,
    );
    assert!(change.renames.is_empty());
    assert_eq!(radar.applied_name(TabId::new(10)), Some("alpha"));
}

#[test]
fn manual_rename_is_preserved_through_focus_and_cwd_changes() {
    let mut radar = RadarState::default();
    radar.tabs_changed(vec![tab(10, 0, "Tab #1", true)]);
    radar.set_tab_panes_for_position(0, vec![focused_pane(1)]);
    radar.cwd_changed(1, "/work/alpha".into(), config::NamingMode::Managed);
    // Zellij echoes our auto-name, then the user renames the tab by hand.
    radar.tabs_changed(vec![tab(10, 0, "alpha", true)]);
    radar.tabs_changed(vec![tab(10, 0, "my-thing", true)]);

    // A focus change must not reclaim the manual name.
    let focus = radar.panes_changed(
        pane_update(HashMap::from([(0, vec![focused_pane(1), pane(2)])])),
        2,
        config::NamingMode::Managed,
    );
    assert!(focus.renames.is_empty());

    // Neither may a later `cd` in the pane.
    let cd = radar.cwd_changed(1, "/work/gamma".into(), config::NamingMode::Managed);
    assert!(cd.renames.is_empty());
}

#[test]
fn applied_tab_name_repicks_when_the_naming_pane_closes() {
    let mut radar = RadarState::default();
    radar.tabs_changed(vec![tab(10, 0, "Tab #1", true)]);
    radar.set_tab_panes_for_position(0, vec![focused_pane(1), pane(2)]);
    radar.cwd_changed(1, "/work/alpha".into(), config::NamingMode::Managed);
    radar.cwd_changed(2, "/work/beta".into(), config::NamingMode::Managed);
    assert_eq!(radar.applied_name(TabId::new(10)), Some("alpha"));
    // Zellij echoes our rename back as a TabUpdate.
    radar.tabs_changed(vec![tab(10, 0, "alpha", true)]);

    // Pane 1 (which justified "alpha") closes. "alpha" is no longer supported,
    // so the tab re-picks from the surviving pane → "beta".
    let change = radar.panes_changed(
        pane_update(HashMap::from([(0, vec![focused_pane(2)])])),
        2,
        config::NamingMode::Managed,
    );
    assert_eq!(
        change.renames,
        vec![TabRename {
            position: 0,
            name: "beta".into(),
        }]
    );
    assert_eq!(radar.applied_name(TabId::new(10)), Some("beta"));
}

#[test]
fn mutating_events_request_a_render() {
    // tabs_changed / command_changed / cwd_changed each carry render=true so
    // the runtime repaints; without it the sidebar would silently go stale
    // after a tab reshuffle, a new tracked command, or a cwd report.
    let mut radar = RadarState::default();
    assert!(
        radar.tabs_changed(vec![tab(1, 0, "a", true)]).render,
        "tabs_changed must request a render"
    );
    assert!(
        radar
            .command_changed(1, &["cargo".into(), "build".into()], true, 0)
            .render,
        "command_changed must request a render"
    );
    assert!(
        radar
            .cwd_changed(1, "/tmp".into(), config::NamingMode::Off)
            .render,
        "cwd_changed must request a render"
    );
}

// ── Recede-while-focused ───────────────────────────────────────────────────
//
// "If they were looking at it when it finished, don't flag it." A completion
// that lands on the FOCUSED pane recedes; a background completion persists
// until visited; errors and pending never recede. `reconcile_focus` carries
// this from `panes_changed` (command exit, fresh focus) and `timer` (the
// cadence path for a watched agent turn). `status_pipe` deliberately defers
// to the timer rather than reconciling on its possibly-stale focus.

fn focused_pane_update(
    active_pos: usize,
    pane_id: u32,
    exits: Vec<(u32, Option<i32>)>,
) -> PaneUpdate {
    PaneUpdate {
        tab_panes: HashMap::from([(active_pos, vec![focused_pane(pane_id)])]),
        live: HashSet::from([pane_id]),
        theme: None,
        exits,
    }
}

#[test]
fn command_done_while_focused_recedes_immediately() {
    let mut radar = RadarState::default();
    radar.tabs_changed(vec![tab(10, 0, "work", true)]);
    // Establish focus on pane 5 FIRST, so the later exit update is not itself
    // a focus transition — isolating settle from the visit-clear path.
    radar.panes_changed(focused_pane_update(0, 5, Vec::new()), 1, config::NamingMode::Off);
    radar.command_changed(5, &["cargo".into(), "build".into()], true, 2);
    radar.timer(3); // promote to Running
    // Pane 5 exits 0 while still focused (focus unchanged this update).
    radar.panes_changed(
        focused_pane_update(0, 5, vec![(5, Some(0))]),
        4,
        config::NamingMode::Off,
    );
    assert_eq!(
        radar.command(5).unwrap().status,
        Status::Idle,
        "a Done that lands on the focused pane recedes immediately"
    );
}

#[test]
fn command_done_in_background_persists_then_clears_on_visit() {
    let mut radar = RadarState::default();
    radar.tabs_changed(vec![tab(10, 0, "active", true), tab(20, 1, "bg", false)]);
    radar.command_changed(5, &["cargo".into(), "build".into()], true, 1);
    radar.timer(2);
    // Focus is on pane 2 (active tab); pane 5 exits in the background tab.
    radar.panes_changed(
        PaneUpdate {
            tab_panes: HashMap::from([(0, vec![focused_pane(2)]), (1, vec![pane(5)])]),
            live: HashSet::from([2, 5]),
            theme: None,
            exits: vec![(5, Some(0))],
        },
        3,
        config::NamingMode::Off,
    );
    assert_eq!(
        radar.command(5).unwrap().status,
        Status::Done,
        "a background completion persists — you weren't looking"
    );
    radar.reconcile_focus(Some(5), 4);
    assert_eq!(
        radar.command(5).unwrap().status,
        Status::Idle,
        "visiting the pane clears the persisted Done"
    );
}

#[test]
fn command_error_while_focused_persists() {
    let mut radar = RadarState::default();
    radar.tabs_changed(vec![tab(10, 0, "work", true)]);
    // Focus established first, so the failing exit is not a focus transition:
    // settle is what runs (and must skip the error), not the visit-clear.
    radar.panes_changed(focused_pane_update(0, 5, Vec::new()), 1, config::NamingMode::Off);
    radar.command_changed(5, &["cargo".into(), "build".into()], true, 2);
    radar.timer(3);
    radar.panes_changed(
        focused_pane_update(0, 5, vec![(5, Some(1))]),
        4,
        config::NamingMode::Off,
    );
    assert_eq!(
        radar.command(5).unwrap().status,
        Status::Error,
        "errors persist even when you were watching"
    );
}

#[test]
fn agent_done_while_focused_recedes_on_timer_not_on_the_pipe_edge() {
    let mut radar = RadarState::default();
    radar.tabs_changed(vec![tab(10, 0, "agent", true)]);
    // Establish focus on the agent's pane 5.
    radar.panes_changed(focused_pane_update(0, 5, Vec::new()), 1, config::NamingMode::Off);
    // Agent turn completes: Done with a queued clear-on-focus.
    let raw = crate::payload::to_wire(
        5,
        Status::Done,
        "repo",
        "main",
        "shipped",
        Some(Status::Idle),
        "claude",
    );
    radar.status_pipe(&raw, 2, config::NamingMode::Off);
    // The raw pipe edge must NOT recede — `last_focused` could be stale there
    // (focus-change PaneUpdate not yet processed), so receding now could drop a
    // completion the user already navigated away from.
    assert_eq!(
        radar.status(5).unwrap().status,
        Status::Done,
        "the pipe edge defers the recede (focus may be stale)"
    );
    // The recede rides the next timer tick, by which point focus has settled.
    radar.timer(3);
    assert_eq!(
        radar.status(5).unwrap().status,
        Status::Idle,
        "a watched agent turn recedes on the confirming timer tick"
    );
}

#[test]
fn command_return_to_shell_while_focused_recedes_on_timer() {
    let mut radar = RadarState::default();
    radar.tabs_changed(vec![tab(10, 0, "work", true)]);
    radar.panes_changed(focused_pane_update(0, 5, Vec::new()), 1, config::NamingMode::Off);
    radar.command_changed(5, &["make".into()], true, 1);
    radar.timer(2); // promote to Running
    assert_eq!(radar.command(5).unwrap().status, Status::Running);
    // Command leaves the foreground; the confirming timer flips it to Done and
    // settle recedes it because pane 5 is still focused.
    radar.command_changed(5, &[], false, 3);
    radar.timer(4);
    assert_eq!(
        radar.command(5).unwrap().status,
        Status::Idle,
        "a command finishing in the focused pane recedes on the confirming timer"
    );
}

#[test]
fn done_finishing_while_focused_recedes_regardless_of_next_focus_direction() {
    // The original reported bug was a direction-dependent Done↔Idle flicker.
    // With recede-at-completion the pane clears the instant it finishes (in
    // the exit's own update), so the outcome is deterministic — Idle —
    // whichever pane focus moves to next. Drives the real `panes_changed`
    // flow so `reconcile_focus` actually runs.
    let run = |next_focus: u32| {
        let mut radar = RadarState::default();
        radar.tabs_changed(vec![tab(10, 0, "work", true)]);
        let update = |focused: u32, exits: Vec<(u32, Option<i32>)>| PaneUpdate {
            tab_panes: HashMap::from([(
                0,
                [1u32, 2, 3]
                    .into_iter()
                    .map(|id| TerminalPane {
                        id,
                        focused_in_tab: id == focused,
                        ..TerminalPane::default()
                    })
                    .collect(),
            )]),
            live: HashSet::from([1, 2, 3]),
            theme: None,
            exits,
        };
        // Focus pane 2 and promote a command there to Running.
        radar.panes_changed(update(2, vec![]), 1, config::NamingMode::Off);
        radar.command_changed(2, &["cargo".into(), "build".into()], true, 2);
        radar.timer(3);
        // Pane 2 exits 0 while focused → recedes via settle (same update).
        radar.panes_changed(update(2, vec![(2, Some(0))]), 4, config::NamingMode::Off);
        // Then focus moves to the next pane (higher or lower).
        radar.panes_changed(update(next_focus, vec![]), 5, config::NamingMode::Off);
        radar.command(2).map(|s| s.status)
    };
    assert_eq!(run(3), Some(Status::Idle), "moving 2→3 leaves pane 2 receded");
    assert_eq!(run(1), Some(Status::Idle), "moving 2→1 leaves pane 2 receded");
}

// ── next_attention_tab integration tests ─────────────────────────────────

#[test]
fn next_attention_tab_skips_running_and_idle() {
    let mut st = RadarState::default();
    // 3 tabs at positions 0,1,2; tab 0 active.
    st.tabs_changed(vec![
        RadarTab { id: TabId::new(1), position: 0, name: "a".into(), active: true,  has_bell: false },
        RadarTab { id: TabId::new(2), position: 1, name: "b".into(), active: false, has_bell: false },
        RadarTab { id: TabId::new(3), position: 2, name: "c".into(), active: false, has_bell: false },
    ]);
    // tab 0: running (not attention); tab 1: pending (attention); tab 2: idle.
    st.set_tab_panes_for_position(0, vec![pane(10)]);
    st.set_tab_panes_for_position(1, vec![pane(11)]);
    st.status_mut().apply(payload_for(10, Status::Running, ""), 1);
    st.status_mut().apply(payload_for(11, Status::Pending, ""), 1);

    assert_eq!(st.next_attention_tab(Direction::Next), Some(1));
    assert_eq!(st.next_attention_tab(Direction::Prev), Some(1));
}

#[test]
fn next_attention_tab_none_when_no_attention() {
    let mut st = RadarState::default();
    st.tabs_changed(vec![
        RadarTab { id: TabId::new(1), position: 0, name: "a".into(), active: true, has_bell: false },
    ]);
    st.set_tab_panes_for_position(0, vec![pane(10)]);
    st.status_mut().apply(payload_for(10, Status::Running, ""), 1);
    assert_eq!(st.next_attention_tab(Direction::Next), None);
    assert_eq!(st.next_attention_tab(Direction::Prev), None);
}

// ── cycle_attention unit tests ────────────────────────────────────────────

#[test]
fn cycle_attention_empty_set_is_none() {
    let tabs = [(0usize, Status::Idle), (1, Status::Running)];
    assert_eq!(cycle_attention(&tabs, Some(0), Direction::Next), None);
    assert_eq!(cycle_attention(&tabs, Some(0), Direction::Prev), None);
}

#[test]
fn cycle_attention_sole_member_equal_to_active_is_none() {
    let tabs = [(0usize, Status::Pending), (1, Status::Running)];
    assert_eq!(cycle_attention(&tabs, Some(0), Direction::Next), None);
    assert_eq!(cycle_attention(&tabs, Some(0), Direction::Prev), None);
}

#[test]
fn cycle_attention_next_and_prev_wrap_around() {
    // attention at positions 2 and 5
    let tabs = [(2usize, Status::Pending), (5, Status::Error)];
    // active = 2 → next is 5, prev wraps to 5
    assert_eq!(cycle_attention(&tabs, Some(2), Direction::Next), Some(5));
    assert_eq!(cycle_attention(&tabs, Some(2), Direction::Prev), Some(5));
    // active = 5 → next wraps to 2, prev is 2
    assert_eq!(cycle_attention(&tabs, Some(5), Direction::Next), Some(2));
    assert_eq!(cycle_attention(&tabs, Some(5), Direction::Prev), Some(2));
}

#[test]
fn cycle_attention_active_outside_set_enters_set() {
    let tabs = [(2usize, Status::Pending), (5, Status::Done)];
    // active = 3 (not an attention tab) → next 5, prev 2
    assert_eq!(cycle_attention(&tabs, Some(3), Direction::Next), Some(5));
    assert_eq!(cycle_attention(&tabs, Some(3), Direction::Prev), Some(2));
    // active = None → next = smallest, prev = largest
    assert_eq!(cycle_attention(&tabs, None, Direction::Next), Some(2));
    assert_eq!(cycle_attention(&tabs, None, Direction::Prev), Some(5));
}

// ── Stateful property test ────────────────────────────────────────────────
//
// `RadarState` is an event aggregator: the host feeds it interleaved tab,
// pane, status-pipe, command, timer and cwd events, and a bug usually hides
// in some *ordering* of them rather than any single event. The example tests
// above cover specific sequences; this drives random sequences and asserts
// the structural invariants that must hold after ANY history.
use proptest::prelude::*;

fn arb_status() -> impl Strategy<Value = Status> {
    prop_oneof![
        Just(Status::Idle),
        Just(Status::Done),
        Just(Status::Running),
        Just(Status::Pending),
        Just(Status::Error),
    ]
}

/// One host event. Pane ids and tab positions are drawn from small ranges so
/// they recur across events — exercising prune, pane recycling, and tab
/// reordering rather than a stream of never-seen ids.
#[derive(Clone, Debug)]
enum Op {
    /// Replace the tab set with tabs at these positions (deduped; first active).
    Tabs(Vec<usize>),
    /// Set the live pane layout (position → pane ids) plus command exits.
    Panes(Vec<(usize, Vec<u32>)>, Vec<(u32, Option<i32>)>),
    /// Deliver a status-pipe payload for a pane.
    Status(u32, Status),
    /// Register a foreground/background command on a pane.
    Command(u32, bool),
    /// Fire the debounce/promotion timer.
    Timer,
    /// Report a pane's cwd.
    Cwd(u32),
}

fn arb_op() -> impl Strategy<Value = Op> {
    prop_oneof![
        proptest::collection::vec(0usize..4, 0..4).prop_map(Op::Tabs),
        (
            proptest::collection::vec(
                (0usize..4, proptest::collection::vec(1u32..6, 0..3)),
                0..4
            ),
            proptest::collection::vec((1u32..6, proptest::option::of(any::<i32>())), 0..2),
        )
            .prop_map(|(layout, exits)| Op::Panes(layout, exits)),
        (1u32..6, arb_status()).prop_map(|(p, s)| Op::Status(p, s)),
        (1u32..6, any::<bool>()).prop_map(|(p, fg)| Op::Command(p, fg)),
        Just(Op::Timer),
        (1u32..6).prop_map(Op::Cwd),
    ]
}

proptest! {
    #[test]
    fn radar_state_invariants_hold_after_any_event_sequence(
        ops in proptest::collection::vec(arb_op(), 0..40)
    ) {
        let mut st = RadarState::default();

        for (i, op) in ops.iter().enumerate() {
            let tick = i as u64;
            match op {
                Op::Tabs(positions) => {
                    let mut seen = HashSet::new();
                    let tabs: Vec<RadarTab> = positions
                        .iter()
                        .filter(|p| seen.insert(**p))
                        .enumerate()
                        .map(|(idx, &p)| RadarTab {
                            id: TabId::new(p + 1),
                            position: p,
                            name: format!("t{p}"),
                            active: idx == 0,
                            has_bell: false,
                        })
                        .collect();
                    st.tabs_changed(tabs);
                }
                Op::Panes(layout, exits) => {
                    let mut tab_panes: HashMap<usize, Vec<TerminalPane>> = HashMap::new();
                    for (pos, ids) in layout {
                        let panes = ids
                            .iter()
                            .map(|&id| TerminalPane {
                                id,
                                title: format!("p{id}"),
                                focused_in_tab: false,
                            })
                            .collect();
                        tab_panes.insert(*pos, panes);
                    }
                    let live: HashSet<u32> = tab_panes
                        .values()
                        .flat_map(|v| v.iter().map(|p| p.id))
                        .collect();
                    let update = PaneUpdate {
                        tab_panes,
                        live: live.clone(),
                        theme: None,
                        exits: exits.clone(),
                    };
                    st.panes_changed(update, tick, config::NamingMode::Off);

                    // Prune contract: immediately after panes_changed every
                    // stored observation belongs to a live pane.
                    for (id, _) in st.status_store().observations() {
                        prop_assert!(
                            live.contains(&id),
                            "status store kept observation for non-live pane {id}"
                        );
                    }
                    for (id, _) in st.command_store().observations() {
                        prop_assert!(
                            live.contains(&id),
                            "command store kept observation for non-live pane {id}"
                        );
                    }
                }
                Op::Status(pane, status) => {
                    let raw = format!(
                        r#"{{"v":1,"source":"claude","pane":{{"type":"terminal","id":{pane}}},"status":"{}","repo":"r","msg":"m"}}"#,
                        status.as_wire()
                    );
                    let _ = st.status_pipe(&raw, tick, config::NamingMode::Off);
                }
                Op::Command(pane, fg) => {
                    st.command_changed(
                        *pane,
                        &["cargo".to_string(), "build".to_string()],
                        *fg,
                        tick,
                    );
                }
                Op::Timer => st.timer(tick),
                Op::Cwd(pane) => {
                    st.cwd_changed(*pane, "/home/u/proj".into(), config::NamingMode::Off);
                }
            }

            // `rows()` is total (never panics) and well-formed after every op.
            let rows = st.rows();
            for w in rows.windows(2) {
                prop_assert!(
                    w[0].number < w[1].number,
                    "rows must be strictly ordered by tab position"
                );
            }
            for r in &rows {
                // 1:1 pane mapping and count sanity flow through the pipeline.
                prop_assert!(r.display.progress.done <= r.display.progress.total);
                prop_assert!(r.display.progress.total <= r.display.panes.len());
            }
        }

        // Snapshot round-trip is identity: serialize → load → serialize again
        // yields byte-identical JSON for the whole accumulated history.
        const SNAPSHOT_TICK: u64 = 9999;
        let s1 = st.snapshot_json(None, SNAPSHOT_TICK);
        let mut reloaded = RadarState::default();
        reloaded.load_snapshot(&s1);
        let s2 = reloaded.snapshot_json(None, SNAPSHOT_TICK);
        prop_assert_eq!(s1, s2, "snapshot round-trip must be identity");
    }

    #[test]
    fn attention_next_visits_every_member_and_returns_to_start(
        members in proptest::collection::btree_set(0usize..64, 1..8),
        start_pick in 0usize..8,
    ) {
        let members: Vec<usize> = members.into_iter().collect();
        let m = members.len();
        let start = members[start_pick % m];
        let tabs: Vec<(usize, Status)> =
            members.iter().map(|&p| (p, Status::Pending)).collect();

        let mut active = Some(start);
        let mut visited = Vec::new();
        for _ in 0..m {
            match cycle_attention(&tabs, active, Direction::Next) {
                None => {
                    // Only legal when the set has a single member equal to active.
                    prop_assert_eq!(m, 1);
                    visited.push(start);
                }
                Some(n) => {
                    prop_assert_ne!(Some(n), active);
                    visited.push(n);
                    active = Some(n);
                }
            }
        }
        // Returned to the origin after a full lap, having touched every member once.
        prop_assert_eq!(active, Some(start));
        let mut seen = visited.clone();
        seen.sort_unstable();
        seen.dedup();
        prop_assert_eq!(seen, members);
    }
}

// `PaneUpdate::from_raw` — the manifest-folding policy that used to live in the
// wasm-only `Event::PaneUpdate` arm (host-untestable). These pin the parts that
// decide observable behavior: plugin-skip, title sanitize, and the
// focused-else-any terminal-color precedence.

fn raw_pane(id: u32, tab_pos: usize) -> RawPane {
    RawPane {
        tab_pos,
        id,
        title: String::new(),
        is_plugin: false,
        is_focused: false,
        default_bg: None,
        default_fg: None,
        exited: false,
        exit_status: None,
    }
}

#[test]
fn from_raw_skips_plugin_panes() {
    let update = PaneUpdate::from_raw(vec![
        RawPane { is_plugin: true, ..raw_pane(1, 0) },
        raw_pane(2, 0),
    ]);
    assert!(!update.live.contains(&1), "the plugin (rail) pane is not a live terminal");
    assert_eq!(update.live, HashSet::from([2]));
    assert_eq!(update.tab_panes[&0].len(), 1);
    assert_eq!(update.tab_panes[&0][0].id, 2);
}

#[test]
fn from_raw_sanitizes_and_truncates_titles() {
    let update = PaneUpdate::from_raw(vec![RawPane { title: "x".repeat(100), ..raw_pane(1, 0) }]);
    assert!(update.tab_panes[&0][0].title.chars().count() <= 40);
}

#[test]
fn from_raw_collects_live_ids_and_exits_and_groups_by_tab() {
    let update = PaneUpdate::from_raw(vec![
        raw_pane(1, 0),
        raw_pane(2, 0),
        RawPane { exited: true, exit_status: Some(2), ..raw_pane(3, 1) },
    ]);
    assert_eq!(update.live, HashSet::from([1, 2, 3]));
    assert_eq!(update.exits, vec![(3, Some(2))]);
    assert_eq!(update.tab_panes[&0].len(), 2);
    assert_eq!(update.tab_panes[&1].len(), 1);
}

#[test]
fn from_raw_theme_prefers_focused_pane_over_any_regardless_of_order() {
    // An earlier *unfocused* pane reports colors, a later *focused* pane reports
    // different ones — the focused pane must win even though it appears second.
    let update = PaneUpdate::from_raw(vec![
        RawPane { default_bg: Some("#101010".into()), default_fg: Some("#aaaaaa".into()), ..raw_pane(1, 0) },
        RawPane { is_focused: true, default_bg: Some("#202020".into()), default_fg: Some("#bbbbbb".into()), ..raw_pane(2, 0) },
    ]);
    // DerivedColors isn't PartialEq; rail_bg uniquely distinguishes the two inputs.
    let focused = theme::DerivedColors::from_bg_fg((0x20, 0x20, 0x20), (0xbb, 0xbb, 0xbb));
    let any = theme::DerivedColors::from_bg_fg((0x10, 0x10, 0x10), (0xaa, 0xaa, 0xaa));
    let theme = update.theme.expect("a terminal pane reported colors");
    assert_eq!(theme.rail_bg, focused.rail_bg);
    assert_ne!(theme.rail_bg, any.rail_bg);
}

#[test]
fn from_raw_theme_falls_back_to_any_terminal_pane_when_none_focused() {
    let update = PaneUpdate::from_raw(vec![RawPane {
        default_bg: Some("#101010".into()),
        default_fg: Some("#aaaaaa".into()),
        ..raw_pane(1, 0)
    }]);
    let expected = theme::DerivedColors::from_bg_fg((0x10, 0x10, 0x10), (0xaa, 0xaa, 0xaa));
    assert_eq!(update.theme.expect("colors present").rail_bg, expected.rail_bg);
}

#[test]
fn from_raw_theme_is_none_without_color_reports() {
    assert!(PaneUpdate::from_raw(vec![raw_pane(1, 0)]).theme.is_none());
}

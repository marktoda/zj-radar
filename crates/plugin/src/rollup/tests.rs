use super::*;
use std::collections::HashMap;

fn pane(id: u32, title: &str) -> TerminalPane {
    TerminalPane {
        id,
        title: title.to_string(),
        focused_in_tab: false,
    }
}

fn obs(origin: ObservationOrigin, status: Status, tick: u64) -> TrackedObservation {
    TrackedObservation {
        origin,
        status,
        repo: "repo".into(),
        branch: "main".into(),
        msg: "msg".into(),
        task: String::new(),
        kind: Kind::Build,
        last_change_tick: tick,
        ever_active: true,
        exit_code: None,
    }
}

fn command_obs(status: Status, exit_code: Option<i32>) -> TrackedObservation {
    let mut o = obs(ObservationOrigin::Command, status, 1);
    o.exit_code = exit_code;
    o
}

/// Build a resolver from a fixed map — the test-side seam, no RadarState.
fn resolver<'a>(
    map: &'a HashMap<u32, TrackedObservation>,
) -> impl Fn(u32) -> Option<&'a TrackedObservation> {
    move |id| map.get(&id)
}

#[test]
fn empty_panes_roll_up_to_idle() {
    let display = roll_up(&[], |_id| None);
    assert_eq!(display.status, Status::Idle);
    assert_eq!(display.progress, ProgressCounts::default());
    assert!(display.detail.is_none());
    assert!(display.panes.is_empty());
}

#[test]
fn untracked_panes_are_shown_but_not_counted() {
    let mut map = HashMap::new();
    map.insert(1, obs(ObservationOrigin::StatusPipe, Status::Running, 1));
    let panes = [pane(1, "codex"), pane(2, "shell")];

    let display = roll_up(&panes, resolver(&map));

    assert_eq!(display.status, Status::Running);
    assert_eq!(display.progress.total, 1, "only the tracked pane counts");
    assert_eq!(display.panes.len(), 2, "untracked pane is still displayed");
    assert_eq!(display.panes[0].pane_id(), 1);
    assert_eq!(display.panes[1].pane_id(), 2);
    assert!(!display.panes[1].is_tracked());
}

#[test]
fn severity_precedence_picks_highest_active() {
    let mut map = HashMap::new();
    map.insert(1, obs(ObservationOrigin::Command, Status::Done, 1));
    map.insert(2, obs(ObservationOrigin::Command, Status::Error, 1));
    let panes = [pane(1, "a"), pane(2, "b")];

    let display = roll_up(&panes, resolver(&map));

    assert_eq!(display.status, Status::Error, "error outranks done");
    let detail = display.detail.expect("an active pane sets the detail");
    assert_eq!(detail.status, Status::Error);
    assert_eq!(detail.outcome, Some(Outcome::Failed(None)));
}

#[test]
fn tie_break_prefers_later_change_tick_over_position() {
    // Higher-tick pane comes FIRST; positional logic would pick the second.
    let mut map = HashMap::new();
    map.insert(1, obs(ObservationOrigin::StatusPipe, Status::Running, 9));
    map.insert(2, obs(ObservationOrigin::StatusPipe, Status::Running, 5));
    let panes = [pane(1, "a"), pane(2, "b")];

    let display = roll_up(&panes, resolver(&map));

    assert_eq!(display.status, Status::Running);
    assert_eq!(
        display.detail.expect("detail set").since_tick,
        9,
        "the most-recently-changed pane wins the tie, not the last in order"
    );
}

#[test]
fn counts_done_total_and_pending() {
    let mut map = HashMap::new();
    map.insert(1, obs(ObservationOrigin::Command, Status::Done, 1));
    map.insert(2, obs(ObservationOrigin::Command, Status::Running, 1));
    map.insert(3, obs(ObservationOrigin::StatusPipe, Status::Pending, 1));
    let panes = [pane(1, "a"), pane(2, "b"), pane(3, "c")];

    let display = roll_up(&panes, resolver(&map));

    assert_eq!(display.progress.total, 3);
    assert_eq!(display.progress.done, 1);
    assert_eq!(display.progress.pending, 1);
}

#[test]
fn pending_is_only_counted_for_tracked_panes() {
    // A pane rendered as untracked (never ever_active — reachable via a
    // snapshot/legacy load that stores ever_active verbatim) must not inflate the
    // `pending` count while being excluded from `total`. `pending` counts the
    // same set as `total`/`done`, never more, so progress stays internally
    // consistent (pending <= total).
    let mut map = HashMap::new();
    let mut stale = obs(ObservationOrigin::StatusPipe, Status::Pending, 1);
    stale.ever_active = false;
    map.insert(1, stale);
    let panes = [pane(1, "shell")];

    let display = roll_up(&panes, resolver(&map));

    assert_eq!(display.progress.total, 0, "an un-tracked pane is not in total");
    assert_eq!(
        display.progress.pending, 0,
        "pending must not count a pane excluded from total"
    );
    assert!(!display.panes[0].is_tracked());
}

#[test]
fn pane_outcome_maps_finished_commands_only() {
    assert_eq!(pane_outcome(&command_obs(Status::Done, Some(0))), Some(Outcome::Ok));
    assert_eq!(
        pane_outcome(&command_obs(Status::Error, Some(2))),
        Some(Outcome::Failed(Some(2)))
    );
    assert_eq!(
        pane_outcome(&command_obs(Status::Error, None)),
        Some(Outcome::Failed(None))
    );
    // Active commands get no tag.
    assert_eq!(pane_outcome(&command_obs(Status::Running, None)), None);
    // Agents (status pipe) never get a tag, even when Done.
    let mut agent = command_obs(Status::Done, Some(0));
    agent.origin = ObservationOrigin::StatusPipe;
    assert_eq!(pane_outcome(&agent), None);
}

// ── Property tests ────────────────────────────────────────────────────────
//
// `roll_up` is a pure fold over a tab's panes. These properties pin the
// contracts the per-scenario tests only sample: count bounds, the 1:1 pane
// mapping, status == max-active, and order-independence of the aggregate.
use proptest::prelude::*;

/// One pane's optional observation: `None` = untracked. Tuple fields are
/// (origin, status, ever_active, last_change_tick, exit_code).
type Spec = Option<(ObservationOrigin, Status, bool, u64, Option<i32>)>;

fn arb_status() -> impl Strategy<Value = Status> {
    prop_oneof![
        Just(Status::Idle),
        Just(Status::Done),
        Just(Status::Running),
        Just(Status::Pending),
        Just(Status::Error),
    ]
}

fn pane_specs() -> impl Strategy<Value = Vec<Spec>> {
    let spec = proptest::option::of((
        prop_oneof![Just(ObservationOrigin::StatusPipe), Just(ObservationOrigin::Command)],
        arb_status(),
        any::<bool>(),
        0u64..50,
        proptest::option::of(any::<i32>()),
    ));
    proptest::collection::vec(spec, 0..8)
}

/// Build the (panes, resolver-map) pair from specs. Pane ids are 1-based and
/// distinct (a tab never has two panes with the same id).
fn build(specs: &[Spec]) -> (Vec<TerminalPane>, HashMap<u32, TrackedObservation>) {
    let mut panes = Vec::new();
    let mut map = HashMap::new();
    for (i, spec) in specs.iter().enumerate() {
        let id = i as u32 + 1;
        panes.push(pane(id, "t"));
        if let &Some((origin, status, ever_active, tick, exit_code)) = spec {
            map.insert(
                id,
                TrackedObservation {
                    origin,
                    status,
                    repo: "r".into(),
                    branch: String::new(),
                    msg: "m".into(),
                    task: String::new(),
                    kind: Kind::Build,
                    last_change_tick: tick,
                    ever_active,
                    exit_code,
                },
            );
        }
    }
    (panes, map)
}

/// Deterministic Fisher–Yates shuffle (xorshift PRNG seeded by `seed`) — no
/// `rand`, fully reproducible for proptest shrinking.
fn shuffled<T: Clone>(items: &[T], mut seed: u64) -> Vec<T> {
    seed |= 1; // avoid the zero fixed-point
    let mut v = items.to_vec();
    for i in (1..v.len()).rev() {
        seed ^= seed << 13;
        seed ^= seed >> 7;
        seed ^= seed << 17;
        let j = (seed % (i as u64 + 1)) as usize;
        v.swap(i, j);
    }
    v
}

proptest! {
    /// Invariants that hold for ANY tab shape.
    #[test]
    fn roll_up_invariants(specs in pane_specs()) {
        let (panes, map) = build(&specs);
        let d = roll_up(&panes, |id| map.get(&id));

        // Every input pane appears exactly once, in input order.
        prop_assert_eq!(d.panes.len(), panes.len());
        for (out, inp) in d.panes.iter().zip(&panes) {
            prop_assert_eq!(out.pane_id(), inp.id);
        }

        // Count bounds: done ≤ total ≤ #panes, pending ≤ #panes.
        prop_assert!(d.progress.done <= d.progress.total);
        prop_assert!(d.progress.total <= panes.len());
        prop_assert!(d.progress.pending <= panes.len());

        // The tab status mirrors the detail (Idle when no active pane).
        prop_assert_eq!(
            d.status,
            d.detail.as_ref().map_or(Status::Idle, |x| x.status)
        );

        // Status == the max active observation status (Status: Ord = severity).
        let expected_status = panes
            .iter()
            .filter_map(|p| map.get(&p.id))
            .map(|o| o.status)
            .filter(|s| s.is_active())
            .max()
            .unwrap_or(Status::Idle);
        prop_assert_eq!(d.status, expected_status);
        // A detail exists iff some pane is active.
        prop_assert_eq!(d.detail.is_some(), d.status.is_active());

        // total/done count exactly the ever_active (and Done) panes.
        let total = panes.iter().filter_map(|p| map.get(&p.id)).filter(|o| o.ever_active).count();
        let done = panes.iter().filter_map(|p| map.get(&p.id))
            .filter(|o| o.ever_active && o.status == Status::Done).count();
        let pending = panes.iter().filter_map(|p| map.get(&p.id))
            .filter(|o| o.ever_active && o.status == Status::Pending).count();
        prop_assert_eq!(d.progress.total, total);
        prop_assert_eq!(d.progress.done, done);
        prop_assert_eq!(d.progress.pending, pending);
    }

    /// Reordering a tab's panes never changes the aggregate status, the
    /// progress counts, or the detail's status — only the tie-break identity
    /// of `detail` may move, and only on an exact (status, tick) tie.
    #[test]
    fn roll_up_aggregate_is_order_independent(specs in pane_specs(), seed in any::<u64>()) {
        let (panes, map) = build(&specs);
        let d1 = roll_up(&panes, |id| map.get(&id));
        let d2 = roll_up(&shuffled(&panes, seed), |id| map.get(&id));

        prop_assert_eq!(d1.status, d2.status);
        prop_assert_eq!(d1.progress, d2.progress);
        prop_assert_eq!(
            d1.detail.as_ref().map(|x| x.status),
            d2.detail.as_ref().map(|x| x.status)
        );
    }
}

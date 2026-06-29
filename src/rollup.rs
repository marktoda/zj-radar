//! Tab Roll-Up: the per-pane → per-tab aggregation seam.
//!
//! Severity order `error > pending > running > done > idle`, with `done/total`
//! and `pending` counts and a highest-severity detail line. This is the domain
//! operation named "Tab Roll-Up" in `CONTEXT.md`: a deep, pure module that
//! turns a tab's panes plus a per-pane observation lookup into the `TabDisplay`
//! the rail renders. It owns its output vocabulary (`TabDisplay`, `PaneDisplay`,
//! `PrimaryDetail`, `ProgressCounts`, `Outcome`); the renderer consumes it.
//!
//! The "two sources, status wins" knowledge lives in the caller's `resolve`
//! closure — `roll_up` never learns there is more than one store, which keeps
//! the source seam (`StatusStore` / `CommandStore`) free to evolve.

use crate::kind::Kind;
use crate::observation::{ObservationOrigin, TrackedObservation};
use crate::radar_state::TerminalPane;
use crate::status::Status;

/// The end-result of a finished *command* pane, shown as a tag after the
/// activity (`cargo build ✓`, `cargo build (exit 1)`). Built in
/// `rollup::roll_up`; agents never carry one. Kept structured (not baked into
/// `msg`) so the renderer can reserve its width — the outcome survives
/// truncation while the command absorbs the squeeze — and color it
/// independently of the (dim) command text. The display methods
/// (`full`/`minimal`/`role`) live in `render`, since they encode glyphs and a
/// width-driven form; the enum here is pure semantics.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Outcome {
    /// Exit 0 / returned to the shell with no failure evidence.
    Ok,
    /// Nonzero exit; `Some(code)` when known, `None` for a signal/no-code exit.
    Failed(Option<i32>),
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PrimaryDetail {
    pub repo: String,
    pub branch: String,
    pub msg: String,
    pub since_tick: u64,
    pub status: Status,
    pub kind: Kind,
    /// End-result tag for a finished command pane (None for agents/active).
    pub outcome: Option<Outcome>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum PaneDisplay {
    Tracked {
        pane_id: u32,
        kind: Kind,
        status: Status,
        msg: String,
        outcome: Option<Outcome>,
    },
    Untracked {
        pane_id: u32,
        title: String,
    },
}

impl PaneDisplay {
    pub(crate) fn tracked(
        pane_id: u32,
        kind: Kind,
        status: Status,
        msg: String,
        outcome: Option<Outcome>,
    ) -> Self {
        Self::Tracked {
            pane_id,
            kind,
            status,
            msg,
            outcome,
        }
    }

    pub(crate) fn untracked(pane_id: u32, title: &str) -> Self {
        let title = if title.trim().is_empty() {
            "terminal".to_string()
        } else {
            title.to_string()
        };
        Self::Untracked { pane_id, title }
    }

    pub(crate) fn is_tracked(&self) -> bool {
        matches!(self, Self::Tracked { .. })
    }

    pub(crate) fn pane_id(&self) -> u32 {
        match self {
            Self::Tracked { pane_id, .. } | Self::Untracked { pane_id, .. } => *pane_id,
        }
    }

    pub(crate) fn status(&self) -> Option<Status> {
        match self {
            Self::Tracked { status, .. } => Some(*status),
            Self::Untracked { .. } => None,
        }
    }

    pub(crate) fn render_status(&self) -> Status {
        self.status().unwrap_or(Status::Idle)
    }

    pub(crate) fn kind(&self) -> Kind {
        match self {
            Self::Tracked { kind, .. } => *kind,
            Self::Untracked { .. } => Kind::Other,
        }
    }

    pub(crate) fn msg(&self) -> &str {
        match self {
            Self::Tracked { msg, .. } => msg,
            Self::Untracked { title, .. } => title,
        }
    }

    pub(crate) fn outcome(&self) -> Option<Outcome> {
        match self {
            Self::Tracked { outcome, .. } => *outcome,
            Self::Untracked { .. } => None,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TabDisplay {
    pub status: Status,
    pub progress: ProgressCounts,
    pub detail: Option<PrimaryDetail>,
    pub panes: Vec<PaneDisplay>,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct ProgressCounts {
    pub done: usize,
    pub total: usize,
    pub pending: usize,
}

/// Roll a tab's panes up into a single `TabDisplay`.
///
/// `resolve` maps a pane id to its resolved observation, if any. The caller owns
/// the precedence across observation sources (status pipe vs command); this
/// function only sees "is there an observation for this pane?".
///
/// A pane with no observation — or one that has never been active — renders as an
/// untracked pane and does not count toward `done/total`. `pending` is counted
/// whenever an observation reports `Pending`, active or not.
pub fn roll_up<'a>(
    panes: &[TerminalPane],
    resolve: impl Fn(u32) -> Option<&'a TrackedObservation>,
) -> TabDisplay {
    let mut best: Option<PrimaryDetail> = None;
    let mut done = 0usize;
    let mut total = 0usize;
    let mut pending = 0usize;
    let mut pane_displays = Vec::with_capacity(panes.len());

    for pane in panes {
        let Some(s) = resolve(pane.id) else {
            pane_displays.push(PaneDisplay::untracked(pane.id, &pane.title));
            continue;
        };

        if s.ever_active {
            total += 1;
            if s.status == Status::Done {
                done += 1;
            }
            pane_displays.push(PaneDisplay::tracked(
                pane.id,
                Kind::from_source(&s.source),
                s.status,
                s.msg.clone(),
                pane_outcome(s),
            ));
        } else {
            pane_displays.push(PaneDisplay::untracked(pane.id, &pane.title));
        }
        if s.status == Status::Pending {
            pending += 1;
        }
        // Most-urgent active pane wins, ties broken by most-recent change.
        // `Status: Ord` ranks severity, so this is a single lexicographic
        // `(status, tick)` compare — `>=` keeps the last pane on a full tie.
        if s.status.is_active() {
            let key = (s.status, s.last_change_tick);
            let wins = best
                .as_ref()
                .is_none_or(|d| key >= (d.status, d.since_tick));
            if wins {
                best = Some(PrimaryDetail {
                    repo: s.repo.clone(),
                    branch: s.branch.clone(),
                    msg: s.msg.clone(),
                    since_tick: s.last_change_tick,
                    status: s.status,
                    kind: Kind::from_source(&s.source),
                    outcome: pane_outcome(s),
                });
            }
        }
    }

    TabDisplay {
        status: best.as_ref().map_or(Status::Idle, |d| d.status),
        progress: ProgressCounts {
            done,
            total,
            pending,
        },
        detail: best,
        panes: pane_displays,
    }
}

/// Derive the end-result outcome tag for a pane, scoped to *command-origin*
/// panes — agents (status pipe) keep their hook msg with no tag. Done → `Ok`
/// (`✓`); Error → `Failed(exit_code)` (`(exit N)`, or `✗` when the code is
/// unknown). Returns `None` for active/idle panes and all agents.
fn pane_outcome(s: &TrackedObservation) -> Option<Outcome> {
    if s.origin != ObservationOrigin::Command {
        return None;
    }
    match s.status {
        Status::Done => Some(Outcome::Ok),
        Status::Error => Some(Outcome::Failed(s.exit_code)),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
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
            source: "build".into(),
            last_change_tick: tick,
            seq: None,
            on_focus: None,
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
                        source: "build".into(),
                        last_change_tick: tick,
                        seq: None,
                        on_focus: None,
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
                .filter(|o| o.status == Status::Pending).count();
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
}

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
    pub task: String,
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
        task: String,
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
        task: String,
        outcome: Option<Outcome>,
    ) -> Self {
        Self::Tracked {
            pane_id,
            kind,
            status,
            msg,
            task,
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

    pub(crate) fn task(&self) -> &str {
        match self {
            Self::Tracked { task, .. } => task,
            Self::Untracked { .. } => "",
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
            // Counted with `total`/`done`, not outside the gate: a pane excluded
            // from `total` (never ever_active, e.g. a snapshot-loaded row) must
            // not inflate `pending`, or progress reads inconsistent (pending > total).
            if s.status == Status::Pending {
                pending += 1;
            }
            pane_displays.push(PaneDisplay::tracked(
                pane.id,
                s.kind,
                s.status,
                s.msg.clone(),
                s.task.clone(),
                pane_outcome(s),
            ));
        } else {
            pane_displays.push(PaneDisplay::untracked(pane.id, &pane.title));
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
                    task: s.task.clone(),
                    since_tick: s.last_change_tick,
                    status: s.status,
                    kind: s.kind,
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
mod tests;

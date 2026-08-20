//! Tab Roll-Up: the per-pane → per-tab aggregation seam.
//!
//! Severity order `error > pending > running > done > idle`, with `done/total`
//! and `pending` counts and a highest-severity detail line. This is the domain
//! operation named "Tab Roll-Up" in `CONTEXT.md`: a deep, pure module that
//! turns a tab's panes plus a per-pane observation lookup into the `TabDisplay`
//! the rail renders. It owns the whole render-input vocabulary — `TabDisplay`,
//! `PaneDisplay`, `PrimaryDetail`, `ProgressCounts`, `Outcome`, plus the
//! rail-row types `TabRow`/`LedgerLine` and the topology record
//! `TerminalPane` — so the arrows run one way: `radar_state` builds these,
//! `render` consumes them, and neither imports the other.
//!
//! The "two sources, status wins" knowledge lives in the caller's `resolve`
//! closure — `roll_up` never learns there is more than one store, which keeps
//! the source seam (`StatusStore` / `CommandStore`) free to evolve.

use crate::kind::Kind;
use crate::observation::{ObservationOrigin, TrackedObservation};
use crate::status::Status;

/// One terminal pane of a tab's topology — the input record `roll_up`
/// aggregates and `RadarState` stores per tab.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct TerminalPane {
    pub id: u32,
    pub title: String,
    pub focused_in_tab: bool,
}

/// The end-result of a finished *command* pane, shown as a tag after the
/// activity (`cargo build exit 1`; `Ok` renders no tag — the line-1 status
/// glyph is the one done signal). Built in `rollup::roll_up`; agents never
/// carry one. Kept structured (not baked into
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
    /// Wall-clock stamp of the waiting-on-you edge (Pending only) — the
    /// renderer turns it into the `· 12m` wait tag against its own
    /// `now_epoch_s`, so no epoch threads through the roll-up itself.
    pub pending_epoch_s: Option<u64>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum PaneDisplay {
    Tracked {
        pane_id: u32,
        kind: Kind,
        origin: ObservationOrigin,
        status: Status,
        msg: String,
        task: String,
        since_tick: u64,
        outcome: Option<Outcome>,
        /// Waiting-on-you stamp (Pending only) — see `PrimaryDetail`.
        pending_epoch_s: Option<u64>,
        /// See `PrimaryDetail::acknowledged`.
        acknowledged: bool,
    },
    Untracked {
        pane_id: u32,
        title: String,
    },
    /// A pane whose foreground is an interactive command (editor/pager/TUI) —
    /// the Companion class (`docs/activity-model.md`): it waits on the user,
    /// so it is pane *context*, not activity. Renders as a muted identity
    /// label (`└ ○ $ nvim README.md`), never a spinner, and contributes
    /// nothing to counts, severity, or notifications. Built from the command
    /// store's quiet pending; shown only when no live observation outranks it
    /// (an active/attention observation keeps its `Tracked` row).
    Interactive {
        pane_id: u32,
        kind: Kind,
        msg: String,
    },
}

impl PaneDisplay {
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

    /// Whether this pane earns a line in the rail's pane roster: tracked
    /// panes (they have an observation to show) and interactive panes (their
    /// muted identity label IS the line). Untracked panes render nothing.
    pub(crate) fn earns_pane_line(&self) -> bool {
        matches!(self, Self::Tracked { .. } | Self::Interactive { .. })
    }

    pub(crate) fn is_interactive(&self) -> bool {
        matches!(self, Self::Interactive { .. })
    }

    pub(crate) fn pane_id(&self) -> u32 {
        match self {
            Self::Tracked { pane_id, .. }
            | Self::Untracked { pane_id, .. }
            | Self::Interactive { pane_id, .. } => *pane_id,
        }
    }

    pub(crate) fn status(&self) -> Option<Status> {
        match self {
            Self::Tracked { status, .. } => Some(*status),
            Self::Untracked { .. } | Self::Interactive { .. } => None,
        }
    }

    pub(crate) fn render_status(&self) -> Status {
        self.status().unwrap_or(Status::Idle)
    }

    pub(crate) fn kind(&self) -> Kind {
        match self {
            Self::Tracked { kind, .. } | Self::Interactive { kind, .. } => *kind,
            Self::Untracked { .. } => Kind::Other,
        }
    }

    pub(crate) fn msg(&self) -> &str {
        match self {
            Self::Tracked { msg, .. } | Self::Interactive { msg, .. } => msg,
            Self::Untracked { title, .. } => title,
        }
    }

    pub(crate) fn task(&self) -> &str {
        match self {
            Self::Tracked { task, .. } => task,
            Self::Untracked { .. } | Self::Interactive { .. } => "",
        }
    }

    /// The tick this pane's status last changed (`None` for an untracked
    /// pane, which has no observation to date it from). Feeds `spin_glyph`'s
    /// long-runner easing in `render.rs`.
    pub(crate) fn since_tick(&self) -> Option<u64> {
        match self {
            Self::Tracked { since_tick, .. } => Some(*since_tick),
            Self::Untracked { .. } | Self::Interactive { .. } => None,
        }
    }

    pub(crate) fn outcome(&self) -> Option<Outcome> {
        match self {
            Self::Tracked { outcome, .. } => *outcome,
            Self::Untracked { .. } | Self::Interactive { .. } => None,
        }
    }

    /// Waiting-on-you stamp (Pending only) — feeds `render::wait_tag`.
    pub(crate) fn pending_epoch_s(&self) -> Option<u64> {
        match self {
            Self::Tracked { pending_epoch_s, .. } => *pending_epoch_s,
            Self::Untracked { .. } | Self::Interactive { .. } => None,
        }
    }

    pub(crate) fn has_unacknowledged_status_pending(&self) -> bool {
        matches!(self, Self::Tracked {
            status: Status::Pending,
            acknowledged: false,
            origin: ObservationOrigin::StatusPipe,
            ..
        })
    }

    pub(crate) fn is_status_origin(&self) -> bool {
        matches!(self, Self::Tracked { origin: ObservationOrigin::StatusPipe, .. })
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
/// function only sees "is there an observation for this pane?". `quiet` maps a
/// pane id to its interactive foreground identity `(msg, kind)`, if any — the
/// command store's quiet pending (`docs/activity-model.md` §5).
///
/// A pane with no observation — or one that has never been active — renders as an
/// untracked pane and does not count toward `done/total`. `pending` is counted
/// whenever an observation reports `Pending`, active or not. A quiet identity
/// renders as [`PaneDisplay::Interactive`] whenever no live observation outranks
/// it: it replaces the Untracked face and an *Idle* observation's muted row
/// ("nvim" beats a stale finished-command echo), but never a non-idle one —
/// counts and severity always come from observations alone, so an open editor
/// can't read as work in `done/total`.
pub fn roll_up<'a, 'q>(
    panes: &[TerminalPane],
    resolve: impl Fn(u32) -> Option<&'a TrackedObservation>,
    quiet: impl Fn(u32) -> Option<(&'q str, Kind)>,
) -> TabDisplay {
    let mut best: Option<PrimaryDetail> = None;
    let mut done = 0usize;
    let mut total = 0usize;
    let mut pending = 0usize;
    let mut pane_displays = Vec::with_capacity(panes.len());

    let interactive = |pane_id: u32| {
        quiet(pane_id).map(|(msg, kind)| PaneDisplay::Interactive {
            pane_id,
            kind,
            msg: msg.to_string(),
        })
    };

    for pane in panes {
        let Some(s) = resolve(pane.id) else {
            let display = interactive(pane.id)
                .unwrap_or_else(|| PaneDisplay::untracked(pane.id, &pane.title));
            pane_displays.push(display);
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
            let display = if s.status == Status::Idle {
                interactive(pane.id)
            } else {
                None
            };
            pane_displays.push(display.unwrap_or_else(|| PaneDisplay::Tracked {
                pane_id: pane.id,
                kind: s.kind,
                origin: s.origin,
                status: s.status,
                msg: s.msg.clone(),
                task: s.task.clone(),
                since_tick: s.last_change_tick,
                outcome: pane_outcome(s),
                pending_epoch_s: s.pending_epoch_s,
                acknowledged: s.acknowledged,
            }));
        } else {
            let display = interactive(pane.id)
                .unwrap_or_else(|| PaneDisplay::untracked(pane.id, &pane.title));
            pane_displays.push(display);
        }
        // Most-urgent active pane wins; on equal severity a bounded *job*
        // outranks a *service* (a spinning build summarizes the tab better
        // than a dev server that is merely up — `docs/activity-model.md` §3);
        // remaining ties break by most-recent change. `Status: Ord` ranks
        // severity, so this is a single lexicographic `(status, job, tick)`
        // compare — `>=` keeps the last pane on a full tie.
        if s.status.is_active() {
            let key = (s.status, !s.kind.is_service(), s.last_change_tick);
            let wins = best
                .as_ref()
                .is_none_or(|d| key >= (d.status, !d.kind.is_service(), d.since_tick));
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
                    pending_epoch_s: s.pending_epoch_s,
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
/// (no tag; the line-1 status glyph is the one done signal); Error →
/// `Failed(exit_code)` (`exit N`, or `✗` when the code is unknown). Returns
/// `None` for active/idle panes and all agents.
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

/// One rail row as the renderer consumes it: the tab's identity bits plus its
/// rolled-up [`TabDisplay`]. Built by `RadarState::rows`; `render_rail` never
/// reaches past it into state.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TabRow {
    pub number: u32,
    pub name: String,
    pub active: bool,
    pub has_bell: bool,
    /// True for the two ticks after this tab's pane flipped from not-Pending
    /// to Pending (`RadarState::flash_until`) — the one-shot "ping" that
    /// outranks the active tint in `card_tint` in the renderer.
    pub flash: bool,
    pub display: TabDisplay,
}

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

#[cfg(test)]
mod tests;

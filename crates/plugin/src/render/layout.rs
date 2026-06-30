//! Pure layout planning for the rail: deciding which rows render and at how many
//! lines each, given the vertical budget and density. No ANSI, no `RailTarget` —
//! this module never builds a drawn line, so it cannot affect render/target
//! lockstep. `render_rail` consumes `plan_layout`; the rest is internal.

use crate::config::Density;
use crate::status::Status;

/// The ONE source of truth for inter- and intra-card spacing, by density.
///
/// Three knobs, all measured in terminal cells:
///   - `pad_x`: columns of internal LEFT padding inserted before a card's
///     content (so content isn't flush to the band edge).
///   - `pad_y`: rows of internal TOP padding — blank rows painted with THIS
///     card's own surface bg (internal breathing room). Currently 0 for all
///     densities; retained as a knob for future tuning.
///   - `gap`: rows of EXTERNAL separation after a card — blank rows painted
///     `rail_bg` in Cards (panel shows through), plain blank in Comfortable.
///     Sheds first under overflow.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct CardSpacing {
    pub(crate) pad_x: usize,
    pub(crate) pad_y: usize,
    pub(crate) gap: usize,
}

/// Map a density to its spacing knobs. This is the single place to tune the
/// sidebar's vertical/horizontal rhythm.
pub(crate) fn card_spacing(d: Density) -> CardSpacing {
    match d {
        Density::Compact => CardSpacing {
            pad_x: 0,
            pad_y: 0,
            gap: 0,
        },
        Density::Comfortable => CardSpacing {
            pad_x: 0,
            pad_y: 0,
            gap: 1,
        },
        Density::Cards => CardSpacing {
            pad_x: 0,
            pad_y: 0,
            gap: 1,
        },
    }
}

/// Carries the planner's view of a row: status (for compression priority)
/// and the pre-rendered line count sourced directly from the block that will be emitted.
pub(crate) struct RowMeta {
    pub(crate) status: Status,
    pub(crate) full_lines: usize,
}

/// Single source of truth for a card's full vertical footprint (top→bottom:
/// `pad_y` internal-pad rows + the card's uncompressed content rows + `gap`
/// external-separation rows). `render_rail()` budgets in terms of this so the
/// emitted ANSI lines and line targets stay exact.
pub(crate) fn card_block_lines(full_lines: usize, spacing: CardSpacing) -> usize {
    spacing.pad_y + full_lines + spacing.gap
}

/// Returns whether a row's status is "calm" (can be compressed first).
fn is_calm(status: Status) -> bool {
    matches!(status, Status::Done | Status::Running)
}

/// Decide which rows render and at how many lines each, given the vertical
/// budget. Returns `(Vec<(row_idx, rendered_lines)>, strip_folded_count)`.
///
/// Compression order when `sum(full lines of kept rows) > body_budget`:
///   1. Fold idle rows into a strip (existing behaviour).
///   2. Drop the strip line itself (set `strip_folded_count = 0`) if even the
///      non-idle rows exceed the budget.
///   3. Compress calm non-idle rows (Done, Running) to 1 line each —
///      lowest-position first, one at a time until it fits.
///   4. Compress urgent rows (Pending, Error) toward 1 line — drop msg line
///      first, then drop the branch/needs-you detail line — lowest-position
///      first, one step at a time.
///   5. If still over: include rows from the top as long as they fit; drop the
///      rest (never panics, never exceeds budget).
///
/// `full_lines` (from `RowMeta`) is the *uncompressed* line count; this function
/// produces the *planned* per-row line count actually rendered.
pub(crate) fn plan_overflow(rows: &[RowMeta], body_budget: usize) -> (Vec<(usize, usize)>, usize) {
    let total: usize = rows.iter().map(|r| r.full_lines).sum();
    if total <= body_budget {
        // Everything fits at full fidelity.
        let plan = rows
            .iter()
            .enumerate()
            .map(|(i, r)| (i, r.full_lines))
            .collect();
        return (plan, 0);
    }

    // Step 1: fold idle rows; keep non-idle at full line counts.
    let non_idle_idx: Vec<usize> = rows
        .iter()
        .enumerate()
        .filter(|(_, r)| r.status != Status::Idle)
        .map(|(i, _)| i)
        .collect();
    let folded_count = rows
        .iter()
        .filter(|r| r.status == Status::Idle)
        .count();

    // Each kept row starts at its full (uncompressed) line count.
    let mut planned: Vec<(usize, usize)> = non_idle_idx
        .iter()
        .map(|&i| (i, rows[i].full_lines))
        .collect();

    // Step 2: check if the strip line itself (1 extra line) would overflow.
    let strip_line: usize = if folded_count > 0 { 1 } else { 0 };
    let mut used: usize = planned.iter().map(|(_, l)| l).sum();
    // `strip_folded` is what we actually show in the strip (0 = dropped).
    let strip_folded = if used + strip_line <= body_budget {
        folded_count // strip fits
    } else {
        0 // drop the strip; don't count its line
    };
    // `used` now reflects whether the strip is shown.
    let strip_used = if strip_folded > 0 { 1 } else { 0 };

    if used + strip_used <= body_budget {
        return (planned, strip_folded);
    }

    // Step 3: compress calm rows (Done/Running) to 1 line, lowest-idx first.
    for entry in planned.iter_mut() {
        let (idx, ref mut lines) = *entry;
        if *lines <= 1 {
            continue;
        }
        if is_calm(rows[idx].status) {
            used -= *lines - 1; // account for the lines we're dropping
            *lines = 1;
            if used + strip_used <= body_budget {
                return (planned, strip_folded);
            }
        }
    }

    // Step 4: compress urgent rows (Pending/Error) toward 1 line.
    // Each urgent row: full -> drop msg -> drop branch/needs-you -> 1 line.
    // We decrement one line at a time per row (lowest-idx first), repeating
    // until the row reaches 1 or the budget is satisfied.
    let mut changed = true;
    while changed {
        changed = false;
        if used + strip_used <= body_budget {
            return (planned, strip_folded);
        }
        for entry in planned.iter_mut() {
            let (idx, ref mut lines) = *entry;
            if *lines <= 1 {
                continue;
            }
            if !is_calm(rows[idx].status) {
                used -= 1;
                *lines -= 1;
                changed = true;
                if used + strip_used <= body_budget {
                    return (planned, strip_folded);
                }
                // Restart from lowest-idx for next step.
                break;
            }
        }
    }

    // Step 5: everything is at 1 line; if still over, drop bottom rows that
    // don't fit. (Extreme case: even 1-line rows exceed the budget.)
    let mut trimmed: Vec<(usize, usize)> = Vec::new();
    let mut remaining = body_budget;
    for (idx, lines) in &planned {
        if *lines <= remaining {
            trimmed.push((*idx, *lines));
            remaining -= lines;
        } else if remaining > 0 {
            trimmed.push((*idx, remaining));
            remaining = 0;
        } else {
            break;
        }
    }
    (trimmed, strip_folded)
}

/// Single source of truth for the layout plan consumed by `render_rail()`.
/// Returns:
///   - the per-row planned content-line counts (same as `plan_overflow`),
///   - the number of idle rows folded into the strip (`strip_folded`), and
///   - the EFFECTIVE `CardSpacing` actually applied (luxury rows may be shed
///     under overflow): `pad_x` is unchanged; `pad_y` and `gap` are each 1 or 0.
///
/// Luxury-shedding rule (mirrors "gaps are dropped first"): the per-card block
/// is `pad_y + content + gap`. Under overflow we shed the cheapest separation
/// first — drop `gap`, then drop `pad_y` — before letting `plan_overflow`
/// compress the content itself. We pick the richest spacing whose total block
/// footprint (plus the strip line) still fits the budget.
pub(crate) fn plan_layout(
    rows: &[RowMeta],
    body_budget: usize,
    density: Density,
) -> (Vec<(usize, usize)>, usize, CardSpacing) {
    let base = card_spacing(density);

    // Fast path: if every row's FULL block (pad_y + content + gap) fits, render
    // everything at full fidelity with full spacing. `card_block_lines` is the
    // single footprint source shared with the budgeting below.
    let full_footprint: usize = rows
        .iter()
        .map(|r| card_block_lines(r.full_lines, base))
        .sum();
    if full_footprint <= body_budget {
        let plan = rows
            .iter()
            .enumerate()
            .map(|(i, r)| (i, r.full_lines))
            .collect();
        return (plan, 0, base);
    }

    // Candidate spacings, richest → leanest: full, then drop gap, then drop pad_y.
    // pad_x never sheds (it's a fixed horizontal inset, not a vertical row).
    let candidates = [
        base,
        CardSpacing { gap: 0, ..base },
        CardSpacing {
            gap: 0,
            pad_y: 0,
            ..base
        },
    ];

    for spacing in candidates {
        // Budget the content against the space left after this spacing's luxury
        // rows. Each kept row costs `pad_y + gap` luxury rows on top of content.
        let (plan, strip_folded) = plan_overflow(rows, body_budget);
        let content_total: usize = plan.iter().map(|(_, l)| l).sum();
        let kept = plan.len();
        let strip_line = if strip_folded > 0 { 1 } else { 0 };
        let luxury = kept * (spacing.pad_y + spacing.gap);
        if content_total + luxury + strip_line <= body_budget {
            return (plan, strip_folded, spacing);
        }
    }

    // Even the leanest spacing (no pad_y, no gap) overflows: let plan_overflow
    // compress content against the raw budget and apply no luxury rows.
    let lean = CardSpacing {
        gap: 0,
        pad_y: 0,
        ..base
    };
    let (plan, strip_folded) = plan_overflow(rows, body_budget);
    (plan, strip_folded, lean)
}

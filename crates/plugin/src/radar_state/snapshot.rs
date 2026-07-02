//! Snapshot (de)serialization for `RadarState`: the persisted v3 record (which
//! also carries the completion ledger), the v2/v1 legacy migrations, and the
//! live-pane merge.
//!
//! This is the self-contained "observations ⇄ JSON" concern lifted out of
//! `RadarState`. It is pure: `load` turns a snapshot string into resolved
//! observations, the snapshot tick, and the persisted ledger (or `None` for a
//! corrupt/unknown snapshot), and `to_json` turns the current observations —
//! merged with any still-live entries from an existing snapshot — plus this
//! instance's ledger back into the v3 record. Store routing (which store a
//! loaded observation belongs to) and the ledger's TTL re-base on load stay in
//! `RadarState`, which owns the stores; this module never learns there is more
//! than one store.

use crate::kind::Kind;
use crate::ledger::{Ledger, LedgerEntry};
use crate::observation::{ObservationOrigin, TrackedObservation};
use crate::status::Status;
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, HashSet};

const RADAR_SNAPSHOT_V: u32 = 3;
/// The pre-ledger record shape: identical wire format to v3 except the
/// `ledger` field is never present, so it loads through the very same struct
/// (`#[serde(default)]` fills it empty) — see `load_v2`.
const PRE_LEDGER_SNAPSHOT_V: u32 = 2;
const LEGACY_STATUS_SNAPSHOT_V: u32 = 1;

/// One v2 snapshot record: a `pane_id` key plus the pane's `TrackedObservation`
/// flattened inline. `TrackedObservation` serializes itself (enum fields as wire
/// tokens, optional fields defaulted), so this wrapper is the *only* snapshot
/// glue — there is no field-by-field mirror struct or mapper.
#[derive(Serialize, Deserialize)]
struct SnapshotEntry {
    pane_id: u32,
    #[serde(flatten)]
    obs: TrackedObservation,
}

#[derive(Serialize, Deserialize)]
struct RadarSnapshot {
    v: u32,
    tick: u64,
    observations: Vec<SnapshotEntry>,
    /// The completion ledger, added in v3. Absent in v2 records, so it
    /// defaults to empty on load rather than rejecting the snapshot.
    #[serde(default)]
    ledger: Vec<LedgerEntry>,
}

#[derive(Serialize, Deserialize)]
struct LegacyStatusSnapshot {
    v: u32,
    tick: u64,
    panes: Vec<LegacyPaneSnapshot>,
}

#[derive(Serialize, Deserialize)]
struct LegacyPaneSnapshot {
    pane_id: u32,
    status: String,
    repo: String,
    branch: String,
    msg: String,
    source: String,
    last_change_tick: u64,
    ever_active: bool,
}

/// Serialize the current observations into the v3 snapshot record, merging any
/// still-live entries from an `existing` snapshot (so a value another tab's
/// instance persisted is not lost when this one writes). `current` carries each
/// observation's `origin`, so the merge keys on `(pane_id, origin)` without this
/// module needing to know which store an observation came from. `ledger` is
/// this instance's own ring; it is unioned with the existing snapshot's ring
/// (if any) via `Ledger::merge` so no instance's completions are lost on write.
pub(crate) fn to_json<'a>(
    current: impl Iterator<Item = (u32, &'a TrackedObservation)>,
    live_panes: Option<&HashSet<u32>>,
    existing: Option<&str>,
    tick: u64,
    ledger: Vec<LedgerEntry>,
) -> String {
    let mut snapshot_tick = tick;
    let mut observations: BTreeMap<(u32, ObservationOrigin), TrackedObservation> = BTreeMap::new();
    let mut merged_ledger = ledger;

    if let Some(raw) = existing {
        if let Some((existing_observations, existing_tick, existing_ledger)) = load(raw) {
            snapshot_tick = snapshot_tick.max(existing_tick);
            for (pane_id, observation) in existing_observations {
                if live_panes.is_some_and(|live| !live.contains(&pane_id)) {
                    continue;
                }
                observations.insert((pane_id, observation.origin), observation);
            }
            merged_ledger = Ledger::merge(merged_ledger, existing_ledger);
        }
    }

    for (pane_id, observation) in current {
        observations.insert((pane_id, observation.origin), observation.clone());
    }

    let snapshot = RadarSnapshot {
        v: RADAR_SNAPSHOT_V,
        tick: snapshot_tick,
        observations: observations
            .into_iter()
            .map(|((pane_id, _), obs)| SnapshotEntry { pane_id, obs })
            .collect(),
        ledger: merged_ledger,
    };
    serde_json::to_string(&snapshot).unwrap_or_default()
}

/// Resolved observations, the snapshot tick, and the persisted ledger — what
/// every version-specific loader hands back to `load`.
type LoadedSnapshot = (Vec<(u32, TrackedObservation)>, u64, Vec<LedgerEntry>);

/// Parse a snapshot string into resolved observations, the snapshot tick, and
/// the persisted ledger. Dispatches on the `v` field: the current v3 record,
/// the pre-ledger v2 record (same struct shape, `ledger` defaults empty), or
/// the v1 legacy status-only snapshot (migrated to `TrackedObservation`s, no
/// ledger). Any other version, invalid JSON, or a corrupt entry yields `None`
/// (the whole snapshot is dropped) — this is the drop-forward guard: a future
/// version this build doesn't know is never partially trusted.
pub(crate) fn load(raw: &str) -> Option<LoadedSnapshot> {
    let value: serde_json::Value = serde_json::from_str(raw).ok()?;
    match value.get("v").and_then(serde_json::Value::as_u64)? as u32 {
        RADAR_SNAPSHOT_V => load_v3(value),
        PRE_LEDGER_SNAPSHOT_V => load_v2(value),
        LEGACY_STATUS_SNAPSHOT_V => load_legacy_status(value),
        _ => None,
    }
}

fn load_v3(value: serde_json::Value) -> Option<LoadedSnapshot> {
    // `TrackedObservation` deserializes itself; an entry with an unknown origin
    // fails deserialization, which drops the whole snapshot (`.ok()?`).
    let snapshot: RadarSnapshot = serde_json::from_value(value).ok()?;
    let observations = snapshot
        .observations
        .into_iter()
        .map(|entry| (entry.pane_id, entry.obs))
        .collect();
    Some((observations, snapshot.tick, snapshot.ledger))
}

/// A v2 (pre-ledger) record shares v3's exact struct shape — `ledger` is
/// simply absent on disk, so `#[serde(default)]` fills it empty rather than
/// rejecting the snapshot.
fn load_v2(value: serde_json::Value) -> Option<LoadedSnapshot> {
    load_v3(value)
}

fn load_legacy_status(value: serde_json::Value) -> Option<LoadedSnapshot> {
    let snapshot: LegacyStatusSnapshot = serde_json::from_value(value).ok()?;
    let observations = snapshot
        .panes
        .into_iter()
        .map(|pane| {
            (
                pane.pane_id,
                TrackedObservation {
                    origin: ObservationOrigin::StatusPipe,
                    status: Status::from_wire(&pane.status),
                    repo: pane.repo,
                    branch: pane.branch,
                    msg: pane.msg,
                    task: String::new(),
                    kind: Kind::from_source(&pane.source),
                    last_change_tick: pane.last_change_tick,
                    ever_active: pane.ever_active,
                    exit_code: None,
                    completed_epoch_s: None,
                },
            )
        })
        .collect();
    Some((observations, snapshot.tick, Vec::new()))
}

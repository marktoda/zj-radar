//! Snapshot (de)serialization for `RadarState`: the persisted v2 record, the v1
//! legacy migration, and the live-pane merge.
//!
//! This is the self-contained "observations ⇄ JSON" concern lifted out of
//! `RadarState`. It is pure: `load` turns a snapshot string into resolved
//! observations (or `None` for a corrupt/unknown snapshot), and `to_json`
//! turns the current observations — merged with any still-live entries from an
//! existing snapshot — back into the v2 record. Store routing (which store a
//! loaded observation belongs to) stays in `RadarState`, which owns the stores;
//! this module never learns there is more than one store.

use crate::kind::Kind;
use crate::observation::{ObservationOrigin, TrackedObservation};
use crate::status::Status;
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, HashSet};

const RADAR_SNAPSHOT_V: u32 = 2;
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

/// Serialize the current observations into the v2 snapshot record, merging any
/// still-live entries from an `existing` snapshot (so a value another tab's
/// instance persisted is not lost when this one writes). `current` carries each
/// observation's `origin`, so the merge keys on `(pane_id, origin)` without this
/// module needing to know which store an observation came from.
pub(crate) fn to_json<'a>(
    current: impl Iterator<Item = (u32, &'a TrackedObservation)>,
    live_panes: Option<&HashSet<u32>>,
    existing: Option<&str>,
    tick: u64,
) -> String {
    let mut snapshot_tick = tick;
    let mut observations: BTreeMap<(u32, ObservationOrigin), TrackedObservation> = BTreeMap::new();

    if let Some(raw) = existing {
        if let Some((existing_observations, existing_tick)) = load(raw) {
            snapshot_tick = snapshot_tick.max(existing_tick);
            for (pane_id, observation) in existing_observations {
                if live_panes.is_some_and(|live| !live.contains(&pane_id)) {
                    continue;
                }
                observations.insert((pane_id, observation.origin), observation);
            }
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
    };
    serde_json::to_string(&snapshot).unwrap_or_default()
}

/// Parse a snapshot string into resolved observations plus the snapshot tick.
/// Dispatches on the `v` field: the current v2 record, or the v1 legacy
/// status-only snapshot (migrated to `TrackedObservation`s). Any other version,
/// invalid JSON, or a corrupt entry yields `None` (the whole snapshot is dropped).
pub(crate) fn load(raw: &str) -> Option<(Vec<(u32, TrackedObservation)>, u64)> {
    let value: serde_json::Value = serde_json::from_str(raw).ok()?;
    match value.get("v").and_then(serde_json::Value::as_u64)? as u32 {
        RADAR_SNAPSHOT_V => load_v2(value),
        LEGACY_STATUS_SNAPSHOT_V => load_legacy_status(value),
        _ => None,
    }
}

fn load_v2(value: serde_json::Value) -> Option<(Vec<(u32, TrackedObservation)>, u64)> {
    // `TrackedObservation` deserializes itself; an entry with an unknown origin
    // fails deserialization, which drops the whole snapshot (`.ok()?`).
    let snapshot: RadarSnapshot = serde_json::from_value(value).ok()?;
    let observations = snapshot
        .observations
        .into_iter()
        .map(|entry| (entry.pane_id, entry.obs))
        .collect();
    Some((observations, snapshot.tick))
}

fn load_legacy_status(value: serde_json::Value) -> Option<(Vec<(u32, TrackedObservation)>, u64)> {
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
    Some((observations, snapshot.tick))
}

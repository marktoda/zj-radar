# Deepen RadarState - implementation record

> For implementation workers: use this plan task by task. Keep the branch scoped
> to the RadarState seam and do not reopen the sealed rail layout seam except for
> the display-model type it receives.

## Implementation status

Implemented on branch `radar-state-plan`.

- `RadarState` owns tabs, live terminal panes, observations, focus transitions,
  snapshot serialization, and tab rename ownership.
- Runtime now coordinates config, permission, timers, rendering, mouse effects,
  and host effects; it no longer owns pane-state maps.
- `model.rs`, `naming.rs`, and the old status `state.rs` module were deleted.
- The status-payload store is `status_store.rs::StatusStore`; command-derived
  observations remain in `command.rs::CommandStore`.
- The render-facing display model is explicit: `TabDisplay`, `PaneDisplay`,
  `ProgressCounts`, and `PrimaryDetail`.
- Every live terminal pane is represented. Untracked panes are rendered and
  click-targetable when expanded, but do not affect tab status or progress.
- Active tabs expand every live pane. Inactive summaries distinguish total copy
  (`2 working`) from hidden remainder copy (`+2 more working`).
- Collapsed rosters that include untracked terminals use generic pane copy
  (`2 panes`) rather than counting those terminals as `working` or `done`.
- Snapshots are `RadarSnapshot` v2, owned by `RadarState`, with legacy v1
  migration and merge-on-write through `SessionFiles`.

## Goal

Create one deep `RadarState` module for the plugin's session-state facts:
tabs, live terminal panes, tracked pane observations, focus transitions,
snapshot serialization, and rename ownership.

This fixes the multi-pane UX bugs by making the semantic pane roster explicit:
every live terminal pane is represented, active tabs show every live pane, and
inactive summaries distinguish total counts from hidden remainders.

## Pre-refactor friction

The previous implementation spread one concept across several shallow modules:

- `runtime.rs` owns `StatusStore`, `CommandStore`, `tabs`, `tab_panes`,
  `pane_cwd`, `applied_names`, `last_focused`, snapshot writes, and render row
  construction.
- `model.rs` aggregates `pane_ids + StatusStore + CommandStore` into `TabDisplay`,
  but it drops live panes that have no observation.
- `naming.rs` owns `PaneLite`, cwd/title/repo naming, and applied-name guards,
  but it sees a separate pane view from the rail.
- `render.rs` has a good sealed rail seam now, but it still receives old
  `TabDisplay` semantics and has to infer whether pane counts mean live, tracked,
  or hidden.

The deletion test says `model.rs` and the public `naming.rs` interface are not
earning their keep: deleting them does not remove complexity, it exposes the
same coordination work that is already in runtime. `StatusStore` and
`CommandStore` do earn their keep: deleting them would scatter source-specific
payload and command lifecycle rules.

Pre-refactor code anchors from the review:

- `runtime.rs` owned all pane-state maps on `PluginRuntime`.
- `runtime.rs::build_rows` converted live panes to ids and called
  `model::aggregate`, so row construction cannot see untracked live panes.
- `model.rs::aggregate` dropped panes when neither `StatusStore` nor
  `CommandStore` has an observation.
- `render.rs::pane_tree_plan` capped/deduped active calm panes despite
  comments and UX expectations that active tabs are inspectable.
- `naming.rs` owned `PaneLite` and applied-name logic even though the same
  topology is also needed for display, focus, and pruning.
- Zellij 0.44 exposes `TabInfo.tab_id` as stable tab identity, while
  `PaneInfo.is_focused` is "focused in its layer", not global session focus.

## Decisions

- `RadarState` is the deep module.
- The status-payload store and `CommandStore` stay as source-specific modules.
  The status-payload store is `StatusStore` in `status_store.rs`; it is not
  responsible for all plugin state.
- `model.rs` collapses into `RadarState`; aggregation becomes internal.
- The rail remains the sealed render module: `render_rail` and `onboarding`
  return `RenderedRail`.
- `TabRow` receives an explicit semantic display model.
- Every live terminal pane becomes one `PaneDisplay` entry.
- `Tracked` means there is a resolved radar observation from status payload or
  promoted command state.
- `Untracked` means live terminal pane with no resolved observation.
- Untracked panes do not affect tab status or progress.
- Pending command debounce remains untracked until promotion.
- Idle-but-ever-active panes remain tracked while live.
- Active tabs show every live pane.
- Inactive tabs expand urgent panes and summarize calm panes.
- Fully collapsed inactive summaries use totals like `2 working`; `+N more` is
  only a hidden remainder after visible child rows.
- Rename ownership moves into `RadarState` and is keyed by stable `tab_id`.
- Snapshot persistence includes resolved tracked observations from status
  payload and command state, with provenance. It excludes pending debounce and
  live topology.

## Target module map

```text
lib.rs              Zellij adapter: raw event facts in, host effects out
runtime.rs          permission/config/render orchestration, no pane-state maps
radar_state.rs      deep session-state module and its external seam
status_store.rs     status-payload observation store, formerly state.rs
command.rs          command-derived observation store
render.rs           sealed rail module; consumes semantic TabRow display data
session_files.rs    filesystem adapter for opaque snapshots/permission marker
```

The key direction is replace, not layer. `RadarState` should not wrap the old
`model::aggregate` and `naming::compute_renames`; it should absorb those concepts
and delete the public old interfaces.

## Type and terminology collapse

### Keep

- `RenderedRail`, `RailTarget`, `RenderOpts`, and `TabRow` stay in `render.rs`.
  They are the rail seam and are already deep.
- `CommandStore` stays in `command.rs`. It owns debounce, foreground command
  interpretation, exit status handling, and command clear-on-focus.
- `StatusStore` stays as a source-specific status-payload store, moved to
  `status_store.rs`.

### Replace

- `src/state.rs` -> `src/status_store.rs`.
  Reason: once `RadarState` exists, `state.rs` is too vague. The store applies
  the status pipe contract, not all plugin state.
- `AgentState` -> `TrackedObservation`.
  Reason: command-derived panes are not agents. This shared type should describe
  the resolved pane observation both stores expose.
- `PaneLite` -> `TerminalPane`.
  Reason: this is topology, not naming-specific data.
- `TabLite` -> `RadarTab`.
  Reason: the runtime-owned "lite" name hides that this is the repo-owned tab
  fact crossing the adapter seam.
- `TabAgg` -> `TabDisplay`.
  Reason: render needs a semantic display model, not an aggregation scratch type.
- `PaneEntry` -> `PaneDisplay`.
  Reason: the pane roster includes tracked and untracked live panes; "entry"
  does not say what the value means.
- `Detail` -> `PrimaryDetail`.
  Reason: the field is the highest-priority tracked observation for the tab.
- `PaneSnapshot`/`Snapshot` in `state.rs` -> `RadarSnapshot` owned by
  `RadarState`.
  Reason: snapshots persist resolved radar observations from multiple sources,
  not just the status-payload store.

### Delete

- Delete `model.rs` after its tests move to the `RadarState` seam.
- Delete or privatize the public `naming.rs` interface after its logic moves
  behind `RadarState`.
- Delete runtime fields that became `RadarState` internals: `status`, `command`,
  `tabs`, `tab_panes`, `pane_cwd`, `applied_names`, and `last_focused`.

## External seam

`RadarState` should present one small interface to runtime and tests:

```rust
pub(crate) struct RadarState;

pub(crate) enum RadarEvent {
    TabsChanged(Vec<RadarTab>),
    PanesChanged(PaneTopology),
    StatusPayload(StatusPayload),
    CwdChanged { pane_id: u32, path: String },
    CommandChanged { pane_id: u32, argv: Vec<String>, is_foreground: bool },
    Timer,
    NamingModeChanged(config::NamingMode),
}

pub(crate) struct RadarChange {
    pub render: bool,
    pub arm_timer: bool,
    pub persist_snapshot: bool,
    pub renames: Vec<TabRename>,
}

impl RadarState {
    pub(crate) fn load_snapshot(&mut self, raw: Option<&str>) -> Option<u64>;
    pub(crate) fn apply(&mut self, event: RadarEvent, tick: u64) -> RadarChange;
    pub(crate) fn rows(&self) -> Vec<render::TabRow>;
    pub(crate) fn snapshot_json(&self, existing: Option<&str>, tick: u64) -> String;
    pub(crate) fn has_active_or_pending_work(&self) -> bool;
}
```

The interface is intentionally event-shaped. Runtime should not know which inner
store receives a status payload, command event, focus transition, or cwd update.
The interface is the test surface.

## Core internal types

```rust
pub(crate) type TabId = usize;

pub(crate) struct RadarTab {
    pub id: TabId,
    pub position: usize,
    pub name: String,
    pub active: bool,
    pub has_bell: bool,
}

pub(crate) struct TerminalPane {
    pub pane_id: u32,
    pub title: String,
    pub focused_in_tab: bool,
}

pub(crate) struct PaneTopology {
    pub panes_by_position: HashMap<usize, Vec<TerminalPane>>,
    pub live_panes: HashSet<u32>,
    pub exits: Vec<(u32, Option<i32>)>,
}

pub(crate) enum ObservationOrigin {
    StatusPipe,
    Command,
}

pub(crate) struct TrackedObservation {
    pub origin: ObservationOrigin,
    pub status: Status,
    pub repo: String,
    pub branch: String,
    pub msg: String,
    pub source: String,
    pub last_change_tick: u64,
    pub seq: Option<u64>,
    pub on_focus: Option<Status>,
    pub ever_active: bool,
}
```

The renderer-facing types should remain in `render.rs`:

```rust
pub struct TabDisplay {
    pub status: Status,
    pub progress: ProgressCounts,
    pub detail: Option<PrimaryDetail>,
    pub panes: Vec<PaneDisplay>,
}

pub struct ProgressCounts {
    pub done: usize,
    pub total: usize,
    pub pending: usize,
}

pub enum PaneDisplay {
    Tracked {
        pane_id: u32,
        kind: Kind,
        status: Status,
        msg: String,
    },
    Untracked {
        pane_id: u32,
        title: String,
    },
}

pub struct PrimaryDetail {
    pub repo: String,
    pub branch: String,
    pub msg: String,
    pub since_tick: u64,
    pub status: Status,
    pub kind: Kind,
}
```

Important seam rule: render sees `Kind`, `Status`, and pane roster display
data. It does not see observation provenance, sequence numbers, snapshot merge
rules, command pending state, or rename ownership.

## Snapshot seam

`SessionFiles` remains a filesystem adapter. It does not know snapshot schema.

Use a v2 snapshot owned by `RadarState`:

```json
{
  "v": 2,
  "tick": 42,
  "observations": [
    {
      "pane_id": 12,
      "origin": "status_pipe",
      "status": "running",
      "repo": "zj-radar",
      "branch": "main",
      "msg": "reviewing",
      "source": "codex",
      "last_change_tick": 41,
      "seq": 9,
      "on_focus": null,
      "ever_active": true
    }
  ]
}
```

Load must accept old v1 `StatusStore` snapshots and migrate them as
`origin = status_pipe`.

Persist must merge with the latest on-disk snapshot before writing so stale
sidebar instances do not erase observations they missed. Keep `SessionFiles`
schema-free by changing the effect path:

```rust
enum Effect {
    PersistRadarSnapshot,
    // ...
}
```

When handling `PersistRadarSnapshot`, `State` should read the current opaque
snapshot from `SessionFiles`, call `runtime.snapshot_json(existing.as_deref())`,
then write the returned JSON with `SessionFiles::persist_snapshot`.

Merge rules:

- Pending command debounce is never serialized.
- Live topology is never serialized.
- Same pane and same origin: higher `seq` wins when both have `seq`; otherwise
  higher `last_change_tick` wins.
- Same pane, different origin: `StatusPipe` wins over `Command`. This preserves
  the existing "pipe has spoken for this pane" rule.
- If live topology has been seen and the local store no longer has an
  observation for a pane because the pane was pruned as not live, the merged
  snapshot must also drop it. Do not resurrect known-dead panes from disk.
  Before the first `PaneUpdate`, do not infer death from absence.

## Implementation tasks

### Task 1: Add RadarState glossary and plan

Files:
- Modify `CONTEXT.md`
- Add this plan

Status:
- Done in this branch as the planning foundation.

### Task 2: Introduce observation vocabulary without changing behavior

Files:
- Create `src/observation.rs`
- Rename `src/state.rs` to `src/status_store.rs`
- Modify `src/command.rs`
- Modify imports in `src/lib.rs`, `src/runtime.rs`, tests

Steps:
- Move `TrackedObservation::apply_on_focus` semantics to `TrackedObservation`.
- Rename `StatusStore` to `StatusStore`.
- Make `StatusStore` and `CommandStore` expose `get(pane_id) ->
  Option<&TrackedObservation>`.
- Preserve all existing status-store and command-store tests under the new names.
- Do not add `RadarState` in this task.

Expected tests:
- `cargo test --all-features status_store command -- --nocapture`
- `cargo test --all-features --lib`

### Task 3: Create RadarState with current behavior

Files:
- Create `src/radar_state.rs`
- Modify `src/lib.rs` module list
- Modify `src/runtime.rs` to delegate state events to `RadarState`

Steps:
- Move `tabs`, `tab_panes`, `pane_cwd`, `applied_names`, `last_focused`,
  `StatusStore`, and `CommandStore` into `RadarState`.
- Keep render-facing output temporarily compatible with current `TabRow` shape.
- Move current `model::aggregate` logic behind `RadarState::rows`.
- Move current naming logic behind `RadarState`.
- Runtime keeps permission flow, config, theme, render cache, mouse click
  effects, and host effect assembly.

Expected tests:
- Existing runtime tests still pass.
- Add one seam test that drives `RadarState::apply` through tabs, panes, status
  payload, command change, timer, cwd change, and rename.

### Task 4: Add stable tab identity

Files:
- Modify `src/lib.rs`
- Modify `src/radar_state.rs`
- Modify runtime/lib tests

Steps:
- Include `TabInfo.tab_id` in `RadarTab`.
- Store tabs by `TabId` internally and maintain order by current `position`.
- Key rename ownership by `TabId`.
- Continue emitting host effects by current tab `position` because existing
  host calls use position.
- Map `PaneManifest` positions to latest known `TabId`. If panes arrive before
  tabs, keep them in a small `pending_panes_by_position` map and resolve on the
  next tab update rather than dropping them.

Expected tests:
- Reordering tabs does not transfer applied rename ownership.
- Panes arriving before tabs are rendered after the next tab update.

### Task 5: Fix active-tab and untracked-pane semantics at RadarState seam

Files:
- Modify `src/radar_state.rs`
- Modify `src/render.rs` display model types
- Delete `src/model.rs` when no longer referenced

Steps:
- Replace `TabDisplay` with `render::TabDisplay`.
- Replace `PaneDisplay` with `render::PaneDisplay`.
- Build one `PaneDisplay` for every live `TerminalPane`.
- Resolve tracked observations by `StatusStore.get(id).or(CommandStore.get(id))`.
- Use `PaneDisplay::Untracked` when neither store has a resolved observation.
- Exclude untracked panes from status severity and progress counts.
- Keep idle-but-ever-active observations as tracked.
- Active tab display must include all live panes, with no dedupe.

Expected tests:
- Active tab with two identical running Codex panes shows two child targets.
- Active tab with Codex and Claude panes both running shows two child targets.
- Tab with two live panes and one tracked observation has two pane displays, one
  tracked and one untracked.
- Untracked pane does not change tab status/progress.

### Task 6: Update rail copy for total vs hidden remainder

Files:
- Modify `src/render.rs`

Steps:
- Keep `render_rail` as the only rail seam.
- Update private pane-tree planning to operate on `PaneDisplay`.
- Active tabs expand all live panes.
- Inactive tabs expand `Pending` and `Error` tracked panes.
- Inactive fully collapsed calm roster shows total copy, for example
  `2 working`.
- Inactive mixed urgent/calm roster shows hidden remainder copy, for example
  `+2 more working`.
- Untracked rows render muted and click-targetable when expanded. Collapsed
  rosters that include any untracked pane use generic pane copy (`N panes`) so
  untracked terminals are not counted as `working` or `done`.

Expected tests:
- Fully collapsed inactive two-running tab renders `2 working`, not
  `+2 working`.
- Inactive one pending plus two running renders the pending child and
  `+2 more working`.
- Active tab duplicate calm panes have separate child lines and pane targets.
- Existing lockstep proptest remains the authority for target line correctness.

### Task 7: Move snapshot schema to RadarState

Files:
- Modify `src/radar_state.rs`
- Modify `src/runtime.rs`
- Modify `src/lib.rs`
- Modify `src/session_files.rs`
- Remove snapshot structs from `status_store.rs`

Steps:
- Add `RadarSnapshot` v2 serialization in `RadarState`.
- Migrate v1 `StatusStore` snapshots on load.
- Persist status-pipe and command resolved observations with provenance.
- Exclude pending command debounce and live topology.
- Add `SessionFiles::snapshot()` or equivalent read method that returns the
  current opaque snapshot.
- Change effect handling to merge on write through `RadarState`, keeping
  `SessionFiles` schema-free.

Expected tests:
- v1 snapshot rehydrates as status-pipe observations.
- v2 snapshot round-trips both status-pipe and command observations.
- Two stale instances with disjoint observations do not erase each other on
  persist.
- A pruned dead pane is not resurrected by merge-on-write.
- A sidebar that has loaded a snapshot but has not yet seen `PaneUpdate` does
  not erase on-disk observations just because they are absent locally.

### Task 8: Delete old public modules and stale tests

Files:
- Delete `src/model.rs`
- Either delete `src/naming.rs` or make it private under `radar_state`
- Modify docs that describe `model::aggregate`

Steps:
- Move surviving model tests to `radar_state` seam tests.
- Move surviving naming tests to `RadarState` rename tests.
- Update `CONTEXT.md` if the final names differ from this plan.
- Search source for deleted concepts: `TabAgg`, `PaneEntry`, `PaneLite`,
  `AgentState`, `model::`, `naming::`, `mod model`, `mod naming`, and
  payload-bearing `PersistSnapshot(...)`.

Expected tests:
- `rg -n "TabAgg|PaneEntry|PaneLite|AgentState|model::|naming::|mod model|mod naming|PersistSnapshot\\(" src`
  should be empty.
- `cargo test --all-features`
- `cargo build --release --target wasm32-wasip1`

## Runtime after refactor

Runtime should still be a pure module, but it should be shallower and more
focused:

- permission flow
- config parsing
- theme storage
- tick incrementing
- timer arming effect assembly
- render option assembly
- last `RenderedRail` cache
- mouse-click to host effect mapping

Runtime should not own:

- live pane topology
- tracked observations
- pane cwd map
- tab rename ownership
- focus transition semantics
- snapshot schema
- row aggregation

## Fresh-eyes self review

### 1. Does `RadarState` become too large?

It will be larger than the current `model.rs`, but the interface is much
smaller than the behavior it hides. The implementation has natural internal
sections: topology, observation resolution, naming, snapshots, and row
construction. Those are internal seams, not external ones. The external seam is
`apply`, `rows`, `snapshot_json`, and `has_active_or_pending_work`.

Verdict: acceptable depth. Do not split into public `PaneLedger`,
`SessionTopology`, and `PaneRosterPolicy` modules unless the implementation
proves unmaintainable after the first extraction.

### 2. Are we adding types without deleting old ones?

The plan intentionally deletes or renames the old terms:

- `TabDisplay`, `PaneDisplay`, `PrimaryDetail` disappear into render-facing
  `TabDisplay`, `PaneDisplay`, `PrimaryDetail`.
- `PaneLite` disappears into `TerminalPane`.
- `TabLite` disappears into `RadarTab`.
- `TrackedObservation` disappears into `TrackedObservation`.
- `StatusStore` becomes `StatusStore`.
- Snapshot structs leave the status-payload store and move into `RadarState`.

Verdict: this is a replacement plan, not a layering plan.

### 3. Should `TrackedObservation` live in `RadarState` instead of
`observation.rs`?

Putting it directly in `RadarState` would make `CommandStore` depend on
`RadarState`, which inverts the direction: source-specific stores should not
depend on the coordinating module. A tiny vocabulary module is justified because
there are two real adapters/producers for the same resolved observation shape:
status pipe and command tracking.

Verdict: `observation.rs` is a real shared vocabulary module, not speculative
indirection.

### 4. Should `StatusStore` and `CommandStore` be collapsed into one store?

No. They have different source rules:

- status payloads use `seq` and status-pipe sanitization.
- commands use debounce, foreground interpretation, exit dedupe, and cwd-derived
  repo labels.

Collapsing them would produce a single store with many source-condition branches
and worse locality.

Verdict: keep both stores, expose the same observation type.

### 5. Should render own `PaneDisplay::Untracked`?

Yes. The rail owns how to draw an untracked pane, but not how to decide whether
a pane is untracked. That decision belongs to `RadarState`, where live topology
and observations meet.

Verdict: `PaneDisplay` in render is correct. It is part of the rail interface
because the rail needs it to draw and target pane rows.

### 6. Does snapshot merge leak filesystem concerns into `RadarState`?

No, as long as `RadarState` only receives opaque existing snapshot text and
returns new JSON. `SessionFiles` reads and writes bytes; `RadarState` owns schema
and merge semantics. Runtime coordinates the two.

Verdict: tight enough. Avoid putting merge rules in `SessionFiles`.

### 7. Is stable `tab_id` worth the churn?

Yes. Rename ownership keyed by position is a real correctness risk when tabs
move. Zellij 0.44 exposes `TabInfo.tab_id`, and effects can still use position.

Verdict: use `TabId` internally, emit position effects at the host edge.

### 8. What is the highest-risk slice?

Task 5 and Task 6 are the risky behavioral slices because they change render
semantics and snapshots. Keep Task 3 behavior-preserving first so failures are
localized. Then use seam tests to assert the new user-facing behavior before
editing render copy.

Verdict: do not combine behavior-preserving extraction with UX behavior changes.

### 9. What should not be changed in this refactor?

- Do not change Zellij host action behavior except as needed for tab ids in
  internal state.
- Do not change the status pipe wire contract.
- Do not change `CommandStore` debounce behavior.
- Do not refactor the internal layout machinery of `render.rs` beyond replacing
  old display types and copy semantics.
- Do not move `SessionFiles` into `RadarState`.

Verdict: the seam stays narrow and the blast radius is controlled.

## Acceptance criteria

- The worktree compiles and tests on host:
  `cargo test --all-features`
- The wasm target builds:
  `cargo build --release --target wasm32-wasip1`
- The rail lockstep property still passes.
- Active multi-pane tabs show and target every live pane.
- Inactive fully collapsed counts read as totals, not hidden remainders.
- Live untracked panes do not disappear from the pane roster.
- Rename ownership survives tab reorder because it is keyed by stable `tab_id`.
- Snapshot rehydration works for both old v1 and new v2 snapshots.
- `SessionFiles` remains schema-free.

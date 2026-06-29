//! Session radar state: live tabs/panes plus source-specific observations.
//! No zellij-tile dependency.

use crate::command::CommandStore;
use crate::config;
use crate::observation::{ObservationOrigin, TrackedObservation};
use crate::payload;
use crate::render::TabRow;
use crate::rollup::{self, TabDisplay};
use crate::status::Status;
use crate::status_store::StatusStore;
use crate::theme;
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, HashMap, HashSet};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub(crate) struct TabId(usize);

impl TabId {
    pub(crate) fn new(raw: usize) -> Self {
        Self(raw)
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct RadarTab {
    pub id: TabId,
    pub position: usize,
    pub name: String,
    pub active: bool,
    pub has_bell: bool,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct TerminalPane {
    pub id: u32,
    pub title: String,
    pub focused_in_tab: bool,
}

#[derive(Debug)]
pub(crate) struct PaneUpdate {
    pub tab_panes: HashMap<usize, Vec<TerminalPane>>,
    pub live: HashSet<u32>,
    pub theme: Option<theme::DerivedColors>,
    pub exits: Vec<(u32, Option<i32>)>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct TabRename {
    pub position: usize,
    pub name: String,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct RadarChange {
    pub render: bool,
    pub persist_snapshot: bool,
    pub renames: Vec<TabRename>,
    /// Terminal panes whose working directory should be read once (via a
    /// blocking `get_pane_cwd` host call in the wasm glue) to bootstrap a name
    /// for a freshly-opened tab that has not emitted a `CwdChanged` yet.
    pub cwd_bootstrap: Vec<u32>,
}

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
    #[serde(default)]
    seq: Option<u64>,
    #[serde(default)]
    on_focus: Option<String>,
    ever_active: bool,
}

const RADAR_SNAPSHOT_V: u32 = 2;
const LEGACY_STATUS_SNAPSHOT_V: u32 = 1;

/// Upper bound on the number of one-shot `get_pane_cwd` reads requested per
/// `PaneUpdate`. Each read is a blocking host round-trip, so we cap a single
/// update's fan-out (e.g. a session restore surfacing many panes at once),
/// resolving focused panes first. The overflow is picked up on the next
/// `PaneUpdate` that occurs — so in the rare case of a large burst followed by
/// total inactivity, the tail stays unnamed until the next interaction. This is
/// the postmortem's "cap concurrent in-flight host calls" rule; the cap is
/// generous enough that opening tabs one at a time never hits it.
const MAX_CWD_BOOTSTRAP_PER_UPDATE: usize = 8;

#[derive(Default)]
pub(crate) struct RadarState {
    status: StatusStore,
    command: CommandStore,
    tabs: Vec<RadarTab>,
    tab_panes: HashMap<usize, Vec<TerminalPane>>,
    pane_cwd: HashMap<u32, String>,
    applied_names: HashMap<TabId, String>,
    last_focused: Option<u32>,
    live_panes: Option<HashSet<u32>>,
    /// Pane ids we have already requested an initial `get_pane_cwd` read for.
    /// Tracks *attempts*, not successes, so a pane that has no cwd yet is never
    /// re-polled; pruned with `pane_cwd` so a recycled id can bootstrap again.
    cwd_bootstrap_attempted: HashSet<u32>,
}

impl RadarState {
    pub(crate) fn load_snapshot(&mut self, raw: &str) -> Option<u64> {
        let (observations, tick) = parse_snapshot(raw)?;
        self.status = StatusStore::default();
        self.command = CommandStore::default();
        // This match is the SINGLE origin→store guard: each entry's intrinsic
        // origin (strict on deserialize) routes it to exactly one store, so the
        // stores trust what they're handed and don't re-check. Deserialize already
        // rejects unknown origins, dropping the whole snapshot.
        for (pane_id, observation) in observations {
            match observation.origin {
                ObservationOrigin::StatusPipe => self
                    .status
                    .insert_snapshot_observation(pane_id, observation),
                ObservationOrigin::Command => self
                    .command
                    .insert_snapshot_observation(pane_id, observation),
            }
        }
        Some(tick)
    }

    pub(crate) fn snapshot_json(&self, existing: Option<&str>, tick: u64) -> String {
        let mut snapshot_tick = tick;
        let mut observations: BTreeMap<(u32, ObservationOrigin), TrackedObservation> =
            BTreeMap::new();

        if let Some(raw) = existing {
            if let Some((existing_observations, existing_tick)) = parse_snapshot(raw) {
                snapshot_tick = snapshot_tick.max(existing_tick);
                for (pane_id, observation) in existing_observations {
                    if self
                        .live_panes
                        .as_ref()
                        .is_some_and(|live| !live.contains(&pane_id))
                    {
                        continue;
                    }
                    observations.insert((pane_id, observation.origin), observation);
                }
            }
        }

        for (pane_id, observation) in self.status.observations() {
            observations.insert(
                (pane_id, ObservationOrigin::StatusPipe),
                observation.clone(),
            );
        }
        for (pane_id, observation) in self.command.observations() {
            observations.insert((pane_id, ObservationOrigin::Command), observation.clone());
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

    pub(crate) fn rows(&self) -> Vec<TabRow> {
        let mut rows = Vec::new();
        let mut sorted = self.tabs.clone();
        sorted.sort_by_key(|t| t.position);
        for t in &sorted {
            let empty = Vec::new();
            let panes = self.tab_panes.get(&t.position).unwrap_or(&empty);
            rows.push(TabRow {
                number: t.position as u32 + 1,
                name: t.name.clone(),
                active: t.active,
                has_bell: t.has_bell,
                display: self.tab_display(panes),
            });
        }
        rows
    }

    pub(crate) fn tabs_changed(&mut self, tabs: Vec<RadarTab>) -> RadarChange {
        self.tabs = tabs;
        RadarChange {
            render: true,
            ..RadarChange::default()
        }
    }

    pub(crate) fn panes_changed(
        &mut self,
        update: PaneUpdate,
        tick: u64,
        naming: config::NamingMode,
    ) -> RadarChange {
        for (pane_id, exit_status) in update.exits {
            self.command.on_exit(pane_id, exit_status, tick);
        }
        self.live_panes = Some(update.live.clone());
        self.tab_panes = update.tab_panes;
        self.status.prune(&update.live);
        self.command.prune(&update.live);
        self.pane_cwd.retain(|id, _| update.live.contains(id));
        self.cwd_bootstrap_attempted
            .retain(|id| update.live.contains(id));
        self.apply_focus_transition(self.focused_terminal_in_active_tab(), tick);

        RadarChange {
            render: true,
            persist_snapshot: true,
            renames: self.rename_tabs(naming),
            cwd_bootstrap: self.cwd_bootstrap_targets(naming),
        }
    }

    pub(crate) fn timer(&mut self, tick: u64) {
        self.command.on_timer(tick);
    }

    pub(crate) fn cwd_changed(
        &mut self,
        pane_id: u32,
        path: String,
        naming: config::NamingMode,
    ) -> RadarChange {
        self.pane_cwd.insert(pane_id, path);
        RadarChange {
            render: true,
            renames: self.rename_tabs(naming),
            ..RadarChange::default()
        }
    }

    pub(crate) fn command_changed(
        &mut self,
        pane_id: u32,
        command: &[String],
        is_foreground: bool,
        tick: u64,
    ) -> RadarChange {
        let cwd = self.pane_cwd.get(&pane_id).map(String::as_str);
        self.command
            .on_command_changed(pane_id, command, is_foreground, cwd, tick);
        RadarChange {
            render: true,
            ..RadarChange::default()
        }
    }

    pub(crate) fn status_pipe(
        &mut self,
        raw: &str,
        tick: u64,
        naming: config::NamingMode,
    ) -> Option<RadarChange> {
        let p = payload::parse(raw)?;
        self.status.apply(p, tick);
        Some(RadarChange {
            render: true,
            persist_snapshot: true,
            renames: self.rename_tabs(naming),
            cwd_bootstrap: Vec::new(),
        })
    }

    pub(crate) fn has_active_or_pending_work(&self) -> bool {
        self.status.any_active() || self.command.has_pending_or_active()
    }

    pub(crate) fn recompute_renames(&mut self, naming: config::NamingMode) -> Vec<TabRename> {
        self.rename_tabs(naming)
    }

    pub(crate) fn apply_focus_transition(&mut self, focused: Option<u32>, tick: u64) -> bool {
        if focused == self.last_focused {
            return false;
        }
        self.last_focused = focused;
        if let Some(id) = focused {
            self.status.on_pane_focused(id, tick);
            self.command.on_pane_focused(id, tick);
        }
        true
    }

    pub(crate) fn last_focused(&self) -> Option<u32> {
        self.last_focused
    }

    #[cfg(test)]
    pub(crate) fn status(&self, pane_id: u32) -> Option<&crate::observation::TrackedObservation> {
        self.status.get(pane_id)
    }

    #[cfg(test)]
    pub(crate) fn command(&self, pane_id: u32) -> Option<&crate::observation::TrackedObservation> {
        self.command.get(pane_id)
    }

    #[cfg(test)]
    pub(crate) fn status_mut(&mut self) -> &mut StatusStore {
        &mut self.status
    }

    #[cfg(test)]
    pub(crate) fn status_store(&self) -> &StatusStore {
        &self.status
    }

    #[cfg(test)]
    pub(crate) fn command_mut(&mut self) -> &mut CommandStore {
        &mut self.command
    }

    #[cfg(test)]
    pub(crate) fn command_store(&self) -> &CommandStore {
        &self.command
    }

    #[cfg(test)]
    pub(crate) fn set_last_focused(&mut self, pane_id: Option<u32>) {
        self.last_focused = pane_id;
    }

    #[cfg(test)]
    pub(crate) fn set_tab_panes_for_position(&mut self, position: usize, panes: Vec<TerminalPane>) {
        self.tab_panes.insert(position, panes);
    }

    #[cfg(test)]
    pub(crate) fn applied_name(&self, tab_id: TabId) -> Option<&str> {
        self.applied_names.get(&tab_id).map(String::as_str)
    }

    fn focused_terminal_in_active_tab(&self) -> Option<u32> {
        let active = self.tabs.iter().find(|tab| tab.active)?;
        let panes = self.tab_panes.get(&active.position)?;
        panes
            .iter()
            .find(|pane| pane.focused_in_tab)
            .map(|pane| pane.id)
    }

    /// Live terminal panes whose cwd we have neither learned (via `CwdChanged`)
    /// nor yet attempted to read. Focused panes come first — they name their tab
    /// — and the result is capped at `MAX_CWD_BOOTSTRAP_PER_UPDATE` so one update
    /// never fans out an unbounded number of blocking host calls. Every returned
    /// id is recorded as attempted, so it is requested at most once per lifetime.
    ///
    /// Bootstrap exists only to name tabs, so it is a no-op (and pays for no
    /// blocking reads) when naming is `Off` — mirroring `rename_tabs`.
    fn cwd_bootstrap_targets(&mut self, naming_mode: config::NamingMode) -> Vec<u32> {
        if naming_mode == config::NamingMode::Off {
            return Vec::new();
        }
        let mut focused = Vec::new();
        let mut others = Vec::new();
        for panes in self.tab_panes.values() {
            for p in panes {
                if self.pane_cwd.contains_key(&p.id)
                    || self.cwd_bootstrap_attempted.contains(&p.id)
                {
                    continue;
                }
                if p.focused_in_tab {
                    focused.push(p.id);
                } else {
                    others.push(p.id);
                }
            }
        }
        // Deterministic order regardless of HashMap iteration; focused first.
        focused.sort_unstable();
        others.sort_unstable();
        let targets: Vec<u32> = focused
            .into_iter()
            .chain(others)
            .take(MAX_CWD_BOOTSTRAP_PER_UPDATE)
            .collect();
        for id in &targets {
            self.cwd_bootstrap_attempted.insert(*id);
        }
        targets
    }

    fn rename_tabs(&mut self, naming_mode: config::NamingMode) -> Vec<TabRename> {
        if naming_mode == config::NamingMode::Off {
            return Vec::new();
        }
        let force = naming_mode == config::NamingMode::Force;
        let mut out = Vec::new();
        let tabs: Vec<RadarTab> = self.tabs.clone();
        for tab in tabs {
            let empty = Vec::new();
            let panes = self.tab_panes.get(&tab.position).unwrap_or(&empty);
            let ours = self.applied_names.get(&tab.id) == Some(&tab.name);
            // Stickiness: a name we applied stays put as long as some pane still
            // justifies it, so moving focus between panes in different repos does
            // not flip the tab name. We only re-pick once no pane supports it
            // (e.g. the pane that named the tab closed).
            if ours && self.name_supported(panes, &tab.name) {
                continue;
            }
            let Some(desired) = self.computed_name(panes) else {
                continue;
            };
            if desired == tab.name {
                continue;
            }
            if force || is_default_name(&tab.name) || ours {
                self.applied_names.insert(tab.id, desired.clone());
                out.push(TabRename {
                    position: tab.position,
                    name: desired,
                });
            }
        }
        out
    }

    /// The ordered space of names this tab could take, highest priority first:
    /// the focused pane's repo, then any pane's repo, then focused/any
    /// worktree-resolved cwd, then focused/any pane title. `computed_name` takes
    /// the first; `name_supported` asks whether a name sits anywhere in this
    /// space. Deriving both from this one list is what keeps applied-name
    /// stickiness (`name_supported`) in lockstep with what the renamer would
    /// actually pick (`computed_name`) — they cannot disagree about the candidate
    /// space because there is only one.
    fn name_candidates(&self, panes: &[TerminalPane]) -> Vec<String> {
        let repo_of = |p: &TerminalPane| {
            self.status
                .get(p.id)
                .map(|s| s.repo.clone())
                .filter(|r| !r.is_empty())
        };
        let worktree_of =
            |p: &TerminalPane| self.pane_cwd.get(&p.id).and_then(|cwd| worktree_repo_dir(cwd));
        let title_of = |p: &TerminalPane| title_name(&p.title);
        let focused = panes.iter().find(|p| p.focused_in_tab);

        let mut out = Vec::new();
        out.extend(focused.and_then(&repo_of));
        out.extend(panes.iter().filter_map(&repo_of));
        out.extend(focused.and_then(&worktree_of));
        out.extend(panes.iter().filter_map(&worktree_of));
        out.extend(focused.and_then(&title_of));
        out.extend(panes.iter().filter_map(&title_of));
        out
    }

    /// The tab's preferred name: the top of [`Self::name_candidates`].
    fn computed_name(&self, panes: &[TerminalPane]) -> Option<String> {
        self.name_candidates(panes).into_iter().next()
    }

    /// Does any pane still justify `name`? True when `name` is anywhere in
    /// [`Self::name_candidates`] — used to keep an applied name "sticky" so focus
    /// changes between panes don't churn it.
    fn name_supported(&self, panes: &[TerminalPane], name: &str) -> bool {
        self.name_candidates(panes).iter().any(|c| c == name)
    }

    /// Roll this tab's panes up into a `TabDisplay`. The "status wins over
    /// command" precedence across observation sources lives here, with the
    /// stores; `rollup::roll_up` only sees "is there an observation for this
    /// pane?" — keeping the aggregation rules behind the Tab Roll-Up seam.
    fn tab_display(&self, panes: &[TerminalPane]) -> TabDisplay {
        rollup::roll_up(panes, |id| {
            self.status.get(id).or_else(|| self.command.get(id))
        })
    }
}

fn title_name(title: &str) -> Option<String> {
    let trimmed = title.trim_start();
    let stable = strip_activity_prefix(trimmed).trim();
    if stable.is_empty() {
        None
    } else {
        Some(stable.to_string())
    }
}

fn is_default_name(name: &str) -> bool {
    name.strip_prefix("Tab #")
        .is_some_and(|rest| !rest.is_empty() && rest.chars().all(|c| c.is_ascii_digit()))
}

fn cwd_basename(path: &str) -> Option<String> {
    let trimmed = path.trim_end_matches('/');
    if trimmed.is_empty() {
        return None;
    }
    trimmed
        .rsplit('/')
        .find(|s| !s.is_empty())
        .map(str::to_string)
}

/// Path markers for agent-managed git worktrees created under the standard
/// conventions: Claude (`<repo>/.claude/worktrees/<branch>`) and Codex
/// (`<repo>/.Codex/worktrees/<branch>`). A cwd sitting inside such a worktree
/// belongs, conceptually, to the parent repo — so naming should follow the repo,
/// not the (per-branch) worktree directory. Mirrors the CLI's git-common-dir
/// resolution (`cli::notify`) without needing a `git` process from inside wasm.
const WORKTREE_MARKERS: [&str; 2] = ["/.claude/worktrees/", "/.Codex/worktrees/"];

/// Resolve the directory NAME a tab should take from a pane's cwd. For a cwd
/// under a [`WORKTREE_MARKERS`] segment, this is the parent repo's basename (so
/// every worktree of `zj-radar` reads as `zj-radar` instead of flipping to each
/// branch dir); otherwise it is the plain cwd basename.
fn worktree_repo_dir(path: &str) -> Option<String> {
    match WORKTREE_MARKERS.iter().filter_map(|m| path.find(m)).min() {
        Some(idx) => cwd_basename(&path[..idx]),
        None => cwd_basename(path),
    }
}

fn strip_activity_prefix(title: &str) -> &str {
    let Some(first) = title.chars().next() else {
        return title;
    };
    if !('\u{2800}'..='\u{28ff}').contains(&first) {
        return title;
    }
    let rest = &title[first.len_utf8()..];
    if rest.chars().next().is_some_and(char::is_whitespace) {
        rest
    } else {
        title
    }
}

fn parse_snapshot(raw: &str) -> Option<(Vec<(u32, TrackedObservation)>, u64)> {
    let value: serde_json::Value = serde_json::from_str(raw).ok()?;
    match value.get("v").and_then(serde_json::Value::as_u64)? as u32 {
        RADAR_SNAPSHOT_V => parse_v2_snapshot(value),
        LEGACY_STATUS_SNAPSHOT_V => parse_legacy_status_snapshot(value),
        _ => None,
    }
}

fn parse_v2_snapshot(value: serde_json::Value) -> Option<(Vec<(u32, TrackedObservation)>, u64)> {
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

fn parse_legacy_status_snapshot(
    value: serde_json::Value,
) -> Option<(Vec<(u32, TrackedObservation)>, u64)> {
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
                    source: pane.source,
                    last_change_tick: pane.last_change_tick,
                    seq: pane.seq,
                    on_focus: pane.on_focus.as_deref().map(Status::from_wire),
                    ever_active: pane.ever_active,
                    exit_code: None,
                },
            )
        })
        .collect();
    Some((observations, snapshot.tick))
}

#[cfg(test)]
mod tests {
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

    fn titled_pane(id: u32, title: &str, focused_in_tab: bool) -> TerminalPane {
        TerminalPane {
            id,
            title: title.into(),
            focused_in_tab,
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
            seq: None,
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
    fn cwd_basename_handles_normal_trailing_root_and_empty_paths() {
        assert_eq!(
            cwd_basename("/Users/m/dev/zj-radar"),
            Some("zj-radar".into())
        );
        assert_eq!(
            cwd_basename("/Users/m/dev/zj-radar/"),
            Some("zj-radar".into())
        );
        assert_eq!(cwd_basename("/"), None);
        assert_eq!(cwd_basename(""), None);
    }

    #[test]
    fn default_name_matches_zellij_tab_numbers_only() {
        assert!(is_default_name("Tab #1"));
        assert!(is_default_name("Tab #12"));
        assert!(!is_default_name("Tab #"));
        assert!(!is_default_name("Tab #x"));
        assert!(!is_default_name("custom"));
    }

    #[test]
    fn computed_name_prefers_focused_repo_then_cwd_then_title() {
        let mut radar = RadarState::default();
        radar
            .status_mut()
            .apply(payload_for(1, Status::Running, "repo-one"), 1);
        radar
            .status_mut()
            .apply(payload_for(2, Status::Running, "repo-two"), 1);
        let panes = vec![titled_pane(1, "one", false), titled_pane(2, "two", true)];
        assert_eq!(radar.computed_name(&panes), Some("repo-two".into()));

        let mut radar = RadarState::default();
        radar.pane_cwd.insert(1, "/work/one".into());
        radar.pane_cwd.insert(2, "/work/two".into());
        assert_eq!(radar.computed_name(&panes), Some("two".into()));

        let panes = vec![
            titled_pane(1, "first", false),
            titled_pane(2, "⠀ spinner-title", true),
        ];
        let radar = RadarState::default();
        assert_eq!(radar.computed_name(&panes), Some("spinner-title".into()));
    }

    #[test]
    fn computed_name_falls_back_to_the_first_pane_that_has_a_title() {
        // No repo, no cwd, nothing focused; the first pane has no usable title
        // but a later one does. The name falls through to that pane's title
        // rather than giving up — the title tier mirrors name_supported, which
        // already accepts any pane's title.
        let radar = RadarState::default();
        let panes = vec![titled_pane(1, "   ", false), titled_pane(2, "scratch", false)];
        assert_eq!(radar.computed_name(&panes), Some("scratch".into()));
    }

    #[test]
    fn every_computed_name_is_supported() {
        // computed_name and name_supported share one candidate space, so any
        // name computed_name can yield must be "supported" (sticky) — and any
        // pane attribute name_supported accepts must be computable. This pins
        // the two against drift across repo / worktree / title tiers.
        let mut radar = RadarState::default();
        radar
            .status_mut()
            .apply(payload_for(1, Status::Running, "repo-one"), 1);
        radar.pane_cwd.insert(2, "/work/two".into());
        let panes = vec![titled_pane(1, "t1", false), titled_pane(2, "t2", true)];
        let name = radar
            .computed_name(&panes)
            .expect("a name should be computable here");
        assert!(
            radar.name_supported(&panes, &name),
            "computed name {name:?} must be considered supported"
        );
        // A non-focused, non-first pane's title is both supported AND computable
        // (the case that used to diverge).
        assert!(radar.name_supported(&panes, "t1"));
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
        done.seq = Some(9);
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
        assert_eq!(pane.seq, Some(9));
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
        let legacy = r#"{"v":1,"tick":7,"panes":[{"pane_id":9,"status":"running","repo":"repo","branch":"main","msg":"work","source":"claude","last_change_tick":6,"seq":3,"on_focus":"idle","ever_active":true}]}"#;
        let mut radar = RadarState::default();

        let tick = radar.load_snapshot(legacy).expect("legacy snapshot loads");

        assert_eq!(tick, 7);
        let pane = radar.status(9).expect("legacy pane restored");
        assert_eq!(pane.origin, ObservationOrigin::StatusPipe);
        assert_eq!(pane.status, Status::Running);
        assert_eq!(pane.on_focus, Some(Status::Idle));
        assert_eq!(pane.seq, Some(3));
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
        restored.apply_focus_transition(Some(5), 9);

        assert_eq!(restored.status(5).unwrap().status, Status::Idle);
        assert_eq!(restored.status(5).unwrap().on_focus, None);
    }

    #[test]
    fn worktree_repo_dir_resolves_claude_worktree_paths_to_parent_repo() {
        // A worktree under the standard `<repo>/.claude/worktrees/<branch>` path
        // resolves to the PARENT repo's basename, not the branch dir.
        assert_eq!(
            worktree_repo_dir("/Users/m/dev/zj-radar/.claude/worktrees/feat-x"),
            Some("zj-radar".into())
        );
        // Deeper cwd inside the worktree still resolves to the repo.
        assert_eq!(
            worktree_repo_dir("/Users/m/dev/zj-radar/.claude/worktrees/feat-x/src/app"),
            Some("zj-radar".into())
        );
        // Codex worktrees live under `<repo>/.Codex/worktrees/<branch>`.
        assert_eq!(
            worktree_repo_dir("/Users/m/dev/zj-radar/.Codex/worktrees/sidebar-ui-polish"),
            Some("zj-radar".into())
        );
        assert_eq!(
            worktree_repo_dir("/Users/m/dev/zj-radar/.Codex/worktrees/sidebar-ui-polish/src"),
            Some("zj-radar".into())
        );
        // A normal (non-worktree) path keeps its plain basename.
        assert_eq!(
            worktree_repo_dir("/Users/m/dev/zj-radar"),
            Some("zj-radar".into())
        );
        assert_eq!(
            worktree_repo_dir("/Users/m/dev/zj-radar/src"),
            Some("src".into())
        );
    }

    #[test]
    fn computed_name_resolves_worktree_cwd_to_parent_repo() {
        let mut radar = RadarState::default();
        radar
            .pane_cwd
            .insert(1, "/Users/m/dev/zj-radar/.claude/worktrees/feat-x".into());
        let panes = vec![focused_pane(1)];
        assert_eq!(radar.computed_name(&panes), Some("zj-radar".into()));
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
    }
}

// On a plain host build (`cargo build`, not wasm, not test) the only consumers
// of the pure modules are the wasm glue (cfg'd out) and the unit tests (cfg'd
// out), so every public item appears dead. The pure modules stay warning-free
// under `cargo test` via their own tests; this scoped allow covers only the
// non-test host build and leaves the module sources untouched.
#[cfg_attr(all(not(target_arch = "wasm32"), not(test)), allow(dead_code))]
mod config;
#[cfg_attr(all(not(target_arch = "wasm32"), not(test)), allow(dead_code))]
mod kind;
#[cfg_attr(all(not(target_arch = "wasm32"), not(test)), allow(dead_code))]
mod model;
#[cfg_attr(all(not(target_arch = "wasm32"), not(test)), allow(dead_code))]
mod naming;
#[cfg_attr(all(not(target_arch = "wasm32"), not(test)), allow(dead_code))]
mod payload;
#[cfg_attr(all(not(target_arch = "wasm32"), not(test)), allow(dead_code))]
mod render;
#[cfg_attr(all(not(target_arch = "wasm32"), not(test)), allow(dead_code))]
mod state;
#[cfg_attr(all(not(target_arch = "wasm32"), not(test)), allow(dead_code))]
mod status;
// `theme` is only consumed by the wasm glue; on a non-wasm non-test host build
// everything in it appears dead. Its own unit tests exercise it on the host.
#[cfg_attr(all(not(target_arch = "wasm32"), not(test)), allow(dead_code))]
mod command;
#[cfg_attr(all(not(target_arch = "wasm32"), not(test)), allow(dead_code))]
mod theme;

#[cfg(feature = "cli")]
pub mod cli;

// `render::TabRow` and `state::StateStore` are referenced by the pure helpers
// and the wasm glue; the helpers themselves are only consumed by tests on the
// host target, so these imports look dead to a non-test host build.
use naming::PaneLite;
#[cfg_attr(all(not(target_arch = "wasm32"), not(test)), allow(unused_imports))]
use render::TabRow;
use state::StateStore;
use std::collections::HashMap;

#[cfg(target_arch = "wasm32")]
use std::collections::{BTreeMap, HashSet};
#[cfg(target_arch = "wasm32")]
use zellij_tile::prelude::*;

#[cfg(target_arch = "wasm32")]
const PIPE_NAME: &str = "zj_radar.status.v1";
#[cfg(target_arch = "wasm32")]
const CONFIG_PIPE: &str = "zj_radar.config.v1";

#[cfg_attr(all(not(target_arch = "wasm32"), not(test)), allow(dead_code))]
#[derive(Clone)]
struct TabLite {
    position: usize,
    name: String,
    active: bool,
    has_bell: bool,
}

#[cfg_attr(all(not(target_arch = "wasm32"), not(test)), allow(dead_code))]
#[derive(Default)]
pub struct State {
    store: StateStore,
    tabs: Vec<TabLite>,
    tab_panes: HashMap<usize, Vec<PaneLite>>, // tab position -> terminal panes
    // `pane_cwd` maps terminal pane id → last-seen cwd string. Updated on
    // CwdChanged; pruned in sync with tab_panes on PaneUpdate.
    #[cfg_attr(not(target_arch = "wasm32"), allow(dead_code))]
    pane_cwd: HashMap<u32, String>,
    // `tick`/`timer_armed`/`applied_names` are read only by the wasm glue; on
    // any host build (including tests, which construct State but never read
    // them) they are dead.
    #[cfg_attr(not(target_arch = "wasm32"), allow(dead_code))]
    tick: u64,
    #[cfg_attr(not(target_arch = "wasm32"), allow(dead_code))]
    timer_armed: bool,
    #[cfg_attr(not(target_arch = "wasm32"), allow(dead_code))]
    applied_names: HashMap<usize, String>,
    #[cfg_attr(not(target_arch = "wasm32"), allow(dead_code))]
    last_render_height: usize,
    // `permission_granted` is read by both the wasm glue and the host tests,
    // so no dead_code gate is needed. `config` carries the parsed plugin
    // config (naming/header/glyphs) and is read by the wasm glue.
    #[cfg_attr(not(target_arch = "wasm32"), allow(dead_code))]
    config: config::Config,
    permission_granted: bool,
    // Terminal-derived surface + dim colors; updated on PaneUpdate from the
    // panes' reported default_bg/default_fg; only used by the wasm render path.
    #[cfg_attr(not(target_arch = "wasm32"), allow(dead_code))]
    theme: theme::DerivedColors,
    #[cfg_attr(not(target_arch = "wasm32"), allow(dead_code))]
    command: command::CommandStore,
    // The terminal pane focused as of the last PaneUpdate. Clear-on-focus fires
    // only when this CHANGES (a focus transition into a pane), so a pane that
    // becomes Done while already focused stays lit until you leave and return —
    // the design's "stays lit until visited" rule. None until the first focus.
    #[cfg_attr(not(target_arch = "wasm32"), allow(dead_code))]
    last_focused: Option<u32>,
    // The shared `/cache` snapshot this instance reads on load and writes on
    // every store mutation. zj-radar runs one instance PER TAB (it lives in the
    // tab template), and a CLI-pipe broadcast only reaches instances alive at
    // send time — it is never replayed. So a tab opened later spawns a blank
    // instance that missed every prior broadcast and would show all tabs idle.
    // `/cache` is the one plugin folder shared across all instances (keyed by
    // plugin URL, not by instance), so it is where a newcomer rehydrates from.
    // `cache_path` is `/cache/zj-radar.<zellij_pid>.json` (session-scoped by the
    // Zellij server pid); `cache_tmp` is the per-instance temp file we write then
    // atomically rename, so concurrent writers never expose a torn snapshot.
    #[cfg_attr(not(target_arch = "wasm32"), allow(dead_code))]
    cache_path: Option<String>,
    #[cfg_attr(not(target_arch = "wasm32"), allow(dead_code))]
    cache_tmp: Option<String>,
}

// ── Pure helpers (no host calls) — compiled and tested on the host target ──

#[cfg_attr(all(not(target_arch = "wasm32"), not(test)), allow(dead_code))]
impl State {
    fn build_rows(&self) -> Vec<TabRow> {
        let mut rows = Vec::new();
        let mut sorted = self.tabs.clone();
        sorted.sort_by_key(|t| t.position);
        for t in &sorted {
            let empty = Vec::new();
            let panes = self.tab_panes.get(&t.position).unwrap_or(&empty);
            let ids: Vec<u32> = panes.iter().map(|p| p.id).collect();
            rows.push(TabRow {
                number: t.position as u32 + 1,
                name: t.name.clone(),
                active: t.active,
                has_bell: t.has_bell,
                agg: model::aggregate(&ids, &self.store, &self.command),
            });
        }
        rows
    }

    /// Apply clear-on-focus only on a focus TRANSITION — when focus enters a
    /// pane that wasn't focused on the previous PaneUpdate. The "visit" that
    /// clears a Done/finished pane (per the design's "stays lit until visited")
    /// is the act of focusing INTO it, not merely being focused: a pane that
    /// finishes while already focused must stay lit until you leave and return.
    ///
    /// Without this gate, `on_pane_focused` ran on EVERY PaneUpdate for the
    /// focused pane, so a pane that became Done while focused was auto-cleared to
    /// Idle by the next (frequent) update — and whether a focus-move "beat" that
    /// clearing update was a timing race, surfacing as direction-dependent
    /// Done↔Idle flicker. Gating on the transition makes it deterministic.
    ///
    /// Returns true when a transition was applied (focus actually changed).
    fn apply_focus_transition(&mut self, focused: Option<u32>, tick: u64) -> bool {
        if focused == self.last_focused {
            return false;
        }
        self.last_focused = focused;
        if let Some(id) = focused {
            self.store.on_pane_focused(id, tick);
            self.command.on_pane_focused(id, tick);
        }
        true
    }

    /// Map a clicked line back to a tab position by replaying render()'s fold
    /// plan. Thin wrapper over `target_at_line` that drops the pane id; used by
    /// host unit tests that only assert tab membership (the wasm click handler
    /// calls `target_at_line` directly to also resolve the pane target).
    #[cfg(test)]
    fn tab_position_at_line(&self, line: isize) -> Option<usize> {
        self.target_at_line(line).map(|(pos, _pane)| pos)
    }

    /// Map a clicked line to `(tab_position, Option<pane_id>)`. Both render()
    /// and this function consult the SAME `plan_layout` + `pane_tree_plan`, so
    /// "line N → tab X (pane P)" is exactly what the user sees on screen.
    ///
    /// Each tab's visual block is `pad_y + content + gap` (the cohesive
    /// CardSpacing footprint). The `pad_y` rows and content rows belong to that
    /// tab; the trailing `gap` rows are external separation and map to None.
    ///
    /// For a MULTI-PANE tab the content rows are: header (line 0, tab-only) +
    /// one child line per expanded pane (each maps to its pane id) + a collapse
    /// line (tab-only). Clicking an expanded child line targets THAT pane via
    /// `show_pane_with_id`; everything else targets the tab.
    fn target_at_line(&self, line: isize) -> Option<(usize, Option<u32>)> {
        if line < 0 {
            return None;
        }
        let target = line as usize;
        let rows = self.build_rows();
        if rows.is_empty() {
            return None;
        }
        let mut cursor = render::header_lines(&rows, self.config.header, self.config.density);
        if target < cursor {
            return None; // click landed on the header → no tab
        }
        // Replay render()'s layout plan. Height 0 means "not yet rendered" →
        // treat as unbounded so no folding/gap-dropping is assumed.
        let body_budget = if self.last_render_height == 0 {
            usize::MAX
        } else {
            self.last_render_height.saturating_sub(render::header_lines(
                &rows,
                self.config.header,
                self.config.density,
            ))
        };
        let (plan, _strip_folded, spacing) =
            render::plan_layout(&rows, body_budget, self.config.density);
        for &(i, planned_lines) in &plan {
            // owned = pad_y + content rows belonging to this tab; trailing gap
            // rows are external separation and do not belong to any tab.
            let content = planned_lines.max(1);
            let owned = spacing.pad_y + content;
            if target >= cursor && target < cursor + owned {
                let tab_pos = (rows[i].number - 1) as usize;
                // Offset of the clicked line within this tab's CONTENT rows
                // (after pad_y). 0 = header line; 1.. = child/collapse lines.
                let content_off = (target - cursor).saturating_sub(spacing.pad_y);
                let pane = self.pane_at_content_offset(&rows[i], content_off);
                return Some((tab_pos, pane));
            }
            cursor += owned + spacing.gap;
        }
        // Any line at/after the folded idle strip maps to no tab.
        None
    }

    /// Resolve which pane (if any) a content-row offset within a tab targets.
    /// Offset 0 is the header (tab-only → None). For a multi-pane tab, offsets
    /// 1.. index the expanded child lines from `pane_tree_plan`; the collapse
    /// line (past the expanded children) is tab-only. Single-pane tabs have no
    /// per-pane child target (their line 2 is tab-only).
    fn pane_at_content_offset(&self, row: &TabRow, content_off: usize) -> Option<u32> {
        if content_off == 0 || !render::is_multi_pane(&row.agg) {
            return None;
        }
        let plan = render::pane_tree_plan(&row.agg, row.active);
        // child index k = content_off - 1; expanded children come first.
        let k = content_off - 1;
        plan.expanded.get(k).map(|p| p.pane_id)
    }
}

// ── Wasm-only glue — each item gated so host `cargo test` never links these.
// `register_plugin!` lives in the BINARY crate (`src/main.rs`) so the `fn main`
// it generates becomes the wasm `_start` Zellij requires; here we only provide
// the `ZellijPlugin` impl + host-fn helpers it drives.

#[cfg(target_arch = "wasm32")]
const CACHE_DIR: &str = "/cache";
#[cfg(target_arch = "wasm32")]
const SNAPSHOT_PREFIX: &str = "zj-radar.";
#[cfg(target_arch = "wasm32")]
const SNAPSHOT_MAX_AGE_SECS: u64 = 24 * 60 * 60;

#[cfg(target_arch = "wasm32")]
impl State {
    /// Resolve the shared snapshot paths from the Zellij server pid, seed this
    /// instance's store from any existing snapshot, and best-effort prune stale
    /// ones. Called once from `load()`. All filesystem work is best-effort: a
    /// failure just means this instance starts empty and the next broadcast
    /// repopulates it — the plugin must never break the host over a cache miss.
    fn init_cache(&mut self) {
        let ids = get_plugin_ids();
        let path = format!("{CACHE_DIR}/{SNAPSHOT_PREFIX}{}.json", ids.zellij_pid);
        // Temp file is per-instance (plugin_id) so two tabs writing at once never
        // clobber each other's in-progress write before the atomic rename.
        let tmp = format!("{path}.{}.tmp", ids.plugin_id);
        if let Ok(raw) = std::fs::read_to_string(&path) {
            if let Some((store, tick)) = StateStore::from_json(&raw) {
                self.store = store;
                self.tick = tick;
            }
        }
        self.prune_stale_snapshots(&path);
        self.cache_path = Some(path);
        self.cache_tmp = Some(tmp);
    }

    /// Mirror the current store into the shared snapshot. Every live instance
    /// writes identical content after a given broadcast, so concurrent writes
    /// converge; the temp-file + atomic rename guarantees a reader (a newly
    /// loading instance) never sees a half-written file. Best-effort: errors are
    /// swallowed, since the live broadcast is the source of truth for everyone
    /// already running — the snapshot only seeds future newcomers.
    fn persist(&self) {
        let (Some(path), Some(tmp)) = (&self.cache_path, &self.cache_tmp) else {
            return;
        };
        let json = self.store.to_json(self.tick);
        if std::fs::write(tmp, json.as_bytes()).is_ok() {
            let _ = std::fs::rename(tmp, path);
        }
    }

    /// Remove this plugin's snapshots from dead sessions. `/cache` is shared and
    /// persists across sessions, so without this a small file would accumulate
    /// per past session. Age-based (mtime) so it never deletes a concurrently
    /// running session's file (that one keeps a fresh mtime) nor our own.
    fn prune_stale_snapshots(&self, own_path: &str) {
        let Ok(entries) = std::fs::read_dir(CACHE_DIR) else {
            return;
        };
        let now = std::time::SystemTime::now();
        for entry in entries.flatten() {
            let name = entry.file_name();
            let name = name.to_string_lossy();
            if !name.starts_with(SNAPSHOT_PREFIX) || !name.ends_with(".json") {
                continue;
            }
            let path = entry.path();
            if path.to_string_lossy() == own_path {
                continue;
            }
            let stale = entry
                .metadata()
                .and_then(|m| m.modified())
                .ok()
                .and_then(|t| now.duration_since(t).ok())
                .is_some_and(|age| age.as_secs() > SNAPSHOT_MAX_AGE_SECS);
            if stale {
                let _ = std::fs::remove_file(path);
            }
        }
    }

    fn arm_timer_if_needed(&mut self) {
        if !self.timer_armed && (self.store.any_active() || self.command.has_pending_or_active()) {
            set_timeout(1.0);
            self.timer_armed = true;
        }
    }

    fn apply_renames(&mut self) {
        if self.config.naming == config::NamingMode::Off {
            return;
        }
        let force = self.config.naming == config::NamingMode::Force;
        let tabs: Vec<(usize, String)> = self
            .tabs
            .iter()
            .map(|t| (t.position, t.name.clone()))
            .collect();
        let changes = naming::compute_renames(
            &tabs,
            &self.tab_panes,
            &self.store,
            &self.applied_names,
            force,
            &self.pane_cwd,
        );
        for (pos, name) in changes {
            rename_tab(pos as u32 + 1, &name);
            self.applied_names.insert(pos, name);
        }
    }
}

#[cfg(target_arch = "wasm32")]
impl ZellijPlugin for State {
    fn load(&mut self, config: BTreeMap<String, String>) {
        self.config = config::Config::from_map(&config);
        request_permission(&[
            PermissionType::ReadApplicationState,
            PermissionType::ReadCliPipes,
            PermissionType::ChangeApplicationState,
        ]);
        subscribe(&[
            EventType::TabUpdate,
            EventType::PaneUpdate,
            EventType::CwdChanged,
            EventType::CommandChanged,
            EventType::Timer,
            EventType::Mouse,
            EventType::PermissionRequestResult,
        ]);
        set_selectable(false);
        // Seed from the shared snapshot so a tab opened after agents were already
        // running shows their real status instead of a blank (all-idle) rail.
        self.init_cache();
    }

    fn update(&mut self, event: Event) -> bool {
        match event {
            Event::TabUpdate(tabs) => {
                self.tabs = tabs
                    .into_iter()
                    .map(|t| TabLite {
                        position: t.position,
                        name: t.name,
                        active: t.active,
                        has_bell: t.has_bell_notification,
                    })
                    .collect();
                true
            }
            Event::PaneUpdate(manifest) => {
                let mut tab_panes: HashMap<usize, Vec<PaneLite>> = HashMap::new();
                let mut live: HashSet<u32> = HashSet::new();
                let mut focused_terminal: Option<u32> = None;
                // Capture the terminal's reported default bg/fg so we can derive
                // the dark-panel surfaces in the terminal's own theme. Prefer the
                // focused pane; otherwise accept the first terminal pane that
                // reports both colors.
                let mut focused_colors: Option<(theme::Rgb, theme::Rgb)> = None;
                let mut any_colors: Option<(theme::Rgb, theme::Rgb)> = None;
                for (tab_pos, panes) in manifest.panes {
                    for p in panes {
                        if p.is_plugin {
                            continue;
                        }
                        let colors = match (
                            p.default_bg.as_deref().and_then(theme::parse_hex),
                            p.default_fg.as_deref().and_then(theme::parse_hex),
                        ) {
                            (Some(bg), Some(fg)) => Some((bg, fg)),
                            _ => None,
                        };
                        if let Some(c) = colors {
                            any_colors.get_or_insert(c);
                            if p.is_focused {
                                focused_colors = Some(c);
                            }
                        }
                        tab_panes.entry(tab_pos).or_default().push(PaneLite {
                            id: p.id,
                            title: payload::sanitize(&p.title, 40),
                            is_focused: p.is_focused,
                        });
                        live.insert(p.id);
                        if p.is_focused {
                            focused_terminal = Some(p.id);
                        }
                        if p.exited {
                            self.command.on_exit(p.id, p.exit_status, self.tick);
                        }
                    }
                }
                if let Some((bg, fg)) = focused_colors.or(any_colors) {
                    self.theme = theme::DerivedColors::from_bg_fg(bg, fg);
                }
                self.tab_panes = tab_panes;
                self.store.prune(&live);
                self.command.prune(&live);
                self.pane_cwd.retain(|id, _| live.contains(id));
                // Clear-on-focus fires only on a focus TRANSITION (focus enters
                // a new pane), not on every update while a pane stays focused —
                // otherwise a pane that finishes while already focused is
                // auto-cleared to Idle by the next update (a timing race that
                // showed up as direction-dependent Done↔Idle flicker).
                self.apply_focus_transition(focused_terminal, self.tick);
                self.apply_renames();
                // Prune (closed panes) and the focus-clear both mutate the store,
                // so refresh the shared snapshot newcomers seed from.
                self.persist();
                true
            }
            Event::Timer(_) => {
                self.timer_armed = false;
                self.tick += 1;
                self.command.on_timer(self.tick);
                self.arm_timer_if_needed();
                self.store.any_active() || self.command.has_pending_or_active()
            }
            Event::Mouse(Mouse::LeftClick(line, _col)) => {
                if self.permission_granted {
                    if let Some((pos, pane)) = self.target_at_line(line) {
                        match pane {
                            // A child pane line → focus THAT pane (and switch to
                            // its tab). show_pane_with_id unsuppresses + focuses
                            // the pane by id and switches to its tab in one call.
                            Some(id) => {
                                show_pane_with_id(PaneId::Terminal(id), false, true);
                            }
                            // Header / collapse / single-pane lines → switch tab.
                            // switch_tab_to is 1-based; `pos` is 0-based, so +1.
                            None => switch_tab_to(pos as u32 + 1),
                        }
                    }
                }
                false
            }
            Event::PermissionRequestResult(status) => {
                self.permission_granted = status == PermissionStatus::Granted;
                true
            }
            Event::CwdChanged(pane_id, path, _clients) => {
                if let PaneId::Terminal(id) = pane_id {
                    self.pane_cwd.insert(id, path.to_string_lossy().to_string());
                    self.apply_renames();
                }
                true
            }
            Event::CommandChanged(pane_id, command, is_foreground, _clients) => {
                if let PaneId::Terminal(id) = pane_id {
                    let cwd = self.pane_cwd.get(&id).map(|s| s.as_str());
                    self.command
                        .on_command_changed(id, &command, is_foreground, cwd, self.tick);
                    self.arm_timer_if_needed();
                }
                true
            }
            _ => false,
        }
    }

    fn pipe(&mut self, message: PipeMessage) -> bool {
        if message.name == PIPE_NAME {
            if let Some(raw) = &message.payload {
                if let Some(p) = payload::parse(raw) {
                    self.store.apply(p, self.tick);
                    self.apply_renames();
                    self.arm_timer_if_needed();
                    // Mirror the new status to the shared snapshot so the next
                    // tab to open seeds from it instead of starting blank.
                    self.persist();
                    return true;
                }
            }
        } else if message.name == CONFIG_PIPE {
            if let Some(raw) = &message.payload {
                // Parse as a flat JSON object; scalar values (bool/number) are
                // stringified so callers may omit quotes for simple values.
                if let Ok(val) = serde_json::from_str::<serde_json::Value>(raw) {
                    if let Some(obj) = val.as_object() {
                        let kv: std::collections::BTreeMap<String, String> = obj
                            .iter()
                            .filter_map(|(k, v)| {
                                let s = match v {
                                    serde_json::Value::String(s) => Some(s.clone()),
                                    serde_json::Value::Bool(b) => {
                                        Some(if *b { "true" } else { "false" }.to_string())
                                    }
                                    serde_json::Value::Number(n) => Some(n.to_string()),
                                    _ => None,
                                };
                                s.map(|s| (k.clone(), s))
                            })
                            .collect();
                        self.config.apply_overrides(&kv);
                        self.apply_renames();
                        return true;
                    }
                }
            }
        }
        false
    }

    fn render(&mut self, rows: usize, cols: usize) {
        self.last_render_height = rows;
        let tabrows = self.build_rows();
        let opts = render::RenderOpts {
            width: cols.max(1),
            height: rows,
            now_tick: self.tick,
            glyphs: self.config.glyphs,
            header: self.config.header,
            density: self.config.density,
            theme: self.theme.clone(),
        };
        if !self.permission_granted || tabrows.is_empty() {
            print!("{}", render::onboarding(&opts));
        } else {
            print!("{}", render::render(&tabrows, &opts));
        }
    }
}

// ── Host unit tests (no host calls — pure helpers only) ──

#[cfg(test)]
mod tests {
    use super::*;
    use crate::payload::StatusPayload;
    use crate::status::Status;

    fn pane(id: u32) -> PaneLite {
        PaneLite {
            id,
            ..Default::default()
        }
    }

    fn make_state_with_tabs(tab_specs: &[(usize, &str, bool)]) -> State {
        // tab_specs: (position, name, active)
        // Uses Compact density so existing click-mapping tests (which hard-code
        // line numbers assuming no gap lines) continue to pass unchanged.
        let tabs = tab_specs
            .iter()
            .map(|&(pos, name, active)| TabLite {
                position: pos,
                name: name.to_string(),
                active,
                has_bell: false,
            })
            .collect();
        State {
            tabs,
            config: config::Config {
                density: config::Density::Compact,
                ..config::Config::default()
            },
            ..Default::default()
        }
    }

    fn apply_payload(state: &mut State, pane_id: u32, status: Status, tick: u64) {
        apply_payload_with_msg(state, pane_id, status, tick, "msg");
    }

    fn apply_payload_with_msg(
        state: &mut State,
        pane_id: u32,
        status: Status,
        tick: u64,
        msg: &str,
    ) {
        state.store.apply(
            StatusPayload {
                pane_id,
                status,
                repo: "repo".into(),
                branch: "branch".into(),
                msg: msg.into(),
                on_focus: None,
                seq: None,
                source: "test".into(),
            },
            tick,
        );
    }

    // ── build_rows tests ──

    #[test]
    fn build_rows_empty_state_returns_empty() {
        let state = State::default();
        assert!(state.build_rows().is_empty());
    }

    #[test]
    fn build_rows_returns_one_row_per_tab_in_position_order() {
        let state = make_state_with_tabs(&[(2, "c", false), (0, "a", true), (1, "b", false)]);
        let rows = state.build_rows();
        assert_eq!(rows.len(), 3);
        assert_eq!(rows[0].name, "a");
        assert_eq!(rows[1].name, "b");
        assert_eq!(rows[2].name, "c");
    }

    #[test]
    fn build_rows_number_is_position_plus_one() {
        let state = make_state_with_tabs(&[(0, "first", false), (1, "second", false)]);
        let rows = state.build_rows();
        assert_eq!(rows[0].number, 1);
        assert_eq!(rows[1].number, 2);
    }

    #[test]
    fn build_rows_active_flag_passes_through() {
        let state = make_state_with_tabs(&[(0, "t0", false), (1, "t1", true)]);
        let rows = state.build_rows();
        assert!(!rows[0].active);
        assert!(rows[1].active);
    }

    #[test]
    fn build_rows_agg_reflects_pane_status() {
        let mut state = make_state_with_tabs(&[(0, "agent-tab", false)]);
        // Assign pane 42 to tab position 0
        state.tab_panes.insert(0, vec![pane(42)]);
        apply_payload(&mut state, 42, Status::Running, 1);
        let rows = state.build_rows();
        assert_eq!(rows[0].agg.status, Status::Running);
        assert!(rows[0].agg.detail.is_some());
    }

    #[test]
    fn build_rows_tab_without_known_panes_is_idle() {
        let state = make_state_with_tabs(&[(0, "plain", false)]);
        // No entry in tab_panes for position 0 — no agent state
        let rows = state.build_rows();
        assert_eq!(rows[0].agg.status, Status::Idle);
        assert!(rows[0].agg.detail.is_none());
    }

    // ── tab_position_at_line tests ──

    #[test]
    fn click_negative_line_returns_none() {
        let state = make_state_with_tabs(&[(0, "t0", false)]);
        assert!(state.tab_position_at_line(-1).is_none());
    }

    #[test]
    fn plain_tabs_each_occupy_one_line() {
        // 3 plain tabs at positions 0, 1, 2 → 2-line header, then lines 2, 3, 4
        let state = make_state_with_tabs(&[(0, "a", false), (1, "b", false), (2, "c", false)]);
        assert_eq!(state.tab_position_at_line(0), None); // header
        assert_eq!(state.tab_position_at_line(1), None); // header
        assert_eq!(state.tab_position_at_line(2), Some(0));
        assert_eq!(state.tab_position_at_line(3), Some(1));
        assert_eq!(state.tab_position_at_line(4), Some(2));
    }

    #[test]
    fn click_beyond_last_tab_returns_none() {
        let state = make_state_with_tabs(&[(0, "a", false)]);
        // 1 plain tab → header (lines 0,1) + tab (line 2); line 3 is beyond
        assert!(state.tab_position_at_line(3).is_none());
    }

    #[test]
    fn agent_tab_running_occupies_two_lines() {
        let mut state = make_state_with_tabs(&[(0, "agent", false), (1, "plain", false)]);
        state.tab_panes.insert(0, vec![pane(10)]);
        apply_payload(&mut state, 10, Status::Running, 1); // running → 2 lines
                                                           // rows 0,1 = header
        assert_eq!(state.tab_position_at_line(1), None);
        // rows 2,3 = running agent tab (position 0)
        assert_eq!(state.tab_position_at_line(2), Some(0));
        assert_eq!(state.tab_position_at_line(3), Some(0));
        // row 4 = plain tab (position 1)
        assert_eq!(state.tab_position_at_line(4), Some(1));
        assert!(state.tab_position_at_line(5).is_none());
    }

    #[test]
    fn agent_tab_pending_with_msg_occupies_two_lines() {
        // New line-2 rule: pending + msg → 2 lines (mark + activity). Old 3-line case gone.
        let mut state = make_state_with_tabs(&[(0, "agent", false), (1, "plain", false)]);
        state.tab_panes.insert(0, vec![pane(10)]);
        apply_payload_with_msg(&mut state, 10, Status::Pending, 1, "approve?"); // pending+msg → 2
        assert_eq!(state.tab_position_at_line(1), None); // header
        assert_eq!(state.tab_position_at_line(2), Some(0)); // line 1
        assert_eq!(state.tab_position_at_line(3), Some(0)); // line 2 (mark + activity)
        assert_eq!(state.tab_position_at_line(4), Some(1)); // plain tab (was line 5 before)
        assert!(state.tab_position_at_line(5).is_none());
    }

    #[test]
    fn switch_tab_to_index_is_position_plus_one() {
        // Confirm that tab_position_at_line returns the 0-based position,
        // so the caller must add 1 before calling switch_tab_to.
        // With the always-on header, tabs start at line 2.
        let state = make_state_with_tabs(&[(0, "first", false), (1, "second", false)]);
        // Position 0 → switch_tab_to(0 + 1 = 1)
        assert_eq!(state.tab_position_at_line(2), Some(0));
        // Position 1 → switch_tab_to(1 + 1 = 2)
        assert_eq!(state.tab_position_at_line(3), Some(1));
    }

    #[test]
    fn idle_rail_still_has_header_click_offset_by_two() {
        // All-idle tabs still render the always-on header (2 lines), so the
        // first tab maps to line 2, not line 0.
        let state = make_state_with_tabs(&[(0, "a", false), (1, "b", false)]);
        assert_eq!(state.tab_position_at_line(0), None); // header
        assert_eq!(state.tab_position_at_line(1), None); // header
        assert_eq!(state.tab_position_at_line(2), Some(0));
        assert_eq!(state.tab_position_at_line(3), Some(1));
    }

    #[test]
    fn click_mapping_is_fold_aware_when_overflowing() {
        // 6 idle tabs + one pending (urgent) tab at the end; tiny height forces folding.
        let mut state = make_state_with_tabs(&[
            (0, "a", false),
            (1, "b", false),
            (2, "c", false),
            (3, "d", false),
            (4, "e", false),
            (5, "pinky", false),
        ]);
        state.tab_panes.insert(5, vec![pane(50)]);
        apply_payload(&mut state, 50, Status::Pending, 1); // pending → non-idle, kept
        state.last_render_height = 6; // body_budget = 4

        // header = lines 0,1. Idle rows fold; only the pending tab (position 5) is kept.
        // It renders right after the header.
        assert_eq!(state.tab_position_at_line(0), None); // header
        assert_eq!(state.tab_position_at_line(1), None); // header
        assert_eq!(state.tab_position_at_line(2), Some(5)); // the kept non-idle tab
    }

    #[test]
    fn click_mapping_unchanged_when_not_overflowing() {
        // Large height → no folding → same as plain position order (offset by 2-line header).
        let mut state = make_state_with_tabs(&[(0, "a", false), (1, "b", false)]);
        state.last_render_height = 100;
        assert_eq!(state.tab_position_at_line(0), None);
        assert_eq!(state.tab_position_at_line(2), Some(0));
        assert_eq!(state.tab_position_at_line(3), Some(1));
    }

    #[test]
    fn state_defaults_glyphs_to_plain_and_ungranted() {
        let s = State::default();
        assert_eq!(s.config.glyphs, crate::status::GlyphSet::Plain);
        assert!(!s.permission_granted);
    }

    #[test]
    fn sidebar_stays_selectable_until_permissions_are_granted() {
        let mut s = State::default();
        assert!(
            s.sidebar_should_be_selectable(),
            "first-run permission prompt must remain focusable"
        );

        s.permission_granted = true;
        assert!(
            !s.sidebar_should_be_selectable(),
            "after permissions are granted the sidebar returns to passive mode"
        );
    }

    #[test]
    fn multi_pane_inactive_collapses_to_header_plus_count() {
        // A tab with 2 panes both running, NOT active → multi-pane tree: both
        // calm panes collapse, so row_lines = 1 header + 1 collapse = 2 lines.
        let mut state = make_state_with_tabs(&[(0, "team", false), (1, "plain", false)]);
        state.tab_panes.insert(0, vec![pane(10), pane(11)]);
        apply_payload(&mut state, 10, Status::Running, 1);
        apply_payload(&mut state, 11, Status::Running, 1);
        // header = lines 0,1
        assert_eq!(state.tab_position_at_line(0), None);
        assert_eq!(state.tab_position_at_line(1), None);
        // 2-line multi-pane tab at position 0: header (line 2) + collapse (line 3).
        assert_eq!(state.tab_position_at_line(2), Some(0));
        assert_eq!(state.tab_position_at_line(3), Some(0));
        // Both lines are tab-only (collapse line has no per-pane target).
        assert_eq!(state.target_at_line(2), Some((0, None)));
        assert_eq!(state.target_at_line(3), Some((0, None)));
        // plain tab at position 1: line 4
        assert_eq!(state.tab_position_at_line(4), Some(1));
        assert!(state.tab_position_at_line(5).is_none());
    }

    #[test]
    fn multi_pane_child_line_click_targets_that_pane() {
        // 3-pane tab: pane 10 Pending (expands), 11 + 12 Running (collapse).
        // Tree: header + 1 expanded child (pane 10) + collapse line = 3 lines.
        let mut state = make_state_with_tabs(&[(0, "monorepo", false), (1, "plain", false)]);
        state
            .tab_panes
            .insert(0, vec![pane(10), pane(11), pane(12)]);
        apply_payload_with_msg(&mut state, 10, Status::Pending, 1, "run migration?");
        apply_payload(&mut state, 11, Status::Running, 1);
        apply_payload(&mut state, 12, Status::Running, 1);
        state.last_render_height = 100;
        // header = lines 0,1
        assert_eq!(
            state.target_at_line(2),
            Some((0, None)),
            "header → tab only"
        );
        // child line → pane 10 (the expanded Pending pane)
        assert_eq!(
            state.target_at_line(3),
            Some((0, Some(10))),
            "child line → pane 10"
        );
        // collapse line → tab only
        assert_eq!(
            state.target_at_line(4),
            Some((0, None)),
            "collapse line → tab only"
        );
        // plain tab follows at line 5
        assert_eq!(state.tab_position_at_line(5), Some(1));
        assert!(state.tab_position_at_line(6).is_none());
    }

    #[test]
    fn multi_pane_active_all_children_clickable() {
        // Active tab → ALL panes expand; each child line targets its pane.
        let mut state = make_state_with_tabs(&[(0, "team", true)]);
        state.tab_panes.insert(0, vec![pane(20), pane(21)]);
        apply_payload(&mut state, 20, Status::Running, 1);
        apply_payload(&mut state, 21, Status::Done, 1);
        state.last_render_height = 100;
        // header(2) + 2 children, no collapse.
        assert_eq!(state.target_at_line(2), Some((0, None)), "header");
        assert_eq!(
            state.target_at_line(3),
            Some((0, Some(20))),
            "child 0 → pane 20"
        );
        assert_eq!(
            state.target_at_line(4),
            Some((0, Some(21))),
            "child 1 → pane 21"
        );
        assert!(state.tab_position_at_line(5).is_none());
    }

    /// Click mapping uses PLANNED (compressed) line counts, not uncompressed
    /// `row_lines`. When Running rows are compressed to 1 line each under
    /// pressure, each click mapping span must shrink accordingly.
    #[test]
    fn click_mapping_matches_compressed_layout() {
        // Setup: 3 Running tabs (each normally 2 lines) + 1 Pending-with-msg (now 2 lines).
        // Uncompressed body = 3×2 + 2 = 8 lines. Header = 2.
        // height = 7 → body_budget = 5.
        // plan_overflow compresses Running rows to 1 line; Pending stays at 2.
        // Final plan spans: [1, 1, 1, 2] → total = 5.
        // After header (lines 0-1):
        //   position 0 (Running, 1 line) → line 2
        //   position 1 (Running, 1 line) → line 3
        //   position 2 (Running, 1 line) → line 4
        //   position 3 (Pending, 2 lines) → lines 5-6
        let mut state = make_state_with_tabs(&[
            (0, "r0", false),
            (1, "r1", false),
            (2, "r2", false),
            (3, "urgent", false),
        ]);
        state.tab_panes.insert(0, vec![pane(10)]);
        state.tab_panes.insert(1, vec![pane(11)]);
        state.tab_panes.insert(2, vec![pane(12)]);
        state.tab_panes.insert(3, vec![pane(13)]);
        apply_payload(&mut state, 10, Status::Running, 1);
        apply_payload(&mut state, 11, Status::Running, 1);
        apply_payload(&mut state, 12, Status::Running, 1);
        apply_payload_with_msg(&mut state, 13, Status::Pending, 1, "please approve");
        state.last_render_height = 7; // body_budget = 5

        // Header lines
        assert_eq!(state.tab_position_at_line(0), None);
        assert_eq!(state.tab_position_at_line(1), None);
        // Each Running tab compressed to 1 line → one click per tab.
        assert_eq!(state.tab_position_at_line(2), Some(0));
        assert_eq!(state.tab_position_at_line(3), Some(1));
        assert_eq!(state.tab_position_at_line(4), Some(2));
        // Pending tab gets 2 lines (mark + activity; fits without compression).
        assert_eq!(state.tab_position_at_line(5), Some(3));
        assert_eq!(state.tab_position_at_line(6), Some(3));
        // Nothing beyond.
        assert_eq!(state.tab_position_at_line(7), None);
    }

    /// Lockstep: a multi-pane tab's tree lines (header + expanded children +
    /// collapse) must consume exactly `row_lines()` lines in the click-mapping,
    /// so a plain tab immediately after it maps to the correct line. The tree
    /// plan is the single source of truth for both render() and click-mapping.
    #[test]
    fn multi_pane_tree_click_mapping_lockstep() {
        use crate::status::Status;
        // 3-pane tab (1 Pending expands, 2 Running collapse) → header + 1 child
        // + collapse = 3 content lines. Followed by a plain tab.
        let mut state = make_state_with_tabs(&[(0, "team", false), (1, "plain", false)]);
        state
            .tab_panes
            .insert(0, vec![pane(10), pane(11), pane(12)]);
        apply_payload_with_msg(&mut state, 10, Status::Pending, 1, "approve?");
        apply_payload(&mut state, 11, Status::Running, 1);
        apply_payload(&mut state, 12, Status::Running, 1);
        state.last_render_height = 100;
        // Confirm row_lines agrees: header(1) + 1 expanded + collapse(1) = 3.
        let rows = state.build_rows();
        assert_eq!(
            render::row_lines(&rows[0].agg, rows[0].active),
            3,
            "tree is 3 content lines"
        );
        // header = lines 0,1. Tree at position 0: lines 2,3,4. Plain tab: line 5.
        assert_eq!(state.tab_position_at_line(0), None, "header line 0");
        assert_eq!(state.tab_position_at_line(1), None, "header line 1");
        assert_eq!(state.tab_position_at_line(2), Some(0), "tree header line");
        assert_eq!(state.tab_position_at_line(3), Some(0), "tree child line");
        assert_eq!(state.tab_position_at_line(4), Some(0), "tree collapse line");
        // The plain tab must start at line 5, not earlier.
        assert_eq!(
            state.tab_position_at_line(5),
            Some(1),
            "plain tab follows the tree"
        );
        assert_eq!(state.tab_position_at_line(6), None, "beyond last tab");
    }

    // ── Density click-mapping tests ──

    #[test]
    fn click_mapping_accounts_for_gaps_comfortable() {
        // Comfortable density, large height → spacing.gap = 1, pad_y = 0.
        // 2 idle tabs, header=2 lines.
        // Layout: header(2) | tab0 content(1) | tab0 gap(1) | tab1 content(1) | tab1 gap(1)
        // Lines:   0,1      | 2               | 3           | 4               | 5
        //
        // The gap is EXTERNAL separation, so the gap line (3) maps to None — only
        // the owned pad_y + content rows belong to a tab. Tab 1 starts at line 4.
        let mut state = make_state_with_tabs(&[(0, "a", false), (1, "b", false)]);
        state.last_render_height = 100; // large → no overflow
        state.config = config::Config {
            density: config::Density::Comfortable,
            ..config::Config::default()
        };

        // header lines
        assert_eq!(state.tab_position_at_line(0), None, "header line 0");
        assert_eq!(state.tab_position_at_line(1), None, "header line 1");
        // tab 0 content line
        assert_eq!(state.tab_position_at_line(2), Some(0), "tab 0 content line");
        // tab 0 gap line — external separation, maps to None
        assert_eq!(
            state.tab_position_at_line(3),
            None,
            "tab 0 gap line maps to None"
        );
        // tab 1 content line starts at 4
        assert_eq!(state.tab_position_at_line(4), Some(1), "tab 1 content line");
        // tab 1 gap line — external separation, maps to None
        assert_eq!(
            state.tab_position_at_line(5),
            None,
            "tab 1 gap line maps to None"
        );
        // beyond
        assert_eq!(state.tab_position_at_line(6), None, "beyond last tab");
    }

    #[test]
    fn click_mapping_compact_no_gaps() {
        // Compact density → no gaps, tabs are adjacent.
        // 2 idle tabs, header=2 lines.
        // Lines: 0,1 header | 2 tab0 | 3 tab1
        let mut state = make_state_with_tabs(&[(0, "a", false), (1, "b", false)]);
        state.last_render_height = 100;
        state.config = config::Config {
            density: config::Density::Compact,
            ..config::Config::default()
        };

        assert_eq!(state.tab_position_at_line(0), None);
        assert_eq!(state.tab_position_at_line(1), None);
        assert_eq!(state.tab_position_at_line(2), Some(0));
        assert_eq!(state.tab_position_at_line(3), Some(1));
        assert_eq!(state.tab_position_at_line(4), None);
    }

    #[test]
    fn click_mapping_cards_one_line_header() {
        // Cards density: the carded hero is a single " RADAR …" title line (no
        // rule), so the header occupies ONE line, not two. Cards now carries
        // gap = 1 (trailing rail_bg row after each card) and pad_y = 0.
        // The gap rows map to None (they are external separation, not owned by
        // a tab). The click mapping must stay in lockstep with render():
        //   line 0  → header (1 line, no rule) → None
        //   line 1  → tab 0 content            → Some(0)
        //   line 2  → tab 0 gap (rail)         → None
        //   line 3  → tab 1 content            → Some(1)
        //   line 4  → tab 1 gap (rail)         → None
        //   line 5  → None (beyond last tab)
        let mut state = make_state_with_tabs(&[(0, "a", false), (1, "b", false)]);
        state.last_render_height = 100;
        state.config = config::Config {
            density: config::Density::Cards,
            ..config::Config::default()
        };

        assert_eq!(
            state.tab_position_at_line(0),
            None,
            "1-line header in Cards"
        );
        assert_eq!(state.tab_position_at_line(1), Some(0), "tab 0 content");
        assert_eq!(state.tab_position_at_line(2), None, "tab 0 gap row → None");
        assert_eq!(state.tab_position_at_line(3), Some(1), "tab 1 content");
        assert_eq!(state.tab_position_at_line(4), None, "tab 1 gap row → None");
        assert_eq!(state.tab_position_at_line(5), None, "beyond last tab");
    }

    #[test]
    fn click_mapping_cards_pad_y_and_post_content_row() {
        // Exercises the gap semantics explicitly with a multi-line card so the
        // boundary between one card's last content row and the gap row is clear.
        //   header(1) | tab0 content×2 | tab0 gap(1) | tab1 content(1) | tab1 gap(1)
        //   line 0    | lines 1,2      | line 3      | line 4          | line 5
        // The gap row (line 3) maps to None; the tab 1 content (line 4) maps to Some(1).
        // tab 0 is a Running tab WITH detail → 2 content lines.
        let mut state = make_state_with_tabs(&[(0, "work", false), (1, "b", false)]);
        // Make tab 0 a running agent with a detail line (2 content lines).
        state.tab_panes.insert(0, vec![pane(10)]);
        apply_payload(&mut state, 10, Status::Running, 1);
        state.last_render_height = 100;
        state.config = config::Config {
            density: config::Density::Cards,
            ..config::Config::default()
        };

        // Confirm tab 0 really is 2 content lines.
        let rows = state.build_rows();
        assert_eq!(
            render::row_lines(&rows[0].agg, rows[0].active),
            2,
            "tab 0 should be 2 content lines"
        );

        assert_eq!(state.tab_position_at_line(0), None, "header");
        assert_eq!(
            state.tab_position_at_line(1),
            Some(0),
            "tab 0 content line 1"
        );
        assert_eq!(
            state.tab_position_at_line(2),
            Some(0),
            "tab 0 content line 2"
        );
        assert_eq!(state.tab_position_at_line(3), None, "tab 0 gap row → None");
        assert_eq!(state.tab_position_at_line(4), Some(1), "tab 1 content");
        assert_eq!(state.tab_position_at_line(5), None, "tab 1 gap row → None");
        assert_eq!(state.tab_position_at_line(6), None, "beyond last tab");
    }

    // ── Clear-on-focus fires only on a focus TRANSITION ──

    #[test]
    fn focus_transition_clears_only_on_entry_not_while_focused() {
        let mut state = make_state_with_tabs(&[(0, "a", true), (1, "b", false)]);
        state.tab_panes.insert(0, vec![pane(10)]);
        state.tab_panes.insert(1, vec![pane(11)]);
        // Pane 10 finished a command WHILE focused → Done with on_focus=Idle.
        state.command.on_exit(10, Some(0), 1);
        state.last_focused = Some(10);
        // A subsequent PaneUpdate with the SAME focused pane is not a transition
        // → must NOT clear it (stays lit while you sit on it).
        assert!(
            !state.apply_focus_transition(Some(10), 2),
            "no transition when focus unchanged"
        );
        assert_eq!(
            state.command.get(10).unwrap().status,
            Status::Done,
            "Done pane stays lit while focus remains on it"
        );
        // Leaving to pane 11 is a transition, but must not touch the pane we left.
        assert!(state.apply_focus_transition(Some(11), 3));
        assert_eq!(
            state.command.get(10).unwrap().status,
            Status::Done,
            "leaving does not change the pane you left"
        );
        // Re-entering pane 10 is a transition → NOW it clears to Idle ("visited").
        assert!(state.apply_focus_transition(Some(10), 4));
        assert_eq!(
            state.command.get(10).unwrap().status,
            Status::Idle,
            "re-entering a Done pane clears it to Idle"
        );
    }

    #[test]
    fn done_pane_left_behind_is_direction_independent() {
        // Reproduce the reported bug: a command pane that finished while focused
        // must show the SAME state whether you then move to a higher- or
        // lower-numbered pane. Before the fix, a redraw update while still
        // focused could clear it to Idle, so the result depended on timing.
        let run = |dest: u32| {
            let mut state =
                make_state_with_tabs(&[(0, "l", false), (1, "mid", true), (2, "r", false)]);
            state.tab_panes.insert(0, vec![pane(1)]);
            state.tab_panes.insert(1, vec![pane(2)]);
            state.tab_panes.insert(2, vec![pane(3)]);
            // Focus is on pane 2; its command finishes → Done while focused.
            state.last_focused = Some(2);
            state.command.on_exit(2, Some(0), 1);
            // A redraw update arrives while still focused (must not clear).
            state.apply_focus_transition(Some(2), 2);
            // Now move focus to the destination pane.
            state.apply_focus_transition(Some(dest), 3);
            state.command.get(2).unwrap().status
        };
        assert_eq!(
            run(3),
            Status::Done,
            "moving 'right' (2→3) leaves pane 2 Done"
        );
        assert_eq!(
            run(1),
            Status::Done,
            "moving 'left' (2→1) leaves pane 2 Done"
        );
    }
}

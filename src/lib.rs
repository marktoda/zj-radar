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
mod observation;
mod payload;
#[cfg_attr(all(not(target_arch = "wasm32"), not(test)), allow(dead_code))]
mod radar_state;
#[cfg_attr(all(not(target_arch = "wasm32"), not(test)), allow(dead_code))]
mod tab_namer;
#[cfg_attr(all(not(target_arch = "wasm32"), not(test)), allow(dead_code))]
mod render;
#[cfg_attr(all(not(target_arch = "wasm32"), not(test)), allow(dead_code))]
mod rollup;
#[cfg_attr(all(not(target_arch = "wasm32"), not(test)), allow(dead_code))]
mod runtime;
#[cfg_attr(all(not(target_arch = "wasm32"), not(test)), allow(dead_code))]
mod session_files;
#[cfg_attr(all(not(target_arch = "wasm32"), not(test)), allow(dead_code))]
mod status;
// Shared wire-enum macros (wire_serde! / wire_enum!) used by `status` and
// `observation`. Path-imported (`use crate::wire::…`), so declaration order
// among the modules doesn't matter.
mod wire;
#[cfg_attr(all(not(target_arch = "wasm32"), not(test)), allow(dead_code))]
mod status_store;
// `theme` is only consumed by the wasm glue; on a non-wasm non-test host build
// everything in it appears dead. Its own unit tests exercise it on the host.
#[cfg_attr(all(not(target_arch = "wasm32"), not(test)), allow(dead_code))]
mod command;
#[cfg_attr(all(not(target_arch = "wasm32"), not(test)), allow(dead_code))]
mod theme;

#[cfg(test)]
mod reference_tests;

#[cfg(feature = "cli")]
pub mod cli;

// Radar state types are referenced by host tests and wasm glue; the helper
// imports are only consumed by tests on the host target.
#[cfg_attr(all(not(target_arch = "wasm32"), not(test)), allow(unused_imports))]
use radar_state::{RadarTab, TabId, TerminalPane};
#[cfg(test)]
use render::TabRow;
use runtime::PluginRuntime;
use session_files::SessionFiles;

#[cfg(target_arch = "wasm32")]
use runtime::Effect;
#[cfg(target_arch = "wasm32")]
use session_files::SessionFileIds;
#[cfg(target_arch = "wasm32")]
use std::collections::{BTreeMap, HashMap, HashSet};
#[cfg(target_arch = "wasm32")]
use zellij_tile::prelude::*;

#[cfg(target_arch = "wasm32")]
const PIPE_NAME: &str = "zj_radar.status.v1";
#[cfg(target_arch = "wasm32")]
const CONFIG_PIPE: &str = "zj_radar.config.v1";

#[cfg_attr(all(not(target_arch = "wasm32"), not(test)), allow(dead_code))]
#[derive(Default)]
pub struct State {
    runtime: PluginRuntime,
    #[cfg_attr(not(target_arch = "wasm32"), allow(dead_code))]
    session_files: SessionFiles,
}

// ── Pure helper wrappers (no host calls) — compiled only for host tests ──

#[cfg(test)]
impl State {
    fn build_rows(&self) -> Vec<TabRow> {
        self.runtime.build_rows()
    }

    /// Apply the *visit*-clear only on a focus TRANSITION — when focus enters a
    /// pane that wasn't focused on the previous PaneUpdate. This is the
    /// background-completion path: a pane that finished while you weren't looking
    /// stays lit until you focus INTO it, at which point its queued clear is
    /// applied (this clears even an error — once seen, it's seen).
    ///
    /// The transition gate is load-bearing in two ways. (1) Without it,
    /// `on_pane_focused` would run on EVERY PaneUpdate for the focused pane and
    /// would wipe a focused *error* the instant the next update arrived —
    /// errors must persist while watched. (2) It keeps this path off the
    /// completed-while-focused case entirely, which is handled separately by
    /// `RadarState::settle_focused` (run from `panes_changed` and `timer`): a
    /// `Done` on the focused pane recedes Done-only and monotonically, so it never
    /// produces the direction-dependent Done↔Idle flicker an earlier "clear on
    /// every update" version did.
    ///
    /// Returns true when a transition was applied (focus actually changed).
    #[cfg(test)]
    fn apply_focus_transition(&mut self, focused: Option<u32>, tick: u64) -> bool {
        self.runtime.apply_focus_transition(focused, tick)
    }

    /// Zellij's permission prompt for a visible status/sidebar plugin is tied to
    /// the pane that called `request_permission`. Keep only that pane selectable
    /// while a y/n answer is pending; peer sidebar instances stay passive while
    /// they wait for the first grant to populate Zellij's permission cache.
    fn sidebar_should_be_selectable(&self) -> bool {
        self.runtime.sidebar_should_be_selectable()
    }

    fn record_permission_request_started(&mut self) {
        self.runtime.record_permission_request_started();
    }

    fn record_permission_result(&mut self, granted: bool) {
        self.runtime.record_permission_result(granted);
    }

    /// Render at an explicit width so `last_rendered` is populated.
    /// Click tests historically asserted the width-80 layout; keep that explicit.
    /// When no height has been set yet (last_render_height == 0), use a large
    /// height so folding/overflow never discards rows unexpectedly.
    ///
    /// # Contract — LIVE, permission-granted rail only
    ///
    /// This helper unconditionally sets `permission_granted = true` so that
    /// `runtime.render` produces a real tab rail rather than the onboarding
    /// screen. It is intentionally a LIVE-RAIL fixture and MUST NOT be used
    /// to test the no-permission / onboarding case. Onboarding tests must
    /// drive `runtime.mouse_click` directly (as `mouse_click_without_permission_is_inert`
    /// does) — they must never call `render_at`, `tab_position_at_line`, or
    /// `target_at_line`, or they will silently force-grant permission and
    /// produce misleading results.
    #[cfg(test)]
    fn render_at(&mut self, width: usize) {
        self.runtime.permission_granted = true;
        let height = if self.runtime.last_render_height == 0 {
            usize::MAX / 2
        } else {
            self.runtime.last_render_height
        };
        let _ = self.runtime.render(height, width);
    }

    /// Map a clicked line back to a tab position through the renderer-owned
    /// target map. Thin wrapper over `target_at_line` that drops the pane id;
    /// used by host unit tests that only assert tab membership.
    #[cfg(test)]
    fn tab_position_at_line(&mut self, line: isize) -> Option<usize> {
        self.render_at(80);
        self.runtime.tab_position_at_line(line)
    }

    /// Map a clicked line to `(tab_position, Option<pane_id>)` through the
    /// render module's `RenderedRail` target map. Runtime tests use this helper
    /// to exercise the same render-owned targeting seam as the wasm mouse path.
    #[cfg(test)]
    fn target_at_line(&mut self, line: isize) -> Option<(usize, Option<u32>)> {
        self.render_at(80);
        self.runtime.target_at_line(line)
    }
}

// ── Wasm-only glue — each item gated so host `cargo test` never links these.
// `register_plugin!` lives in the BINARY crate (`src/main.rs`) so the `fn main`
// it generates becomes the wasm `_start` Zellij requires; here we only provide
// the `ZellijPlugin` impl + host-fn helpers it drives.

#[cfg(target_arch = "wasm32")]
impl State {
    fn handle_outcome(&mut self, outcome: runtime::Outcome) -> bool {
        let render = outcome.render;
        self.handle_effects(outcome.effects);
        render
    }

    fn handle_effects(&mut self, effects: Vec<Effect>) {
        for effect in effects {
            match effect {
                Effect::RequestPermission => request_permission(&[
                    PermissionType::ReadApplicationState,
                    PermissionType::ReadCliPipes,
                    PermissionType::ChangeApplicationState,
                ]),
                Effect::SetSelectable(selectable) => set_selectable(selectable),
                Effect::SetTimeout => set_timeout(1.0),
                Effect::PersistSnapshot => {
                    let existing = self.session_files.snapshot();
                    let json = self.runtime.snapshot_json(existing.as_deref());
                    self.session_files.persist_snapshot(&json);
                }
                Effect::PersistPermissionMarker(marker) => {
                    self.session_files.persist_permission_marker(marker)
                }
                Effect::RenameTab { position, name } => rename_tab(position as u32 + 1, &name),
                Effect::SwitchTab { position } => switch_tab_to(position as u32 + 1),
                Effect::ShowPane { pane_id } => {
                    show_pane_with_id(PaneId::Terminal(pane_id), false, true);
                }
                Effect::ResolveCwd { pane_ids } => self.resolve_cwd(pane_ids),
            }
        }
    }

    /// Bootstrap tab names for freshly-opened panes by reading each pane's cwd
    /// once. `get_pane_cwd` is a blocking host round-trip, but the runtime has
    /// already gated this to at most once per pane id (capped per update), so it
    /// runs at pane-creation rate — never the per-output re-poll that melted the
    /// predecessor plugin. A successful read feeds the existing `cwd_changed`
    /// path, which performs the rename; a failure is simply dropped (the id is
    /// already marked attempted and a later `cd` will still name the tab).
    fn resolve_cwd(&mut self, pane_ids: Vec<u32>) {
        for id in pane_ids {
            if let Ok(path) = get_pane_cwd(PaneId::Terminal(id)) {
                let outcome = self
                    .runtime
                    .cwd_changed(id, path.to_string_lossy().to_string());
                self.handle_effects(outcome.effects);
            }
        }
    }
}

#[cfg(target_arch = "wasm32")]
impl ZellijPlugin for State {
    fn load(&mut self, config: BTreeMap<String, String>) {
        subscribe(&[
            EventType::TabUpdate,
            EventType::PaneUpdate,
            EventType::CwdChanged,
            EventType::CommandChanged,
            EventType::Timer,
            EventType::Mouse,
            EventType::PermissionRequestResult,
        ]);
        // Seed from the shared snapshot so a tab opened after agents were already
        // running shows their real status instead of a blank (all-idle) rail.
        let ids = get_plugin_ids();
        let session = SessionFiles::open(SessionFileIds {
            plugin_id: ids.plugin_id,
            zellij_pid: ids.zellij_pid,
        });
        self.session_files = session.files;
        let outcome = self.runtime.load(
            config::Config::from_map(&config),
            session.snapshot.as_deref(),
            session.permission,
        );
        self.handle_outcome(outcome);
    }

    fn update(&mut self, event: Event) -> bool {
        match event {
            Event::TabUpdate(tabs) => {
                let tabs = tabs
                    .into_iter()
                    .map(|t| RadarTab {
                        id: TabId::new(t.tab_id),
                        position: t.position,
                        name: t.name,
                        active: t.active,
                        has_bell: t.has_bell_notification,
                    })
                    .collect();
                let outcome = self.runtime.tabs_changed(tabs);
                self.handle_outcome(outcome)
            }
            Event::PaneUpdate(manifest) => {
                let mut tab_panes: HashMap<usize, Vec<TerminalPane>> = HashMap::new();
                let mut live: HashSet<u32> = HashSet::new();
                let mut exits: Vec<(u32, Option<i32>)> = Vec::new();
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
                        tab_panes.entry(tab_pos).or_default().push(TerminalPane {
                            id: p.id,
                            title: payload::sanitize(&p.title, 40),
                            focused_in_tab: p.is_focused,
                        });
                        live.insert(p.id);
                        if p.exited {
                            exits.push((p.id, p.exit_status));
                        }
                    }
                }
                let theme = focused_colors
                    .or(any_colors)
                    .map(|(bg, fg)| theme::DerivedColors::from_bg_fg(bg, fg));
                let update = radar_state::PaneUpdate {
                    tab_panes,
                    live,
                    theme,
                    exits,
                };
                let outcome = self.runtime.panes_changed(update);
                self.handle_outcome(outcome)
            }
            Event::Timer(_) => {
                // Re-probe (marker + lock) each tick so a waiting peer can take
                // over a prompt whose owner died holding a now-stale lock.
                let probe = self.session_files.refresh_permission_probe();
                let outcome = self.runtime.timer(probe);
                self.handle_outcome(outcome)
            }
            Event::Mouse(Mouse::LeftClick(line, _col)) => {
                let outcome = self.runtime.mouse_click(line);
                self.handle_outcome(outcome)
            }
            Event::PermissionRequestResult(status) => {
                let granted = status == PermissionStatus::Granted;
                let outcome = self.runtime.permission_result(granted);
                self.handle_outcome(outcome)
            }
            Event::CwdChanged(pane_id, path, _clients) => {
                if let PaneId::Terminal(id) = pane_id {
                    let outcome = self
                        .runtime
                        .cwd_changed(id, path.to_string_lossy().to_string());
                    return self.handle_outcome(outcome);
                }
                true
            }
            Event::CommandChanged(pane_id, command, is_foreground, _clients) => {
                if let PaneId::Terminal(id) = pane_id {
                    let outcome = self.runtime.command_changed(id, &command, is_foreground);
                    return self.handle_outcome(outcome);
                }
                true
            }
            _ => false,
        }
    }

    fn pipe(&mut self, message: PipeMessage) -> bool {
        if message.name == PIPE_NAME {
            if let Some(raw) = &message.payload {
                let outcome = self.runtime.status_pipe(raw);
                return self.handle_outcome(outcome);
            }
        } else if message.name == CONFIG_PIPE {
            if let Some(raw) = &message.payload {
                let outcome = self.runtime.config_pipe(raw);
                return self.handle_outcome(outcome);
            }
        }
        false
    }

    fn render(&mut self, rows: usize, cols: usize) {
        print!("{}", self.runtime.render(rows, cols));
    }
}

// ── Host unit tests (no host calls — pure helpers only) ──

#[cfg(test)]
mod tests {
    use super::*;
    use crate::payload::StatusPayload;
    use crate::status::Status;

    fn pane(id: u32) -> TerminalPane {
        TerminalPane {
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
            .map(|&(pos, name, active)| RadarTab {
                id: TabId::new(pos + 1),
                position: pos,
                name: name.to_string(),
                active,
                has_bell: false,
            })
            .collect();
        let mut state = State::default();
        state.runtime.tabs_changed(tabs);
        state.runtime.config = config::Config {
            density: config::Density::Compact,
            ..config::Config::default()
        };
        state
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
        state.runtime.radar.status_mut().apply(
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
    fn build_rows_display_reflects_pane_status() {
        let mut state = make_state_with_tabs(&[(0, "agent-tab", false)]);
        // Assign pane 42 to tab position 0
        state
            .runtime
            .radar
            .set_tab_panes_for_position(0, vec![pane(42)]);
        apply_payload(&mut state, 42, Status::Running, 1);
        let rows = state.build_rows();
        assert_eq!(rows[0].display.status, Status::Running);
        assert!(rows[0].display.detail.is_some());
    }

    #[test]
    fn build_rows_tab_without_known_panes_is_idle() {
        let state = make_state_with_tabs(&[(0, "plain", false)]);
        // No entry in tab_panes for position 0 — no agent state
        let rows = state.build_rows();
        assert_eq!(rows[0].display.status, Status::Idle);
        assert!(rows[0].display.detail.is_none());
    }

    // ── tab_position_at_line tests ──

    #[test]
    fn click_negative_line_returns_none() {
        let mut state = make_state_with_tabs(&[(0, "t0", false)]);
        assert!(state.tab_position_at_line(-1).is_none());
    }

    #[test]
    fn plain_tabs_each_occupy_one_line() {
        // 3 plain tabs at positions 0, 1, 2 → 2-line header, then lines 2, 3, 4
        let mut state = make_state_with_tabs(&[(0, "a", false), (1, "b", false), (2, "c", false)]);
        assert_eq!(state.tab_position_at_line(0), None); // header
        assert_eq!(state.tab_position_at_line(1), None); // header
        assert_eq!(state.tab_position_at_line(2), Some(0));
        assert_eq!(state.tab_position_at_line(3), Some(1));
        assert_eq!(state.tab_position_at_line(4), Some(2));
    }

    #[test]
    fn click_beyond_last_tab_returns_none() {
        let mut state = make_state_with_tabs(&[(0, "a", false)]);
        // 1 plain tab → header (lines 0,1) + tab (line 2); line 3 is beyond
        assert!(state.tab_position_at_line(3).is_none());
    }

    #[test]
    fn agent_tab_running_occupies_two_lines() {
        let mut state = make_state_with_tabs(&[(0, "agent", false), (1, "plain", false)]);
        state
            .runtime
            .radar
            .set_tab_panes_for_position(0, vec![pane(10)]);
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
        state
            .runtime
            .radar
            .set_tab_panes_for_position(0, vec![pane(10)]);
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
        let mut state = make_state_with_tabs(&[(0, "first", false), (1, "second", false)]);
        // Position 0 → switch_tab_to(0 + 1 = 1)
        assert_eq!(state.tab_position_at_line(2), Some(0));
        // Position 1 → switch_tab_to(1 + 1 = 2)
        assert_eq!(state.tab_position_at_line(3), Some(1));
    }

    #[test]
    fn idle_rail_still_has_header_click_offset_by_two() {
        // All-idle tabs still render the always-on header (2 lines), so the
        // first tab maps to line 2, not line 0.
        let mut state = make_state_with_tabs(&[(0, "a", false), (1, "b", false)]);
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
        state
            .runtime
            .radar
            .set_tab_panes_for_position(5, vec![pane(50)]);
        apply_payload(&mut state, 50, Status::Pending, 1); // pending → non-idle, kept
        state.runtime.last_render_height = 6; // body_budget = 4

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
        state.runtime.last_render_height = 100;
        assert_eq!(state.tab_position_at_line(0), None);
        assert_eq!(state.tab_position_at_line(2), Some(0));
        assert_eq!(state.tab_position_at_line(3), Some(1));
    }

    #[test]
    fn state_defaults_glyphs_to_plain_and_ungranted() {
        let s = State::default();
        assert_eq!(s.runtime.config.glyphs, crate::status::GlyphSet::Plain);
        assert!(!s.runtime.permission_granted);
    }

    #[test]
    fn sidebar_stays_selectable_until_permissions_are_granted() {
        let mut s = State::default();
        assert!(
            !s.sidebar_should_be_selectable(),
            "peer sidebars that did not request permission stay passive"
        );

        s.record_permission_request_started();
        assert!(
            s.sidebar_should_be_selectable(),
            "the sidebar that owns the first-run prompt must remain focusable"
        );

        s.record_permission_result(true);
        assert!(
            !s.sidebar_should_be_selectable(),
            "after permissions are granted the sidebar returns to passive mode"
        );

        let mut s = State::default();
        s.record_permission_result(false);
        assert!(
            !s.sidebar_should_be_selectable(),
            "after permissions are denied the prompt is gone, so the rail is passive"
        );
    }

    #[test]
    fn multi_pane_inactive_collapses_to_header_plus_count() {
        // A tab with 2 panes both running, NOT active → new line-per-pane design:
        // row_lines = 1 header + 2 pane lines = 3 lines.
        let mut state = make_state_with_tabs(&[(0, "team", false), (1, "plain", false)]);
        state
            .runtime
            .radar
            .set_tab_panes_for_position(0, vec![pane(10), pane(11)]);
        apply_payload(&mut state, 10, Status::Running, 1);
        apply_payload(&mut state, 11, Status::Running, 1);
        // header = lines 0,1
        assert_eq!(state.tab_position_at_line(0), None);
        assert_eq!(state.tab_position_at_line(1), None);
        // 3-line multi-pane tab at position 0: header (line 2) + pane10 (line 3) + pane11 (line 4).
        assert_eq!(state.tab_position_at_line(2), Some(0));
        assert_eq!(state.tab_position_at_line(3), Some(0));
        assert_eq!(state.tab_position_at_line(4), Some(0));
        // Header line is tab-only; pane lines each target their pane.
        assert_eq!(state.target_at_line(2), Some((0, None)));
        assert_eq!(state.target_at_line(3), Some((0, Some(10))));
        assert_eq!(state.target_at_line(4), Some((0, Some(11))));
        // plain tab at position 1: line 5
        assert_eq!(state.tab_position_at_line(5), Some(1));
        assert!(state.tab_position_at_line(6).is_none());
    }

    #[test]
    fn multi_pane_child_line_click_targets_that_pane() {
        // 3-pane tab: pane 10 Pending, 11 + 12 Running. New line-per-pane design:
        // header (line 2) + pane 10 (line 3) + pane 11 (line 4) + pane 12 (line 5) = 4 lines.
        let mut state = make_state_with_tabs(&[(0, "monorepo", false), (1, "plain", false)]);
        state
            .runtime
            .radar
            .set_tab_panes_for_position(0, vec![pane(10), pane(11), pane(12)]);
        apply_payload_with_msg(&mut state, 10, Status::Pending, 1, "run migration?");
        apply_payload(&mut state, 11, Status::Running, 1);
        apply_payload(&mut state, 12, Status::Running, 1);
        state.runtime.last_render_height = 100;
        // header = lines 0,1
        assert_eq!(
            state.target_at_line(2),
            Some((0, None)),
            "header → tab only"
        );
        // pane lines each target their pane (in position order)
        assert_eq!(
            state.target_at_line(3),
            Some((0, Some(10))),
            "pane line → pane 10"
        );
        assert_eq!(
            state.target_at_line(4),
            Some((0, Some(11))),
            "pane line → pane 11"
        );
        assert_eq!(
            state.target_at_line(5),
            Some((0, Some(12))),
            "pane line → pane 12"
        );
        // plain tab follows at line 6
        assert_eq!(state.tab_position_at_line(6), Some(1));
        assert!(state.tab_position_at_line(7).is_none());
    }

    #[test]
    fn multi_pane_active_all_children_clickable() {
        // Active tab → ALL panes expand; each child line targets its pane.
        let mut state = make_state_with_tabs(&[(0, "team", true)]);
        state
            .runtime
            .radar
            .set_tab_panes_for_position(0, vec![pane(20), pane(21)]);
        apply_payload(&mut state, 20, Status::Running, 1);
        apply_payload(&mut state, 21, Status::Done, 1);
        state.runtime.last_render_height = 100;
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

    #[test]
    fn multi_pane_active_tracked_and_untracked_children_clickable() {
        // Only 1 tracked pane (pane 20 Running); pane 21 is untracked.
        // is_multi_pane = false → single-pane path: header + detail line (2 lines total).
        // The untracked pane has no rendered line; the detail line has no per-pane target.
        let mut state = make_state_with_tabs(&[(0, "team", true)]);
        state
            .runtime
            .radar
            .set_tab_panes_for_position(0, vec![pane(20), pane(21)]);
        apply_payload(&mut state, 20, Status::Running, 1);
        state.runtime.last_render_height = 100;

        assert_eq!(state.target_at_line(2), Some((0, None)), "header");
        assert_eq!(
            state.target_at_line(3),
            Some((0, None)),
            "detail line (single-pane mode, no per-pane target)"
        );
        assert!(state.tab_position_at_line(4).is_none(), "tab ends after 2 lines");

        let rows = state.build_rows();
        assert_eq!(
            rows[0].display.progress.total, 1,
            "untracked pane is not progress"
        );
        assert_eq!(
            rows[0].display.panes.len(),
            2,
            "both live panes are visible in display"
        );
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
        state
            .runtime
            .radar
            .set_tab_panes_for_position(0, vec![pane(10)]);
        state
            .runtime
            .radar
            .set_tab_panes_for_position(1, vec![pane(11)]);
        state
            .runtime
            .radar
            .set_tab_panes_for_position(2, vec![pane(12)]);
        state
            .runtime
            .radar
            .set_tab_panes_for_position(3, vec![pane(13)]);
        apply_payload(&mut state, 10, Status::Running, 1);
        apply_payload(&mut state, 11, Status::Running, 1);
        apply_payload(&mut state, 12, Status::Running, 1);
        apply_payload_with_msg(&mut state, 13, Status::Pending, 1, "please approve");
        state.runtime.last_render_height = 7; // body_budget = 5

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

    /// Lockstep: a multi-pane tab's rendered line count must consume exactly
    /// `row_lines()` lines in the click-mapping, so a plain tab immediately
    /// after it maps to the correct line.
    #[test]
    fn multi_pane_tree_click_mapping_lockstep() {
        use crate::status::Status;
        // 3-pane tab (all tracked) → new line-per-pane design:
        // header (line 2) + pane10 (line 3) + pane11 (line 4) + pane12 (line 5) = 4 lines.
        // Followed by a plain tab at line 6.
        let mut state = make_state_with_tabs(&[(0, "team", false), (1, "plain", false)]);
        state
            .runtime
            .radar
            .set_tab_panes_for_position(0, vec![pane(10), pane(11), pane(12)]);
        apply_payload_with_msg(&mut state, 10, Status::Pending, 1, "approve?");
        apply_payload(&mut state, 11, Status::Running, 1);
        apply_payload(&mut state, 12, Status::Running, 1);
        state.runtime.last_render_height = 100;
        // header = lines 0,1. Multi-pane tab at position 0: lines 2-5. Plain tab: line 6.
        assert_eq!(state.tab_position_at_line(0), None, "header line 0");
        assert_eq!(state.tab_position_at_line(1), None, "header line 1");
        assert_eq!(state.tab_position_at_line(2), Some(0), "tab header line");
        assert_eq!(state.tab_position_at_line(3), Some(0), "pane 10 line");
        assert_eq!(state.tab_position_at_line(4), Some(0), "pane 11 line");
        assert_eq!(state.tab_position_at_line(5), Some(0), "pane 12 line");
        // The plain tab must start at line 6, not earlier.
        assert_eq!(
            state.tab_position_at_line(6),
            Some(1),
            "plain tab follows the multi-pane tab"
        );
        assert_eq!(state.tab_position_at_line(7), None, "beyond last tab");
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
        state.runtime.last_render_height = 100; // large → no overflow
        state.runtime.config = config::Config {
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
        state.runtime.last_render_height = 100;
        state.runtime.config = config::Config {
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
        state.runtime.last_render_height = 100;
        state.runtime.config = config::Config {
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
        state
            .runtime
            .radar
            .set_tab_panes_for_position(0, vec![pane(10)]);
        apply_payload(&mut state, 10, Status::Running, 1);
        state.runtime.last_render_height = 100;
        state.runtime.config = config::Config {
            density: config::Density::Cards,
            ..config::Config::default()
        };

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

    // ── The visit-clear primitive fires only on a focus TRANSITION ──

    #[test]
    fn focus_transition_clears_only_on_entry() {
        // Unit test of the `apply_focus_transition` *primitive* in isolation: it
        // must clear a pane's queued completion only on ENTRY (a focus change),
        // never merely because the pane is focused. We seed the Done directly
        // (bypassing `panes_changed`/`settle_focused`) precisely to exercise the
        // primitive alone — in the real flow a Done landing on the focused pane
        // recedes at completion time via settle, not here. This gating is what
        // lets a *background* completion persist until visited, and is why a
        // focused *error* is not wiped on every update (settle skips errors).
        let mut state = make_state_with_tabs(&[(0, "a", true), (1, "b", false)]);
        state
            .runtime
            .radar
            .set_tab_panes_for_position(0, vec![pane(10)]);
        state
            .runtime
            .radar
            .set_tab_panes_for_position(1, vec![pane(11)]);
        state.runtime.radar.command_mut().on_exit(10, Some(0), 1);
        state.runtime.radar.set_last_focused(Some(10));
        // Same focused pane → not a transition → the primitive must not clear it.
        assert!(
            !state.apply_focus_transition(Some(10), 2),
            "no transition when focus unchanged"
        );
        assert_eq!(
            state.runtime.radar.command_store().get(10).unwrap().status,
            Status::Done,
            "the visit-clear never fires without a focus transition"
        );
        // Leaving to pane 11 is a transition, but must not touch the pane we left.
        assert!(state.apply_focus_transition(Some(11), 3));
        assert_eq!(
            state.runtime.radar.command_store().get(10).unwrap().status,
            Status::Done,
            "leaving does not change the pane you left"
        );
        // Re-entering pane 10 is a transition → NOW it clears to Idle ("visited").
        assert!(state.apply_focus_transition(Some(10), 4));
        assert_eq!(
            state.runtime.radar.command_store().get(10).unwrap().status,
            Status::Idle,
            "re-entering a finished pane clears it to Idle"
        );
    }

    // ── Event-sequence walk: shell→idle, fg command→pending→Running, exit→Done, focus→Idle ──

    #[test]
    fn command_pane_walks_idle_running_done_idle() {
        use crate::command::DEBOUNCE_TICKS;

        let mut state = make_state_with_tabs(&[(0, "t", true)]);
        state
            .runtime
            .radar
            .set_tab_panes_for_position(0, vec![pane(5)]);

        // Tick counter managed locally, matching what PluginRuntime.tick would be.
        let mut tick: u64 = 0;

        // 1) shell prompt only → idle (zsh is in IGNORE_NAMES, no resolved state)
        state.runtime.radar.command_mut().on_command_changed(
            5,
            &["zsh".to_string()],
            true,
            Some("/home/u/repo"),
            tick,
        );
        tick += 1;
        state.runtime.radar.command_mut().on_timer(tick);
        assert!(
            state.runtime.radar.command_store().get(5).is_none(),
            "shell prompt must leave pane idle"
        );

        // 2) real fg command → pending (not yet Running); after DEBOUNCE_TICKS timer → Running
        // DEBOUNCE_TICKS = 1: pending since_tick = tick; next tick satisfies
        // (tick + 1) - tick = 1 >= DEBOUNCE_TICKS.
        let since = tick;
        state.runtime.radar.command_mut().on_command_changed(
            5,
            &["cargo".to_string(), "test".to_string()],
            true,
            Some("/home/u/repo"),
            tick,
        );
        // Still within debounce window: promote tick by exactly DEBOUNCE_TICKS.
        tick += DEBOUNCE_TICKS;
        state.runtime.radar.command_mut().on_timer(tick);
        assert_eq!(
            state.runtime.radar.command_store().get(5).map(|s| s.status),
            Some(Status::Running),
            "must promote to Running after debounce (since={}, tick={})",
            since,
            tick
        );

        // 3) pane exits with code 0 → Done
        tick += 1;
        state.runtime.radar.command_mut().on_exit(5, Some(0), tick);
        assert_eq!(
            state.runtime.radar.command_store().get(5).map(|s| s.status),
            Some(Status::Done),
            "exit 0 must set Done"
        );

        // 4) pane gains focus → clear-on-focus → Idle
        tick += 1;
        state.runtime.radar.command_mut().on_pane_focused(5, tick);
        let st = state.runtime.radar.command_store().get(5).map(|s| s.status);
        assert!(
            st == Some(Status::Idle) || st.is_none(),
            "Done must clear to Idle on focus, got {:?}",
            st
        );
    }

    // ── mouse_click → Effect end-to-end tests ──

    #[test]
    fn mouse_click_on_tab_row_emits_switch_tab_effect() {
        use runtime::Effect;
        // 2 idle tabs, permission granted, render at width 80.
        // header=2 lines (Compact), tab 0 at line 2.
        let mut state = make_state_with_tabs(&[(0, "first", false), (1, "second", false)]);
        state.runtime.permission_granted = true;
        let _ = state.runtime.render(100, 80);
        // line 2 is the first tab content row (lines 0-1 are the header)
        let outcome = state.runtime.mouse_click(2);
        assert_eq!(outcome.effects, vec![Effect::SwitchTab { position: 0 }]);
    }

    #[test]
    fn mouse_click_without_permission_is_inert() {
        let mut state = make_state_with_tabs(&[(0, "first", false), (1, "second", false)]);
        state.runtime.permission_granted = false;
        let _ = state.runtime.render(100, 80);
        assert!(state.runtime.mouse_click(2).effects.is_empty());
    }

    // ── Click round-trip proptest ──

    proptest::proptest! {
        #[test]
        fn click_round_trip_hits_drawn_target(
            n_tabs in 1usize..6,
            active_idx in 0usize..6,
            // Draw statuses straight from the `statuses!` table, so a new variant
            // is covered by this proptest automatically — no ladder to update.
            statuses in proptest::collection::vec(
                proptest::sample::select(Status::ALL.to_vec()),
                1..6,
            ),
            width in proptest::sample::select(vec![24usize, 40, 80]),
        ) {
            // Build a state with n_tabs, each tab one pane with a status.
            let specs: Vec<(usize, &str, bool)> = (0..n_tabs)
                .map(|i| (i, "t", i == active_idx % n_tabs))
                .collect();
            let mut state = make_state_with_tabs(&specs);
            for (i, &st) in statuses.iter().take(n_tabs).enumerate() {
                // one pane per tab; pane id = tab index
                apply_payload(&mut state, i as u32, st, 1);
                state.runtime.radar.set_tab_panes_for_position(i, vec![pane(i as u32)]);
            }
            // Render through the production path at the given width; this populates last_rendered.
            state.runtime.permission_granted = true;
            let ansi = state.runtime.render(200, width);
            let rail_lines = ansi.matches('\n').count();
            // For every drawn line, target_at_line must resolve to a real tab or None.
            // A resolved tab must be in-range.
            let mut resolved = 0usize;
            for line in 0..(rail_lines as isize + 2) {
                if let Some((tab, _pane)) = state.runtime.target_at_line(line) {
                    proptest::prop_assert!(
                        tab < n_tabs,
                        "resolved tab {} out of range {} (width={}, line={})", tab, n_tabs, width, line
                    );
                    resolved += 1;
                }
            }
            proptest::prop_assert!(
                resolved >= 1,
                "test resolved no targets at all — setup may be broken (n_tabs={}, rail_lines={}, width={})",
                n_tabs, rail_lines, width
            );
        }
    }
}

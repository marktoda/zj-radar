// On a plain host build (`cargo build`, not wasm, not test) the only consumers
// of the pure modules are the wasm glue (cfg'd out) and the unit tests (cfg'd
// out), so every public item appears dead. The pure modules stay warning-free
// under `cargo test` via their own tests; this scoped allow covers only the
// non-test host build and leaves the module sources untouched.
#[cfg_attr(all(not(target_arch = "wasm32"), not(test)), allow(dead_code))]
mod status;
#[cfg_attr(all(not(target_arch = "wasm32"), not(test)), allow(dead_code))]
mod payload;
#[cfg_attr(all(not(target_arch = "wasm32"), not(test)), allow(dead_code))]
mod state;
#[cfg_attr(all(not(target_arch = "wasm32"), not(test)), allow(dead_code))]
mod model;
#[cfg_attr(all(not(target_arch = "wasm32"), not(test)), allow(dead_code))]
mod render;
#[cfg_attr(all(not(target_arch = "wasm32"), not(test)), allow(dead_code))]
mod naming;

// `render::TabRow` and `state::StateStore` are referenced by the pure helpers
// and the wasm glue; the helpers themselves are only consumed by tests on the
// host target, so these imports look dead to a non-test host build.
#[cfg_attr(all(not(target_arch = "wasm32"), not(test)), allow(unused_imports))]
use render::TabRow;
use state::StateStore;
use naming::PaneLite;
use std::collections::HashMap;

#[cfg(target_arch = "wasm32")]
use zellij_tile::prelude::*;
#[cfg(target_arch = "wasm32")]
use std::collections::{BTreeMap, HashSet};

#[cfg(target_arch = "wasm32")]
const PIPE_NAME: &str = "zj_radar.status.v1";

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
    // `tick`/`timer_armed`/`applied_names` are read only by the wasm glue; on
    // any host build (including tests, which construct State but never read
    // them) they are dead.
    #[cfg_attr(not(target_arch = "wasm32"), allow(dead_code))]
    tick: u64,
    #[cfg_attr(not(target_arch = "wasm32"), allow(dead_code))]
    timer_armed: bool,
    #[cfg_attr(not(target_arch = "wasm32"), allow(dead_code))]
    applied_names: HashMap<usize, String>,
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
                agg: model::aggregate(&ids, &self.store),
            });
        }
        rows
    }

    /// Map a clicked line back to a tab position by replaying render()'s line
    /// counting: 1 line for plain tabs, 2 for tabs with agent detail.
    fn tab_position_at_line(&self, line: isize) -> Option<usize> {
        if line < 0 {
            return None;
        }
        let target = line as usize;
        let rows = self.build_rows();
        let mut cursor = render::header_lines(&rows); // header occupies the first line(s)
        if target < cursor {
            return None; // click landed on the header → no tab
        }
        let mut sorted = self.tabs.clone();
        sorted.sort_by_key(|t| t.position);
        for t in &sorted {
            let empty = Vec::new();
            let panes = self.tab_panes.get(&t.position).unwrap_or(&empty);
            let ids: Vec<u32> = panes.iter().map(|p| p.id).collect();
            let agg = model::aggregate(&ids, &self.store);
            let span = render::row_lines(&agg);
            if target >= cursor && target < cursor + span {
                return Some(t.position);
            }
            cursor += span;
        }
        None
    }
}

// ── Wasm-only glue — each item gated so host `cargo test` never links these.
// `register_plugin!` lives in the BINARY crate (`src/main.rs`) so the `fn main`
// it generates becomes the wasm `_start` Zellij requires; here we only provide
// the `ZellijPlugin` impl + host-fn helpers it drives.

#[cfg(target_arch = "wasm32")]
impl State {
    fn arm_timer_if_needed(&mut self) {
        if !self.timer_armed && self.store.any_active() {
            set_timeout(1.0);
            self.timer_armed = true;
        }
    }

    fn apply_renames(&mut self) {
        let tabs: Vec<(usize, String)> = self
            .tabs
            .iter()
            .map(|t| (t.position, t.name.clone()))
            .collect();
        let changes =
            naming::compute_renames(&tabs, &self.tab_panes, &self.store, &self.applied_names);
        for (pos, name) in changes {
            rename_tab(pos as u32 + 1, &name);
            self.applied_names.insert(pos, name);
        }
    }
}

#[cfg(target_arch = "wasm32")]
impl ZellijPlugin for State {
    fn load(&mut self, _config: BTreeMap<String, String>) {
        request_permission(&[
            PermissionType::ReadApplicationState,
            PermissionType::ReadCliPipes,
            PermissionType::ChangeApplicationState,
        ]);
        subscribe(&[
            EventType::TabUpdate,
            EventType::PaneUpdate,
            EventType::Timer,
            EventType::Mouse,
            EventType::PermissionRequestResult,
        ]);
        set_selectable(false);
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
                for (tab_pos, panes) in manifest.panes {
                    for p in panes {
                        if p.is_plugin {
                            continue;
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
                    }
                }
                self.tab_panes = tab_panes;
                self.store.prune(&live);
                if let Some(id) = focused_terminal {
                    self.store.on_pane_focused(id, self.tick);
                }
                self.apply_renames();
                true
            }
            Event::Timer(_) => {
                self.timer_armed = false;
                self.tick += 1;
                self.arm_timer_if_needed();
                self.store.any_active()
            }
            Event::Mouse(Mouse::LeftClick(line, _col)) => {
                if let Some(pos) = self.tab_position_at_line(line) {
                    // switch_tab_to is 1-based; `pos` is 0-based position,
                    // so position + 1 gives the correct tab index.
                    switch_tab_to(pos as u32 + 1);
                }
                false
            }
            Event::PermissionRequestResult(_) => true,
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
                    return true;
                }
            }
        }
        false
    }

    fn render(&mut self, rows: usize, cols: usize) {
        let tabrows = self.build_rows();
        let opts = render::RenderOpts {
            width: cols.max(1),
            height: rows,
            now_tick: self.tick,
            glyphs: render::GlyphSet::Nerd,
        };
        print!("{}", render::render(&tabrows, &opts));
    }
}

// ── Host unit tests (no host calls — pure helpers only) ──

#[cfg(test)]
mod tests {
    use super::*;
    use crate::payload::StatusPayload;
    use crate::status::Status;

    fn pane(id: u32) -> PaneLite {
        PaneLite { id, ..Default::default() }
    }

    fn make_state_with_tabs(tab_specs: &[(usize, &str, bool)]) -> State {
        // tab_specs: (position, name, active)
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
    fn agent_tab_pending_with_msg_occupies_three_lines() {
        let mut state = make_state_with_tabs(&[(0, "agent", false), (1, "plain", false)]);
        state.tab_panes.insert(0, vec![pane(10)]);
        apply_payload_with_msg(&mut state, 10, Status::Pending, 1, "approve?"); // pending+msg → 3
        assert_eq!(state.tab_position_at_line(1), None);       // header
        assert_eq!(state.tab_position_at_line(2), Some(0));    // line 1
        assert_eq!(state.tab_position_at_line(3), Some(0));    // line 2
        assert_eq!(state.tab_position_at_line(4), Some(0));    // line 3
        assert_eq!(state.tab_position_at_line(5), Some(1));    // plain tab
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
}

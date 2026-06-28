//! Deep runtime module: repo-owned events in, ordered host effects out.
//! No zellij-tile dependency.

use crate::command;
use crate::config;
use crate::model;
use crate::naming::{self, PaneLite};
use crate::payload;
#[cfg(test)]
use crate::render::RailTarget;
use crate::render::{self, RenderedRail, TabRow};
use crate::state::StateStore;
use crate::theme;
use std::collections::{BTreeMap, HashMap, HashSet};

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct TabLite {
    pub position: usize,
    pub name: String,
    pub active: bool,
    pub has_bell: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum PermissionMarker {
    Granted,
    Denied,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) struct PermissionProbe {
    pub marker: Option<PermissionMarker>,
    pub lock_acquired: bool,
}

#[derive(Debug)]
pub(crate) struct PaneUpdate {
    pub tab_panes: HashMap<usize, Vec<PaneLite>>,
    pub live: HashSet<u32>,
    pub focused_terminal: Option<u32>,
    pub theme: Option<theme::DerivedColors>,
    pub exits: Vec<(u32, Option<i32>)>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum Effect {
    RequestPermission,
    SetSelectable(bool),
    SetTimeout,
    PersistSnapshot(String),
    PersistPermissionMarker(PermissionMarker),
    RenameTab { position: usize, name: String },
    SwitchTab { position: usize },
    ShowPane { pane_id: u32 },
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct Outcome {
    pub render: bool,
    pub effects: Vec<Effect>,
}

impl Outcome {
    fn none() -> Self {
        Self::default()
    }

    fn render() -> Self {
        Self {
            render: true,
            effects: Vec::new(),
        }
    }

    fn with_effects(render: bool, effects: Vec<Effect>) -> Self {
        Self { render, effects }
    }
}

#[derive(Default)]
pub(crate) struct PluginRuntime {
    pub(crate) store: StateStore,
    pub(crate) tabs: Vec<TabLite>,
    pub(crate) tab_panes: HashMap<usize, Vec<PaneLite>>,
    pub(crate) pane_cwd: HashMap<u32, String>,
    pub(crate) tick: u64,
    pub(crate) timer_armed: bool,
    pub(crate) applied_names: HashMap<usize, String>,
    pub(crate) last_render_height: usize,
    pub(crate) config: config::Config,
    pub(crate) permission_granted: bool,
    pub(crate) permission_response_received: bool,
    pub(crate) permission_request_started: bool,
    pub(crate) permission_waiting_for_peer: bool,
    pub(crate) theme: theme::DerivedColors,
    pub(crate) command: command::CommandStore,
    pub(crate) last_focused: Option<u32>,
    last_rendered: RenderedRail,
}

impl PluginRuntime {
    pub(crate) fn load(
        &mut self,
        config: config::Config,
        snapshot: Option<&str>,
        permission: PermissionProbe,
    ) -> Outcome {
        self.config = config;
        if let Some(raw) = snapshot {
            if let Some((store, tick)) = StateStore::from_json(raw) {
                self.store = store;
                self.tick = tick;
            }
        }
        self.begin_permission_flow(permission)
    }

    pub(crate) fn build_rows(&self) -> Vec<TabRow> {
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

    pub(crate) fn tabs_changed(&mut self, tabs: Vec<TabLite>) -> Outcome {
        self.tabs = tabs;
        Outcome::render()
    }

    pub(crate) fn panes_changed(&mut self, update: PaneUpdate) -> Outcome {
        for (pane_id, exit_status) in update.exits {
            self.command.on_exit(pane_id, exit_status, self.tick);
        }
        if let Some(theme) = update.theme {
            self.theme = theme;
        }
        self.tab_panes = update.tab_panes;
        self.store.prune(&update.live);
        self.command.prune(&update.live);
        self.pane_cwd.retain(|id, _| update.live.contains(id));
        self.apply_focus_transition(update.focused_terminal, self.tick);

        let mut effects = self.rename_effects();
        effects.push(Effect::PersistSnapshot(self.store.to_json(self.tick)));
        Outcome::with_effects(true, effects)
    }

    pub(crate) fn timer(&mut self, permission_marker: Option<PermissionMarker>) -> Outcome {
        self.timer_armed = false;
        let mut effects = Vec::new();
        let permission_changed =
            self.check_deferred_permission_request(permission_marker, &mut effects);
        self.tick += 1;
        self.command.on_timer(self.tick);
        self.arm_timer_if_needed(&mut effects);
        Outcome::with_effects(
            permission_changed || self.store.any_active() || self.command.has_pending_or_active(),
            effects,
        )
    }

    pub(crate) fn mouse_click(&self, line: isize) -> Outcome {
        if !self.permission_granted {
            return Outcome::none();
        }
        let Some(target) = self.last_rendered.target_at_line(line) else {
            return Outcome::none();
        };
        let effect = match target.pane_id {
            Some(pane_id) => Effect::ShowPane { pane_id },
            None => Effect::SwitchTab {
                position: target.tab_position,
            },
        };
        Outcome::with_effects(false, vec![effect])
    }

    pub(crate) fn permission_result(&mut self, granted: bool) -> Outcome {
        self.record_permission_result(granted);
        Outcome::with_effects(
            true,
            vec![
                Effect::PersistPermissionMarker(if granted {
                    PermissionMarker::Granted
                } else {
                    PermissionMarker::Denied
                }),
                Effect::SetSelectable(self.sidebar_should_be_selectable()),
            ],
        )
    }

    pub(crate) fn cwd_changed(&mut self, pane_id: u32, path: String) -> Outcome {
        self.pane_cwd.insert(pane_id, path);
        Outcome::with_effects(true, self.rename_effects())
    }

    pub(crate) fn command_changed(
        &mut self,
        pane_id: u32,
        command: &[String],
        is_foreground: bool,
    ) -> Outcome {
        let cwd = self.pane_cwd.get(&pane_id).map(|s| s.as_str());
        self.command
            .on_command_changed(pane_id, command, is_foreground, cwd, self.tick);
        let mut effects = Vec::new();
        self.arm_timer_if_needed(&mut effects);
        Outcome::with_effects(true, effects)
    }

    pub(crate) fn status_pipe(&mut self, raw: &str) -> Outcome {
        let Some(p) = payload::parse(raw) else {
            return Outcome::none();
        };
        self.store.apply(p, self.tick);
        let mut effects = self.rename_effects();
        self.arm_timer_if_needed(&mut effects);
        effects.push(Effect::PersistSnapshot(self.store.to_json(self.tick)));
        Outcome::with_effects(true, effects)
    }

    pub(crate) fn config_pipe(&mut self, raw: &str) -> Outcome {
        let Ok(val) = serde_json::from_str::<serde_json::Value>(raw) else {
            return Outcome::none();
        };
        let Some(obj) = val.as_object() else {
            return Outcome::none();
        };
        let kv: BTreeMap<String, String> = obj
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
        Outcome::with_effects(true, self.rename_effects())
    }

    pub(crate) fn render(&mut self, rows: usize, cols: usize) -> String {
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
        let rail = if !self.permission_granted || tabrows.is_empty() {
            render::onboarding(&opts)
        } else {
            render::render_rail(&tabrows, &opts)
        };
        let ansi = rail.ansi.clone();
        self.last_rendered = rail;
        ansi
    }

    pub(crate) fn sidebar_should_be_selectable(&self) -> bool {
        self.permission_request_started && !self.permission_response_received
    }

    pub(crate) fn record_permission_request_started(&mut self) {
        self.permission_request_started = true;
    }

    pub(crate) fn record_permission_result(&mut self, granted: bool) {
        self.permission_granted = granted;
        self.permission_response_received = true;
        self.permission_request_started = false;
        self.permission_waiting_for_peer = false;
    }

    pub(crate) fn apply_focus_transition(&mut self, focused: Option<u32>, tick: u64) -> bool {
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

    #[cfg(test)]
    pub(crate) fn target_at_line_for_current_rows(
        &self,
        line: isize,
    ) -> Option<(usize, Option<u32>)> {
        let target = self.rendered_target_for_current_rows(line)?;
        Some((target.tab_position, target.pane_id))
    }

    #[cfg(test)]
    pub(crate) fn tab_position_at_line_for_current_rows(&self, line: isize) -> Option<usize> {
        self.target_at_line_for_current_rows(line)
            .map(|(pos, _pane)| pos)
    }

    #[cfg(test)]
    fn rendered_target_for_current_rows(&self, line: isize) -> Option<RailTarget> {
        let rows = self.build_rows();
        if rows.is_empty() {
            return None;
        }
        let height = if self.last_render_height == 0 {
            usize::MAX
        } else {
            self.last_render_height
        };
        let opts = render::RenderOpts {
            width: 80,
            height,
            now_tick: self.tick,
            glyphs: self.config.glyphs,
            header: self.config.header,
            density: self.config.density,
            theme: self.theme.clone(),
        };
        render::render_rail(&rows, &opts).target_at_line(line)
    }

    fn begin_permission_flow(&mut self, permission: PermissionProbe) -> Outcome {
        let mut effects = Vec::new();
        match permission.marker {
            Some(PermissionMarker::Granted) => {
                self.record_permission_request_started();
                effects.push(Effect::RequestPermission);
            }
            Some(PermissionMarker::Denied) => {
                self.record_permission_result(false);
            }
            None if permission.lock_acquired => {
                self.record_permission_request_started();
                effects.push(Effect::RequestPermission);
            }
            None => {
                self.permission_waiting_for_peer = true;
                self.arm_timer_if_needed(&mut effects);
            }
        }
        effects.push(Effect::SetSelectable(self.sidebar_should_be_selectable()));
        Outcome::with_effects(false, effects)
    }

    fn check_deferred_permission_request(
        &mut self,
        marker: Option<PermissionMarker>,
        effects: &mut Vec<Effect>,
    ) -> bool {
        if !self.permission_waiting_for_peer {
            return false;
        }
        match marker {
            Some(PermissionMarker::Granted) => {
                self.permission_waiting_for_peer = false;
                self.record_permission_request_started();
                effects.push(Effect::RequestPermission);
                effects.push(Effect::SetSelectable(self.sidebar_should_be_selectable()));
                true
            }
            Some(PermissionMarker::Denied) => {
                self.record_permission_result(false);
                effects.push(Effect::SetSelectable(self.sidebar_should_be_selectable()));
                true
            }
            None => false,
        }
    }

    fn arm_timer_if_needed(&mut self, effects: &mut Vec<Effect>) {
        if !self.timer_armed
            && (self.permission_waiting_for_peer
                || self.store.any_active()
                || self.command.has_pending_or_active())
        {
            self.timer_armed = true;
            effects.push(Effect::SetTimeout);
        }
    }

    fn rename_effects(&mut self) -> Vec<Effect> {
        if self.config.naming == config::NamingMode::Off {
            return Vec::new();
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
        for (pos, name) in &changes {
            self.applied_names.insert(*pos, name.clone());
        }
        changes
            .into_iter()
            .map(|(position, name)| Effect::RenameTab { position, name })
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::{Density, NamingMode};
    use crate::payload::{self, StatusPayload};
    use crate::status::{GlyphSet, Status};

    fn tab(position: usize, name: &str, active: bool) -> TabLite {
        TabLite {
            position,
            name: name.into(),
            active,
            has_bell: false,
        }
    }

    fn pane(id: u32) -> PaneLite {
        PaneLite {
            id,
            ..Default::default()
        }
    }

    fn payload_for(pane_id: u32, status: Status) -> StatusPayload {
        StatusPayload {
            pane_id,
            status,
            repo: "repo".into(),
            branch: "main".into(),
            msg: "working".into(),
            on_focus: None,
            seq: None,
            source: "claude".into(),
        }
    }

    fn config() -> config::Config {
        config::Config {
            naming: NamingMode::Off,
            density: Density::Compact,
            ..config::Config::default()
        }
    }

    fn runtime_with_config(config: config::Config) -> PluginRuntime {
        PluginRuntime {
            config,
            ..Default::default()
        }
    }

    #[test]
    fn load_rehydrates_snapshot_and_requests_permission_for_owner() {
        let mut seeded = StateStore::default();
        seeded.apply(payload_for(9, Status::Running), 7);
        let snapshot = seeded.to_json(7);

        let mut runtime = PluginRuntime::default();
        let outcome = runtime.load(
            config(),
            Some(&snapshot),
            PermissionProbe {
                marker: None,
                lock_acquired: true,
            },
        );

        assert_eq!(runtime.tick, 7);
        assert_eq!(runtime.store.get(9).unwrap().status, Status::Running);
        assert!(runtime.permission_request_started);
        assert_eq!(
            outcome,
            Outcome {
                render: false,
                effects: vec![Effect::RequestPermission, Effect::SetSelectable(true)],
            }
        );
    }

    #[test]
    fn load_denied_marker_records_denial_without_requesting_permission() {
        let mut runtime = PluginRuntime::default();
        let outcome = runtime.load(
            config(),
            None,
            PermissionProbe {
                marker: Some(PermissionMarker::Denied),
                lock_acquired: false,
            },
        );

        assert!(!runtime.permission_granted);
        assert!(runtime.permission_response_received);
        assert_eq!(
            outcome,
            Outcome {
                render: false,
                effects: vec![Effect::SetSelectable(false)],
            }
        );
    }

    #[test]
    fn peer_waits_then_requests_after_granted_marker() {
        let mut runtime = PluginRuntime::default();
        let load = runtime.load(
            config(),
            None,
            PermissionProbe {
                marker: None,
                lock_acquired: false,
            },
        );
        assert!(runtime.permission_waiting_for_peer);
        assert_eq!(
            load.effects,
            vec![Effect::SetTimeout, Effect::SetSelectable(false)]
        );

        let timer = runtime.timer(Some(PermissionMarker::Granted));

        assert!(timer.render);
        assert!(runtime.permission_request_started);
        assert!(!runtime.permission_waiting_for_peer);
        assert_eq!(
            timer.effects,
            vec![Effect::RequestPermission, Effect::SetSelectable(true)]
        );
    }

    #[test]
    fn permission_result_persists_marker_and_updates_selectability() {
        let mut runtime = PluginRuntime::default();
        runtime.record_permission_request_started();

        let outcome = runtime.permission_result(true);

        assert!(runtime.permission_granted);
        assert!(runtime.permission_response_received);
        assert_eq!(
            outcome,
            Outcome {
                render: true,
                effects: vec![
                    Effect::PersistPermissionMarker(PermissionMarker::Granted),
                    Effect::SetSelectable(false),
                ],
            }
        );
    }

    #[test]
    fn status_pipe_mutates_store_arms_timer_and_persists_snapshot() {
        let mut runtime = runtime_with_config(config());
        let raw = payload::to_wire(
            5,
            Status::Running,
            "repo",
            "main",
            "cargo test",
            Some(Status::Idle),
            "claude",
        );

        let outcome = runtime.status_pipe(&raw);

        assert!(outcome.render);
        assert!(runtime.store.any_active());
        assert_eq!(outcome.effects.len(), 2);
        assert_eq!(outcome.effects[0], Effect::SetTimeout);
        let Effect::PersistSnapshot(json) = &outcome.effects[1] else {
            panic!("expected persisted snapshot");
        };
        let (restored, tick) = StateStore::from_json(json).expect("valid snapshot");
        assert_eq!(tick, 0);
        assert_eq!(restored.get(5).unwrap().status, Status::Running);
    }

    #[test]
    fn panes_changed_prunes_focuses_and_persists_snapshot() {
        let mut runtime = runtime_with_config(config());
        runtime.store.apply(payload_for(10, Status::Running), 1);
        runtime.store.apply(payload_for(11, Status::Running), 1);
        runtime.command.on_exit(12, Some(0), 1);

        let mut live = HashSet::new();
        live.insert(10);
        let mut tab_panes = HashMap::new();
        tab_panes.insert(0, vec![pane(10)]);

        let outcome = runtime.panes_changed(PaneUpdate {
            tab_panes,
            live,
            focused_terminal: Some(10),
            theme: Some(theme::DerivedColors::default()),
            exits: vec![(10, Some(0))],
        });

        assert!(outcome.render);
        assert_eq!(runtime.last_focused, Some(10));
        assert!(runtime.store.get(11).is_none());
        assert!(runtime.command.get(12).is_none());
        assert!(outcome
            .effects
            .iter()
            .any(|effect| matches!(effect, Effect::PersistSnapshot(_))));
    }

    #[test]
    fn cwd_change_renames_default_named_tab_and_command_uses_cwd() {
        let mut runtime = runtime_with_config(config::Config {
            naming: NamingMode::Managed,
            density: Density::Compact,
            ..config::Config::default()
        });
        runtime.tabs_changed(vec![tab(0, "Tab #1", true)]);
        runtime.tab_panes.insert(0, vec![pane(7)]);

        let rename = runtime.cwd_changed(7, "/work/myrepo".into());

        assert_eq!(
            rename.effects,
            vec![Effect::RenameTab {
                position: 0,
                name: "myrepo".into(),
            }]
        );
        assert_eq!(
            runtime.applied_names.get(&0).map(String::as_str),
            Some("myrepo")
        );

        let command = vec!["cargo".to_string(), "test".to_string()];
        let command_outcome = runtime.command_changed(7, &command, true);
        assert_eq!(command_outcome.effects, vec![Effect::SetTimeout]);

        let timer = runtime.timer(None);
        assert!(timer.render);
        assert_eq!(timer.effects, vec![Effect::SetTimeout]);
        let state = runtime.command.get(7).expect("promoted command");
        assert_eq!(state.status, Status::Running);
        assert_eq!(state.repo, "myrepo");
    }

    #[test]
    fn config_pipe_accepts_json_scalars() {
        let mut runtime = PluginRuntime::default();

        let outcome = runtime
            .config_pipe(r#"{"header":false,"density":"compact","glyphs":"nerd","naming":"off"}"#);

        assert!(outcome.render);
        assert_eq!(runtime.config.naming, NamingMode::Off);
        assert_eq!(runtime.config.density, Density::Compact);
        assert_eq!(runtime.config.glyphs, GlyphSet::Nerd);
        assert!(!runtime.config.header);
    }

    #[test]
    fn render_records_targets_and_mouse_click_returns_host_effect() {
        let mut runtime = PluginRuntime {
            permission_granted: true,
            config: config(),
            ..Default::default()
        };
        runtime.tabs_changed(vec![tab(0, "team", false), tab(1, "plain", false)]);
        runtime
            .tab_panes
            .insert(0, vec![pane(20), pane(21), pane(22)]);
        runtime.store.apply(payload_for(20, Status::Pending), 1);
        runtime.store.apply(payload_for(21, Status::Running), 1);
        runtime.store.apply(payload_for(22, Status::Running), 1);

        let ansi = runtime.render(100, 80);
        assert!(ansi.contains("team"));

        let tab_click = runtime.mouse_click(2);
        let pane_click = runtime.mouse_click(3);
        let collapse_click = runtime.mouse_click(4);

        assert_eq!(tab_click.effects, vec![Effect::SwitchTab { position: 0 }]);
        assert_eq!(pane_click.effects, vec![Effect::ShowPane { pane_id: 20 }]);
        assert_eq!(
            collapse_click.effects,
            vec![Effect::SwitchTab { position: 0 }]
        );
    }

    #[test]
    fn mouse_click_is_ignored_until_permission_granted() {
        let mut runtime = runtime_with_config(config());
        runtime.tabs_changed(vec![tab(0, "team", false)]);
        runtime.render(100, 80);

        assert_eq!(runtime.mouse_click(2), Outcome::default());
    }
}

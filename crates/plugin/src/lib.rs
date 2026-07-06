//! The Zellij sidebar wasm plugin: per-tab agent-status rail.
//!
//! # Shape
//! - [`State`] is the per-tab plugin instance Zellij loads. It owns a
//!   [`runtime::PluginRuntime`] (the host-testable state machine — pure apart
//!   from wall-clock reads via `clock::now_epoch_s`; the stores it drives take
//!   epochs as arguments, which is where determinism matters) and a
//!   [`session_files::SessionFiles`] (snapshot + permission-marker persistence).
//! - The `#[cfg(target_arch = "wasm32")] impl ZellijPlugin for State` block is the
//!   only code that touches the Zellij host API; everything it calls is pure logic
//!   in the modules below. Host tests exercise that logic without a wasm build.
//! - The `#[cfg(test)] impl State` block holds test-only fixtures (`build_rows`,
//!   …) that are never linked into the shipped wasm.
//!
//! The shared wire/classification types are re-exported from [`zj_radar_core`]
//! (see below) so the modules here address them as `crate::status`,
//! `crate::payload`, … with no per-reference churn at the crate boundary.

// On a plain host build (`cargo build`, not wasm, not test) the only consumers
// of the pure modules are the wasm glue (cfg'd out) and the unit tests (cfg'd
// out), so every public item appears dead. One crate-level allow covers all
// modules, replacing the 13 per-module #[cfg_attr] annotations from the
// previous monolithic layout. Dead-code detection still runs under
// `cargo test` (not(test) is false) and the wasm build.
#![cfg_attr(all(not(target_arch = "wasm32"), not(test)), allow(dead_code))]

// Re-export the shared core so the plugin glue and modules keep addressing these
// as `crate::status`, `crate::payload`, … with no per-reference churn.
pub(crate) use zj_radar_core::{command, kind, observation, payload, status};

mod clock;
mod config;
mod control;
mod ledger;
mod notify_rules;
mod permission;
mod radar_state;
#[cfg(test)]
mod reference_tests;
mod render;
mod rollup;
mod runtime;
mod session_files;
mod status_store;
mod tab_namer;
mod theme;

// RadarTab/TabId are used by the wasm glue (and tests); on a plain host build
// both the glue and tests are cfg'd out, so the import is allowed to go unused
// there. TerminalPane is only ever a test fixture, so gate it on `test` directly
// — that keeps the shipped wasm build (where unused_imports is denied) clean.
#[cfg_attr(all(not(target_arch = "wasm32"), not(test)), allow(unused_imports))]
use radar_state::{RadarTab, TabId};
#[cfg(test)]
use rollup::TerminalPane;
#[cfg(test)]
use rollup::TabRow;
use runtime::PluginRuntime;
use session_files::SessionFiles;

#[cfg(target_arch = "wasm32")]
use runtime::Effect;
#[cfg(target_arch = "wasm32")]
use session_files::SessionFileIds;
#[cfg(target_arch = "wasm32")]
use std::collections::BTreeMap;
#[cfg(target_arch = "wasm32")]
use zellij_tile::prelude::*;

// Pipe names live with their vocabularies (host-testable, so the guard tests
// in control.rs/lib.rs can pin them against docs and the bash producer);
// these aliases just keep the match arms below short.
#[cfg(target_arch = "wasm32")]
const PIPE_NAME: &str = payload::STATUS_PIPE_NAME;
#[cfg(target_arch = "wasm32")]
const CONFIG_PIPE: &str = config::CONFIG_PIPE;
#[cfg(target_arch = "wasm32")]
const CMD_PIPE: &str = control::CMD_PIPE;

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
    /// When no height has been set yet (last_render_height == 0), use this
    /// session's natural content height (see `PluginRuntime::natural_height`)
    /// so folding/overflow never discards rows unexpectedly — and, since Task
    /// 13, so the bottom region's pinned footer doesn't pad the render out to
    /// an unboundedly large height (`usize::MAX / 2` used to be a safe "big
    /// enough" sentinel; now it would land in the footer's unbounded-filler
    /// branch and blow the allocator).
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
        self.runtime.permission = crate::permission::PermissionState::Resolved { granted: true };
        let height = if self.runtime.last_render_height == 0 {
            self.runtime.natural_height(width)
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
                // Keep in lockstep with REQUIRED_PLUGIN_PERMISSIONS in
                // crates/cli/src/run.rs: adding a permission here makes every
                // existing grant partial, and the CLI must detect that (a
                // partial grant re-prompts illegibly inside the rail).
                Effect::RequestPermission => request_permission(&[
                    PermissionType::ReadApplicationState,
                    PermissionType::ReadCliPipes,
                    PermissionType::ChangeApplicationState,
                    PermissionType::RunCommands,
                ]),
                Effect::SetSelectable(selectable) => set_selectable(selectable),
                Effect::SetTimeout(cadence) => set_timeout(cadence.seconds()),
                Effect::PersistSnapshot => {
                    let existing = self.session_files.snapshot();
                    let json = self.runtime.snapshot_json(existing.as_deref());
                    self.session_files.persist_snapshot(&json);
                }
                Effect::PersistPermissionMarker(marker) => {
                    self.session_files.persist_permission_marker(marker)
                }
                Effect::HeartbeatPermissionLock => {
                    self.session_files.heartbeat_permission_lock()
                }
                Effect::RenameTab { position, name } => rename_tab(position as u32 + 1, &name),
                Effect::SwitchTab { position } => switch_tab_to(position as u32 + 1),
                Effect::ShowPane { pane_id } => {
                    show_pane_with_id(PaneId::Terminal(pane_id), false, true);
                }
                Effect::ResolveCwd { pane_ids } => self.resolve_cwd(pane_ids),
                Effect::CloseSelf => close_self(),
                Effect::Notify { key, title, body } => {
                    // Every per-tab instance emits this same effect for the
                    // same event; the shared claim file elects exactly one
                    // dispatcher so N visited tabs ≠ N identical toasts.
                    if self.session_files.claim_notification(&key) {
                        let argv = crate::notify_rules::notify_command(&title, &body);
                        let args: Vec<&str> = argv.iter().map(String::as_str).collect();
                        run_command(&args, std::collections::BTreeMap::new());
                    }
                }
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
                // Pure adapter: copy the host's `PaneInfo` fields into `RawPane`s.
                // All policy — plugin-skip, title sanitize, theme-color precedence —
                // lives in the host-tested `PaneUpdate::from_raw`.
                let raw: Vec<radar_state::RawPane> = manifest
                    .panes
                    .into_iter()
                    .flat_map(|(tab_pos, panes)| {
                        panes.into_iter().map(move |p| radar_state::RawPane {
                            tab_pos,
                            id: p.id,
                            title: p.title,
                            is_plugin: p.is_plugin,
                            is_focused: p.is_focused,
                            default_bg: p.default_bg,
                            default_fg: p.default_fg,
                            exited: p.exited,
                            exit_status: p.exit_status,
                        })
                    })
                    .collect();
                let outcome = self
                    .runtime
                    .panes_changed(radar_state::PaneUpdate::from_raw(raw));
                self.handle_outcome(outcome)
            }
            Event::Timer(elapsed_s) => {
                // Re-probe (marker + lock) each tick so a waiting peer can take
                // over a prompt whose owner died holding a now-stale lock.
                // Upstream documents `elapsed_s` only as the timer-expiry
                // payload: elapsed seconds since the `set_timeout` call, which
                // for a fired one-shot is ~= the duration it was armed with —
                // not a guaranteed-exact echo of it. That's all the runtime
                // needs: its 5s stale-fire threshold discriminates a ~1s (Fast)
                // arm from a ~60s (Slow) one with wide margin either way, so
                // scheduler jitter can't flip the classification.
                let probe = self.session_files.refresh_permission_probe();
                let outcome = self.runtime.timer(probe, elapsed_s);
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
                false // plugin panes: nothing observed, nothing to repaint
            }
            Event::CommandChanged(pane_id, command, is_foreground, _clients) => {
                if let PaneId::Terminal(id) = pane_id {
                    let outcome = self.runtime.command_changed(id, &command, is_foreground);
                    return self.handle_outcome(outcome);
                }
                false // plugin panes: nothing observed, nothing to repaint
            }
            _ => false,
        }
    }

    fn pipe(&mut self, message: PipeMessage) -> bool {
        let Some(raw) = &message.payload else { return false };
        let outcome = match message.name.as_str() {
            PIPE_NAME => self.runtime.status_pipe(raw),
            CONFIG_PIPE => self.runtime.config_pipe(raw),
            CMD_PIPE => self.runtime.control_pipe(raw),
            _ => return false,
        };
        self.handle_outcome(outcome)
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

    /// The bash fallback producer can't import `payload::STATUS_PIPE_NAME`, so
    /// its hand-typed literal is pinned here — one drifted character in a pipe
    /// name is a silently dead producer, the least debuggable failure this
    /// system has. (This crate isn't published to crates.io, so reaching
    /// outside the crate dir is safe — same pattern as `reference_tests.rs`.)
    #[test]
    fn bash_producer_broadcasts_on_the_shared_pipe_name() {
        let sh = include_str!("../../../plugins/zj-radar-claude/scripts/notify.sh");
        let broadcast = format!("zellij pipe --name {} --", payload::STATUS_PIPE_NAME);
        assert!(
            sh.contains(&broadcast),
            "notify.sh must broadcast via `{broadcast}`; if the pipe name is \
             versioned, bump payload::STATUS_PIPE_NAME and notify.sh together"
        );
    }

    /// The CLI's grant probe (`REQUIRED_PLUGIN_PERMISSIONS` in
    /// crates/cli/src/run.rs) must cover exactly the set this plugin requests:
    /// a permission added to the request list but not the probe makes every
    /// existing grant partial, and Zellij's re-prompt is illegible inside the
    /// rail (zellij#4749) — the failure presents as a silently blank rail.
    /// Text-pinned like the pipe-name guard above, because the two lists live
    /// on opposite sides of the wasm boundary and can't share a const.
    #[test]
    fn cli_grant_probe_covers_the_exact_requested_permission_set() {
        let full_src = include_str!("lib.rs");
        // Scan only the production half of this file (this test would match
        // itself); there, every such token is the request list — the SDK type
        // is referenced nowhere else in the plugin.
        let plugin_src = &full_src[..full_src.find("mod tests").expect("tests module marker")];
        let requested: std::collections::BTreeSet<&str> = plugin_src
            .match_indices("PermissionType::")
            .map(|(at, pat)| {
                let rest = &plugin_src[at + pat.len()..];
                &rest[..rest.find(|c: char| !c.is_alphanumeric()).unwrap_or(rest.len())]
            })
            .collect();
        assert!(!requested.is_empty(), "request list not found in lib.rs");

        let cli_src = include_str!("../../cli/src/run.rs");
        let decl = cli_src.find("const REQUIRED_PLUGIN_PERMISSIONS").expect("cli probe const present");
        let eq = decl + cli_src[decl..].find('=').expect("const has an initializer");
        let open = eq + cli_src[eq..].find('[').expect("initializer is an array");
        let close = open + cli_src[open..].find(']').expect("array closes");
        let probed: std::collections::BTreeSet<&str> =
            cli_src[open..close].split('"').skip(1).step_by(2).collect();

        assert_eq!(
            requested, probed,
            "the plugin's request_permission list and the CLI's \
             REQUIRED_PLUGIN_PERMISSIONS probe drifted — update both together \
             (a partial grant presents as a silently blank rail)"
        );
    }

    /// The doctor's version gate (`SUPPORTED_ZELLIJ_MINOR` +
    /// `MIN_SUPPORTED_ZELLIJ_PATCH` in the CLI's setup/analyze.rs) and this
    /// crate's exact `zellij-tile` pin must move together: the pin is what the
    /// ABI actually targets, the gate is what `--check` tells users about it.
    /// Text-pinned like the grant-probe guard above, for the same reason.
    #[test]
    fn doctor_version_gate_matches_the_zellij_tile_pin() {
        let manifest = include_str!("../Cargo.toml");
        let pin_line = manifest
            .lines()
            .find(|l| l.trim_start().starts_with("zellij-tile"))
            .expect("plugin Cargo.toml declares zellij-tile");
        let analyze_src = include_str!("../../cli/src/setup/analyze.rs");
        let const_value = |name: &str| {
            let decl = analyze_src.find(&format!("const {name}")).expect("const present in analyze.rs");
            let eq = decl + analyze_src[decl..].find('=').expect("const has an initializer");
            analyze_src[eq + 1..].split(';').next().unwrap().trim().trim_matches('"').to_string()
        };
        let gate = format!(
            "\"={}.{}\"",
            const_value("SUPPORTED_ZELLIJ_MINOR"),
            const_value("MIN_SUPPORTED_ZELLIJ_PATCH")
        );
        assert!(
            pin_line.contains(&gate),
            "the doctor's version gate targets {gate} but the plugin pins: `{pin_line}` — \
             update SUPPORTED_ZELLIJ_MINOR / MIN_SUPPORTED_ZELLIJ_PATCH alongside the pin"
        );
    }

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
        let _ = state.runtime.radar.status_mut().apply(
            StatusPayload {
                pane_id,
                status,
                repo: "repo".into(),
                branch: "branch".into(),
                msg: msg.into(),
                task: String::new(),
                source: "test".into(),
            },
            tick,
            0,
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
        assert!(!s.runtime.permission.granted());
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
        // The untracked pane has no rendered line; the detail line targets the
        // tab's one tracked pane (20), same as the multi-pane tree rows — a
        // click routes to `Effect::ShowPane`, not `SwitchTab`.
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
            Some((0, Some(20))),
            "detail line targets the single tracked pane"
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

    // ── Event-sequence walk: shell→idle, fg command→pending→Running, exit→Done ──

    #[test]
    fn command_pane_walks_idle_running_done() {
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
        state.runtime.radar.command_mut().on_timer(tick, 0);
        assert!(
            state.runtime.radar.command_store().get(5).is_none(),
            "shell prompt must leave pane idle"
        );

        // 2) real fg command → pending (not yet Running); after DEBOUNCE_TICKS timer → Running
        // pending since_tick = tick; a timer at (tick + DEBOUNCE_TICKS) satisfies
        // (tick + DEBOUNCE_TICKS) - tick = DEBOUNCE_TICKS >= DEBOUNCE_TICKS.
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
        state.runtime.radar.command_mut().on_timer(tick, 0);
        assert_eq!(
            state.runtime.radar.command_store().get(5).map(|s| s.status),
            Some(Status::Running),
            "must promote to Running after debounce (since={}, tick={})",
            since,
            tick
        );

        // 3) pane exits with code 0 → Done (and stays Done; focus no longer clears it)
        tick += 1;
        state.runtime.radar.command_mut().on_exit(5, Some(0), tick, 0);
        assert_eq!(
            state.runtime.radar.command_store().get(5).map(|s| s.status),
            Some(Status::Done),
            "exit 0 must set Done"
        );
    }

    // ── mouse_click → Effect end-to-end tests ──

    #[test]
    fn mouse_click_on_tab_row_emits_switch_tab_effect() {
        use runtime::Effect;
        // 2 idle tabs, permission granted, render at width 80.
        // header=2 lines (Compact), tab 0 at line 2.
        let mut state = make_state_with_tabs(&[(0, "first", false), (1, "second", false)]);
        state.runtime.permission = crate::permission::PermissionState::Resolved { granted: true };
        let _ = state.runtime.render(100, 80);
        // line 2 is the first tab content row (lines 0-1 are the header)
        let outcome = state.runtime.mouse_click(2);
        assert_eq!(outcome.effects, vec![Effect::SwitchTab { position: 0 }]);
    }

    #[test]
    fn mouse_click_without_permission_is_inert() {
        let mut state = make_state_with_tabs(&[(0, "first", false), (1, "second", false)]);
        state.runtime.permission = crate::permission::PermissionState::default();
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
            state.runtime.permission = crate::permission::PermissionState::Resolved { granted: true };
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

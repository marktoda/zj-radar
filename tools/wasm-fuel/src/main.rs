//! `wasm-fuel`: what one Zellij event costs the rail, in the interpreter's
//! own unit.
//!
//! Zellij 0.44 runs plugins under wasmi, an interpreter, so the metric that
//! predicts server CPU is *executed wasm instructions*, not host-side ns.
//! This harness instantiates `zj_radar.wasm` exactly the way Zellij does
//! (`zellij-server/src/plugins/plugin_loader.rs`: wasmi + WASI preview1,
//! `/cache` preopened, events as a JSON-array-of-bytes text line on stdin,
//! `zellij.host_run_plugin_command` as the one host import) with fuel
//! metering on, then drives it through a steady state — T tabs, P panes per
//! tab, agents running in some — and reports per-scenario fuel and wall
//! time for the events the rail sees in real life.
//!
//! Pass one wasm to measure it, two to compare (a `Δ` column appears).
//!
//! ```sh
//! just bench-wasm                          # current build
//! just bench-wasm old.wasm new.wasm        # before / after
//! ```
//!
//! Fuel is deterministic for a fixed input (the only jitter is the
//! plugin's own clock reads), so a single-digit-percent change is signal.
//! Wall time is native wasmi on the bench machine; treat it as a sanity
//! check on the fuel column, not as a Zellij number.

mod proto;

use std::collections::{BTreeMap, VecDeque};
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::Instant;

use prost::Message;
use wasmi::{Caller, Config, Engine, Instance, Linker, Module, Store, StoreLimits, StoreLimitsBuilder, TypedFunc};
use wasmi_wasi::wasi_common::pipe::{ReadPipe, WritePipe};
use wasmi_wasi::{ambient_authority, Dir, WasiCtx, WasiCtxBuilder};

use proto::api::event::{self as ev, Event};
use proto::api::pipe_message::PipeMessage;
use proto::api::plugin_ids::PluginIds;
use proto::api::{action, input_mode, key, style};
use proto::command;

// ── knobs ────────────────────────────────────────────────────────────────

/// Session shape for the steady state. Chosen to match the sessions the
/// prior perf sweeps were measured on (8 tabs × 3 terminal panes + rail).
struct Shape {
    tabs: u32,
    panes_per_tab: u32,
    /// Rail geometry Zellij calls `render(rows, cols)` with.
    rows: i32,
    cols: i32,
    /// Timed repetitions per scenario; the median is reported.
    iters: usize,
    /// Keybinds in the "legacy" `ModeUpdate` (the stock config carries 141).
    keybinds: usize,
}

impl Shape {
    fn from_args(args: &mut Vec<String>) -> Shape {
        let mut shape = Shape { tabs: 8, panes_per_tab: 3, rows: 40, cols: 30, iters: 15, keybinds: 141 };
        let mut i = 0;
        while i < args.len() {
            let take = |args: &mut Vec<String>, i: usize| -> u32 {
                args.remove(i);
                args.remove(i).parse().expect("numeric flag value")
            };
            match args[i].as_str() {
                "--tabs" => shape.tabs = take(args, i),
                "--panes" => shape.panes_per_tab = take(args, i),
                "--rows" => shape.rows = take(args, i) as i32,
                "--cols" => shape.cols = take(args, i) as i32,
                "--iters" => shape.iters = take(args, i) as usize,
                "--keybinds" => shape.keybinds = take(args, i) as usize,
                _ => i += 1,
            }
        }
        shape
    }
}

// ── host side: the slice of Zellij the plugin can observe ────────────────

/// Both ends of Zellij's plugin stdio: a byte queue the host appends event
/// lines to (the plugin's stdin) and one the plugin prints commands and
/// render output into (its stdout).
#[derive(Clone, Default)]
struct Pipe(Arc<Mutex<VecDeque<u8>>>);

impl Read for Pipe {
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        let mut q = self.0.lock().unwrap();
        let n = buf.len().min(q.len());
        for (dst, src) in buf.iter_mut().zip(q.drain(..n)) {
            *dst = src;
        }
        Ok(n)
    }
}

impl Write for Pipe {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        self.0.lock().unwrap().extend(buf);
        Ok(buf.len())
    }
    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

impl Pipe {
    /// Zellij's `wasi_write_object`: JSON-encode the protobuf bytes as a
    /// text array and terminate the line — the plugin `read_line`s it.
    fn push_object(&self, bytes: &[u8]) {
        let mut line = serde_json::to_string(bytes).unwrap();
        line.push_str("\r\n");
        self.0.lock().unwrap().extend(line.as_bytes());
    }

    fn drain_string(&self) -> String {
        let bytes: Vec<u8> = self.0.lock().unwrap().drain(..).collect();
        String::from_utf8_lossy(&bytes).into_owned()
    }
}

/// Per-instance host state: WASI context, stdio, resource limits, and a
/// tally of the plugin commands seen (`set_timeout`, `run_command`, …).
struct HostEnv {
    wasi: WasiCtx,
    stdin: Pipe,
    stdout: Pipe,
    limits: StoreLimits,
    commands: BTreeMap<i32, u32>,
    plugin_id: i32,
}

/// `zellij.host_run_plugin_command`: the plugin has just printed a
/// `PluginCommand` to stdout. Answer the two blocking queries the rail
/// makes (`GetPluginIds` at load, `GetPaneCwd` in the naming bootstrap)
/// and swallow everything else, as a host with no screen would.
fn host_run_plugin_command(mut caller: Caller<'_, HostEnv>) {
    let env = caller.data_mut();
    let printed = env.stdout.drain_string();
    let Some(line) = printed.lines().rev().find(|l| !l.trim().is_empty()) else { return };
    let bytes: Vec<u8> = serde_json::from_str(line)
        .unwrap_or_else(|e| panic!("plugin command is not a JSON byte array ({e}); stdout held {:?}", &printed[..printed.len().min(120)]));
    let head = command::Head::decode(bytes.as_slice()).expect("plugin command protobuf");
    *env.commands.entry(head.name).or_default() += 1;
    match head.name {
        command::GET_PLUGIN_IDS => {
            let ids = PluginIds { plugin_id: env.plugin_id, zellij_pid: 4242, initial_cwd: "/host".into(), client_id: 1 };
            env.stdin.push_object(&ids.encode_to_vec());
        }
        command::GET_PANE_CWD => {
            let reply = command::GetPaneCwdResponse {
                result: Some(command::get_pane_cwd_response::Result::Cwd("/home/u/src/zj-radar".into())),
            };
            env.stdin.push_object(&reply.encode_to_vec());
        }
        command::GET_ZELLIJ_VERSION => {
            let reply = command::ZellijVersion { version: "0.44.3".into() };
            env.stdin.push_object(&reply.encode_to_vec());
        }
        _ => {}
    }
}

/// One loaded rail instance with fuel metering.
struct Plugin {
    store: Store<HostEnv>,
    load: TypedFunc<(), ()>,
    update: TypedFunc<(), i32>,
    pipe: TypedFunc<(), i32>,
    render: TypedFunc<(i32, i32), ()>,
    stdin: Pipe,
    stdout: Pipe,
    _cache: tempfile::TempDir,
}

/// Fuel budget handed to the store before every measured call. Large
/// enough that no realistic event exhausts it; the spent amount is the
/// difference afterwards.
const FUEL_TANK: u64 = 1 << 40;

struct Cost {
    fuel: u64,
    micros: u128,
    returned_true: bool,
}

impl Plugin {
    fn instantiate(wasm: &Path, engine: &Engine, module: &Module) -> Plugin {
        let cache = tempfile::tempdir().expect("tempdir for /cache");
        let stdin = Pipe::default();
        let stdout = Pipe::default();
        let mut builder = WasiCtxBuilder::new();
        builder.inherit_env().unwrap();
        // Zellij preopens /host, /data, /cache and /tmp; the rail persists
        // only under /cache (design.md §5), so that is the one that matters.
        let dir = Dir::open_ambient_dir(cache.path(), ambient_authority()).expect("open cache dir");
        builder.preopened_dir(dir, "/cache").unwrap();
        builder.stdin(Box::new(ReadPipe::new(stdin.clone())));
        builder.stdout(Box::new(WritePipe::new(stdout.clone())));
        builder.inherit_stderr();
        let wasi = builder.build();

        let env = HostEnv {
            wasi,
            stdin: stdin.clone(),
            stdout: stdout.clone(),
            // Mirrors plugin_loader.rs::create_optimized_store_limits.
            limits: StoreLimitsBuilder::new().instances(1).memories(4).memory_size(16 * 1024 * 1024).tables(16).build(),
            commands: BTreeMap::new(),
            plugin_id: 7,
        };
        let mut store = Store::new(engine, env);
        store.limiter(|env| &mut env.limits);
        store.set_fuel(FUEL_TANK).unwrap();

        let mut linker = Linker::new(engine);
        wasmi_wasi::add_to_linker(&mut linker, |env: &mut HostEnv| &mut env.wasi).unwrap();
        linker.func_wrap("zellij", "host_run_plugin_command", host_run_plugin_command).unwrap();

        let instance: Instance = linker
            .instantiate_and_start(&mut store, module)
            .unwrap_or_else(|e| panic!("instantiate {}: {e}", wasm.display()));
        let export = |store: &Store<HostEnv>, name: &str| {
            instance.get_func(store, name).unwrap_or_else(|| panic!("wasm export `{name}` missing"))
        };
        // Zellij runs the WASI command's `_start` before `load`
        // (plugin_loader.rs::load_plugin_instance). That executes Rust's
        // `main` — `register_plugin!`'s panic-hook install — and the
        // runtime's exit cleanup, which swaps stdout's LineWriter for an
        // unbuffered one. Skip it and every `print!`ed frame holds its last
        // line back until the next `println!`, corrupting the host command
        // that follows; the harness would then measure a plugin Zellij
        // never runs.
        let start = export(&store, "_start").typed::<(), ()>(&store).unwrap();
        start.call(&mut store, ()).expect("_start");
        let load = export(&store, "load").typed::<(), ()>(&store).unwrap();
        let update = export(&store, "update").typed::<(), i32>(&store).unwrap();
        let pipe = export(&store, "pipe").typed::<(), i32>(&store).unwrap();
        let render = export(&store, "render").typed::<(i32, i32), ()>(&store).unwrap();
        Plugin { store, load, update, pipe, render, stdin, stdout, _cache: cache }
    }

    fn measure<R>(&mut self, call: impl FnOnce(&mut Store<HostEnv>) -> R) -> (R, u64, u128) {
        self.store.set_fuel(FUEL_TANK).unwrap();
        let t0 = Instant::now();
        let out = call(&mut self.store);
        let micros = t0.elapsed().as_micros();
        let fuel = FUEL_TANK - self.store.get_fuel().unwrap();
        (out, fuel, micros)
    }

    /// `load()` with an empty configuration map, as a layout without
    /// plugin options produces.
    fn load(&mut self) -> Cost {
        let config = action::PluginConfiguration { name_and_value: vec![] };
        self.stdin.push_object(&config.encode_to_vec());
        let load = self.load;
        let ((), fuel, micros) = self.measure(|s| load.call(s, ()).expect("load"));
        self.stdout.drain_string();
        Cost { fuel, micros, returned_true: false }
    }

    fn update(&mut self, event: &Event) -> Cost {
        self.stdin.push_object(&event.encode_to_vec());
        let update = self.update;
        let (ret, fuel, micros) = self.measure(|s| update.call(s, ()).expect("update"));
        self.stdout.drain_string();
        Cost { fuel, micros, returned_true: ret != 0 }
    }

    fn pipe(&mut self, message: &PipeMessage) -> Cost {
        self.stdin.push_object(&message.encode_to_vec());
        let pipe = self.pipe;
        let (ret, fuel, micros) = self.measure(|s| pipe.call(s, ()).expect("pipe"));
        self.stdout.drain_string();
        Cost { fuel, micros, returned_true: ret != 0 }
    }

    fn render(&mut self, rows: i32, cols: i32) -> Cost {
        let render = self.render;
        let ((), fuel, micros) = self.measure(|s| render.call(s, (rows, cols)).expect("render"));
        self.stdout.drain_string();
        Cost { fuel, micros, returned_true: false }
    }

    /// Zellij's contract: an `update`/`pipe` returning true is followed by
    /// `render`. Returns the two costs so a table can show them apart.
    fn deliver(&mut self, shape: &Shape, fire: impl FnOnce(&mut Plugin) -> Cost) -> (Cost, Option<Cost>) {
        let cost = fire(self);
        let paint = cost.returned_true.then(|| self.render(shape.rows, shape.cols));
        (cost, paint)
    }
}

// ── event fixtures ───────────────────────────────────────────────────────

const RAIL_URL: &str = "file:/home/u/.local/share/zj-radar/zj_radar.wasm";

fn event(name: ev::EventType, payload: ev::event::Payload) -> Event {
    Event { name: name as i32, payload: Some(payload) }
}

fn tab_update(shape: &Shape, active: u32) -> Event {
    let tab_info = (0..shape.tabs)
        .map(|i| ev::TabInfo {
            position: i,
            name: format!("tab-{i}"),
            active: i == active,
            panes_to_hide: 0,
            is_fullscreen_active: false,
            is_sync_panes_active: false,
            are_floating_panes_visible: false,
            other_focused_clients: vec![],
            active_swap_layout_name: Some("compact".into()),
            is_swap_layout_dirty: false,
            viewport_rows: 40,
            viewport_columns: 160,
            display_area_rows: 42,
            display_area_columns: 160,
            selectable_tiled_panes_count: shape.panes_per_tab,
            selectable_floating_panes_count: 0,
            tab_id: i,
            has_bell_notification: false,
            is_flashing_bell: false,
        })
        .collect();
    event(ev::EventType::TabUpdate, ev::event::Payload::TabUpdatePayload(ev::TabUpdatePayload { tab_info }))
}

/// Terminal pane ids are `tab * 100 + n`; each tab also carries its rail
/// (a plugin pane, id `1000 + tab`), which is what a real manifest has.
fn pane_id(tab: u32, n: u32) -> u32 {
    tab * 100 + n
}

fn pane_update(shape: &Shape, focused: (u32, u32)) -> Event {
    let pane = |tab: u32, n: u32| ev::PaneInfo {
        id: pane_id(tab, n),
        is_plugin: false,
        is_focused: (tab, n) == focused,
        is_fullscreen: false,
        is_floating: false,
        is_suppressed: false,
        title: format!("zsh — pane {}", pane_id(tab, n)),
        exited: false,
        exit_status: None,
        is_held: false,
        pane_x: 30 + (n * 40),
        pane_content_x: 31 + (n * 40),
        pane_y: 0,
        pane_content_y: 1,
        pane_rows: 40,
        pane_content_rows: 38,
        pane_columns: 40,
        pane_content_columns: 38,
        cursor_coordinates_in_pane: Some(action::Position { line: 3, column: 12 }),
        terminal_command: Some("zsh".into()),
        plugin_url: None,
        is_selectable: true,
        index_in_pane_group: vec![],
        default_fg: None,
        default_bg: None,
    };
    let rail = |tab: u32| ev::PaneInfo {
        id: 1000 + tab,
        is_plugin: true,
        is_focused: false,
        is_fullscreen: false,
        is_floating: false,
        is_suppressed: false,
        title: "zj-radar".into(),
        exited: false,
        exit_status: None,
        is_held: false,
        pane_x: 0,
        pane_content_x: 0,
        pane_y: 0,
        pane_content_y: 0,
        pane_rows: 40,
        pane_content_rows: 40,
        pane_columns: 30,
        pane_content_columns: 30,
        cursor_coordinates_in_pane: None,
        terminal_command: None,
        plugin_url: Some(RAIL_URL.into()),
        is_selectable: false,
        index_in_pane_group: vec![],
        default_fg: None,
        default_bg: None,
    };
    let pane_manifest = (0..shape.tabs)
        .map(|tab| ev::PaneManifest {
            tab_index: tab,
            panes: std::iter::once(rail(tab)).chain((0..shape.panes_per_tab).map(|n| pane(tab, n))).collect(),
        })
        .collect();
    event(ev::EventType::PaneUpdate, ev::event::Payload::PaneUpdatePayload(ev::PaneUpdatePayload { pane_manifest }))
}

/// `ModeUpdate` as Zellij sends it: `keybinds` carries the whole table for
/// a legacy plugin and is empty once the plugin subscribes to
/// `InitialKeybinds`. The synthetic table mixes payload-bearing actions the
/// way the stock config does (mode switches, focus moves, resizes, panes).
fn mode_update(n_keybinds: usize, session_name: &str) -> Event {
    let mut keybinds: Vec<ev::InputModeKeybinds> = Vec::new();
    for (i, mode) in [input_mode::InputMode::Normal, input_mode::InputMode::Pane, input_mode::InputMode::Tab, input_mode::InputMode::Resize, input_mode::InputMode::Move, input_mode::InputMode::Scroll, input_mode::InputMode::Session]
        .into_iter()
        .enumerate()
    {
        let per_mode = n_keybinds / 7 + usize::from(i < n_keybinds % 7);
        let key_bind = (0..per_mode).map(|k| {
            let payload = match k % 4 {
                0 => action::action::OptionalPayload::SwitchToModePayload(action::SwitchToModePayload { input_mode: (k % 8) as i32 }),
                1 => action::action::OptionalPayload::MoveFocusPayload((k % 4) as i32),
                2 => action::action::OptionalPayload::ResizePayload(proto::api::resize::Resize { resize_action: 0, direction: Some((k % 4) as i32) }),
                _ => action::action::OptionalPayload::NewPanePayload(action::NewPanePayload { direction: Some((k % 4) as i32), pane_name: None }),
            };
            let name = match k % 4 {
                0 => action::ActionName::SwitchToMode,
                1 => action::ActionName::MoveFocus,
                2 => action::ActionName::Resize,
                _ => action::ActionName::NewPane,
            };
            ev::KeyBind {
                key: Some(key::Key {
                    modifier: Some(if k % 2 == 0 { key::key::KeyModifier::Ctrl as i32 } else { key::key::KeyModifier::Alt as i32 }),
                    additional_modifiers: vec![],
                    main_key: Some(key::key::MainKey::Char((k % 26) as i32)),
                }),
                action: vec![action::Action { name: name as i32, optional_payload: Some(payload) }],
            }
        });
        keybinds.push(ev::InputModeKeybinds { mode: mode as i32, key_bind: key_bind.collect() });
    }
    let payload = ev::ModeUpdatePayload {
        current_mode: input_mode::InputMode::Normal as i32,
        keybinds,
        style: Some(zellij_style()),
        arrow_fonts_support: true,
        session_name: Some(session_name.into()),
        base_mode: Some(input_mode::InputMode::Normal as i32),
        editor: None,
        shell: Some("/bin/zsh".into()),
        web_clients_allowed: None,
        web_sharing: None,
        currently_marking_pane_group: None,
        is_web_client: None,
        web_server_ip: None,
        web_server_port: None,
        web_server_capability: None,
    };
    event(ev::EventType::ModeUpdate, ev::event::Payload::ModeUpdatePayload(payload))
}

/// A `Style` that survives Zellij's strict decode: every `Styling`
/// declaration needs exactly six colors and the multiplayer set ten, all
/// eight-bit here (the plugin never reads them; it ships its own theme).
fn zellij_style() -> style::Style {
    let color = |n: u32| style::Color {
        color_type: style::ColorType::EightBit as i32,
        payload: Some(style::color::Payload::EightBitColorPayload(n)),
    };
    let six = || (0..6).map(color).collect::<Vec<_>>();
    style::Style {
        rounded_corners: true,
        styling: Some(style::Styling {
            text_unselected: six(),
            text_selected: six(),
            ribbon_unselected: six(),
            ribbon_selected: six(),
            table_title: six(),
            table_cell_unselected: six(),
            table_cell_selected: six(),
            list_unselected: six(),
            list_selected: six(),
            frame_unselected: six(),
            frame_selected: six(),
            frame_highlight: six(),
            exit_code_success: six(),
            exit_code_error: six(),
            multiplayer_user_colors: (0..10).map(color).collect(),
        }),
        ..Default::default()
    }
}

fn timer(elapsed_s: f32) -> Event {
    event(ev::EventType::Timer, ev::event::Payload::TimerPayload(elapsed_s))
}

fn visible(is_visible: bool) -> Event {
    event(ev::EventType::Visible, ev::event::Payload::VisiblePayload(is_visible))
}

fn permission_granted() -> Event {
    event(
        ev::EventType::PermissionRequestResult,
        ev::event::Payload::PermissionRequestResultPayload(ev::PermissionRequestResultPayload { granted: true }),
    )
}

fn command_changed(pane: u32, argv: &[&str], is_foreground: bool) -> Event {
    let payload = ev::CommandChangedPayload {
        pane_id: Some(ev::PaneId { pane_type: ev::PaneType::Terminal as i32, id: pane }),
        command: argv.iter().map(|s| s.to_string()).collect(),
        is_foreground,
        focused_client_ids: vec![],
    };
    event(ev::EventType::CommandChanged, ev::event::Payload::CommandChangedPayload(payload))
}

/// A `zj_radar.status.v1` broadcast as `zellij pipe --name` delivers it.
fn status_pipe(pane: u32, status: &str, msg: &str) -> PipeMessage {
    let payload = serde_json::json!({
        "v": 1, "source": "claude",
        "pane": { "type": "terminal", "id": pane },
        "status": status, "repo": "zj-radar", "branch": "main",
        "msg": msg, "task": "deep performance pass",
    });
    PipeMessage {
        source: proto::api::pipe_message::PipeSource::Cli as i32,
        cli_source_id: Some("bench".into()),
        plugin_source_id: None,
        name: "zj_radar.status.v1".into(),
        payload: Some(payload.to_string()),
        args: vec![],
        is_private: false,
    }
}

// ── scenarios ────────────────────────────────────────────────────────────

/// A measured row: a name plus the fuel/µs of the `update`/`pipe` call and,
/// when the plugin asked for a repaint, of the `render` that follows.
struct Row {
    name: &'static str,
    call_fuel: u64,
    call_micros: u128,
    render_fuel: Option<u64>,
    render_micros: Option<u128>,
}

fn median<T: Ord + Copy>(mut xs: Vec<T>) -> T {
    xs.sort();
    xs[xs.len() / 2]
}

/// Runs `fire` `iters` times (after one untimed warm-up so lazy function
/// translation is not charged to the first sample) and folds the medians.
fn scenario(name: &'static str, shape: &Shape, plugin: &mut Plugin, mut fire: impl FnMut(&mut Plugin, usize) -> Cost) -> Row {
    let _ = plugin.deliver(shape, |p| fire(p, 0));
    let mut call_fuel = Vec::new();
    let mut call_micros = Vec::new();
    let mut render_fuel = Vec::new();
    let mut render_micros = Vec::new();
    for i in 1..=shape.iters {
        let (call, paint) = plugin.deliver(shape, |p| fire(p, i));
        call_fuel.push(call.fuel);
        call_micros.push(call.micros);
        if let Some(paint) = paint {
            render_fuel.push(paint.fuel);
            render_micros.push(paint.micros);
        }
    }
    let painted = !render_fuel.is_empty();
    Row {
        name,
        call_fuel: median(call_fuel),
        call_micros: median(call_micros),
        render_fuel: painted.then(|| median(render_fuel)),
        render_micros: painted.then(|| median(render_micros)),
    }
}

/// Drive one wasm through load, grant, steady state, and every scenario.
fn bench(wasm: &Path, shape: &Shape) -> (Row, Vec<Row>, BTreeMap<i32, u32>) {
    let mut config = Config::default();
    config.consume_fuel(true);
    let engine = Engine::new(&config);
    let bytes = std::fs::read(wasm).unwrap_or_else(|e| panic!("read {}: {e}", wasm.display()));
    let module = Module::new(&engine, &bytes).expect("valid wasm module");
    let mut plugin = Plugin::instantiate(wasm, &engine, &module);

    // Boot: load → grant → name → topology. `load` is reported on its own
    // because it runs once per tab open (and under Zellij's 100 ms
    // first-paint RenderBlocker).
    let load = plugin.load();
    let load_row = Row { name: "load", call_fuel: load.fuel, call_micros: load.micros, render_fuel: None, render_micros: None };
    plugin.deliver(shape, |p| p.update(&permission_granted()));
    plugin.deliver(shape, |p| p.update(&mode_update(shape.keybinds, "bench")));
    plugin.deliver(shape, |p| p.update(&tab_update(shape, 0)));
    plugin.deliver(shape, |p| p.update(&pane_update(shape, (0, 0))));
    // Agents running in every other pane so the rail has real rows to paint.
    for tab in 0..shape.tabs {
        for n in (0..shape.panes_per_tab).step_by(2) {
            plugin.deliver(shape, |p| p.pipe(&status_pipe(pane_id(tab, n), "running", "reading render.rs")));
        }
    }
    plugin.deliver(shape, |p| p.update(&timer(1.0)));

    let kb = shape.keybinds;
    let rows = vec![
        scenario("mode_update: legacy (full keybinds)", shape, &mut plugin, |p, _| p.update(&mode_update(kb, "bench"))),
        scenario("mode_update: keybinds stripped", shape, &mut plugin, |p, _| p.update(&mode_update(0, "bench"))),
        scenario("tab_update: identical", shape, &mut plugin, |p, _| p.update(&tab_update(shape, 0))),
        scenario("pane_update: identical manifest", shape, &mut plugin, |p, _| p.update(&pane_update(shape, (0, 0)))),
        scenario("pane_update: focus moved", shape, &mut plugin, |p, i| p.update(&pane_update(shape, (0, (i % shape.panes_per_tab as usize) as u32)))),
        scenario("pipe: identical running re-broadcast", shape, &mut plugin, |p, _| p.pipe(&status_pipe(pane_id(0, 0), "running", "reading render.rs"))),
        scenario("pipe: running relabel", shape, &mut plugin, |p, i| p.pipe(&status_pipe(pane_id(0, 0), "running", &format!("editing file {i}.rs")))),
        scenario("pipe: status edge (running↔done)", shape, &mut plugin, |p, i| p.pipe(&status_pipe(pane_id(1, 0), if i % 2 == 0 { "done" } else { "running" }, "turn"))),
        scenario("command_changed: cargo test start/stop", shape, &mut plugin, |p, i| {
            if i % 2 == 0 { p.update(&command_changed(pane_id(2, 1), &["cargo", "test"], true)) } else { p.update(&command_changed(pane_id(2, 1), &["zsh"], true)) }
        }),
        scenario("timer: fast tick, visible", shape, &mut plugin, |p, _| p.update(&timer(1.0))),
        scenario("render: direct", shape, &mut plugin, |p, _| p.render(shape.rows, shape.cols)),
    ];
    let mut rows = rows;
    // Hidden-tab instance: what 7 of 8 rails do all day.
    plugin.deliver(shape, |p| p.update(&visible(false)));
    rows.push(scenario("timer: fast tick, hidden", shape, &mut plugin, |p, _| p.update(&timer(1.0))));
    rows.push(scenario("pipe: running relabel, hidden", shape, &mut plugin, |p, i| p.pipe(&status_pipe(pane_id(0, 0), "running", &format!("hidden edit {i}.rs")))));
    rows.push(scenario("pipe: status edge, hidden", shape, &mut plugin, |p, i| p.pipe(&status_pipe(pane_id(1, 0), if i % 2 == 0 { "done" } else { "running" }, "turn"))));
    rows.push(scenario("visible: reveal (true) after hidden edits", shape, &mut plugin, |p, i| {
        p.deliver(shape, |q| q.pipe(&status_pipe(pane_id(3, 0), "running", &format!("while hidden {i}"))));
        p.deliver(shape, |q| q.update(&visible(false)));
        p.update(&visible(true))
    }));

    let commands = std::mem::take(&mut plugin.store.data_mut().commands);
    (load_row, rows, commands)
}

// ── report ───────────────────────────────────────────────────────────────

fn kfuel(f: u64) -> String {
    format!("{:.1}k", f as f64 / 1000.0)
}

fn delta(before: u64, after: u64) -> String {
    if before == 0 {
        return "—".into();
    }
    let pct = (after as f64 - before as f64) / before as f64 * 100.0;
    format!("{pct:+.0}%")
}

fn main() {
    let mut args: Vec<String> = std::env::args().skip(1).collect();
    let shape = Shape::from_args(&mut args);
    let wasms: Vec<PathBuf> = if args.is_empty() {
        vec![PathBuf::from("target/wasm32-wasip1/release/zj_radar.wasm")]
    } else {
        args.iter().map(PathBuf::from).collect()
    };
    assert!(wasms.len() <= 2, "pass one wasm to measure or two to compare");

    let results: Vec<_> = wasms.iter().map(|w| bench(w, &shape)).collect();

    println!(
        "wasm-fuel · {} tabs × {} panes, rail {}×{}, {} keybinds, median of {}",
        shape.tabs, shape.panes_per_tab, shape.rows, shape.cols, shape.keybinds, shape.iters
    );
    for (w, (_, _, commands)) in wasms.iter().zip(&results) {
        let size = std::fs::metadata(w).map(|m| m.len()).unwrap_or(0);
        let calls: u32 = commands.values().sum();
        println!("  {} ({} bytes, {} host commands during the run)", w.display(), size, calls);
    }
    println!();

    let compare = results.len() == 2;
    let name_w = results[0].1.iter().map(|r| r.name.chars().count()).max().unwrap_or(10).max(4);
    print!("{:<name_w$} │ {:>10} {:>8}", "scenario", "call fuel", "µs");
    if compare {
        print!(" {:>10} {:>8} {:>6}", "after", "µs", "Δ");
    }
    print!(" │ {:>11} {:>8}", "render fuel", "µs");
    if compare {
        print!(" {:>11} {:>8} {:>6}", "after", "µs", "Δ");
    }
    println!();
    println!("{}", "─".repeat(name_w + 26 + if compare { 28 } else { 0 } + 24 + if compare { 29 } else { 0 }));

    let mut all: Vec<Vec<&Row>> = vec![results.iter().map(|(l, _, _)| l).collect()];
    for i in 0..results[0].1.len() {
        all.push(results.iter().map(|(_, rows, _)| &rows[i]).collect());
    }
    for cols in all {
        let a = cols[0];
        print!("{:<name_w$} │ {:>10} {:>8}", a.name, kfuel(a.call_fuel), a.call_micros);
        if compare {
            let b = cols[1];
            print!(" {:>10} {:>8} {:>6}", kfuel(b.call_fuel), b.call_micros, delta(a.call_fuel, b.call_fuel));
        }
        let paint = |r: &Row| match (r.render_fuel, r.render_micros) {
            (Some(f), Some(us)) => (kfuel(f), us.to_string()),
            _ => ("—".into(), "—".into()),
        };
        let (pf, pu) = paint(a);
        print!(" │ {:>11} {:>8}", pf, pu);
        if compare {
            let b = cols[1];
            let (bf, bu) = paint(b);
            let d = match (a.render_fuel, b.render_fuel) {
                (Some(x), Some(y)) => delta(x, y),
                (None, None) => "—".into(),
                _ => "±paint".into(),
            };
            print!(" {:>11} {:>8} {:>6}", bf, bu, d);
        }
        println!();
    }
}

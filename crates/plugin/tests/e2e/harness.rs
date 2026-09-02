#![allow(dead_code)] // shared across the e2e tests; each test uses a subset

/// E2E test harness: drives a real Zellij in a PTY.
///
/// # Key design decisions
///
/// ## Starting a new session from within an existing Zellij session
/// Plain `zellij --session <name>` fails from inside an existing session because
/// it tries to *attach* rather than create. The `--new-session-with-layout` flag
/// (together with `--session`) always creates a new session even when nested.
///
/// ## Fast shell initialization
/// Using the project root as CWD triggers `direnv` / `devenv` which can take
/// 30+ seconds to initialize. We use `/tmp` as CWD in the test layout and set
/// `DIRENV_DISABLE=1` to get a fast, clean shell.
///
/// ## Permissions
/// zj-radar calls `request_permission` on load. Zellij stores grants in:
/// - `~/Library/Caches/org.Zellij-Contributors.Zellij/permissions.kdl` (macOS)
/// - `$XDG_CACHE_HOME/zellij/permissions.kdl` (Linux)
///
/// `pre_grant_permissions` adds the plugin path to that file before the session
/// starts, so the `PermissionRequestResult(Granted)` fires immediately on load.
///
/// Permission grants are written to an ISOLATED temp environment so the real
/// user cache is never polluted. Every Zellij subprocess shares its HOME, XDG
/// cache/config/data roots, and socket dir; none can discover the user's server.
///
/// ## Pane ID discovery
/// The plugin only renders piped status when the `pane_id` in the payload matches
/// a real terminal pane in the session. In a fresh session connected to an
/// existing server, the terminal pane gets pane_id=0. We verify this at runtime
/// by injecting `echo ZPID=$ZELLIJ_PANE_ID` via `write-chars` + `write 13`
/// (Enter), then reading the output from `dump-screen`.
///
/// ## Screen content
/// `dump-screen` only dumps the *focused* pane (the terminal, not the plugin).
/// The full rendered frame — including the plugin sidebar — is in the PTY master
/// buffer collected in `_buf`. We expose `pty_text()` for raw assertions and
/// `screen()` (vt100-parsed) for cell-level assertions.
///
/// ## Broadcasts / ordering
/// Zellij does NOT replay pipe messages to plugins that start later. `start()`
/// polls until the plugin's " RADAR" header appears in the PTY buffer before
/// returning, ensuring the plugin is loaded and subscribed before the caller
/// sends any pipe messages.
use portable_pty::{CommandBuilder, NativePtySystem, PtySize, PtySystem};
use std::io::{Read, Write};
use std::path::Path;
use std::process::Command;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

pub struct ZellijSession {
    pub name: String,
    _child: Box<dyn portable_pty::Child + Send + Sync>,
    pty_writer: Arc<Mutex<Box<dyn Write + Send>>>,
    _reader: std::thread::JoinHandle<()>,
    buf: Arc<Mutex<Vec<u8>>>,
    /// (rows, cols) of the PTY at session start; `screen()` sizes its vt100
    /// parser from this.
    size: Mutex<(u16, u16)>,
    /// Private filesystem + socket environment used by every subprocess in
    /// this test session. The owner removes it after teardown; siblings borrow
    /// the same root so cross-session tests still share one private server.
    isolated: IsolatedEnvironment,
}

/// Who owns the on-disk lifetime of an isolated test environment.
///
/// `start_sibling` needs a second Zellij session to use the SAME isolated root
/// as an existing one (so both resolve to the same socket and Zellij cache dir
/// — and thus the same plugin `/cache` mount — for presence files to actually
/// cross), but exactly ONE of the two `ZellijSession`s must delete that root on
/// Drop. `Owned` is the original `tempfile::TempDir` (deletes on drop);
/// `Shared` is just the path, borrowed from the owner and inert on drop.
enum EnvironmentRoot {
    Owned(tempfile::TempDir),
    Shared(std::path::PathBuf),
}

impl EnvironmentRoot {
    fn path(&self) -> &Path {
        match self {
            EnvironmentRoot::Owned(t) => t.path(),
            EnvironmentRoot::Shared(p) => p.as_path(),
        }
    }
}

/// The complete process boundary for one E2E server. HOME alone is not an
/// isolation boundary: XDG_CACHE_HOME can redirect permission reads to the
/// user's cache, while Zellij's default socket dir is shared by uid. Keeping
/// the paths and command wiring together makes it impossible for a helper to
/// remember HOME but accidentally reach the ambient cache or server.
pub struct IsolatedEnvironment {
    root: EnvironmentRoot,
    /// The socket dir lives under literal `/tmp`, NOT under the temp HOME:
    /// Unix socket paths cap at ~104 bytes (`sun_path`), and Zellij derives the
    /// max session-name length from what's left after the socket dir — on
    /// macOS the default tempdir (`/var/folders/.../T/...`) leaves a budget of
    /// zero and every `--session` name is rejected.
    socket: EnvironmentRoot,
}

impl IsolatedEnvironment {
    fn new() -> Self {
        Self {
            root: EnvironmentRoot::Owned(
                tempfile::TempDir::new().expect("failed to create isolated Zellij environment"),
            ),
            socket: EnvironmentRoot::Owned(
                tempfile::Builder::new()
                    .prefix("zjr")
                    .tempdir_in("/tmp")
                    .expect("failed to create isolated Zellij socket dir"),
            ),
        }
    }

    fn shared(&self) -> Self {
        Self {
            root: EnvironmentRoot::Shared(self.root.path().to_path_buf()),
            socket: EnvironmentRoot::Shared(self.socket.path().to_path_buf()),
        }
    }

    fn home(&self) -> &Path {
        self.root.path()
    }

    fn cache_home(&self) -> std::path::PathBuf {
        self.home().join(".cache")
    }

    fn config_home(&self) -> std::path::PathBuf {
        self.home().join(".config")
    }

    fn data_home(&self) -> std::path::PathBuf {
        self.home().join(".local/share")
    }

    fn socket_dir(&self) -> std::path::PathBuf {
        self.socket.path().to_path_buf()
    }

    fn zellij_cache_dir(&self) -> std::path::PathBuf {
        if cfg!(target_os = "macos") {
            self.home().join("Library/Caches/org.Zellij-Contributors.Zellij")
        } else {
            self.cache_home().join("zellij")
        }
    }

    fn configure_command(&self, command: &mut Command) {
        command
            .env("HOME", self.home())
            .env("XDG_CACHE_HOME", self.cache_home())
            .env("XDG_CONFIG_HOME", self.config_home())
            .env("XDG_DATA_HOME", self.data_home())
            .env("ZELLIJ_SOCKET_DIR", self.socket_dir())
            .env_remove("ZELLIJ_CONFIG_FILE")
            .env_remove("ZELLIJ_CONFIG_DIR");
    }

    fn command(&self, program: &str) -> Command {
        let mut command = Command::new(program);
        self.configure_command(&mut command);
        command
    }

    fn configure_builder(&self, command: &mut CommandBuilder) {
        command.env("HOME", self.home());
        command.env("XDG_CACHE_HOME", self.cache_home());
        command.env("XDG_CONFIG_HOME", self.config_home());
        command.env("XDG_DATA_HOME", self.data_home());
        command.env("ZELLIJ_SOCKET_DIR", self.socket_dir());
        command.env_remove("ZELLIJ_CONFIG_FILE");
        command.env_remove("ZELLIJ_CONFIG_DIR");
    }
}

impl ZellijSession {
    /// Start a headless Zellij session with the given KDL layout.
    ///
    /// Blocks until the zj-radar plugin has rendered its initial frame
    /// (identified by the " RADAR" header text in the PTY buffer).
    ///
    /// `isolated` owns the private HOME, XDG roots, and socket directory used by
    /// every Zellij subprocess. The caller is responsible for writing
    /// permissions.kdl into it before calling this (see
    /// `pre_grant_permissions`).
    pub fn start(
        name: &str,
        layout_kdl: &str,
        plugin_wasm: &Path,
        isolated: IsolatedEnvironment,
    ) -> Self {
        Self::start_with_size(name, layout_kdl, plugin_wasm, isolated, 40, 100)
    }

    /// `start`, but at an explicit outer PTY size. `start` delegates here with
    /// the historical 40x100; exposed separately so a test can start at a
    /// non-default outer terminal size when it needs one.
    pub fn start_with_size(
        name: &str,
        layout_kdl: &str,
        _plugin_wasm: &Path,
        isolated: IsolatedEnvironment,
        rows: u16,
        cols: u16,
    ) -> Self {
        Self::start_internal(name, layout_kdl, isolated, rows, cols)
    }

    /// Second session on the SAME server socket dir + cache as `self`, so the
    /// two servers exchange `SessionUpdate` and share the plugin cache root.
    ///
    /// Reuses `self`'s isolated root (not `isolated_temp_home()`) — same socket,
    /// permissions.kdl (no re-prompt), and Zellij cache dir, which is where
    /// zellij maps each plugin's `/cache` mount. Two sessions on different
    /// roots are intentionally invisible to one another; siblings share the
    /// private server and plugin cache so presence files actually cross.
    ///
    /// The returned session does NOT own the isolated root — `self` does, and
    /// must outlive it (drop `self` last).
    pub fn start_sibling(&self, name: &str, layout_kdl: &str) -> ZellijSession {
        let (rows, cols) = *self.size.lock().unwrap();
        Self::start_internal(name, layout_kdl, self.isolated.shared(), rows, cols)
    }

    /// Shared implementation behind `start_with_size` (owned environment) and
    /// `start_sibling` (borrowed environment).
    fn start_internal(
        name: &str,
        layout_kdl: &str,
        isolated: IsolatedEnvironment,
        rows: u16,
        cols: u16,
    ) -> Self {
        assert_zellij_version();

        // Kill any previous session with this name to avoid conflicts.
        let _ = isolated.command("zellij")
            .args(["delete-session", name, "--force"])
            .output();
        let _ = isolated.command("zellij")
            .args(["kill-session", name])
            .output();
        std::thread::sleep(Duration::from_millis(300));

        // Write the layout to a temp file.
        let dir = std::env::temp_dir().join(format!("zjradar-e2e-{name}"));
        std::fs::create_dir_all(&dir).unwrap();
        let layout_path = dir.join("layout.kdl");
        std::fs::write(&layout_path, layout_kdl).unwrap();

        // Open a PTY pair. Zellij needs a real TTY to start.
        let pty = NativePtySystem::default()
            .openpty(PtySize {
                rows,
                cols,
                pixel_width: 0,
                pixel_height: 0,
            })
            .unwrap();

        // Build the zellij command.
        // --new-session-with-layout creates a new session even from inside another
        // zellij session (unlike plain --session which tries to attach).
        let mut cmd = CommandBuilder::new("zellij");
        cmd.args([
            "--session",
            name,
            "--new-session-with-layout",
            layout_path.to_str().unwrap(),
        ]);
        // Prevent inheriting the outer session's env vars.
        cmd.env_remove("ZELLIJ");
        cmd.env_remove("ZELLIJ_SESSION_NAME");
        cmd.env_remove("ZELLIJ_PANE_ID");
        isolated.configure_builder(&mut cmd);
        // Disable direnv/devenv to avoid slow shell initialization (30+ s).
        cmd.env("DIRENV_DISABLE", "1");

        let child = pty.slave.spawn_command(cmd).unwrap();

        let writer = pty.master.take_writer().unwrap();
        let pty_writer = Arc::new(Mutex::new(writer));

        let mut reader = pty.master.try_clone_reader().unwrap();
        let buf = Arc::new(Mutex::new(Vec::new()));
        let bufc = buf.clone();
        let _reader = std::thread::spawn(move || {
            let mut chunk = [0u8; 4096];
            loop {
                match reader.read(&mut chunk) {
                    Ok(0) | Err(_) => break,
                    Ok(n) => bufc.lock().unwrap().extend_from_slice(&chunk[..n]),
                }
            }
        });

        let s = ZellijSession {
            name: name.into(),
            _child: child,
            pty_writer,
            _reader,
            buf,
            size: Mutex::new((rows, cols)),
            isolated,
        };
        s.wait_until_ready();
        s
    }

    /// Return the private HOME path used by this session's Zellij processes.
    pub fn temp_home(&self) -> &std::path::Path {
        self.isolated.home()
    }

    /// Poll the PTY buffer until the plugin's " RADAR" header appears, indicating
    /// the plugin has loaded, received permissions, and rendered its first frame.
    /// Gives up after 30 seconds.
    fn wait_until_ready(&self) {
        let deadline = Instant::now() + Duration::from_secs(30);
        let mut perm_sent = false;

        while Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(400));
            let text = self.pty_text();

            // Handle permission prompt (fallback if permissions.kdl wasn't read).
            if !perm_sent
                && (text.contains("Grant")
                    || text.contains("Allow")
                    || text.contains("y/n")
                    || text.contains("Deny"))
            {
                if let Ok(mut w) = self.pty_writer.lock() {
                    let _ = w.write_all(b"y");
                    let _ = w.flush();
                }
                perm_sent = true;
                continue;
            }

            // The plugin renders " RADAR" as its header line in all states
            // (both the onboarding screen and the tab-row view).
            if text.contains(" RADAR") {
                // Extra beat so the plugin finishes its initial render cycle.
                std::thread::sleep(Duration::from_millis(500));
                return;
            }
        }
        panic!(
            "zellij session '{}' never showed plugin header; PTY tail:\n{}",
            self.name,
            self
                .pty_text()
                .chars()
                .rev()
                .take(200)
                .collect::<String>()
                .chars()
                .rev()
                .collect::<String>()
        );
    }

    /// Discover the terminal pane's `ZELLIJ_PANE_ID` by injecting a shell
    /// command via `write-chars` and reading `dump-screen` output.
    ///
    /// In a session connected to an existing Zellij server the terminal pane
    /// typically gets pane_id=0, but we verify at runtime to be safe.
    pub fn discover_terminal_pane_id(&self) -> u32 {
        // Inject: echo "ZPID=$ZELLIJ_PANE_ID"
        let _ = self.isolated.command("zellij")
            .args([
                "--session",
                &self.name,
                "action",
                "write-chars",
                r#"echo "ZPID=$ZELLIJ_PANE_ID""#,
            ])
            .output();
        // Send Enter (keycode 13).
        let _ = self.isolated.command("zellij")
            .args(["--session", &self.name, "action", "write", "13"])
            .output();
        std::thread::sleep(Duration::from_millis(800));

        let screen = self.dump_screen();
        if let Some(cap) = regex_capture_u32(r"ZPID=(\d+)", &screen) {
            return cap;
        }

        eprintln!("[e2e] warn: could not parse ZPID from dump-screen; defaulting to 0");
        eprintln!(
            "[e2e] dump-screen was: {:?}",
            &screen[..screen.len().min(300)]
        );
        0
    }

    /// Move focus to the next pane in the tab, discover its pane ID via
    /// `$ZELLIJ_PANE_ID`, then move focus back. Used to find a sibling
    /// terminal pane's ID in a two-terminal layout.
    ///
    /// Returns `None` if the pane ID cannot be parsed (e.g. only one terminal).
    pub fn discover_next_pane_id(&self) -> Option<u32> {
        // Move focus to the next pane.
        let _ = self.action(&["focus-next-pane"]);
        std::thread::sleep(Duration::from_millis(300));

        // Inject `echo ZPID2=$ZELLIJ_PANE_ID` into the newly focused pane.
        let _ = self.isolated.command("zellij")
            .args([
                "--session",
                &self.name,
                "action",
                "write-chars",
                r#"echo "ZPID2=$ZELLIJ_PANE_ID""#,
            ])
            .output();
        let _ = self.isolated.command("zellij")
            .args(["--session", &self.name, "action", "write", "13"])
            .output();
        std::thread::sleep(Duration::from_millis(600));

        let screen = self.dump_screen();
        let id = regex_capture_u32(r"ZPID2=(\d+)", &screen);

        // Move focus back to the original (first) pane.
        let _ = self.action(&["focus-previous-pane"]);
        std::thread::sleep(Duration::from_millis(200));

        id
    }

    /// Inject a `zellij action` sub-command into the session.
    fn action(&self, args: &[&str]) -> std::process::Output {
        self.isolated.command("zellij")
            .args(["--session", &self.name, "action"])
            .args(args)
            .output()
            .expect("zellij action failed to spawn")
    }

    /// Send a `zj_radar.status.v1` pipe message to the plugin.
    pub fn pipe_status(&self, json: &str) {
        let out = self.isolated.command("zellij")
            .args([
                "--session",
                &self.name,
                "pipe",
                "--name",
                "zj_radar.status.v1",
                "--",
                json,
            ])
            .output()
            .expect("zellij pipe failed to spawn");
        if !out.status.success() {
            eprintln!(
                "[e2e] pipe failed (rc={}): {}",
                out.status,
                String::from_utf8_lossy(&out.stderr)
            );
        }
        // Allow the plugin time to re-render.
        std::thread::sleep(Duration::from_millis(500));
    }

    /// Fire the real notify.sh against this session (true hook->pipe->render).
    ///
    /// Gives notify.sh the session's full isolated environment so that the
    /// `zellij pipe` inside it connects to the same private Zellij server.
    /// `ZELLIJ_SESSION_NAME` is set so `zellij pipe` (which has no `--session`
    /// flag) targets the right session even when invoked outside a live PTY.
    ///
    /// `pane_id` is the numeric terminal pane ID (from `discover_terminal_pane_id`);
    /// it is formatted as `terminal_<pane_id>` for `$ZELLIJ_PANE_ID`.
    pub fn run_notify_sh(&self, status: &str, pane_id: u32, hook_json: &str) {
        // notify.sh lives at the workspace root, not under this crate.
        // CARGO_MANIFEST_DIR is crates/plugin, so go up two levels — mirroring
        // plugin_wasm_path. (Before the workspace split the manifest dir WAS the
        // repo root, so the old `plugins/...` path silently broke this test.)
        let script = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../plugins/zj-radar-claude/scripts/notify.sh");
        use std::io::Write as _;
        let mut command = self.isolated.command("bash");
        command
            .arg(&script)
            .arg(status)
            // Gate: notify.sh exits immediately if these are unset.
            .env("ZELLIJ", "1")
            .env("ZELLIJ_PANE_ID", format!("terminal_{pane_id}"))
            // ZELLIJ_SESSION_NAME lets `zellij pipe` (no --session flag) target
            // the correct session when called outside a live Zellij PTY.
            .env("ZELLIJ_SESSION_NAME", &self.name)
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::inherit());
        let mut child = command
            .spawn()
            .expect("spawn notify.sh");
        child
            .stdin
            .as_mut()
            .unwrap()
            .write_all(hook_json.as_bytes())
            .unwrap();
        child.wait().unwrap();
        // notify.sh's send is synchronous, so on the happy path the pipe has
        // been consumed by the time `wait` returns (if the send deadline
        // expired instead, the event was dropped — the caller's assertion
        // will say so). A short pad covers server→plugin delivery; the render
        // wait itself belongs to the caller's `wait_until` poll, not a flat
        // sleep here.
        std::thread::sleep(std::time::Duration::from_millis(200));
    }

    /// Dump the focused terminal pane as plain text (may contain ANSI).
    pub fn dump_screen(&self) -> String {
        let out = self.action(&["dump-screen"]);
        String::from_utf8_lossy(&out.stdout).into_owned()
    }

    /// All PTY master output since session start, stripped to printable ASCII
    /// for easy `contains()` assertions. This includes both the plugin sidebar
    /// and the terminal pane content interleaved with ANSI escape sequences.
    pub fn pty_text(&self) -> String {
        let raw = self.buf.lock().unwrap().clone();
        strip_ansi(&raw)
    }

    /// Full PTY buffer parsed through vt100 for cell-level assertions.
    /// The parser processes every frame rendered since session start, so
    /// `screen()` reflects the final rendered state.
    pub fn screen(&self) -> vt100::Screen {
        let raw = self.buf.lock().unwrap().clone();
        let (rows, cols) = *self.size.lock().unwrap();
        let mut p = vt100::Parser::new(rows, cols, 0);
        p.process(&raw);
        p.screen().clone()
    }

    /// Poll `cond` against this session until it holds or `timeout` elapses,
    /// re-checking every 150ms. Returns whether it became true. Prefer this over
    /// a fixed `sleep` before an assertion: a fast machine returns the instant
    /// the frame is ready, while a loaded CI runner gets the full budget. The
    /// dominant source of E2E flake is a fixed sleep that under-waits, so the
    /// only fixed waits that should remain are those asserting something *stays*
    /// (a non-event, which polling cannot shorten).
    pub fn wait_until(&self, timeout: Duration, mut cond: impl FnMut(&Self) -> bool) -> bool {
        let deadline = Instant::now() + timeout;
        loop {
            if cond(self) {
                return true;
            }
            if Instant::now() >= deadline {
                return false;
            }
            std::thread::sleep(Duration::from_millis(150));
        }
    }

    /// Wait until the parsed sidebar region (left `width` columns) contains
    /// `needle`, up to `timeout`. The vt100 sidebar region — not `pty_text()` —
    /// is the right surface to assert on: it excludes the terminal panes'
    /// scrollback, so a match cannot be a false positive from echoed input or a
    /// piped payload string landing in the buffer.
    pub fn wait_for_sidebar(&self, width: u16, needle: &str, timeout: Duration) -> bool {
        self.wait_until(timeout, |s| {
            sidebar_region(&s.screen(), width).contains(needle)
        })
    }

    /// Inject a left-button mouse click at 1-based screen (`col`, `row`) by
    /// writing an SGR mouse sequence (`\e[<0;col;row;M` press, `m` release) to the
    /// PTY master — the encoding Zellij requests by default. Zellij routes the
    /// click to the pane under the cursor, so a click within the sidebar columns
    /// reaches the rail plugin. This is the only way to exercise the rail's
    /// click→SwitchTab path through a *real* mouse event end-to-end.
    pub fn click_at(&self, col: u16, row: u16) {
        if let Ok(mut w) = self.pty_writer.lock() {
            let _ = w.write_all(format!("\x1b[<0;{col};{row}M").as_bytes());
            let _ = w.write_all(format!("\x1b[<0;{col};{row}m").as_bytes());
            let _ = w.flush();
        }
    }

    /// Run an arbitrary `zellij action` against this session (e.g. `new-tab`,
    /// `go-to-tab`). Exposed for multi-tab tests; thin wrapper over `action`.
    pub fn run_action(&self, args: &[&str]) {
        let _ = self.action(args);
    }

    /// Like [`run_action`], but panic with the action's own stderr when it
    /// exits non-zero. `run_action` swallows the status, so a `zellij action`
    /// that the host rejected (e.g. a rename against an id that raced a tab
    /// close) becomes a silent no-op and later surfaces as a confusing
    /// "name never changed". Use this for the actions a test's outcome hinges
    /// on, so a rejected action names itself instead.
    pub fn run_action_checked(&self, args: &[&str]) {
        let output = self.action(args);
        assert!(
            output.status.success(),
            "`zellij action {}` failed: {}",
            args.join(" "),
            String::from_utf8_lossy(&output.stderr)
        );
    }

    /// Poll [`tab_names`] until `cond` holds, returning a timeline of every
    /// distinct snapshot seen (one entry per change, timestamped from the
    /// start). The bool says whether `cond` was met before `timeout`. Built
    /// for tab-name assertions: on failure the timeline shows whether a name
    /// ever appeared and was then replaced, or never appeared at all — the
    /// distinction between a host-side overwrite and an action that never
    /// landed, which a single final snapshot cannot tell apart.
    pub fn sample_tab_names_until(
        &self,
        timeout: Duration,
        mut cond: impl FnMut(&[String]) -> bool,
    ) -> (bool, Vec<(Duration, Vec<String>)>) {
        let start = Instant::now();
        let deadline = start + timeout;
        let mut timeline: Vec<(Duration, Vec<String>)> = Vec::new();
        loop {
            let names = self.tab_names();
            if timeline.last().map(|(_, n)| n) != Some(&names) {
                timeline.push((start.elapsed(), names.clone()));
            }
            if cond(&names) {
                return (true, timeline);
            }
            if Instant::now() >= deadline {
                return (false, timeline);
            }
            std::thread::sleep(Duration::from_millis(100));
        }
    }

    /// Names held by Zellij's authoritative tab model, in position order.
    /// Unlike sidebar text, this does not read any plugin instance's possibly
    /// stale render; `query-tab-names` asks the session host directly.
    pub fn tab_names(&self) -> Vec<String> {
        let output = self.isolated.command("zellij")
            .args(["--session", &self.name, "action", "query-tab-names"])
            .output()
            .expect("zellij query-tab-names failed to spawn");
        assert!(
            output.status.success(),
            "zellij query-tab-names failed: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        String::from_utf8_lossy(&output.stdout)
            .lines()
            .map(str::trim)
            .filter(|line| !line.is_empty())
            .map(str::to_string)
            .collect()
    }

    /// Session names visible through this test's private Zellij socket.
    /// Exposed so the isolation regression can prove the spawned session is
    /// present privately while absent from the ambient/default server.
    pub fn isolated_session_names(&self) -> Vec<String> {
        let output = self.isolated.command("zellij")
            .args(["list-sessions", "--short", "--no-formatting"])
            .output()
            .expect("isolated zellij list-sessions failed to spawn");
        session_names(output)
    }

    /// Write raw bytes to the PTY master. Used to answer Zellij's native
    /// permission prompt — a client-side modal, NOT a pane's terminal, so it must
    /// go through the PTY (the same path `wait_until_ready` uses to auto-grant),
    /// not `action write-chars` (which targets the focused pane's shell).
    pub fn press(&self, keys: &str) {
        if let Ok(mut w) = self.pty_writer.lock() {
            let _ = w.write_all(keys.as_bytes());
            let _ = w.flush();
        }
    }
}

/// Fail fast (with a clear message) if `zellij` is missing, and warn loudly if it
/// is not the 0.44.x series the harness layout KDL and permission-prompt handling
/// target. A version skew otherwise surfaces as an opaque `wait_until_ready`
/// timeout — "the plugin never rendered" — instead of "your zellij is too new".
fn assert_zellij_version() {
    match Command::new("zellij").arg("--version").output() {
        Ok(out) => {
            let v = String::from_utf8_lossy(&out.stdout);
            if !v.contains("0.44") {
                eprintln!(
                    "[e2e] WARNING: harness targets zellij 0.44.x but found `{}`. \
                     Layout/permission behavior may differ; a timeout below likely means \
                     a version skew, not a plugin regression.",
                    v.trim()
                );
            }
        }
        Err(e) => panic!(
            "[e2e] `zellij` not found on PATH ({e}). Install zellij 0.44.x to run the \
             live E2E suite (see CONTRIBUTING.md)."
        ),
    }
}

impl Drop for ZellijSession {
    fn drop(&mut self) {
        let _ = self.isolated.command("zellij")
            .args(["delete-session", &self.name, "--force"])
            .output();
        let _ = self.isolated.command("zellij")
            .args(["kill-session", &self.name])
            .output();
        // `isolated` drops here, removing its cache/config/data/socket roots.
    }
}

/// Session names visible to an ordinary Zellij CLI with no harness overrides.
/// A private E2E session appearing here is proof that the isolation boundary
/// leaked. No live server is also valid and reports an empty list.
pub fn ambient_session_names() -> Vec<String> {
    let output = Command::new("zellij")
        .args(["list-sessions", "--short", "--no-formatting"])
        .output()
        .expect("ambient zellij list-sessions failed to spawn");
    session_names(output)
}

fn session_names(output: std::process::Output) -> Vec<String> {
    if !output.status.success() {
        return Vec::new();
    }
    String::from_utf8_lossy(&output.stdout)
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty())
        .map(str::to_string)
        .collect()
}

// ── Helpers ────────────────────────────────────────────────────────────────

/// Strip most ANSI/VT escape sequences, returning only printable text.
/// Keeps spaces so that word searches work correctly.
fn strip_ansi(raw: &[u8]) -> String {
    let text = String::from_utf8_lossy(raw);
    let mut out = String::with_capacity(text.len());
    let bytes = text.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        let b = bytes[i];
        if b == 0x1b {
            // Skip escape sequences.
            match bytes.get(i + 1).copied() {
                Some(b'[') => {
                    i += 2;
                    while i < bytes.len() {
                        let fb = bytes[i];
                        i += 1;
                        if (0x40..=0x7e).contains(&fb) {
                            break;
                        }
                    }
                }
                Some(b']') => {
                    i += 2;
                    while i < bytes.len() {
                        if bytes[i] == 0x07 {
                            i += 1;
                            break;
                        }
                        if bytes[i] == 0x1b && bytes.get(i + 1).copied() == Some(b'\\') {
                            i += 2;
                            break;
                        }
                        i += 1;
                    }
                }
                _ => {
                    i += 2.min(bytes.len() - i);
                }
            }
        } else if b == b'\r' || b == b'\n' || b == b'\t' {
            out.push(' ');
            i += 1;
        } else if b >= 0x20 && b != 0x7f {
            // Also pass multi-byte UTF-8.
            match text.get(i..) {
                Some(s) => match s.chars().next() {
                    Some(c) if !c.is_control() => {
                        out.push(c);
                        i += c.len_utf8();
                    }
                    Some(c) => {
                        i += c.len_utf8();
                    }
                    None => {
                        i += 1;
                    }
                },
                None => {
                    i += 1;
                }
            }
        } else {
            i += 1;
        }
    }
    out
}

/// Find the first occurrence of `marker` followed immediately by one or more
/// decimal digits, and return the parsed integer. Scans all occurrences so a
/// literal `marker` that isn't followed by digits (e.g. `ZPID=$...` in the
/// echoed command text) is skipped and the actual numeric output is found.
fn regex_capture_u32(marker: &str, haystack: &str) -> Option<u32> {
    // Strip the regex grouping syntax if the caller passed "ZPID=(\d+)"-style.
    let marker = marker.split('(').next().unwrap_or(marker);
    let mut start = 0;
    while let Some(pos) = haystack[start..].find(marker) {
        let abs = start + pos;
        let after = &haystack[abs + marker.len()..];
        let digits: String = after.chars().take_while(|c| c.is_ascii_digit()).collect();
        if !digits.is_empty() {
            return digits.parse().ok();
        }
        start = abs + marker.len();
    }
    None
}

// ── Public fixtures ─────────────────────────────────────────────────────────

/// Absolute path to the built wasm plugin.
/// The caller must build it first (via `cargo build --release --target wasm32-wasip1`).
pub fn plugin_wasm_path() -> std::path::PathBuf {
    // Diagnosis/release verification can drive the exact installed or downloaded
    // artifact without copying it over the workspace build. Ordinary E2E runs
    // remain pinned to the freshly built workspace WASM.
    if let Some(path) = std::env::var_os("ZJR_E2E_PLUGIN_WASM") {
        return std::path::PathBuf::from(path);
    }
    // In a virtual workspace every member shares the workspace-root target dir,
    // so `cargo build -p zj-radar-plugin` writes here, not under crates/plugin.
    // CARGO_MANIFEST_DIR is the crate dir (crates/plugin); go up two levels.
    let manifest = env!("CARGO_MANIFEST_DIR");
    std::path::Path::new(manifest).join("../../target/wasm32-wasip1/release/zj_radar.wasm")
}

/// Pre-seed "granted" entries in Zellij's `permissions.kdl` for the plugin path.
///
/// Writes into `home_dir`'s Zellij cache (isolated temp dir, not the real user
/// cache). Zellij reads permission grants from `permissions.kdl` in its cache dir:
/// - macOS: `$HOME/Library/Caches/org.Zellij-Contributors.Zellij/permissions.kdl`
/// - Linux: `$XDG_CACHE_HOME/zellij/permissions.kdl`
///
/// The file format is KDL:
/// ```kdl
/// "/path/to/plugin.wasm" {
///     ReadApplicationState
///     ReadCliPipes
///     ChangeApplicationState
///     RunCommands
/// }
/// ```
///
/// The granted set MUST be a superset of every `PermissionType` the plugin
/// requests in `State::handle_effects` (`Effect::RequestPermission`). If even one
/// requested permission is missing here, Zellij treats the grant as incomplete,
/// re-prompts on load, and withholds `render()` until answered — so the plugin
/// pane stays blank and `wait_until_ready` times out with an empty PTY. Keep this
/// list in lockstep with `request_permission` in `src/lib.rs` (`RunCommands` was
/// added by the notify feature and must stay listed here).
///
/// If the path is already present, the file is left unchanged.
/// Returns the complete isolated environment for `ZellijSession::start`.
pub fn pre_grant_permissions(wasm: &Path) -> IsolatedEnvironment {
    let isolated = IsolatedEnvironment::new();
    let cache_dir = isolated.zellij_cache_dir();
    std::fs::create_dir_all(&cache_dir).ok();

    let perm_file = cache_dir.join("permissions.kdl");
    let wasm_abs = wasm.canonicalize().unwrap_or_else(|_| wasm.to_path_buf());
    let wasm_str = wasm_abs.display().to_string();

    // Read existing file content (or start fresh).
    let existing = std::fs::read_to_string(&perm_file).unwrap_or_default();

    // If the path is already granted, do nothing.
    if !existing.contains(&wasm_str) {
        let new_entry = format!(
            r#""{wasm}" {{
    ReadApplicationState
    ReadCliPipes
    ChangeApplicationState
    RunCommands
}}
"#,
            wasm = wasm_str
        );
        let updated = format!("{existing}{new_entry}");
        if let Err(e) = std::fs::write(&perm_file, &updated) {
            eprintln!("[e2e] warn: could not write permissions.kdl: {e}");
        }
    }

    isolated
}

/// An isolated environment with NO permission grant — the ungranted first-run
/// state. Mirrors `pre_grant_permissions` minus the grant, so a session started
/// with it reproduces "attached but never granted" (nothing in `permissions.kdl`).
pub fn isolated_temp_home() -> IsolatedEnvironment {
    IsolatedEnvironment::new()
}

/// A rail layout whose plugin DEFERS permission (`defer_permission "true"`) and
/// carries NO onboarding float — reproducing an attached, ungranted session
/// (attach applies no layout, so the float never auto-opens). The deferring rail
/// renders `needs_permission` without ever calling `request_permission`, so it
/// never triggers the harness's auto-grant; the ungranted state stays put until a
/// test dispatches the grant float itself.
pub fn deferring_rail_layout(plugin_wasm: &Path) -> String {
    let wasm_abs = plugin_wasm
        .canonicalize()
        .unwrap_or_else(|_| plugin_wasm.to_path_buf());
    format!(
        r#"layout {{
    cwd "/tmp"
    pane split_direction="vertical" {{
        pane size=32 borderless=true {{
            plugin location="file:{wasm}" {{
                defer_permission "true"
            }}
        }}
        pane
    }}
}}"#,
        wasm = wasm_abs.display()
    )
}

/// Build a KDL layout that pins the zj-radar plugin in a 32-column left sidebar.
///
/// Uses `/tmp` as CWD so the shell starts without `direnv`/`devenv` overhead.
/// The `DIRENV_DISABLE=1` env var is also set on the Zellij process.
pub fn sidebar_layout(plugin_wasm: &Path) -> String {
    let wasm_abs = plugin_wasm
        .canonicalize()
        .unwrap_or_else(|_| plugin_wasm.to_path_buf());
    format!(
        r#"layout {{
    cwd "/tmp"
    pane split_direction="vertical" {{
        pane size=32 borderless=true {{
            plugin location="file:{wasm}"
        }}
        pane
    }}
}}"#,
        wasm = wasm_abs.display()
    )
}

/// Build a KDL layout with the zj-radar sidebar and TWO stacked terminal panes.
///
/// Both terminal panes share the same tab, so the plugin's `tab_panes` entry
/// for that tab contains two pane IDs. Piping status messages to BOTH IDs
/// exercises the multi-agent aggregation path in the sidebar.
///
/// Pane-id assignment in a fresh session is sequential (the plugin pane gets
/// one id, the two terminals get the next two). The focused terminal is the
/// one that appears to the right/bottom; `discover_terminal_pane_id` returns
/// its id. The sibling terminal is typically `focused_id - 1` or `focused_id + 1`
/// — but see `discover_next_pane_id` for the safer runtime approach.
pub fn sidebar_layout_two_terminal(plugin_wasm: &Path) -> String {
    let wasm_abs = plugin_wasm
        .canonicalize()
        .unwrap_or_else(|_| plugin_wasm.to_path_buf());
    format!(
        r#"layout {{
    cwd "/tmp"
    pane split_direction="vertical" {{
        pane size=32 borderless=true {{
            plugin location="file:{wasm}"
        }}
        pane split_direction="horizontal" {{
            pane
            pane
        }}
    }}
}}"#,
        wasm = wasm_abs.display()
    )
}

/// Build a KDL layout with TWO tabs, each carrying its own zj-radar sidebar +
/// terminal. This gives two live plugin *instances* (one per tab) so a test can
/// observe what a *background* tab's instance renders — the setup needed to probe
/// cross-instance convergence (does a per-pane `CommandChanged`/exit reach the
/// instance in another tab?).
pub fn two_sidebar_tabs_layout(plugin_wasm: &Path) -> String {
    let wasm_abs = plugin_wasm
        .canonicalize()
        .unwrap_or_else(|_| plugin_wasm.to_path_buf());
    let w = wasm_abs.display();
    // The rail lives in `default_tab_template` so Zellij applies it to *every*
    // tab (each gets its own plugin instance); each `tab` block just declares the
    // terminal `pane` that fills the template's `children`.
    format!(
        r#"layout {{
    default_tab_template {{
        pane split_direction="vertical" {{
            pane size=32 borderless=true {{
                plugin location="file:{w}"
            }}
            children
        }}
    }}
    tab name="one" focus=true {{
        pane focus=true cwd="/tmp"
    }}
    tab name="two" {{
        pane focus=true cwd="/tmp"
    }}
}}"#
    )
}

/// User-shaped starting layout: the initial tab uses `default_tab_template`,
/// while tabs created later through `action new-tab` use the concrete
/// `new_tab_template`. Every tab gets an independent sidebar plugin instance.
/// The initial unnamed tab starts at `/tmp`, allowing Managed naming to claim it
/// before a test applies a manual rename.
pub fn runtime_sidebar_tabs_layout(plugin_wasm: &Path) -> String {
    let wasm_abs = plugin_wasm
        .canonicalize()
        .unwrap_or_else(|_| plugin_wasm.to_path_buf());
    let w = wasm_abs.display();
    format!(
        r#"layout {{
    default_tab_template {{
        pane split_direction="vertical" {{
            pane size=32 borderless=true {{
                plugin location="file:{w}"
            }}
            children
        }}
    }}
    new_tab_template {{
        pane split_direction="vertical" {{
            pane size=32 borderless=true {{
                plugin location="file:{w}"
            }}
            pane focus=true
        }}
    }}
    tab focus=true {{
        pane focus=true cwd="/tmp"
    }}
}}"#
    )
}

/// Extract all visible text from a vt100 Screen (rows x cols grid),
/// joining rows with newlines.
pub fn screen_text(screen: &vt100::Screen) -> String {
    let rows = screen.size().0;
    let cols = screen.size().1;
    (0..rows)
        .map(|r| {
            (0..cols)
                .map(|c| screen.cell(r, c).map(|x| x.contents()).unwrap_or_default())
                .collect::<String>()
        })
        .collect::<Vec<_>>()
        .join("\n")
}

/// The 0-based index of the first sidebar row whose left-`width` text contains
/// `needle`. Lets a test locate a rendered row (e.g. the agent's tab line)
/// without hard-coding a row number that shifts as the layout evolves.
pub fn sidebar_row_index(screen: &vt100::Screen, width: u16, needle: &str) -> Option<usize> {
    sidebar_region(screen, width)
        .lines()
        .position(|l| l.contains(needle))
}

/// The first truecolor (`Rgb`) background color found scanning the left `width`
/// columns of sidebar row `row`, or `None` if every cell uses the terminal
/// default / a 256-index color.
///
/// This reads the color of the *actually rendered* frame — after the plugin's
/// ANSI has round-tripped through Zellij's own compositor and the PTY — so it is
/// the e2e analogue of the unit-test `surface_of` tint oracle. The plugin emits
/// its card surfaces as `\e[48;2;r;g;bm` truecolor, which vt100 reports as
/// `Color::Rgb`; the dark-panel rail base and each card surface are distinct
/// RGBs, so two differently-classed rows return different values here.
pub fn sidebar_row_bg_rgb(screen: &vt100::Screen, row: u16, width: u16) -> Option<(u8, u8, u8)> {
    (0..width).find_map(|c| match screen.cell(row, c)?.bgcolor() {
        vt100::Color::Rgb(r, g, b) => Some((r, g, b)),
        _ => None,
    })
}

/// Extract only the left `width` columns of the vt100 Screen — the sidebar region.
///
/// Returns one string per row joined by newlines. Because `screen()` processes
/// all PTY frames through the vt100 parser, the result contains no ANSI escape
/// sequences: every cell's `contents()` is plain Unicode text (or empty string).
///
/// Use this instead of `pty_text()` when asserting on sidebar content to avoid
/// false positives from text echoed by terminal panes into the right-hand region.
pub fn sidebar_region(screen: &vt100::Screen, width: u16) -> String {
    let rows = screen.size().0;
    (0..rows)
        .map(|r| {
            (0..width)
                .map(|c| screen.cell(r, c).map(|x| x.contents()).unwrap_or_default())
                .collect::<String>()
        })
        .collect::<Vec<_>>()
        .join("\n")
}

/// `sidebar_region`'s text with everything from the bottom region's `─
/// earlier` ledger rule onward stripped off — i.e. just the live card rows.
///
/// A completion that recedes off a card legitimately resurfaces below that
/// rule as a ledger row (spec §9), so a raw
/// `sidebar_region(..).contains(needle)` check does not mean "this row is
/// still live" — it would also match the row's own ledger echo. Tests
/// asserting a card recede should check `card_region_only` instead; tests
/// that want to see the ledger echo can inspect the full `sidebar_region`.
pub fn card_region_only(sidebar: &str) -> &str {
    match sidebar.find("─ earlier") {
        Some(idx) => &sidebar[..idx],
        None => sidebar,
    }
}

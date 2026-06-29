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
/// Permission grants are written to an ISOLATED temp HOME so the real user cache
/// is never polluted. All Zellij subprocesses share the same temp HOME.
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
#[cfg(feature = "e2e")]
use portable_pty::{CommandBuilder, NativePtySystem, PtySize, PtySystem};
#[cfg(feature = "e2e")]
use std::io::{Read, Write};
#[cfg(feature = "e2e")]
use std::path::Path;
#[cfg(feature = "e2e")]
use std::process::Command;
#[cfg(feature = "e2e")]
use std::sync::{Arc, Mutex};
#[cfg(feature = "e2e")]
use std::time::{Duration, Instant};

#[cfg(feature = "e2e")]
pub struct ZellijSession {
    pub name: String,
    _child: Box<dyn portable_pty::Child + Send + Sync>,
    pty_writer: Arc<Mutex<Box<dyn Write + Send>>>,
    _reader: std::thread::JoinHandle<()>,
    buf: Arc<Mutex<Vec<u8>>>,
    /// Isolated temp HOME for this session. Kept alive so Zellij's cache dir
    /// survives for the session duration and is auto-cleaned on Drop.
    _temp_home: tempfile::TempDir,
}

#[cfg(feature = "e2e")]
impl ZellijSession {
    /// Start a headless Zellij session with the given KDL layout.
    ///
    /// Blocks until the zj-radar plugin has rendered its initial frame
    /// (identified by the " RADAR" header text in the PTY buffer).
    ///
    /// `temp_home` is an isolated temp directory used as HOME for all Zellij
    /// subprocesses. This ensures permission grants stay out of the real user
    /// cache. The caller is responsible for writing permissions.kdl under
    /// `temp_home` before calling this (see `pre_grant_permissions`).
    pub fn start(
        name: &str,
        layout_kdl: &str,
        _plugin_wasm: &Path,
        temp_home: tempfile::TempDir,
    ) -> Self {
        let temp_home_path = temp_home.path().to_path_buf();

        // Kill any previous session with this name to avoid conflicts.
        let _ = Command::new("zellij")
            .args(["delete-session", name, "--force"])
            .env("HOME", &temp_home_path)
            .output();
        let _ = Command::new("zellij")
            .args(["kill-session", name])
            .env("HOME", &temp_home_path)
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
                rows: 40,
                cols: 100,
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
        // Disable direnv/devenv to avoid slow shell initialization (30+ s).
        cmd.env("DIRENV_DISABLE", "1");
        // Isolate Zellij's cache/config to the temp HOME so we never touch
        // the real user's permissions.kdl or other Zellij state.
        cmd.env("HOME", &temp_home_path);

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
            _temp_home: temp_home,
        };
        s.wait_until_ready();
        s
    }

    /// Return the temp HOME path used by this session's Zellij processes.
    #[allow(dead_code)]
    pub fn temp_home(&self) -> &std::path::Path {
        self._temp_home.path()
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
            &self
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
        let _ = Command::new("zellij")
            .args([
                "--session",
                &self.name,
                "action",
                "write-chars",
                r#"echo "ZPID=$ZELLIJ_PANE_ID""#,
            ])
            .env("HOME", self._temp_home.path())
            .output();
        // Send Enter (keycode 13).
        let _ = Command::new("zellij")
            .args(["--session", &self.name, "action", "write", "13"])
            .env("HOME", self._temp_home.path())
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
        let _ = Command::new("zellij")
            .args([
                "--session",
                &self.name,
                "action",
                "write-chars",
                r#"echo "ZPID2=$ZELLIJ_PANE_ID""#,
            ])
            .env("HOME", self._temp_home.path())
            .output();
        let _ = Command::new("zellij")
            .args(["--session", &self.name, "action", "write", "13"])
            .env("HOME", self._temp_home.path())
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
        Command::new("zellij")
            .args(["--session", &self.name, "action"])
            .args(args)
            .env("HOME", self._temp_home.path())
            .output()
            .expect("zellij action failed to spawn")
    }

    /// Send a `zj_radar.status.v1` pipe message to the plugin.
    pub fn pipe_status(&self, json: &str) {
        let out = Command::new("zellij")
            .args([
                "--session",
                &self.name,
                "pipe",
                "--name",
                "zj_radar.status.v1",
                "--",
                json,
            ])
            .env("HOME", self._temp_home.path())
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
    /// Sets HOME to the session's isolated temp home so that the `zellij pipe`
    /// inside notify.sh connects to the same isolated Zellij server.
    /// `ZELLIJ_SESSION_NAME` is set so `zellij pipe` (which has no `--session`
    /// flag) targets the right session even when invoked outside a live PTY.
    ///
    /// `pane_id` is the numeric terminal pane ID (from `discover_terminal_pane_id`);
    /// it is formatted as `terminal_<pane_id>` for `$ZELLIJ_PANE_ID`.
    pub fn run_notify_sh(&self, status: &str, pane_id: u32, hook_json: &str) {
        let script = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("plugins/zj-radar-claude/scripts/notify.sh");
        use std::io::Write as _;
        let mut child = std::process::Command::new("bash")
            .arg(&script)
            .arg(status)
            // Gate: notify.sh exits immediately if these are unset.
            .env("ZELLIJ", "1")
            .env("ZELLIJ_PANE_ID", format!("terminal_{pane_id}"))
            // ZELLIJ_SESSION_NAME lets `zellij pipe` (no --session flag) target
            // the correct session when called outside a live Zellij PTY.
            .env("ZELLIJ_SESSION_NAME", &self.name)
            // Critical: same HOME → same isolated Zellij server socket.
            .env("HOME", self._temp_home.path())
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::inherit())
            .spawn()
            .expect("spawn notify.sh");
        child
            .stdin
            .as_mut()
            .unwrap()
            .write_all(hook_json.as_bytes())
            .unwrap();
        child.wait().unwrap();
        // notify.sh backgrounds `zellij pipe` with `&`. Give it time to reach
        // the isolated server and for the plugin to re-render.
        std::thread::sleep(std::time::Duration::from_millis(1500));
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
    #[allow(dead_code)]
    pub fn screen(&self) -> vt100::Screen {
        let raw = self.buf.lock().unwrap().clone();
        let mut p = vt100::Parser::new(40, 100, 0);
        p.process(&raw);
        p.screen().clone()
    }
}

#[cfg(feature = "e2e")]
impl Drop for ZellijSession {
    fn drop(&mut self) {
        let _ = Command::new("zellij")
            .args(["delete-session", &self.name, "--force"])
            .env("HOME", self._temp_home.path())
            .output();
        let _ = Command::new("zellij")
            .args(["kill-session", &self.name])
            .env("HOME", self._temp_home.path())
            .output();
        // _temp_home is dropped here, auto-cleaning the isolated cache dir.
    }
}

// ── Helpers ────────────────────────────────────────────────────────────────

/// Strip most ANSI/VT escape sequences, returning only printable text.
/// Keeps spaces so that word searches work correctly.
#[cfg(feature = "e2e")]
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
#[cfg(feature = "e2e")]
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
/// The caller must build it first (via `nix develop -c cargo build --release --target wasm32-wasip1`).
#[cfg(feature = "e2e")]
pub fn plugin_wasm_path() -> std::path::PathBuf {
    let root = env!("CARGO_MANIFEST_DIR");
    std::path::Path::new(root).join("target/wasm32-wasip1/release/zj_radar.wasm")
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
/// }
/// ```
///
/// If the path is already present, the file is left unchanged.
/// Returns the temp HOME dir so the caller can pass it to `ZellijSession::start`.
#[cfg(feature = "e2e")]
pub fn pre_grant_permissions(wasm: &Path) -> tempfile::TempDir {
    let temp_home = tempfile::TempDir::new().expect("failed to create temp HOME dir");
    let cache_dir = zellij_cache_dir_for(temp_home.path());
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
}}
"#,
            wasm = wasm_str
        );
        let updated = format!("{existing}{new_entry}");
        if let Err(e) = std::fs::write(&perm_file, &updated) {
            eprintln!("[e2e] warn: could not write permissions.kdl: {e}");
        }
    }

    temp_home
}

/// Return the Zellij cache dir inside `home` (same logic Zellij uses for HOME).
#[cfg(feature = "e2e")]
fn zellij_cache_dir_for(home: &Path) -> std::path::PathBuf {
    if cfg!(target_os = "macos") {
        home.join("Library/Caches/org.Zellij-Contributors.Zellij")
    } else {
        home.join(".cache/zellij")
    }
}

/// Build a KDL layout that pins the zj-radar plugin in a 24-column left sidebar.
///
/// Uses `/tmp` as CWD so the shell starts without `direnv`/`devenv` overhead.
/// The `DIRENV_DISABLE=1` env var is also set on the Zellij process.
#[cfg(feature = "e2e")]
pub fn sidebar_layout(plugin_wasm: &Path) -> String {
    let wasm_abs = plugin_wasm
        .canonicalize()
        .unwrap_or_else(|_| plugin_wasm.to_path_buf());
    format!(
        r#"layout {{
    cwd "/tmp"
    pane split_direction="vertical" {{
        pane size=24 borderless=true {{
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
#[cfg(feature = "e2e")]
pub fn sidebar_layout_two_terminal(plugin_wasm: &Path) -> String {
    let wasm_abs = plugin_wasm
        .canonicalize()
        .unwrap_or_else(|_| plugin_wasm.to_path_buf());
    format!(
        r#"layout {{
    cwd "/tmp"
    pane split_direction="vertical" {{
        pane size=24 borderless=true {{
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

/// Extract all visible text from a vt100 Screen (rows x cols grid),
/// joining rows with newlines.
#[cfg(feature = "e2e")]
#[allow(dead_code)]
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

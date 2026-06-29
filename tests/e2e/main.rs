// E2E tests: drive a real Zellij in a PTY, load the zj-radar plugin, and
// assert that piped status messages render in the sidebar.
//
// These are gated behind `--features e2e` and marked `#[ignore]` so that
// `cargo test` stays fast and hermetic. Run with:
//
//   cargo test --features e2e --test e2e -- --include-ignored
//
// or via:
//
//   just test-e2e
//
// Prerequisites:
//   - `zellij` 0.44.x on PATH
//   - The plugin wasm already built:
//     `cargo build --release --target wasm32-wasip1`

#[cfg(feature = "e2e")]
mod harness;

#[cfg(feature = "e2e")]
use harness::*;

/// Smoke test: start Zellij with the plugin sidebar, pipe a status message,
/// and assert that the repo/message text appears in the rendered screen.
///
/// # How it works
/// 1. `pre_grant_permissions` seeds Zellij's `permissions.kdl` in an isolated
///    temp HOME (never touches the real user cache) and returns the TempDir.
/// 2. `ZellijSession::start` spawns Zellij with `--new-session-with-layout`
///    (works from inside an outer session) and a layout using `/tmp` as CWD
///    (avoids slow direnv/devenv init). It polls the PTY buffer until the
///    plugin's " RADAR" header appears. All Zellij subprocesses use the same
///    temp HOME so they share the isolated server/cache.
/// 3. `discover_terminal_pane_id` injects `echo ZPID=$ZELLIJ_PANE_ID` into
///    the terminal pane and reads the result from `dump-screen`. The pane ID
///    must match the `"pane": {"id": ...}` field in the pipe payload for the
///    plugin to display the status row.
/// 4. `pipe_status` sends the JSON payload via `zellij pipe`.
/// 5. We assert that "web" and/or "building" appear in the PTY buffer SUFFIX
///    captured AFTER the pipe call (ordering-sensitive: proves the plugin
///    rendered the status in response to the pipe, not from earlier output).
#[test]
#[ignore = "e2e: requires zellij + built wasm; run via `just test-e2e`"]
fn plugin_loads_and_renders_status() {
    let wasm = plugin_wasm_path();
    assert!(
        wasm.exists(),
        "Plugin wasm not found at {:?}. Build it first:\n  cargo build --release --target wasm32-wasip1",
        wasm
    );

    // Pre-grant plugin permissions so the modal prompt never fires.
    // Returns an isolated temp HOME — never touches the real user's cache.
    let temp_home = pre_grant_permissions(&wasm);

    // Use a process-unique session name to avoid collisions on concurrent runs.
    let session_name = format!("zjr_smoke_{}", std::process::id());
    let layout = sidebar_layout(&wasm);

    eprintln!(
        "[e2e] starting session '{}' with layout:\n{}",
        session_name, layout
    );
    let session = ZellijSession::start(&session_name, &layout, &wasm, temp_home);
    eprintln!("[e2e] plugin loaded (saw ' RADAR' in PTY)");

    // Discover the terminal pane's actual ID so the pipe payload matches.
    let pane_id = session.discover_terminal_pane_id();
    eprintln!("[e2e] terminal pane_id={}", pane_id);

    // Capture the PTY buffer length BEFORE piping so the post-pipe assertion
    // is ordering-sensitive (proves the plugin rendered AFTER receiving the pipe).
    let pre_len = session.pty_text().len();

    // Build the pipe payload with the real pane ID.
    let payload = format!(
        r#"{{"v":1,"source":"claude","pane":{{"type":"terminal","id":{pane_id}}},"status":"running","repo":"web","branch":"main","msg":"building"}}"#
    );
    eprintln!("[e2e] piping: {}", payload);
    session.pipe_status(&payload);

    // Give the plugin time to re-render after the pipe.
    std::thread::sleep(std::time::Duration::from_millis(800));

    let full = session.pty_text();
    // Use only the suffix after the pre-pipe snapshot, proving the plugin
    // rendered AFTER the pipe (not from earlier startup output).
    // `pty_text()` returns a String of printable ASCII/UTF-8, so pre_len is
    // always on a char boundary; the fallback to "" handles any edge case.
    let suffix = full.get(pre_len..).unwrap_or("");
    eprintln!(
        "[e2e] PTY suffix (last 500 chars): {:?}",
        &suffix[suffix.len().saturating_sub(500)..]
    );

    assert!(
        suffix.contains("web") || suffix.contains("building"),
        "sidebar should show the piped status (repo='web', msg='building') AFTER the pipe;\nPTY suffix (last 1000 chars):\n{}",
        &suffix[suffix.len().saturating_sub(1000)..]
    );

    eprintln!("[e2e] PASS: found piped status in post-pipe rendered output");
}

/// Multi-agent scenario: pipe a running agent and a pending ("needs-you") agent
/// to two different pane IDs. Assert that the pending/needs-you content ("api"
/// or "approve") appears in the rendered sidebar.
///
/// Uses a unique session name to avoid collisions with the smoke test when
/// both run in the same `cargo test` invocation.
///
/// Ordering-sensitive: we snapshot the PTY buffer length BEFORE the second pipe
/// and assert the matching text appears in the suffix captured AFTER it.
#[test]
#[ignore = "e2e: requires zellij + built wasm; run via `just test-e2e`"]
fn multi_agent_needs_you_is_visible() {
    let wasm = plugin_wasm_path();
    assert!(
        wasm.exists(),
        "Plugin wasm not found at {:?}. Build it first:\n  cargo build --release --target wasm32-wasip1",
        wasm
    );

    let temp_home = pre_grant_permissions(&wasm);
    let session_name = format!("zjr_multi_{}", std::process::id());
    // Use a two-terminal layout so the plugin's tab_panes has TWO entries for
    // the tab, allowing both agents' states to aggregate and render.
    let layout = sidebar_layout_two_terminal(&wasm);

    eprintln!(
        "[e2e] starting multi-agent session '{}' with two-terminal layout",
        session_name
    );
    let session = ZellijSession::start(&session_name, &layout, &wasm, temp_home);
    eprintln!("[e2e] plugin loaded");

    // Discover the focused terminal pane ID.
    let pane_id = session.discover_terminal_pane_id();
    eprintln!("[e2e] focused terminal pane_id={}", pane_id);

    // Discover the sibling terminal pane ID (focus-next, read, focus-back).
    let sibling_id = session.discover_next_pane_id().unwrap_or_else(|| {
        eprintln!("[e2e] warn: could not discover sibling pane; guessing pane_id+1");
        pane_id + 1
    });
    eprintln!("[e2e] sibling terminal pane_id={}", sibling_id);

    // Pipe a running agent using the focused pane id.
    let running_payload = format!(
        r#"{{"v":1,"source":"claude","pane":{{"type":"terminal","id":{pane_id}}},"status":"running","repo":"web","msg":"building"}}"#
    );
    eprintln!("[e2e] piping running agent: {}", running_payload);
    session.pipe_status(&running_payload);

    // Snapshot PTY length before the pending pipe (ordering-sensitive assertion).
    let pre_len = session.pty_text().len();

    // Pipe a pending ("needs-you") agent to the sibling pane.
    let pending_payload = format!(
        r#"{{"v":1,"source":"claude","pane":{{"type":"terminal","id":{sibling_id}}},"status":"pending","repo":"api","msg":"approve?"}}"#
    );
    eprintln!("[e2e] piping pending agent: {}", pending_payload);
    session.pipe_status(&pending_payload);

    std::thread::sleep(std::time::Duration::from_millis(800));

    let full = session.pty_text();
    let suffix = full.get(pre_len..).unwrap_or("");
    eprintln!(
        "[e2e] PTY suffix (last 500 chars): {:?}",
        &suffix[suffix.len().saturating_sub(500)..]
    );

    assert!(
        suffix.contains("api") || suffix.contains("approve"),
        "pending (needs-you) agent must surface in the sidebar after the pipe;\nPTY suffix (last 1000 chars):\n{}",
        &suffix[suffix.len().saturating_sub(1000)..]
    );

    eprintln!("[e2e] PASS: needs-you agent appeared in post-pipe rendered output");
}

/// End-to-end notify.sh scenario: fire the real notify.sh hook script against
/// the isolated session and assert that its piped status drives the sidebar.
///
/// # How notify.sh reaches the isolated session
/// `run_notify_sh` sets `HOME=<session temp home>` on the bash subprocess.
/// Because all Zellij subprocesses for this session share that temp HOME,
/// the `zellij pipe` call inside notify.sh connects to the same isolated
/// Zellij server and delivers the pipe message to the plugin.
///
/// `ZELLIJ=1` and `ZELLIJ_PANE_ID=terminal_<pane_id>` are also set so
/// notify.sh passes its guard checks and uses the correct pane number.
#[test]
#[ignore = "e2e: requires zellij + built wasm + jq; run via `just test-e2e`"]
fn notify_sh_end_to_end_updates_sidebar() {
    let wasm = plugin_wasm_path();
    assert!(
        wasm.exists(),
        "Plugin wasm not found at {:?}. Build it first:\n  cargo build --release --target wasm32-wasip1",
        wasm
    );

    let temp_home = pre_grant_permissions(&wasm);
    let session_name = format!("zjr_hook_{}", std::process::id());
    let layout = sidebar_layout(&wasm);

    eprintln!("[e2e] starting notify.sh session '{}'", session_name);
    let session = ZellijSession::start(&session_name, &layout, &wasm, temp_home);
    eprintln!("[e2e] plugin loaded");

    // Discover the real terminal pane ID so notify.sh uses a pane that the
    // plugin knows about.
    let pane_id = session.discover_terminal_pane_id();
    eprintln!("[e2e] terminal pane_id={}", pane_id);

    // Snapshot PTY length before the hook fires (ordering-sensitive assertion).
    let pre_len = session.pty_text().len();

    // Fire the real notify.sh with a PostToolUse Edit hook payload.
    // notify.sh will derive msg="editing auth.rs" and pipe it to the plugin.
    let hook_json = r#"{"hook_event_name":"PostToolUse","cwd":".","tool_name":"Edit","tool_input":{"file_path":"src/auth.rs"}}"#;
    eprintln!(
        "[e2e] firing notify.sh running with hook JSON: {}",
        hook_json
    );
    session.run_notify_sh("running", pane_id, hook_json);

    let full = session.pty_text();
    let suffix = full.get(pre_len..).unwrap_or("");
    eprintln!(
        "[e2e] PTY suffix (last 500 chars): {:?}",
        &suffix[suffix.len().saturating_sub(500)..]
    );

    assert!(
        suffix.contains("editing") || suffix.contains("auth.rs"),
        "notify.sh hook should drive the sidebar (expected 'editing' or 'auth.rs');\nPTY suffix (last 1000 chars):\n{}",
        &suffix[suffix.len().saturating_sub(1000)..]
    );

    eprintln!("[e2e] PASS: notify.sh hook drove the sidebar render");
}

/// Line-per-pane structure test: two TRACKED panes in one tab must each render
/// as a distinct line in the sidebar (the redesign's doc scenario H).
///
/// Pipes distinct status payloads to both terminal panes and asserts — using
/// only the SIDEBAR REGION (left 32 columns of the vt100-parsed screen) — that
/// each pane's identifier appears on its own line. This avoids the false-positive
/// trap of checking `pty_text()`, which also contains the terminal panes'
/// scrollback echoing the piped/typed text.
#[test]
#[ignore = "e2e: requires zellij + built wasm; run via `just test-e2e`"]
fn multi_pane_renders_one_line_per_pane() {
    let wasm = plugin_wasm_path();
    assert!(
        wasm.exists(),
        "Plugin wasm not found at {:?}. Build it first:\n  cargo build --release --target wasm32-wasip1",
        wasm
    );

    let temp_home = pre_grant_permissions(&wasm);
    let session_name = format!("zjr_lineper_{}", std::process::id());
    let layout = sidebar_layout_two_terminal(&wasm);

    eprintln!(
        "[e2e] starting line-per-pane session '{}' with two-terminal layout",
        session_name
    );
    let session = ZellijSession::start(&session_name, &layout, &wasm, temp_home);
    eprintln!("[e2e] plugin loaded");

    // Discover the focused terminal pane (pane A).
    let pane_a = session.discover_terminal_pane_id();
    eprintln!("[e2e] pane_a={}", pane_a);

    // Discover the sibling terminal pane (pane B).
    let pane_b = session.discover_next_pane_id().unwrap_or_else(|| {
        eprintln!("[e2e] warn: could not discover sibling pane; guessing pane_a+1");
        pane_a + 1
    });
    eprintln!("[e2e] pane_b={}", pane_b);

    // Pipe a RUNNING status to pane A with a short, distinct repo+msg pair.
    // "alpha" / "taska" are kept to <=8 chars so they fit comfortably in 32 cols.
    let payload_a = format!(
        r#"{{"v":1,"source":"claude","pane":{{"type":"terminal","id":{pane_a}}},"status":"running","repo":"alpha","msg":"taska"}}"#
    );
    eprintln!("[e2e] piping pane A: {}", payload_a);
    session.pipe_status(&payload_a);

    // Pipe a RUNNING status to pane B with a different repo+msg.
    let payload_b = format!(
        r#"{{"v":1,"source":"claude","pane":{{"type":"terminal","id":{pane_b}}},"status":"running","repo":"beta","msg":"taskb"}}"#
    );
    eprintln!("[e2e] piping pane B: {}", payload_b);
    session.pipe_status(&payload_b);

    // Extra settle time so both renders complete before we snapshot.
    std::thread::sleep(std::time::Duration::from_millis(600));

    // Parse the final PTY frame through vt100 and extract the left 32 columns.
    let screen = session.screen();
    let sidebar = sidebar_region(&screen, 32);
    eprintln!("[e2e] sidebar region (32 cols):\n{}", sidebar);

    // Each row of the sidebar is a separate line in `sidebar`.
    // We look for: two different lines — one containing "alpha" or "taska",
    // another containing "beta" or "taskb" — proving the line-per-pane layout.
    let lines: Vec<&str> = sidebar.lines().collect();

    let line_a = lines
        .iter()
        .position(|l| l.contains("alpha") || l.contains("taska"));
    let line_b = lines
        .iter()
        .position(|l| l.contains("beta") || l.contains("taskb"));

    eprintln!(
        "[e2e] pane-A identifier on sidebar row: {:?}",
        line_a
    );
    eprintln!(
        "[e2e] pane-B identifier on sidebar row: {:?}",
        line_b
    );

    assert!(
        line_a.is_some(),
        "pane A (repo='alpha', msg='taska') must appear in the sidebar region;\nsidebar:\n{}",
        sidebar
    );
    assert!(
        line_b.is_some(),
        "pane B (repo='beta', msg='taskb') must appear in the sidebar region;\nsidebar:\n{}",
        sidebar
    );
    assert_ne!(
        line_a, line_b,
        "pane A and pane B must appear on DIFFERENT sidebar lines (line-per-pane layout);\n\
         line_a={:?}, line_b={:?}\nsidebar:\n{}",
        line_a, line_b, sidebar
    );

    eprintln!(
        "[e2e] PASS: two panes rendered on distinct sidebar lines (row {:?} vs row {:?})",
        line_a, line_b
    );
}

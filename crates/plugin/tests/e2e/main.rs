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

    // Build the pipe payload with the real pane ID.
    let payload = format!(
        r#"{{"v":1,"source":"claude","pane":{{"type":"terminal","id":{pane_id}}},"status":"running","repo":"web","branch":"main","msg":"building"}}"#
    );
    eprintln!("[e2e] piping: {}", payload);
    session.pipe_status(&payload);

    // Poll the vt100-parsed sidebar region (left 32 cols) for the agent's
    // activity, up to 5s. Asserting on the sidebar region — not pty_text() —
    // excludes the terminal scrollback that echoes the piped JSON, so a match is
    // a real render, not a false positive; and polling returns the instant the
    // frame is ready rather than fixed-sleeping. (The repo `web` isn't shown —
    // the tab name comes from cwd — so we key off the activity msg `building`.)
    let appeared = session.wait_for_sidebar(32, "building", std::time::Duration::from_secs(5));
    let sidebar = sidebar_region(&session.screen(), 32);
    eprintln!("[e2e] sidebar region (32 cols):\n{}", sidebar);
    assert!(
        appeared,
        "sidebar should show the piped activity 'building' after the pipe;\nsidebar:\n{sidebar}"
    );

    eprintln!("[e2e] PASS: found piped status in the rendered sidebar");
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

    // Pipe a pending ("needs-you") agent to the sibling pane.
    let pending_payload = format!(
        r#"{{"v":1,"source":"claude","pane":{{"type":"terminal","id":{sibling_id}}},"status":"pending","repo":"api","msg":"approve?"}}"#
    );
    eprintln!("[e2e] piping pending agent: {}", pending_payload);
    session.pipe_status(&pending_payload);

    // Poll the vt100 sidebar region (not pty_text) for the needs-you agent's
    // activity — the pending pane is highest-severity, so its msg drives the
    // tab's detail line. Returns as soon as the frame is ready.
    let appeared = session.wait_until(std::time::Duration::from_secs(5), |s| {
        let sb = sidebar_region(&s.screen(), 32);
        sb.contains("api") || sb.contains("approve")
    });
    let sidebar = sidebar_region(&session.screen(), 32);
    eprintln!("[e2e] sidebar region (32 cols):\n{}", sidebar);
    assert!(
        appeared,
        "pending (needs-you) agent must surface in the sidebar after the pipe;\nsidebar:\n{sidebar}"
    );

    eprintln!("[e2e] PASS: needs-you agent appeared in the rendered sidebar");
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

    // Fire the real notify.sh with a PostToolUse Edit hook payload.
    // notify.sh will derive msg="editing auth.rs" and pipe it to the plugin.
    let hook_json = r#"{"hook_event_name":"PostToolUse","cwd":".","tool_name":"Edit","tool_input":{"file_path":"src/auth.rs"}}"#;
    eprintln!(
        "[e2e] firing notify.sh running with hook JSON: {}",
        hook_json
    );
    session.run_notify_sh("running", pane_id, hook_json);

    // Poll the vt100 sidebar region for the activity notify.sh derived.
    let appeared = session.wait_until(std::time::Duration::from_secs(5), |s| {
        let sb = sidebar_region(&s.screen(), 32);
        sb.contains("editing") || sb.contains("auth.rs")
    });
    let sidebar = sidebar_region(&session.screen(), 32);
    eprintln!("[e2e] sidebar region (32 cols):\n{}", sidebar);
    assert!(
        appeared,
        "notify.sh hook should drive the sidebar (expected 'editing' or 'auth.rs');\nsidebar:\n{sidebar}"
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

/// Rendered-output fidelity: assert on the ACTUAL cells Zellij painted — both
/// text and color — not just substring presence in the PTY stream.
///
/// After piping a running agent to the focused pane, the parsed sidebar must show:
///   1. the " RADAR" header on row 0;
///   2. the agent card as TWO rows — a tab row carrying the focus spine `▌` and a
///      running spinner glyph (one of `⠋⠙⠹⠸⠼⠴⠦⠧⠇⠏`), and the next row carrying the
///      activity (`building`) and the Claude identity mark `✳` — proving the
///      two-line agent card layout survived the full plugin → Zellij → PTY
///      round-trip;
///   3. the focused card row painted with a truecolor surface band that DIFFERS
///      from the header's rail band — proving the Cards 3-tint hierarchy renders
///      end-to-end (the unit `tint_map` oracle, but on the real frame).
///
/// Robust to the spinner tick, pane-id assignment, and timing: it pins structure
/// and the color *relationship*, not exact RGB or a byte-for-byte frame. (Note we
/// key off the activity `building`, not the repo `web`: in this layout the tab
/// name comes from Zellij's cwd, so the repo isn't shown — only the msg is.)
#[test]
#[ignore = "e2e: requires zellij + built wasm; run via `just test-e2e`"]
fn rendered_sidebar_paints_focused_card_with_text_and_tint() {
    let wasm = plugin_wasm_path();
    assert!(
        wasm.exists(),
        "Plugin wasm not found at {:?}. Build it first:\n  cargo build --release --target wasm32-wasip1",
        wasm
    );

    let temp_home = pre_grant_permissions(&wasm);
    let session_name = format!("zjr_render_{}", std::process::id());
    let layout = sidebar_layout(&wasm);
    let session = ZellijSession::start(&session_name, &layout, &wasm, temp_home);
    eprintln!("[e2e] plugin loaded");

    let pane_id = session.discover_terminal_pane_id();
    eprintln!("[e2e] terminal pane_id={}", pane_id);

    // Running agent on the focused pane → its tab is the active (focused) card.
    let payload = format!(
        r#"{{"v":1,"source":"claude","pane":{{"type":"terminal","id":{pane_id}}},"status":"running","repo":"web","msg":"building"}}"#
    );
    eprintln!("[e2e] piping: {}", payload);
    session.pipe_status(&payload);
    std::thread::sleep(std::time::Duration::from_millis(800));

    let screen = session.screen();
    let sidebar = sidebar_region(&screen, 32);
    eprintln!("[e2e] sidebar region (32 cols):\n{}", sidebar);

    // (1) Header on row 0.
    let row0 = sidebar.lines().next().unwrap_or("");
    assert!(
        row0.contains("RADAR"),
        "row 0 must be the ' RADAR' header; got {row0:?}\nsidebar:\n{sidebar}"
    );

    // (2) The agent card is two rows: a detail row carrying the activity +
    //     Claude mark, and the tab row above it carrying the focus spine + a
    //     running spinner glyph. Locate by the activity text (reliably rendered).
    let lines: Vec<&str> = sidebar.lines().collect();
    let detail_idx = sidebar_row_index(&screen, 32, "building").unwrap_or_else(|| {
        panic!("the agent activity 'building' must render in the sidebar;\nsidebar:\n{sidebar}")
    });
    assert!(
        lines[detail_idx].contains('✳'),
        "the Claude identity mark '✳' must render on the activity row; got {:?}\nsidebar:\n{sidebar}",
        lines[detail_idx]
    );
    let tab_idx = detail_idx
        .checked_sub(1)
        .expect("the agent tab row must sit above its activity row");
    let tab_row = lines[tab_idx];
    assert!(
        tab_row.contains('▌'),
        "the focused agent's tab row must carry the focus spine '▌'; got {tab_row:?}\nsidebar:\n{sidebar}"
    );
    const SPINNER: [char; 10] = ['⠋', '⠙', '⠹', '⠸', '⠼', '⠴', '⠦', '⠧', '⠇', '⠏'];
    assert!(
        tab_row.chars().any(|c| SPINNER.contains(&c)),
        "the running agent's tab row must carry a spinner glyph (one of {SPINNER:?}); got {tab_row:?}\nsidebar:\n{sidebar}"
    );

    // (3) The focused card row is painted with a truecolor surface band that
    //     differs from the header's rail band — the 3-tint hierarchy is visible
    //     in the real rendered frame.
    let card_bg = sidebar_row_bg_rgb(&screen, tab_idx as u16, 32);
    let header_bg = sidebar_row_bg_rgb(&screen, 0, 32);
    eprintln!("[e2e] focused-card bg={:?}, header bg={:?}", card_bg, header_bg);
    assert!(
        card_bg.is_some(),
        "focused card row must be painted with a truecolor surface in the real frame;\nsidebar:\n{sidebar}"
    );
    assert!(
        header_bg.is_some(),
        "header row must carry the truecolor rail band in the real frame;\nsidebar:\n{sidebar}"
    );
    assert_ne!(
        card_bg, header_bg,
        "the focused card tint must differ from the header rail band (3-tint hierarchy);\nsidebar:\n{sidebar}"
    );

    eprintln!("[e2e] PASS: focused card rendered with correct text + a distinct surface tint");
}

/// Behavior: a completion the user is WATCHING recedes. CONTEXT.md's
/// `reconcile_focus` "focus held" rule — "if they were looking at it when it
/// finished, don't flag it" — carried end-to-end by the timer, not just pinned in
/// unit tests. Pipe a running agent to the focused pane (its activity shows), then
/// pipe `done` with `on_focus:"idle"`. The terminal pane stays focused throughout,
/// so once a timer tick reconciles the held focus, the fresh `Done` recedes and the
/// activity disappears from the final rendered frame.
#[test]
#[ignore = "e2e: requires zellij + built wasm; run via `just test-e2e`"]
fn focused_done_recedes_from_the_rendered_rail() {
    let wasm = plugin_wasm_path();
    assert!(
        wasm.exists(),
        "Plugin wasm not found at {:?}. Build it first:\n  cargo build --release --target wasm32-wasip1",
        wasm
    );

    let temp_home = pre_grant_permissions(&wasm);
    let session_name = format!("zjr_recede_{}", std::process::id());
    let layout = sidebar_layout(&wasm);
    let session = ZellijSession::start(&session_name, &layout, &wasm, temp_home);
    let pane_id = session.discover_terminal_pane_id();
    eprintln!("[e2e] terminal pane_id={}", pane_id);

    // Running agent on the focused pane → its activity renders.
    let running = format!(
        r#"{{"v":1,"source":"claude","pane":{{"type":"terminal","id":{pane_id}}},"status":"running","repo":"web","msg":"deploying"}}"#
    );
    session.pipe_status(&running);
    std::thread::sleep(std::time::Duration::from_millis(800));
    assert!(
        sidebar_region(&session.screen(), 32).contains("deploying"),
        "precondition: the running agent's activity must render before we test recede;\nsidebar:\n{}",
        sidebar_region(&session.screen(), 32)
    );

    // Now it finishes while the pane is still focused. `done` queues on_focus=idle;
    // status_pipe deliberately does NOT reconcile (a pipe can outrun the focus
    // update), so the recede is carried by the next timer tick.
    let done = format!(
        r#"{{"v":1,"source":"claude","pane":{{"type":"terminal","id":{pane_id}}},"status":"done","repo":"web","msg":"deploying","on_focus":"idle"}}"#
    );
    session.pipe_status(&done);
    // The recede rides a timer tick (status_pipe deliberately does not reconcile).
    // Poll up to 6s for the activity to disappear instead of a fixed sleep — this
    // returns the instant the tick fires and tolerates a slow runner.
    let receded = session.wait_until(std::time::Duration::from_secs(6), |s| {
        !sidebar_region(&s.screen(), 32).contains("deploying")
    });
    let sidebar = sidebar_region(&session.screen(), 32);
    eprintln!("[e2e] post-recede sidebar (32 cols):\n{}", sidebar);
    assert!(
        receded,
        "a Done watched under focus must recede from the rail (no lingering activity);\nsidebar:\n{}",
        sidebar
    );
    eprintln!("[e2e] PASS: focused completion receded to idle");
}

/// Behavior: an error the user is watching must NOT recede — CONTEXT.md's hard
/// rule "an Error or a 'needs you' Pending stays lit even while watched". The
/// mirror of `focused_done_recedes_from_the_rendered_rail`: same focused-pane
/// setup, same timer ticks, but an `error` (even with on_focus=idle queued) stays
/// on the rail. Together the two pin both arms of the focus-held branch so a
/// regression that recedes everything — or nothing — is caught end-to-end.
#[test]
#[ignore = "e2e: requires zellij + built wasm; run via `just test-e2e`"]
fn focused_error_stays_lit_on_the_rendered_rail() {
    let wasm = plugin_wasm_path();
    assert!(
        wasm.exists(),
        "Plugin wasm not found at {:?}. Build it first:\n  cargo build --release --target wasm32-wasip1",
        wasm
    );

    let temp_home = pre_grant_permissions(&wasm);
    let session_name = format!("zjr_errstay_{}", std::process::id());
    let layout = sidebar_layout(&wasm);
    let session = ZellijSession::start(&session_name, &layout, &wasm, temp_home);
    let pane_id = session.discover_terminal_pane_id();
    eprintln!("[e2e] terminal pane_id={}", pane_id);

    // An error finishes on the focused pane, with on_focus=idle queued exactly as a
    // Done would carry — only the status differs. The recede guard keys on Done, so
    // this must persist.
    let error = format!(
        r#"{{"v":1,"source":"claude","pane":{{"type":"terminal","id":{pane_id}}},"status":"error","repo":"web","msg":"tests failed","on_focus":"idle"}}"#
    );
    session.pipe_status(&error);
    // Same generous settle as the recede test — if anything were going to recede it,
    // it would have happened within these ticks.
    std::thread::sleep(std::time::Duration::from_secs(3));

    let sidebar = sidebar_region(&session.screen(), 32);
    eprintln!("[e2e] post-settle sidebar (32 cols):\n{}", sidebar);
    assert!(
        sidebar.contains("tests failed"),
        "an Error watched under focus must stay lit on the rail (no recede);\nsidebar:\n{}",
        sidebar
    );
    eprintln!("[e2e] PASS: focused error stayed lit");
}

/// THE headline interaction, end-to-end: a real mouse click on a tab's rail row
/// switches Zellij's active tab to that tab. Host tests pin the click→SwitchTab
/// resolution against the lockstep target map, but until now nothing exercised it
/// through an actual Zellij mouse event.
///
/// Setup: tab 1 carries the sidebar (from the layout). We open a second tab, give
/// each tab a tracked agent row (so both render a clickable rail line), then
/// return to tab 1 — the only tab showing the rail. We click tab 2's row and
/// assert Zellij switched the active tab, observed via `dump-screen` now showing
/// tab 2's terminal (its `ZPID=<pane2>` marker) — a signal *external* to the
/// rail's own rendering, so it proves the SwitchTab effect actually reached
/// Zellij rather than the rail merely repainting.
#[test]
#[ignore = "e2e: requires zellij + built wasm; run via `just test-e2e`"]
fn click_on_a_tab_row_switches_the_active_tab() {
    let wasm = plugin_wasm_path();
    assert!(
        wasm.exists(),
        "Plugin wasm not found at {:?}. Build it first:\n  cargo build --release --target wasm32-wasip1",
        wasm
    );

    let temp_home = pre_grant_permissions(&wasm);
    let session_name = format!("zjr_click_{}", std::process::id());
    let layout = sidebar_layout(&wasm);
    let session = ZellijSession::start(&session_name, &layout, &wasm, temp_home);

    // Tab 1's terminal pane (echoes ZPID=<pane1>) + a tracked agent row ("alpha").
    let pane1 = session.discover_terminal_pane_id();
    eprintln!("[e2e] tab1 pane_id={}", pane1);
    let p1 = format!(
        r#"{{"v":1,"source":"claude","pane":{{"type":"terminal","id":{pane1}}},"status":"running","repo":"one","msg":"alpha"}}"#
    );
    session.pipe_status(&p1);

    // Open tab 2 (default layout → no sidebar). new-tab focuses it; discover its
    // pane id (echoes ZPID=<pane2>), then give it a tracked row ("beta"). The
    // tab-1 plugin instance sees both tabs' panes via the global pane manifest,
    // so its rail renders a row for tab 2 even though tab 2 has no sidebar.
    session.run_action(&["new-tab"]);
    std::thread::sleep(std::time::Duration::from_millis(800));
    let pane2 = session.discover_terminal_pane_id();
    eprintln!("[e2e] tab2 pane_id={}", pane2);
    assert_ne!(pane1, pane2, "tab 2 must have a distinct terminal pane id");
    let p2 = format!(
        r#"{{"v":1,"source":"claude","pane":{{"type":"terminal","id":{pane2}}},"status":"running","repo":"two","msg":"beta"}}"#
    );
    session.pipe_status(&p2);

    // Return to tab 1 — the only tab showing the rail we will click. go-to-tab is
    // fire-and-forget and can be dropped under load, so poll-retry it until
    // dump-screen confirms tab 1's terminal (ZPID=pane1) is focused. This both
    // performs the switch and serves as the precondition.
    let on_tab1 = session.wait_until(std::time::Duration::from_secs(8), |s| {
        s.run_action(&["go-to-tab", "1"]);
        s.dump_screen().contains(&format!("ZPID={pane1}"))
    });
    assert!(on_tab1, "could not return to tab 1 before the click");

    // Both tab rows must render in tab 1's rail before we can click tab 2's.
    let ready = session.wait_until(std::time::Duration::from_secs(6), |s| {
        let sb = sidebar_region(&s.screen(), 32);
        sb.contains("alpha") && sb.contains("beta")
    });
    let screen = session.screen();
    let sidebar = sidebar_region(&screen, 32);
    eprintln!("[e2e] rail before click (32 cols):\n{}", sidebar);
    assert!(
        ready,
        "both tab rows must render in tab 1's rail before clicking;\nsidebar:\n{sidebar}"
    );

    // Click tab 2's rail row. Every rendered line of a tab's card carries that
    // tab's SwitchTab target, so the "beta" detail row is a valid click point.
    // Re-issue the click on each poll: a single synthetic mouse event can be
    // dropped under load, and re-clicking is idempotent (clicking tab 2 once it
    // is already active is a no-op). Assert the switch via dump-screen showing
    // tab 2's terminal — external to the rail's own rendering, so it proves the
    // SwitchTab effect reached Zellij.
    let row2 = sidebar_row_index(&screen, 32, "beta")
        .expect("tab 2 row must be locatable in the rail");
    let click_row = (row2 + 1) as u16;
    eprintln!(
        "[e2e] clicking tab 2 row at sidebar index {} (screen row {})",
        row2, click_row
    );
    let switched = session.wait_until(std::time::Duration::from_secs(8), |s| {
        s.click_at(3, click_row);
        s.dump_screen().contains(&format!("ZPID={pane2}"))
    });
    eprintln!("[e2e] dump-screen after click:\n{}", session.dump_screen());
    assert!(
        switched,
        "clicking tab 2's rail row must switch Zellij's active tab to tab 2 \
         (dump-screen should show ZPID={pane2})"
    );

    eprintln!("[e2e] PASS: a real rail click switched the active tab end-to-end");
}

/// Pins the load-bearing property behind the exit-clear fix: per-pane
/// `CommandChanged` activity is delivered to the plugin instance in *every* tab,
/// not just the tab whose pane it happened in. (Contrast focus, which is
/// per-client and is NOT delivered to background instances — the root of the
/// stale-status desync.) A command run in tab one must appear in tab two's rail
/// too; since the exit-clear rides this same `CommandChanged` signal, this proves
/// it converges across tabs without a producer-side hook.
#[test]
#[ignore = "e2e: requires zellij + built wasm; run via `just test-e2e`"]
fn command_activity_reaches_background_tab_instances() {
    use std::time::Duration;
    let wasm = plugin_wasm_path();
    assert!(
        wasm.exists(),
        "Plugin wasm not found at {wasm:?}. Build it:\n  cargo build --release --target wasm32-wasip1 -p zj-radar-plugin"
    );
    let temp_home = pre_grant_permissions(&wasm);
    let session_name = format!("zjr_xclear_{}", std::process::id());
    let layout = two_sidebar_tabs_layout(&wasm);
    let session = ZellijSession::start(&session_name, &layout, &wasm, temp_home);

    // Discover each tab's terminal pane id. The rail lists ALL tab names on every
    // tab, so switches are confirmed by the per-tab `ZPID` echo (unique per pane),
    // not by rail text. go-to-tab is fire-and-forget, so retry until discovery
    // returns tab two's (distinct) pane id.
    let pane1 = session.discover_terminal_pane_id();
    let mut pane2 = pane1;
    for _ in 0..10 {
        session.run_action(&["go-to-tab", "2"]);
        std::thread::sleep(Duration::from_millis(500));
        let p = session.discover_terminal_pane_id();
        if p != pane1 && p != 0 {
            pane2 = p;
            break;
        }
    }
    eprintln!("[e2e] pane1(tab one)={pane1}  pane2(tab two)={pane2}");
    assert_ne!(pane1, pane2, "could not switch to tab two to discover its pane");

    // Back to tab one, then run a plain foreground command in its terminal. This
    // exercises the *observed-command* path (`CommandChanged`) — no pipe, no pane
    // id matching — which is exactly the signal the exit-clear rides. `sleep 30`
    // stays foreground long enough to observe from the other tab.
    let back1 = session.wait_until(Duration::from_secs(8), |s| {
        s.run_action(&["go-to-tab", "1"]);
        s.dump_screen().contains(&format!("ZPID={pane1}"))
    });
    assert!(back1, "could not return to tab one to run the command");
    session.run_action(&["write-chars", "sleep 30"]);
    session.run_action(&["write", "13"]);

    // Tab one's OWN instance must show the running command (baseline: the
    // command path works in-instance).
    let seen_on_1 = session.wait_for_sidebar(32, "sleep", Duration::from_secs(10));
    eprintln!(
        "[e2e] tab-one rail WHILE running:\n{}",
        sidebar_region(&session.screen(), 32)
    );
    assert!(
        seen_on_1,
        "tab one's own rail should show the running command;\n{}",
        sidebar_region(&session.screen(), 32)
    );

    // THE QUESTION: switch to tab two and read ITS rail. Does the background
    // instance also see tab one's command? That answers whether `CommandChanged`
    // (and thus the exit-clear) reaches instances in other tabs.
    let on2 = session.wait_until(Duration::from_secs(8), |s| {
        s.run_action(&["go-to-tab", "2"]);
        s.dump_screen().contains(&format!("ZPID={pane2}"))
    });
    assert!(on2, "could not switch to tab two to read its rail");
    // Give tab two's instance time to process + promote (debounce) if it got it.
    let converged = session.wait_until(Duration::from_secs(6), |s| {
        sidebar_region(&s.screen(), 32).contains("sleep")
    });
    let tab2 = sidebar_region(&session.screen(), 32);
    eprintln!("[e2e] tab-two rail (background instance):\n{tab2}");
    eprintln!("[e2e] RESULT: background instance sees the command (converges) = {converged}");
    assert!(
        converged,
        "CONVERGENCE: tab two's background instance does NOT show tab one's \
         running command — CommandChanged did not reach the background instance, \
         so the exit-clear converges only for newly-opened tabs (snapshot). The \
         SessionEnd→idle broadcast would be needed to converge live. tab-two rail:\n{tab2}"
    );
}

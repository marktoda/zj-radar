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
/// Focus does NOT drive rail state (CONTEXT.md's convergence rule): a `done` on
/// the pane you are watching stays lit — focus-driven recede was removed because
/// focus is per-client and never reaches background instances, so it desynced
/// tabs. A finished status clears only via *shared* signals every instance
/// receives; here we exercise the broadcast one end-to-end: `done` persists
/// through several timer ticks, then an `idle` broadcast (the `/clear` reset
/// path) recedes it.
#[test]
#[ignore = "e2e: requires zellij + built wasm; run via `just test-e2e`"]
fn focused_done_stays_lit_until_a_shared_clear() {
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
        "precondition: the running agent's activity must render first;\nsidebar:\n{}",
        sidebar_region(&session.screen(), 32)
    );

    // It finishes while the pane is still focused. Focus must not clear it:
    // the row (still carrying its message) stays lit through the settle ticks.
    let done = format!(
        r#"{{"v":1,"source":"claude","pane":{{"type":"terminal","id":{pane_id}}},"status":"done","repo":"web","msg":"deploying"}}"#
    );
    session.pipe_status(&done);
    std::thread::sleep(std::time::Duration::from_secs(3));
    let sidebar = sidebar_region(&session.screen(), 32);
    eprintln!("[e2e] post-done sidebar (32 cols):\n{}", sidebar);
    assert!(
        sidebar.contains("deploying"),
        "a Done must stay lit even while watched (focus never clears state);\nsidebar:\n{}",
        sidebar
    );

    // A shared signal — a fresh broadcast for the pane (the `/clear` → idle
    // reset) — is what recedes it, on every instance alike. Check the CARD
    // region only: the receded completion now legitimately resurfaces as an
    // `─ earlier` ledger row (spec §9's bottom region), so a whole-sidebar
    // substring check would false-negative once that row renders — this
    // asserts the live card is gone, not that the string vanished entirely.
    let idle = format!(
        r#"{{"v":1,"source":"claude","pane":{{"type":"terminal","id":{pane_id}}},"status":"idle","repo":"web","msg":""}}"#
    );
    session.pipe_status(&idle);
    let receded = session.wait_until(std::time::Duration::from_secs(6), |s| {
        !card_region_only(&sidebar_region(&s.screen(), 32)).contains("deploying")
    });
    let sidebar = sidebar_region(&session.screen(), 32);
    eprintln!("[e2e] post-idle sidebar (32 cols):\n{}", sidebar);
    assert!(
        receded,
        "an idle broadcast must recede the finished card row;\nsidebar:\n{}",
        sidebar
    );
    // The feature working: the completion that just receded resurfaces below
    // the `─ earlier` rule as a ledger row rather than vanishing outright.
    assert!(
        sidebar
            .split_once("─ earlier")
            .is_some_and(|(_, after)| after.contains("deploying")),
        "the receded completion should resurface as an `─ earlier` ledger row;\nsidebar:\n{}",
        sidebar
    );
    eprintln!(
        "[e2e] PASS: done stayed lit under focus, receded to the ledger on the idle broadcast"
    );
}

/// Behavior: an error the user is watching must NOT recede — CONTEXT.md's hard
/// rule "an Error or a 'needs you' Pending stays lit even while watched". The
/// sibling of `focused_done_stays_lit_until_a_shared_clear`: same focused-pane
/// setup, same timer ticks; a legacy `on_focus` hint riding the payload is
/// tolerated on the wire but inert, so the error stays on the rail.
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

    // An error finishes on the focused pane. The payload deliberately carries a
    // legacy `on_focus:"idle"` hint: older producers may still send it, and it
    // must be tolerated on the wire yet change nothing — the error persists.
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

/// The auto-dispatch onboarding path (`zj-radar run` on a LIVE attach): attaching
/// applies no layout, so the onboarding float never auto-opens — instead `run`
/// dispatches `launch-or-focus-plugin <wasm> --floating -c role=onboarding` to the
/// running session. This is the ONE runtime claim the host unit tests can't cover:
/// that the dispatched float actually RENDERS and hosts Zellij's grant prompt on a
/// live server. We prove it end-to-end by reproducing the attach steady state (an
/// ungranted, deferring rail with no float), dispatching exactly what `run` sends,
/// and answering the resulting grant prompt.
///
/// The assertion is wording-independent by construction: a deferring rail NEVER
/// calls `request_permission`, so the only thing that can produce a grantable
/// prompt is the dispatched onboarding float. Therefore if pressing `y` makes the
/// rail's `needs_permission` face clear (grant landed → marker → the rail's URL
/// inherits it), the float must have opened and rendered its prompt. We re-press
/// each poll to absorb the float-open → prompt-focus → grant → marker → timer-tick
/// latency, mirroring the re-issue-under-`wait_until` pattern the click test uses.
#[test]
#[ignore = "e2e: requires zellij + built wasm; run via `just test-e2e`"]
fn dispatched_grant_float_enables_ungranted_attached_session() {
    use std::time::Duration;
    let wasm = plugin_wasm_path();
    assert!(
        wasm.exists(),
        "Plugin wasm not found at {wasm:?}. Build it:\n  cargo build --release --target wasm32-wasip1 -p zj-radar-plugin"
    );

    // Ungranted: an isolated HOME with NO grant reproduces "attached, never
    // granted"; a deferring rail with no float is the attach steady state.
    let temp_home = isolated_temp_home();
    let session_name = format!("zjr_grantfloat_{}", std::process::id());
    let layout = deferring_rail_layout(&wasm);
    let session = ZellijSession::start(&session_name, &layout, &wasm, temp_home);
    eprintln!("[e2e] ungranted deferring rail loaded");

    // Precondition: the ungranted rail shows the needs_permission face — a
    // dead-end WITHOUT the dispatch (no float ever opened, nothing to grant).
    let ungranted = session.wait_for_sidebar(32, "needs permission", Duration::from_secs(10));
    let pre = sidebar_region(&session.screen(), 32);
    eprintln!("[e2e] pre-dispatch sidebar (32 cols):\n{}", pre);
    assert!(
        ungranted,
        "precondition: an ungranted deferring rail must show 'needs permission';\nsidebar:\n{pre}"
    );

    // Dispatch EXACTLY what `run` sends on a live attach — `grant_float_args`
    // minus the `--session <s> action` prefix the harness's `action()` adds.
    let wasm_abs = wasm.canonicalize().unwrap_or_else(|_| wasm.clone());
    let url = format!("file:{}", wasm_abs.display());
    session.run_action(&[
        "launch-or-focus-plugin",
        &url,
        "--floating",
        "--move-to-focused-tab",
        "--configuration",
        "role=onboarding",
    ]);
    eprintln!("[e2e] dispatched onboarding float: {url}");

    // Convergence proves the float opened, rendered, and hosted the grant prompt.
    let enabled = session.wait_until(Duration::from_secs(20), |s| {
        s.press("y");
        !sidebar_region(&s.screen(), 32).contains("needs permission")
    });
    let post = sidebar_region(&session.screen(), 32);
    eprintln!("[e2e] post-grant sidebar (32 cols):\n{}", post);
    assert!(
        enabled,
        "dispatching the onboarding float on a LIVE session must open a grant \
         prompt whose 'y' clears the rail's needs_permission face (the deferring \
         rail can't self-grant, so this can only come from the dispatched float);\n\
         sidebar:\n{post}"
    );

    eprintln!("[e2e] PASS: auto-dispatched grant float rendered and enabled the rail");
}

/// Regression pin: the rail paints every column of its pane, not just most of
/// it. Reported live (ghostty): the focused card's bright background band
/// appeared to stop 1-2 columns short of the rail pane's right edge. Reading
/// `crates/plugin/src/lib.rs::render` and `crates/plugin/src/render.rs`'s
/// `paint_card_line`/`render_header` showed every painted band is padded to
/// exactly the `width` Zellij hands the plugin — so a real shortfall could
/// only come from Zellij reporting fewer `cols` than the pane's true
/// on-screen width (an off-by-N upstream of the plugin).
///
/// This drives a REAL Zellij (`size=32 borderless=true` rail, mirroring the
/// live layout) with a focused, active card (`tab.active` selects
/// `surface_active` — the same tint implicated in the report) and reads the
/// OUTER vt100 screen — the terminal's own view of what Zellij emitted, not
/// anything internal to the plugin. It pins: painted-last-col ==
/// pane-last-col (31, since `size=32 borderless=true` gives the pane
/// on-screen columns `0..=31`) on both the header row and the focused card's
/// row.
///
/// A 41-outer-width sweep and live mid-session resizes were also run during
/// the investigation and found zero gap at every point; those aren't kept
/// here (too slow for CI). This test is the load-bearing pin: it proves the
/// plugin-output → outer-terminal seam is gap-free, so the live shortfall is
/// a ghostty-side presentation artifact, not a zj-radar under-paint bug.
#[test]
#[ignore = "e2e: requires zellij + built wasm; run via `just test-e2e`"]
fn rail_paints_every_column_of_its_pane() {
    let wasm = plugin_wasm_path();
    assert!(
        wasm.exists(),
        "Plugin wasm not found at {wasm:?}. Build it:\n  cargo build --release --target wasm32-wasip1 -p zj-radar-plugin"
    );

    let temp_home = pre_grant_permissions(&wasm);
    let session_name = format!("zjr_probe3_{}", std::process::id());
    // Mirrors the user's live layout shape: rail pane size=32 borderless=true
    // on the left, content pane on the right. This is exactly `sidebar_layout`.
    let layout = sidebar_layout(&wasm);
    eprintln!("[e2e] layout:\n{layout}");
    let session = ZellijSession::start(&session_name, &layout, &wasm, temp_home);
    eprintln!("[e2e] plugin loaded");

    let pane_id = session.discover_terminal_pane_id();
    eprintln!("[e2e] terminal pane_id={pane_id}");

    // Active/running status on the sole tab's pane -> tab.active is true (only
    // one tab exists and it's the focused one) -> card_tint picks
    // theme.surface_active for its row (crates/plugin/src/render.rs card_tint).
    let payload = format!(
        r#"{{"v":1,"source":"claude","pane":{{"type":"terminal","id":{pane_id}}},"status":"running","repo":"web","msg":"building"}}"#
    );
    eprintln!("[e2e] piping: {payload}");
    session.pipe_status(&payload);
    std::thread::sleep(std::time::Duration::from_millis(800));

    let screen = session.screen();
    let (screen_rows, screen_cols) = screen.size();
    eprintln!("[e2e] outer vt100 screen size: rows={screen_rows} cols={screen_cols}");

    let sidebar = sidebar_region(&screen, 40);
    eprintln!("[e2e] left-40-col region:\n{sidebar}");

    // Locate the focused card's tab row (spine '▌' + spinner), same technique
    // as `rendered_sidebar_paints_focused_card_with_text_and_tint`.
    let detail_idx = sidebar_row_index(&screen, 40, "building")
        .expect("the agent activity 'building' must render in the sidebar");
    let tab_idx = detail_idx
        .checked_sub(1)
        .expect("the agent tab row must sit above its activity row") as u16;
    eprintln!("[e2e] header row=0, focused-card tab row={tab_idx}, detail row={detail_idx}");

    // Per-column background dump, columns 0..40, for both rows.
    fn dump_row_bg(screen: &vt100::Screen, row: u16, upto: u16) -> Vec<(u16, vt100::Color, String)> {
        (0..upto)
            .map(|c| {
                let cell = screen.cell(row, c);
                let bg = cell.map(|x| x.bgcolor()).unwrap_or(vt100::Color::Default);
                let ch = cell.map(|x| x.contents()).unwrap_or_default();
                (c, bg, ch)
            })
            .collect()
    }

    fn last_painted_col(dump: &[(u16, vt100::Color, String)]) -> Option<u16> {
        dump.iter()
            .rev()
            .find(|(_, bg, _)| !matches!(bg, vt100::Color::Default))
            .map(|(c, _, _)| *c)
    }

    fn fmt_dump(dump: &[(u16, vt100::Color, String)]) -> String {
        dump.iter()
            .map(|(c, bg, ch)| {
                let bgs = match bg {
                    vt100::Color::Default => "default".to_string(),
                    vt100::Color::Idx(i) => format!("idx({i})"),
                    vt100::Color::Rgb(r, g, b) => format!("rgb({r},{g},{b})"),
                };
                let chs = if ch.trim().is_empty() {
                    "' '".to_string()
                } else {
                    format!("{ch:?}")
                };
                format!("  col={c:2} bg={bgs:<14} ch={chs}")
            })
            .collect::<Vec<_>>()
            .join("\n")
    }

    let header_dump = dump_row_bg(&screen, 0, 40);
    let card_dump = dump_row_bg(&screen, tab_idx, 40);

    eprintln!("[e2e] === header row (0) per-column dump ===\n{}", fmt_dump(&header_dump));
    eprintln!("[e2e] === focused-card row ({tab_idx}) per-column dump ===\n{}", fmt_dump(&card_dump));

    let header_last = last_painted_col(&header_dump);
    let card_last = last_painted_col(&card_dump);

    // The rail pane is `size=32 borderless=true`. Borderless means Zellij
    // reserves no column for chrome, so the pane's on-screen column range as
    // the OUTER terminal sees it should be exactly [0, 31] (32 columns), and
    // column 32 onward belongs to the sibling content pane.
    const EXPECTED_PANE_LAST_COL: u16 = 31; // 0-indexed; 32 columns total.

    eprintln!(
        "[e2e] header last painted col = {:?} (expected pane last col = {})",
        header_last, EXPECTED_PANE_LAST_COL
    );
    eprintln!(
        "[e2e] focused-card last painted col = {:?} (expected pane last col = {})",
        card_last, EXPECTED_PANE_LAST_COL
    );

    // Report what's immediately to the right of the expected boundary, so we
    // can see whether col 32+ is sibling-pane content (proving the plugin's
    // own band stopped at 31) or still rail-colored (proving the plugin
    // painted further right than expected).
    eprintln!(
        "[e2e] columns 28..40 header:  {:?}",
        &header_dump[28..40]
            .iter()
            .map(|(c, bg, ch)| format!("{c}:{bg:?}:{ch:?}"))
            .collect::<Vec<_>>()
    );
    eprintln!(
        "[e2e] columns 28..40 card:    {:?}",
        &card_dump[28..40]
            .iter()
            .map(|(c, bg, ch)| format!("{c}:{bg:?}:{ch:?}"))
            .collect::<Vec<_>>()
    );

    // Decisive assertions, written so failures self-document the exact
    // shortfall (or confirm full coverage) in the panic message.
    assert_eq!(
        header_last,
        Some(EXPECTED_PANE_LAST_COL),
        "VERDICT DATA: header row's last painted column is {header_last:?}, expected \
         {EXPECTED_PANE_LAST_COL} (pane region's last column, size=32 borderless=true). \
         If this is Some(n) with n < {EXPECTED_PANE_LAST_COL}, the plugin/harness paints \
         exactly up to col n and the remaining columns up to {EXPECTED_PANE_LAST_COL} are \
         unpainted WITHIN the pane region as this outer terminal sees it -> plugin \
         under-paints relative to the pane. If None, no truecolor bg was found at all \
         (broken probe or theme). Full dumps above."
    );
    assert_eq!(
        card_last,
        Some(EXPECTED_PANE_LAST_COL),
        "VERDICT DATA: focused-card row's last painted column is {card_last:?}, expected \
         {EXPECTED_PANE_LAST_COL} (pane region's last column). Same interpretation as the \
         header assertion. Full dumps above."
    );

    eprintln!(
        "[e2e] VERDICT: painted span covers the full advertised pane width \
         (last col {EXPECTED_PANE_LAST_COL}) for both header and focused-card rows \
         in this PTY+vt100 harness. If this test PASSES, the plugin paints every \
         column Zellij hands it and the pane region is fully covered here — any \
         live-ghostty shortfall is not reproduced at the plugin-output/outer-terminal \
         seam this harness observes, pointing at a ghostty-side (or ghostty+font) \
         rendering artifact rather than a plugin under-paint bug."
    );
}

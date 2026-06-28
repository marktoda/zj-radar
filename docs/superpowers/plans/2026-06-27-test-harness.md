# zj-radar Test Harness Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Build a five-layer test harness (unit/golden, property, boundary, cross-device matrix, live E2E) so zj-radar can ship publicly with cross-device and regression confidence.

**Architecture:** Lean into the existing functional-core/imperative-shell split. Most coverage is fast, deterministic, host-target Rust tests; a small feature-gated live-E2E suite drives a real Zellij in a PTY. New tooling is dev-only and never ships in the wasm artifact.

**Tech Stack:** Rust (host target `aarch64-apple-darwin` / `x86_64-unknown-linux-gnu`), `insta`, `proptest`, `vt100`, `assert_cmd`, `assertables`, `portable-pty`, `bats-core` + `bats-assert`/`bats-support`, `shellcheck`, Nix flake, GitHub Actions.

## Global Constraints

- **Host test command:** `cargo test` (no `CARGO_BUILD_TARGET` set). The flake/CI wasm path sets `CARGO_BUILD_TARGET=wasm32-wasip1`; tests must run on the host target. Never assume bare `cargo test` runs in a wasm-target shell.
- **No new runtime deps:** every tool added goes under `[dev-dependencies]` or an optional feature used only by tests. The shipped wasm artifact must be unchanged.
- **insta in CI:** CI sets `INSTA_UPDATE=no` (and `CI=1`) so drifted snapshots FAIL and never silently rewrite baselines.
- **Panic-free production code stays panic-free:** new code in `src/` (non-test) must not introduce panics; tests may panic/assert freely.
- **NEVER run `cargo fmt` (or `cargo fmt --all`):** this project does not use rustfmt (no `rustfmt.toml`; the codebase fails default `cargo fmt --check` by thousands of lines because it is intentionally hand-formatted — aligned one-line multi-field structs, etc.). Match the surrounding hand-formatting style by hand. There is NO fmt CI gate. Running `cargo fmt` reformats the whole codebase and is a defect.
- **bash compatibility:** `notify.sh` must work on macOS bash 3.2 and Linux bash 5 (a real prior bug). All bats tests run on both OSes in CI.
- **E2E is feature-gated:** live-E2E lives behind a Cargo feature `e2e` and is excluded from the default `cargo test` run.
- **Match existing test style:** new Rust tests live in the same module's `#[cfg(test)] mod tests` where one already exists; reuse existing helpers (`agg`, `ro`, `make_state_with_tabs`, `apply_payload`, `p`, `put`).
- **Host triples used in CI:** macOS → `aarch64-apple-darwin`; Linux → `x86_64-unknown-linux-gnu`.

---

## Phase 0 — Tooling Foundation

### Task 1: Add dev-dependencies, devshell tools, and the documented golden command

**Files:**
- Modify: `Cargo.toml` (add `[dev-dependencies]`, add `e2e` feature)
- Modify: `flake.nix` (devShell packages)
- Create: `justfile`
- Modify: `README.md` (Testing section)

**Interfaces:**
- Produces: dev-deps `insta`, `proptest`, `vt100`, `assert_cmd`, `assertables`, `portable-pty`, `tempfile` available to all later tasks; `just test` / `just test-e2e` commands; Cargo feature `e2e`.

- [ ] **Step 1: Add dev-dependencies and the e2e feature to `Cargo.toml`**

Add after the `[features]` block:

```toml
[features]
cli = ["dep:clap", "dep:toml_edit"]
# Live end-to-end tests that drive a real Zellij in a PTY. Off by default so
# `cargo test` stays fast and hermetic. Requires `zellij` on PATH.
e2e = []

[dev-dependencies]
insta = { version = "1", features = ["filters"] }
proptest = "1"
vt100 = "0.15"
assert_cmd = "2"
assertables = "9"
tempfile = "3"
portable-pty = "0.8"
```

- [ ] **Step 2: Verify the project still builds and existing tests pass**

Run: `cargo test --lib`
Expected: builds; existing 182 tests PASS (new deps compile, nothing else changes).

- [ ] **Step 3: Add test tooling to the Nix devShell**

In `flake.nix`, change the devShell `packages` list to include bats, shellcheck, jq, git, and cargo-insta:

```nix
devShells.default = pkgs.mkShell {
  packages = [
    toolchain
    pkgs.zellij
    pkgs.bats
    pkgs.shellcheck
    pkgs.jq
    pkgs.git
    pkgs.cargo-insta
    pkgs.just
  ];
  shellHook = ''
    echo "zj-radar dev shell: $(rustc --version)"
    echo "build:  cargo build --release --target wasm32-wasip1"
    echo "test:   just test        (host-target deterministic suite)"
    echo "e2e:    just test-e2e     (real Zellij in a PTY)"
  '';
};
```

- [ ] **Step 4: Create `justfile` with the golden commands**

```just
# Deterministic suite (L1-L4): host target, fail on snapshot drift in CI.
test:
    cargo test --all-features

# Bash hook tests (requires bats + shellcheck + jq on PATH).
test-bash:
    shellcheck plugins/zj-radar-claude/scripts/notify.sh
    bats plugins/zj-radar-claude/tests

# Live E2E (L5): builds the wasm plugin, drives a real Zellij in a PTY.
test-e2e:
    cargo build --release --target wasm32-wasip1
    cargo test --features e2e --test e2e -- --include-ignored

# Review/accept snapshot changes after intentional render edits.
review:
    cargo insta review

# Everything a PR must pass locally.
ci: test test-bash
```

Note: `cargo test --all-features` enables `cli` so the CLI integration tests (Task 8-9) build.

- [ ] **Step 5: Document the testing workflow in README.md**

Add a `## Testing` section:

```markdown
## Testing

The suite is a layered pyramid. Run the deterministic layers (fast, hermetic):

    just test          # L1-L4: unit, golden snapshots, property, cross-device matrix
    just test-bash     # notify.sh via bats + shellcheck

Snapshots use [insta]. After an intentional rendering change, review and accept:

    just review

Live end-to-end tests drive a real Zellij in a PTY (slow; needs `zellij` on PATH):

    just test-e2e

CI runs `just test` + `just test-bash` on macOS and Linux for every PR, and
`just test-e2e` nightly and on release tags.

[insta]: https://insta.rs
```

- [ ] **Step 6: Commit**

```bash
git add Cargo.toml Cargo.lock flake.nix justfile README.md
git commit -m "test(infra): add test tooling, e2e feature, and golden commands"
```

---

## Phase 1 — L1: Golden Snapshots & Unit Gaps

### Task 2: Snapshot helpers + convert canonical render scenarios to insta

**Files:**
- Modify: `src/render.rs` (test module: add helpers + snapshot tests; remove the hand-rolled `tint_map` golden once replaced)
- Create: `src/snapshots/` (insta-generated, committed)

**Interfaces:**
- Consumes: `render(rows, opts)`, `strip_ansi`/`visible_width`, `TabRow`, `RenderOpts`, helpers `agg`, `ro`.
- Produces: test helpers `ro_full(width, height, density, glyphs)`, `scenario_canonical() -> Vec<TabRow>`, `grid(s: &str) -> String`; committed `.snap` files.

- [ ] **Step 1: Add a plain-text grid helper to the render test module**

In `src/render.rs` `mod tests`, add (vt100 turns raw output into the visible character grid, the human-readable view a reviewer reads):

```rust
/// Render raw output into the visible character grid (ANSI stripped via a real
/// VT parser), one line per terminal row — the human-readable snapshot.
fn grid(raw: &str, width: u16) -> String {
    let height = raw.lines().count().max(1) as u16;
    let mut parser = vt100::Parser::new(height, width, 0);
    // Drive each rendered line as its own terminal row.
    let joined = raw.replace('\n', "\r\n");
    parser.process(joined.as_bytes());
    let screen = parser.screen();
    (0..height)
        .map(|r| {
            (0..width)
                .map(|c| screen.cell(r, c).map(|cell| cell.contents()).unwrap_or_default())
                .collect::<String>()
                .trim_end()
                .to_string()
        })
        .collect::<Vec<_>>()
        .join("\n")
}

/// A representative multi-state session used by several snapshot tests.
fn scenario_canonical() -> Vec<TabRow> {
    use crate::model::Detail;
    let running = Detail { repo: "web".into(), branch: "".into(), msg: "building…".into(),
        kind: Kind::Claude, since_tick: 0, status: Status::Running };
    let pending = Detail { repo: "api".into(), branch: "fix".into(), msg: "".into(),
        kind: Kind::Claude, since_tick: 0, status: Status::Pending };
    let done = Detail { repo: "worker".into(), branch: "".into(), msg: "".into(),
        kind: Kind::Claude, since_tick: 0, status: Status::Done };
    vec![
        TabRow { number: 1, name: "web".into(),    active: true,  has_bell: false,
                 agg: agg(Status::Running, 0, 1, Some(running)) },
        TabRow { number: 2, name: "api".into(),    active: false, has_bell: false,
                 agg: agg(Status::Pending, 0, 1, Some(pending)) },
        TabRow { number: 3, name: "worker".into(), active: false, has_bell: false,
                 agg: agg(Status::Done, 1, 1, Some(done)) },
        TabRow { number: 4, name: "notes".into(),  active: false, has_bell: false,
                 agg: agg(Status::Idle, 0, 0, None) },
    ]
}

fn ro_full(width: usize, height: usize, density: crate::config::Density, glyphs: GlyphSet) -> RenderOpts {
    RenderOpts { width, height, now_tick: 0, glyphs, header: true, density,
        theme: crate::theme::DerivedColors::default() }
}
```

- [ ] **Step 2: Write the failing snapshot test (grid view)**

Add:

```rust
#[test]
fn snapshot_canonical_cards_grid() {
    let rows = scenario_canonical();
    let raw = render(&rows, &ro_full(30, 100, crate::config::Density::Cards, GlyphSet::Plain));
    insta::assert_snapshot!("canonical_cards_grid", grid(&raw, 30));
}
```

- [ ] **Step 3: Run it to generate the snapshot and inspect**

Run: `cargo insta test --review` (or `cargo test render::tests::snapshot_canonical_cards_grid` then `cargo insta review`)
Expected: a `.snap.new` is produced; review shows the rendered grid; accept it. Confirm the grid is sane (aligned columns, no garbage).

- [ ] **Step 4: Add the raw-escape snapshot (catches color regressions)**

```rust
#[test]
fn snapshot_canonical_cards_raw() {
    let rows = scenario_canonical();
    let raw = render(&rows, &ro_full(30, 100, crate::config::Density::Cards, GlyphSet::Plain));
    // Make escapes visible/diffable.
    let shown = raw.replace('\x1b', "\\e");
    insta::assert_snapshot!("canonical_cards_raw", shown);
}
```

Run: `cargo insta test --review`; accept.

- [ ] **Step 5: Replace the hand-rolled tint golden with a snapshot**

Delete the `cards_3tint_layout_snapshot` test body's manual assertions and the `tint_map` helper if now unused; replace with a snapshot of `tint_map`'s classification OR a grid snapshot. Keep `cards_tint_per_row_class` (it asserts a precise invariant). Add:

```rust
#[test]
fn snapshot_canonical_tint_map() {
    let rows = scenario_canonical();
    let raw = render(&rows, &ro_full(30, 100, crate::config::Density::Cards, GlyphSet::Plain));
    insta::assert_snapshot!("canonical_tint_map", tint_map(&raw));
}
```

(Keep `tint_map` if reused here; otherwise inline it into this test.)

- [ ] **Step 6: Run the full render suite and commit snapshots**

Run: `cargo test --lib render`
Expected: PASS.

```bash
git add src/render.rs src/snapshots
git commit -m "test(render): golden snapshots via insta (grid + raw + tint)"
```

### Task 3: Fill L1 unit gaps (overflow extremes, all-states tree, payload defense)

**Files:**
- Modify: `src/render.rs` (test module)
- Modify: `src/payload.rs` (test module)

**Interfaces:**
- Consumes: `render`, `pane_tree_plan`, `parse`, `sanitize`, `MAX_PAYLOAD_BYTES`, `MAX_MSG_CHARS`.

- [ ] **Step 1: Write failing overflow-extreme tests**

In `src/render.rs` `mod tests`:

```rust
#[test]
fn renders_at_extreme_small_width_without_panic_or_overflow() {
    let rows = scenario_canonical();
    let s = render(&rows, &ro_full(8, 100, crate::config::Density::Cards, GlyphSet::Plain));
    for line in s.lines() {
        assert!(visible_width(line) <= 8, "line exceeds width 8: {:?}", line);
    }
}

#[test]
fn renders_at_extreme_small_height_clamps_lines() {
    let rows = scenario_canonical();
    let s = render(&rows, &ro_full(30, 3, crate::config::Density::Cards, GlyphSet::Plain));
    assert!(s.lines().count() <= 3, "exceeded height budget: {}", s.lines().count());
}
```

- [ ] **Step 2: Run them**

Run: `cargo test --lib render::tests::renders_at_extreme`
Expected: PASS if rendering already clamps; if either FAILS, that's a real overflow bug — fix the renderer minimally (clamp width via `truncate`/`visible_width`, clamp line count to `opts.height`) then re-run. Document the fix in the commit.

- [ ] **Step 3: Write the all-states multi-pane tree test**

```rust
#[test]
fn pane_tree_plan_handles_all_states_present() {
    use crate::model::PaneEntry;
    let panes = vec![
        PaneEntry { pane_id: 1, kind: Kind::Claude, status: Status::Error,   msg: "boom".into() },
        PaneEntry { pane_id: 2, kind: Kind::Claude, status: Status::Pending, msg: "approve?".into() },
        PaneEntry { pane_id: 3, kind: Kind::Claude, status: Status::Running, msg: "work".into() },
        PaneEntry { pane_id: 4, kind: Kind::Claude, status: Status::Done,    msg: "".into() },
        PaneEntry { pane_id: 5, kind: Kind::Claude, status: Status::Idle,    msg: "".into() },
    ];
    let mut a = agg(Status::Error, 1, 5, None);
    a.panes = panes;
    let plan = pane_tree_plan(&a, false);
    // Needs-you panes (Error, Pending) are always expanded.
    assert!(plan.expanded.iter().any(|p| p.status == Status::Error));
    assert!(plan.expanded.iter().any(|p| p.status == Status::Pending));
}
```

- [ ] **Step 4: Run it**

Run: `cargo test --lib render::tests::pane_tree_plan_handles_all_states_present`
Expected: PASS (adjust the assertion to the actual expand policy if needed — read `pane_tree_plan` and assert its documented behavior).

- [ ] **Step 5: Write payload defense-in-depth tests**

In `src/payload.rs` `mod tests`:

```rust
#[test]
fn rejects_oversized_payload() {
    let big = format!(r#"{{"v":1,"pane":{{"type":"terminal","id":1}},"status":"running","msg":"{}"}}"#,
        "x".repeat(MAX_PAYLOAD_BYTES));
    assert!(parse(&big).is_none());
}

#[test]
fn rejects_malformed_json() {
    assert!(parse("{not json").is_none());
    assert!(parse("").is_none());
    assert!(parse("null").is_none());
}

#[test]
fn sanitize_strips_control_and_truncates() {
    let dirty = "\x1b[31mred\x07\nbeep\ttab";
    let clean = sanitize(dirty, MAX_MSG_CHARS);
    assert!(!clean.contains('\x1b'));
    assert!(!clean.contains('\x07'));
    assert!(!clean.contains('\n'));
    assert!(clean.chars().count() <= MAX_MSG_CHARS);
}
```

- [ ] **Step 6: Run and commit**

Run: `cargo test --lib payload render`
Expected: PASS.

```bash
git add src/render.rs src/payload.rs
git commit -m "test(unit): overflow extremes, all-states tree, payload defense-in-depth"
```

---

## Phase 2 — L2: Property / Invariant Tests

### Task 4: proptest — sanitizer invariants

**Files:**
- Modify: `src/payload.rs` (test module)

- [ ] **Step 1: Write the failing property test**

In `src/payload.rs` `mod tests`, add:

```rust
proptest::proptest! {
    #[test]
    fn sanitize_never_emits_control_or_overlong(input in ".{0,500}", max in 1usize..120) {
        let out = sanitize(&input, max);
        prop_assert!(out.chars().count() <= max, "len {} > max {}", out.chars().count(), max);
        for ch in out.chars() {
            prop_assert!(ch != '\x1b', "ESC leaked");
            prop_assert!(!ch.is_control(), "control char leaked: {:?}", ch);
        }
    }
}
```

Add `use proptest::prelude::*;` at the top of the test module if not present.

- [ ] **Step 2: Run it**

Run: `cargo test --lib payload::tests::sanitize_never_emits`
Expected: PASS. If proptest shrinks to a failing input, that's a real sanitizer hole — fix `sanitize` minimally and re-run.

- [ ] **Step 3: Commit**

```bash
git add src/payload.rs
git commit -m "test(payload): proptest sanitizer never leaks control chars or overruns"
```

### Task 5: proptest — render layout invariants (width/height/no-panic)

**Files:**
- Modify: `src/render.rs` (test module)

**Interfaces:**
- Produces: a `proptest` strategy `arb_rows()` reused by Task 6.

- [ ] **Step 1: Add a row strategy and the layout property test**

```rust
use proptest::prelude::*;

prop_compose! {
    fn arb_status()(n in 0u8..5) -> Status {
        match n { 0 => Status::Idle, 1 => Status::Done, 2 => Status::Running,
                  3 => Status::Pending, _ => Status::Error }
    }
}

prop_compose! {
    fn arb_row(i: u32)(
        status in arb_status(),
        name in "[a-zA-Z0-9_-]{0,20}",
        active in any::<bool>(),
        total in 0usize..4,
    ) -> TabRow {
        let detail = if total > 0 {
            Some(crate::model::Detail { repo: "r".into(), branch: "".into(), msg: "m".into(),
                kind: Kind::Claude, since_tick: 0, status })
        } else { None };
        TabRow { number: i + 1, name, active, has_bell: false,
                 agg: agg(status, 0, total, detail) }
    }
}

fn arb_rows() -> impl Strategy<Value = Vec<TabRow>> {
    proptest::collection::vec(0u32..8, 0..8)
        .prop_flat_map(|ids| {
            ids.into_iter().enumerate()
                .map(|(i, _)| arb_row(i as u32))
                .collect::<Vec<_>>()
        })
}

proptest! {
    #[test]
    fn render_respects_width_height_and_never_panics(
        rows in arb_rows(),
        width in 4usize..120,
        height in 1usize..60,
    ) {
        let opts = ro_full(width, height, crate::config::Density::Cards, GlyphSet::Plain);
        let s = render(&rows, &opts); // must not panic
        prop_assert!(s.lines().count() <= height, "lines {} > height {}", s.lines().count(), height);
        for line in s.lines() {
            prop_assert!(visible_width(line) <= width, "line width {} > {}: {:?}",
                visible_width(line), width, line);
        }
    }
}
```

- [ ] **Step 2: Run it**

Run: `cargo test --lib render::tests::render_respects_width_height`
Expected: PASS. Any shrunk failure is a real layout/overflow/panic bug — fix the renderer minimally and re-run. (This is the property that *proves* the "panic-free" claim across the input space.)

- [ ] **Step 3: Commit**

```bash
git add src/render.rs
git commit -m "test(render): proptest layout invariants — width/height bound, panic-free"
```

### Task 6: proptest — click round-trip (render ↔ target_at_line)

**Files:**
- Modify: `src/lib.rs` (test module — `target_at_line` lives on `State`)

**Interfaces:**
- Consumes: `State`, `build_rows`, `target_at_line`, `make_state_with_tabs`, `apply_payload`.

- [ ] **Step 1: Read `target_at_line` and `build_rows` to learn the exact line→(tab,pane) contract**

Run: `sed -n '99,210p' src/lib.rs` (review `build_rows`, `tab_position_at_line`, `target_at_line`).
No code change; this step ensures the property matches the real contract.

- [ ] **Step 2: Write the round-trip property test**

In `src/lib.rs` `mod tests`:

```rust
use proptest::prelude::*;

proptest! {
    #[test]
    fn click_round_trip_hits_drawn_target(
        n_tabs in 1usize..6,
        active_idx in 0usize..6,
        statuses in proptest::collection::vec(0u8..5, 1..6),
    ) {
        // Build a state with n_tabs, each tab one pane with a status.
        let specs: Vec<(usize, &str, bool)> = (0..n_tabs)
            .map(|i| (i, "t", i == active_idx % n_tabs)).collect();
        let mut state = make_state_with_tabs(&specs);
        state.last_render_height = 200;
        for (i, &s) in statuses.iter().take(n_tabs).enumerate() {
            let st = match s { 0 => Status::Idle, 1 => Status::Done, 2 => Status::Running,
                               3 => Status::Pending, _ => Status::Error };
            // one pane per tab; pane id = tab index
            apply_payload(&mut state, i as u32, st, 1);
            state.tab_panes.insert(i, vec![pane(i as u32)]);
        }
        // For every drawn line, target_at_line must resolve to a real tab.
        let rows = state.build_rows();
        let total_body: usize = rows.iter()
            .map(|r| crate::render::row_lines(&r.agg, r.active)).sum();
        for line in 0..(total_body as isize + 2) {
            if let Some((tab, _pane)) = state.target_at_line(line) {
                prop_assert!(tab < n_tabs, "resolved tab {} out of range {}", tab, n_tabs);
            }
        }
    }
}
```

- [ ] **Step 3: Run it**

Run: `cargo test --lib tests::click_round_trip`
Expected: PASS. A shrunk failure means clicks can land on a non-existent/wrong tab — a real bug. Fix `target_at_line`/`build_rows` agreement, re-run.

- [ ] **Step 4: Commit**

```bash
git add src/lib.rs
git commit -m "test(lib): proptest click round-trip — every drawn line resolves to a real tab"
```

### Task 7: proptest — state/command seq dedup convergence + payload round-trip

**Files:**
- Modify: `src/state.rs` (test module)
- Modify: `src/payload.rs` (test module)

- [ ] **Step 1: Write the seq-convergence property in `src/state.rs`**

```rust
use proptest::prelude::*;
use crate::status::Status;

proptest! {
    #[test]
    fn apply_order_independent_with_seq(seqs in proptest::collection::vec(0u64..20, 1..12)) {
        // Same payloads, applied in given order vs sorted order, converge.
        let mk = |seq: u64| StatusPayload {
            pane_id: 1, status: if seq % 2 == 0 { Status::Running } else { Status::Done },
            repo: "r".into(), branch: "".into(), msg: "".into(),
            on_focus: None, seq: Some(seq), source: "t".into(),
        };
        let mut a = StateStore::default();
        for &s in &seqs { a.apply(mk(s), s); }

        let mut sorted = seqs.clone(); sorted.sort_unstable();
        let mut b = StateStore::default();
        for &s in &sorted { b.apply(mk(s), s); }

        prop_assert_eq!(a.get(1).map(|x| x.status), b.get(1).map(|x| x.status));
    }
}
```

- [ ] **Step 2: Run it**

Run: `cargo test --lib state::tests::apply_order_independent`
Expected: PASS (seq dedup must make application order-independent). Shrunk failure = a dedup bug.

- [ ] **Step 3: Write the payload round-trip property in `src/payload.rs`**

```rust
proptest::proptest! {
    #[test]
    fn parse_to_wire_round_trip(
        pane in 0u32..1000,
        repo in "[a-z]{0,15}",
        branch in "[a-z/]{0,15}",
        msg in "[a-zA-Z0-9 ]{0,40}",
    ) {
        let wire = to_wire(pane, Status::Running, &repo, &branch, &msg, None, "test");
        let got = parse(&wire).expect("our own wire output must parse");
        prop_assert_eq!(got.pane_id, pane);
        prop_assert_eq!(got.status, Status::Running);
        prop_assert_eq!(got.repo, repo);
        prop_assert_eq!(got.branch, branch);
    }
}
```

- [ ] **Step 4: Run and commit**

Run: `cargo test --lib state payload`
Expected: PASS.

```bash
git add src/state.rs src/payload.rs
git commit -m "test(state,payload): proptest seq convergence + parse/to_wire round-trip"
```

---

## Phase 3 — L3: Boundary / Integration Tests

### Task 8: CLI I/O integration — recording shims + assert_cmd

**Files:**
- Create: `tests/support/mod.rs` (recording-shim PATH helper)
- Create: `tests/cli_notify.rs`

**Interfaces:**
- Produces: `support::ShimDir` with `fn new() -> ShimDir`, `fn add_recorder(&self, name: &str)`, `fn path_env(&self) -> OsString`, `fn recorded(&self, name: &str) -> Vec<Recorded>` where `Recorded { args: Vec<String>, stdin: String }`.

- [ ] **Step 1: Write the recording-shim support helper**

Create `tests/support/mod.rs`:

```rust
use std::ffi::OsString;
use std::fs;
use std::path::PathBuf;
use tempfile::TempDir;

pub struct ShimDir { pub dir: TempDir }

#[derive(Debug)]
pub struct Recorded { pub args: Vec<String>, pub stdin: String }

impl ShimDir {
    pub fn new() -> Self { ShimDir { dir: TempDir::new().unwrap() } }

    /// Install a fake `name` binary that records argv + stdin to
    /// `<dir>/<name>.log` (one JSON line per invocation) and exits 0.
    pub fn add_recorder(&self, name: &str) {
        let log = self.dir.path().join(format!("{name}.log"));
        let script = format!(
            "#!/usr/bin/env bash\nstdin=\"$(cat)\"\n\
             printf '%s\\t%s\\n' \"$*\" \"${{stdin//$'\\n'/ }}\" >> {log:?}\nexit 0\n",
            log = log
        );
        let bin = self.dir.path().join(name);
        fs::write(&bin, script).unwrap();
        let mut perms = fs::metadata(&bin).unwrap().permissions();
        use std::os::unix::fs::PermissionsExt;
        perms.set_mode(0o755);
        fs::set_permissions(&bin, perms).unwrap();
    }

    /// Install a fake `git` that answers rev-parse/branch deterministically.
    pub fn add_fake_git(&self, repo_toplevel: &str, branch: &str) {
        let script = format!(
            "#!/usr/bin/env bash\n\
             case \"$1 $2\" in\n\
               'rev-parse --show-toplevel') echo {repo:?};;\n\
               'branch --show-current') echo {branch:?};;\n\
               *) exit 0;;\nesac\n",
            repo = repo_toplevel, branch = branch
        );
        let bin = self.dir.path().join("git");
        fs::write(&bin, script).unwrap();
        use std::os::unix::fs::PermissionsExt;
        let mut perms = fs::metadata(&bin).unwrap().permissions();
        perms.set_mode(0o755);
        fs::set_permissions(&bin, perms).unwrap();
    }

    /// PATH value with this shim dir prepended.
    pub fn path_env(&self) -> OsString {
        let existing = std::env::var_os("PATH").unwrap_or_default();
        let mut p = self.dir.path().as_os_str().to_owned();
        p.push(":");
        p.push(existing);
        p
    }

    pub fn recorded(&self, name: &str) -> Vec<Recorded> {
        let log = self.dir.path().join(format!("{name}.log"));
        let body = fs::read_to_string(&log).unwrap_or_default();
        body.lines().filter(|l| !l.is_empty()).map(|l| {
            let mut parts = l.splitn(2, '\t');
            Recorded {
                args: parts.next().unwrap_or("").split_whitespace().map(String::from).collect(),
                stdin: parts.next().unwrap_or("").to_string(),
            }
        }).collect()
    }

    pub fn path_buf(&self) -> PathBuf { self.dir.path().to_path_buf() }
}
```

- [ ] **Step 2: Write the failing CLI integration test**

Create `tests/cli_notify.rs`:

```rust
mod support;
use assert_cmd::Command;
use support::ShimDir;

#[test]
fn claude_posttooluse_edit_broadcasts_editing_activity() {
    let shims = ShimDir::new();
    shims.add_recorder("zellij");
    shims.add_fake_git("/home/u/myrepo", "main");

    let hook = r#"{"hook_event_name":"PostToolUse","cwd":"/home/u/myrepo","tool_name":"Edit","tool_input":{"file_path":"/home/u/myrepo/src/auth.rs"}}"#;

    Command::cargo_bin("zj-radar").unwrap()
        .arg("notify").arg("claude")
        .env("PATH", shims.path_env())
        .env("ZELLIJ", "1")
        .env("ZELLIJ_PANE_ID", "terminal_7")
        .write_stdin(hook)
        .assert().success();

    let calls = shims.recorded("zellij");
    assert_eq!(calls.len(), 1, "expected exactly one zellij pipe broadcast");
    let c = &calls[0];
    assert!(c.args.contains(&"pipe".to_string()));
    assert!(c.stdin.contains("\"pane\""), "payload missing pane");
    assert!(c.stdin.contains("editing auth.rs"), "payload missing activity: {}", c.stdin);
}
```

Note: confirm whether `zj-radar notify` passes the payload via argv (`-- <json>`) or stdin to `zellij pipe`; the recorder captures both. Adjust the assertion to whichever the code uses (read `cli/notify.rs` line ~ the `zellij pipe` invocation).

- [ ] **Step 3: Run it**

Run: `cargo test --features cli --test cli_notify`
Expected: FAIL first if the binary isn't built with `cli`; with `--features cli` it builds. If the activity string differs, fix the assertion to match the real (correct) output, or fix a real bug if the output is wrong.

- [ ] **Step 4: Add a graceful-degradation test (not in Zellij = no broadcast, clean exit)**

```rust
#[test]
fn no_zellij_env_exits_clean_without_broadcast() {
    let shims = ShimDir::new();
    shims.add_recorder("zellij");
    Command::cargo_bin("zj-radar").unwrap()
        .arg("notify").arg("claude")
        .env("PATH", shims.path_env())
        .env_remove("ZELLIJ")
        .env_remove("ZELLIJ_PANE_ID")
        .write_stdin(r#"{"hook_event_name":"Stop","cwd":"/tmp"}"#)
        .assert().success();
    assert!(shims.recorded("zellij").is_empty(), "must not broadcast outside Zellij");
}
```

- [ ] **Step 5: Run and commit**

Run: `cargo test --features cli --test cli_notify`
Expected: PASS.

```bash
git add tests/support tests/cli_notify.rs
git commit -m "test(cli): notify I/O integration via recording shims + assert_cmd"
```

### Task 9: CLI setup integration

**Files:**
- Create: `tests/cli_setup.rs`

- [ ] **Step 1: Write the failing setup integration test**

```rust
mod support;
use assert_cmd::Command;
use tempfile::TempDir;
use std::fs;

#[test]
fn setup_dry_run_does_not_write_config() {
    let home = TempDir::new().unwrap();
    let codex_dir = home.path().join(".codex");
    fs::create_dir_all(&codex_dir).unwrap();
    let cfg = codex_dir.join("config.toml");
    fs::write(&cfg, "").unwrap();

    Command::cargo_bin("zj-radar").unwrap()
        .arg("setup").arg("codex").arg("--dry-run").arg("--yes")
        .env("HOME", home.path())
        .assert().success();

    assert_eq!(fs::read_to_string(&cfg).unwrap(), "", "dry-run must not modify config");
}

#[test]
fn setup_then_setup_is_idempotent() {
    let home = TempDir::new().unwrap();
    let codex_dir = home.path().join(".codex");
    fs::create_dir_all(&codex_dir).unwrap();
    let cfg = codex_dir.join("config.toml");
    fs::write(&cfg, "").unwrap();

    let run = || Command::cargo_bin("zj-radar").unwrap()
        .arg("setup").arg("codex").arg("--yes")
        .env("HOME", home.path()).assert().success();
    run();
    let after_first = fs::read_to_string(&cfg).unwrap();
    run();
    let after_second = fs::read_to_string(&cfg).unwrap();
    assert_eq!(after_first, after_second, "second setup must be a no-op");
}
```

Note: verify `setup` honors `HOME` for locating `~/.codex/config.toml` (read `cli/setup.rs`). If it uses a different env/path, adjust the test to drive that path.

- [ ] **Step 2: Run, fixing path assumptions to match `setup.rs`**

Run: `cargo test --features cli --test cli_setup`
Expected: PASS.

- [ ] **Step 3: Commit**

```bash
git add tests/cli_setup.rs
git commit -m "test(cli): setup dry-run + idempotency integration"
```

### Task 10: notify.sh bats suite + shellcheck

**Files:**
- Create: `plugins/zj-radar-claude/tests/notify.bats`
- Create: `plugins/zj-radar-claude/tests/helper.bash`

**Interfaces:**
- Produces: bats tests invoked by `bats plugins/zj-radar-claude/tests` and `just test-bash`.

- [ ] **Step 1: Write the bats helper that installs fakes on PATH**

Create `plugins/zj-radar-claude/tests/helper.bash`:

```bash
# Shared bats setup: a temp dir of fake binaries on PATH that record calls.
setup_fakes() {
  FAKEBIN="$(mktemp -d)"
  RECORD="$FAKEBIN/zellij.log"
  cat >"$FAKEBIN/zellij" <<EOF
#!/usr/bin/env bash
stdin="\$(cat 2>/dev/null || true)"
printf '%s\t%s\n' "\$*" "\$stdin" >> "$RECORD"
exit 0
EOF
  cat >"$FAKEBIN/git" <<'EOF'
#!/usr/bin/env bash
case "$1 $2" in
  'rev-parse --show-toplevel') echo /home/u/myrepo;;
  'branch --show-current') echo main;;
  *) exit 0;;
esac
EOF
  chmod +x "$FAKEBIN/zellij" "$FAKEBIN/git"
  # Keep the REAL jq (we want real JSON building); only fake zellij/git.
  export PATH="$FAKEBIN:$PATH"
  export ZELLIJ=1 ZELLIJ_PANE_ID=terminal_7
  # Force the bash fallback path (not the native CLI).
  PATH="${PATH/$FAKEBIN:/}"; export PATH="$FAKEBIN:$PATH"
}

teardown_fakes() { rm -rf "$FAKEBIN"; }

last_payload() { tail -n1 "$RECORD" | cut -f2-; }
```

Note: the script prefers `zj-radar` on PATH; to exercise the bash fallback, ensure `zj-radar` is NOT on PATH in tests (the fakes dir doesn't add it).

- [ ] **Step 2: Write the failing bats tests**

Create `plugins/zj-radar-claude/tests/notify.bats`:

```bash
#!/usr/bin/env bats
load helper

SCRIPT="$BATS_TEST_DIRNAME/../scripts/notify.sh"

setup() { setup_fakes; }
teardown() { teardown_fakes; }

@test "PostToolUse Edit derives 'editing <basename>'" {
  echo '{"hook_event_name":"PostToolUse","cwd":"/home/u/myrepo","tool_name":"Edit","tool_input":{"file_path":"/home/u/myrepo/src/auth.rs"}}' \
    | "$SCRIPT" running
  run last_payload
  [[ "$output" == *"editing auth.rs"* ]]
}

@test "Bash git push derives 'pushing'" {
  echo '{"hook_event_name":"PostToolUse","cwd":"/home/u/myrepo","tool_name":"Bash","tool_input":{"command":"git push origin main"}}' \
    | "$SCRIPT" running
  run last_payload
  [[ "$output" == *"pushing"* ]]
}

@test "generic pending message is skipped (defense-in-depth)" {
  rm -f "$RECORD"
  echo '{"hook_event_name":"Notification","cwd":"/home/u/myrepo","message":"Claude needs your attention"}' \
    | "$SCRIPT" pending || true
  [ ! -s "$RECORD" ] || { run last_payload; [[ "$output" != *"pending"* ]]; }
}

@test "not in Zellij: clean exit, no broadcast" {
  unset ZELLIJ ZELLIJ_PANE_ID
  rm -f "$RECORD"
  run bash -c "echo '{\"hook_event_name\":\"Stop\",\"cwd\":\"/tmp\"}' | '$SCRIPT' done"
  [ "$status" -eq 0 ]
  [ ! -s "$RECORD" ]
}

@test "done sets on_focus=idle (clear-on-focus)" {
  echo '{"hook_event_name":"Stop","cwd":"/home/u/myrepo"}' | "$SCRIPT" done
  run last_payload
  [[ "$output" == *"on_focus"* ]]
  [[ "$output" == *"idle"* ]]
}
```

- [ ] **Step 3: Run the bats suite**

Run: `bats plugins/zj-radar-claude/tests`
Expected: PASS. If an assertion fails because the activity string differs, align the test with the real (correct) output; if the script is genuinely wrong, fix `notify.sh` (keeping bash 3.2 compatibility).

- [ ] **Step 4: Add shellcheck and confirm clean**

Run: `shellcheck plugins/zj-radar-claude/scripts/notify.sh`
Expected: no findings. Fix any (e.g. quoting) without changing behavior; re-run bats.

- [ ] **Step 5: Commit**

```bash
git add plugins/zj-radar-claude/tests
git commit -m "test(hook): bats coverage for notify.sh + shellcheck clean"
```

### Task 11: bash ↔ Rust producer parity test

**Files:**
- Create: `plugins/zj-radar-claude/tests/parity.bats`

**Interfaces:**
- Consumes: built `zj-radar` binary (release or debug) + `notify.sh`; recording fakes from `helper.bash`.

- [ ] **Step 1: Build the CLI binary so both producers are available**

Run: `cargo build --features cli --bin zj-radar`
Expected: produces `target/debug/zj-radar`.

- [ ] **Step 2: Write the parity test**

Create `plugins/zj-radar-claude/tests/parity.bats`:

```bash
#!/usr/bin/env bats
load helper

SCRIPT="$BATS_TEST_DIRNAME/../scripts/notify.sh"
CLI="$BATS_TEST_DIRNAME/../../../target/debug/zj-radar"

# Extract just the "msg" field from a recorded zj_radar.status.v1 payload.
payload_msg() { last_payload | jq -r '.msg'; }

parity_case() { # $1 = hook JSON, $2 = status arg
  # --- bash producer (fallback path: no zj-radar on PATH) ---
  rm -f "$RECORD"
  echo "$1" | "$SCRIPT" "$2"
  local bash_msg; bash_msg="$(payload_msg)"

  # --- rust producer ---
  rm -f "$RECORD"
  echo "$1" | "$CLI" notify claude --status "$2"
  local rust_msg; rust_msg="$(payload_msg)"

  echo "bash=[$bash_msg] rust=[$rust_msg]"
  [ "$bash_msg" = "$rust_msg" ]
}

setup() { setup_fakes; }
teardown() { teardown_fakes; }

@test "parity: Edit activity" {
  parity_case '{"hook_event_name":"PostToolUse","cwd":"/home/u/myrepo","tool_name":"Edit","tool_input":{"file_path":"/home/u/myrepo/src/auth.rs"}}' running
}

@test "parity: Bash git commit activity" {
  parity_case '{"hook_event_name":"PostToolUse","cwd":"/home/u/myrepo","tool_name":"Bash","tool_input":{"command":"git commit -m x"}}' running
}

@test "parity: Read activity" {
  parity_case '{"hook_event_name":"PostToolUse","cwd":"/home/u/myrepo","tool_name":"Read","tool_input":{"file_path":"/home/u/myrepo/README.md"}}' running
}
```

Note: confirm the Rust CLI accepts `--status` and reads hook JSON from stdin (it does, per `run(agent, input, status_arg, dry_run)`). If the CLI takes the JSON as a positional `input` instead of stdin, adapt the invocation.

- [ ] **Step 3: Run it**

Run: `bats plugins/zj-radar-claude/tests/parity.bats`
Expected: PASS. A mismatch is exactly the drift this test exists to catch — fix whichever producer is wrong so both agree, then re-run.

- [ ] **Step 4: Commit**

```bash
git add plugins/zj-radar-claude/tests/parity.bats
git commit -m "test(hook): bash<->Rust producer parity for tool->activity mapping"
```

### Task 12: Extract lib.rs event handlers + synthetic event-replay tests

**Files:**
- Modify: `src/lib.rs` (extract handler bodies into host-testable `State` methods; add tests)

**Interfaces:**
- Produces: pure methods on `State`: `on_pane_exit(&mut self, pane_id: u32, status: Option<i32>)`, `on_command(&mut self, pane_id: u32, command: &[String], fg: bool, cwd: Option<&str>)`, `on_tick(&mut self)`, `on_focus(&mut self, pane_id: u32)` — each taking plain types, callable on the host. The wasm `update()` match arms call these.

- [ ] **Step 1: Read `update()` to see which arms have inline logic vs delegate**

Run: `sed -n '256,372p' src/lib.rs`
Identify arms with non-trivial inline bodies (e.g. PaneUpdate exit handling, CommandChanged, Timer, Mouse). No code change yet.

- [ ] **Step 2: Extract one handler at a time — start with command + timer + exit**

Add host-callable methods on `State` (outside the `#[cfg(target_arch="wasm32")]` impl so they compile on host). Example for the command/timer/exit trio (use the real field names `self.command`, `self.tick`):

```rust
impl State {
    /// Host-testable: a foreground command changed on a terminal pane.
    pub(crate) fn on_command(&mut self, pane_id: u32, command: &[String], fg: bool, cwd: Option<&str>) {
        self.command.on_command_changed(pane_id, command, fg, cwd, self.tick);
    }
    /// Host-testable: timer tick — advance and promote pending commands.
    pub(crate) fn on_tick(&mut self) {
        self.tick += 1;
        self.command.on_timer(self.tick);
    }
    /// Host-testable: a pane exited with an optional status code.
    pub(crate) fn on_pane_exit(&mut self, pane_id: u32, status: Option<i32>) {
        self.command.on_exit(pane_id, status, self.tick);
    }
    /// Host-testable: a pane gained focus (clear-on-focus transitions).
    pub(crate) fn on_focus(&mut self, pane_id: u32) {
        self.store.on_pane_focused(pane_id, self.tick);
        self.command.on_pane_focused(pane_id, self.tick);
    }
}
```

- [ ] **Step 3: Point the wasm `update()` arms at the new methods**

In the `#[cfg(target_arch="wasm32")]` `update()`, replace the inline bodies of the Timer / CommandChanged / pane-exit / focus arms with calls to `self.on_tick()`, `self.on_command(...)`, `self.on_pane_exit(...)`, `self.on_focus(...)`. Keep behavior identical (this is a pure refactor of the shell).

- [ ] **Step 4: Write the failing event-replay test (state machine walk)**

In `src/lib.rs` `mod tests`:

```rust
#[test]
fn command_pane_walks_idle_running_done_idle() {
    let mut state = make_state_with_tabs(&[(0, "t", true)]);
    state.tab_panes.insert(0, vec![pane(5)]);

    // 1) shell prompt only → idle (no resolved command)
    state.on_command(5, &["zsh".into()], true, Some("/home/u/repo"));
    state.on_tick(); // tick 1
    assert!(state.command.get(5).is_none(), "shell stays idle");

    // 2) real fg command → pending, then Running after debounce
    state.on_command(5, &["cargo".into(), "test".into()], true, Some("/home/u/repo"));
    state.on_tick(); // promote
    state.on_tick();
    assert_eq!(state.command.get(5).map(|s| s.status), Some(Status::Running));

    // 3) exit 0 → Done
    state.on_pane_exit(5, Some(0));
    assert_eq!(state.command.get(5).map(|s| s.status), Some(Status::Done));

    // 4) focus → Idle (clear-on-focus)
    state.on_focus(5);
    let st = state.command.get(5).map(|s| s.status);
    assert!(st == Some(Status::Idle) || st.is_none(), "done clears on focus, got {:?}", st);
}
```

Note: the exact tick counts for debounce promotion follow `DEBOUNCE_TICKS = 1`; adjust the number of `on_tick()` calls to match. `self.command`/`self.store` are private fields in the same module, so the test (same module) can read them.

- [ ] **Step 5: Run it**

Run: `cargo test --lib tests::command_pane_walks`
Expected: PASS. If transitions differ, align ticks/assertions with the real debounce/exit policy (read `command.rs`).

- [ ] **Step 6: Confirm the wasm build still compiles**

Run: `cargo build --target wasm32-wasip1`
Expected: builds (the refactored `update()` arms still typecheck against zellij-tile).

- [ ] **Step 7: Commit**

```bash
git add src/lib.rs
git commit -m "refactor(lib): extract host-testable event handlers; test event-sequence walk"
```

---

## Phase 4 — L4: Cross-Device Matrix

### Task 13: Color-depth axis (truecolor / ANSI-16 / NO_COLOR)

**Files:**
- Modify: `src/render.rs` (test module)
- Possibly modify: `src/render.rs` or `src/theme.rs` (only if a NO_COLOR/depth knob does not yet exist)

**Interfaces:**
- Consumes: `render`, `RenderOpts`, `DerivedColors`.
- Note: the library currently emits truecolor unconditionally. If a color-mode knob is needed for NO_COLOR, add a minimal `ColorMode` to `RenderOpts` (default `Truecolor`) — but FIRST check whether the renderer already has a path that suppresses color; do not add API unless a test requires it (YAGNI).

- [ ] **Step 1: Write the truecolor-present test (baseline)**

```rust
#[test]
fn truecolor_mode_emits_24bit_sgr() {
    let rows = scenario_canonical();
    let s = render(&rows, &ro_full(30, 100, crate::config::Density::Cards, GlyphSet::Plain));
    assert!(s.contains("\x1b[48;2;"), "expected 24-bit background SGR");
}
```

Run: `cargo test --lib render::tests::truecolor_mode_emits`
Expected: PASS (documents current behavior).

- [ ] **Step 2: Decide NO_COLOR handling and write its test**

Read `render.rs` for any existing color suppression. If none and you add `ColorMode`:

```rust
#[test]
fn no_color_mode_emits_no_sgr_color_but_same_text() {
    let rows = scenario_canonical();
    let mut opts = ro_full(30, 100, crate::config::Density::Cards, GlyphSet::Plain);
    opts.color = crate::render::ColorMode::None; // new field, default Truecolor
    let s = render(&rows, &opts);
    assert!(!s.contains("\x1b[48;2;"), "NO_COLOR must not emit 24-bit bg");
    assert!(!s.contains("\x1b[38;2;"), "NO_COLOR must not emit 24-bit fg");
    // text/layout identical to colored grid:
    let colored = render(&rows, &ro_full(30, 100, crate::config::Density::Cards, GlyphSet::Plain));
    assert_eq!(grid(&s, 30), grid(&colored, 30), "NO_COLOR changes only color, not layout");
}
```

If you add `ColorMode`, define it minimally and thread it through the SGR emit points (`tc_bg`/`tc_fg`/status role coloring) so `None` emits no color escapes. The wasm `render()` sets it from a `NO_COLOR` env / config later (out of scope here — default stays Truecolor).

- [ ] **Step 3: Run it**

Run: `cargo test --lib render::tests::no_color_mode`
Expected: PASS. If you chose NOT to add a knob (because suppression already exists), drive the existing path instead.

- [ ] **Step 4: Commit**

```bash
git add src/render.rs src/theme.rs
git commit -m "test(render): color-depth axis — truecolor present, NO_COLOR suppresses color only"
```

### Task 14: Glyph-set + unicode-width axis (alignment via vt100)

**Files:**
- Modify: `src/render.rs` (test module)

**Interfaces:**
- Consumes: `render`, `GlyphSet::{Plain, Nerd}`, `vt100`.

- [ ] **Step 1: Write the glyph-alignment property over both glyph sets**

```rust
#[test]
fn both_glyph_sets_keep_columns_within_width() {
    for glyphs in [GlyphSet::Plain, GlyphSet::Nerd] {
        let rows = scenario_canonical();
        let width = 30u16;
        let raw = render(&rows, &ro_full(width as usize, 100,
            crate::config::Density::Cards, glyphs));
        // Parse with a real VT and assert no cell spills past `width`.
        let g = grid(&raw, width);
        for line in g.lines() {
            assert!(visible_width(line) <= width as usize,
                "glyphs={:?} line wider than {}: {:?}", glyphs, width, line);
        }
    }
}
```

- [ ] **Step 2: Run it**

Run: `cargo test --lib render::tests::both_glyph_sets`
Expected: PASS. A failure means Nerd-glyph width math is off — fix the width accounting in the renderer.

- [ ] **Step 3: Write the unicode-width stressor test**

```rust
#[test]
fn wide_and_combining_chars_do_not_break_alignment() {
    use crate::model::Detail;
    let detail = Detail { repo: "café".into(), branch: "".into(),
        msg: "测试 🚀 e\u{0301}".into(), kind: Kind::Claude, since_tick: 0, status: Status::Running };
    let rows = vec![TabRow { number: 1, name: "测试café🚀".into(), active: true,
        has_bell: false, agg: agg(Status::Running, 0, 1, Some(detail)) }];
    let width = 24u16;
    let raw = render(&rows, &ro_full(width as usize, 100, crate::config::Density::Cards, GlyphSet::Plain));
    for line in grid(&raw, width).lines() {
        assert!(visible_width(line) <= width as usize, "alignment broke: {:?}", line);
    }
}
```

- [ ] **Step 4: Run and commit**

Run: `cargo test --lib render::tests::wide_and_combining`
Expected: PASS (fix renderer width math if not).

```bash
git add src/render.rs
git commit -m "test(render): glyph-set + unicode-width axes — column alignment via vt100"
```

---

## Phase 5 — L5: Live E2E

### Task 15: E2E harness scaffolding (build wasm, spawn Zellij in a PTY, dump-screen)

**Files:**
- Create: `tests/e2e/main.rs` (the `e2e` integration target)
- Create: `tests/e2e/harness.rs`
- Modify: `Cargo.toml` (declare the `e2e` test target if needed)

**Interfaces:**
- Produces: `harness::ZellijSession` with `fn start(layout: &str, plugin_wasm: &Path) -> ZellijSession`, `fn pipe_status(&self, json: &str)`, `fn run_notify_sh(&self, status: &str, hook_json: &str)`, `fn dump_screen(&self) -> String`, `fn grid(&self) -> vt100::Screen`, `Drop` kills the session.

- [ ] **Step 1: Declare the e2e test target in Cargo.toml**

```toml
[[test]]
name = "e2e"
path = "tests/e2e/main.rs"
required-features = ["e2e"]
```

- [ ] **Step 2: Write the harness (PTY + Zellij control)**

Create `tests/e2e/harness.rs`:

```rust
use portable_pty::{CommandBuilder, NativePtySystem, PtySize, PtySystem};
use std::io::Read;
use std::path::Path;
use std::process::Command;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

pub struct ZellijSession {
    pub name: String,
    _child: Box<dyn portable_pty::Child + Send + Sync>,
    _reader: std::thread::JoinHandle<()>,
    _buf: Arc<Mutex<Vec<u8>>>,
}

impl ZellijSession {
    /// Start a headless Zellij in a PTY with `layout` (KDL) that pins the
    /// zj-radar plugin from `plugin_wasm`. The session name is unique per call.
    pub fn start(name: &str, layout_kdl: &str, _plugin_wasm: &Path) -> Self {
        let dir = std::env::temp_dir().join(format!("zjradar-e2e-{name}"));
        std::fs::create_dir_all(&dir).unwrap();
        let layout_path = dir.join("layout.kdl");
        std::fs::write(&layout_path, layout_kdl).unwrap();

        let pty = NativePtySystem::default()
            .openpty(PtySize { rows: 40, cols: 100, pixel_width: 0, pixel_height: 0 })
            .unwrap();
        let mut cmd = CommandBuilder::new("zellij");
        cmd.args(["--session", name, "--layout", layout_path.to_str().unwrap()]);
        cmd.env("ZELLIJ", "0"); // force a fresh server
        let child = pty.slave.spawn_command(cmd).unwrap();

        let mut reader = pty.master.try_clone_reader().unwrap();
        let buf = Arc::new(Mutex::new(Vec::new()));
        let bufc = buf.clone();
        let handle = std::thread::spawn(move || {
            let mut chunk = [0u8; 4096];
            while let Ok(n) = reader.read(&mut chunk) {
                if n == 0 { break; }
                bufc.lock().unwrap().extend_from_slice(&chunk[..n]);
            }
        });

        let s = ZellijSession { name: name.into(), _child: child, _reader: handle, _buf: buf };
        s.wait_until_ready();
        s
    }

    fn wait_until_ready(&self) {
        // Poll dump-screen until it succeeds (server is up).
        let deadline = Instant::now() + Duration::from_secs(15);
        while Instant::now() < deadline {
            if !self.dump_screen().trim().is_empty() { return; }
            std::thread::sleep(Duration::from_millis(200));
        }
        panic!("zellij session {} never became ready", self.name);
    }

    fn action(&self, args: &[&str]) -> std::process::Output {
        Command::new("zellij")
            .args(["--session", &self.name, "action"])
            .args(args)
            .output()
            .expect("zellij action failed to spawn")
    }

    pub fn pipe_status(&self, json: &str) {
        let out = Command::new("zellij")
            .args(["--session", &self.name, "pipe", "--name", "zj_radar.status.v1", "--", json])
            .output().expect("zellij pipe failed");
        assert!(out.status.success(), "pipe failed: {}", String::from_utf8_lossy(&out.stderr));
        std::thread::sleep(Duration::from_millis(300)); // let the plugin re-render
    }

    pub fn dump_screen(&self) -> String {
        let out = self.action(&["dump-screen", "-"]);
        String::from_utf8_lossy(&out.stdout).into_owned()
    }

    /// dump-screen parsed through vt100 for cell-level assertions.
    pub fn screen(&self) -> vt100::Screen {
        let raw = self.dump_screen();
        let mut p = vt100::Parser::new(40, 100, 0);
        p.process(raw.replace('\n', "\r\n").as_bytes());
        p.screen().clone()
    }
}

impl Drop for ZellijSession {
    fn drop(&mut self) {
        let _ = Command::new("zellij").args(["delete-session", &self.name, "--force"]).output();
        let _ = Command::new("zellij").args(["kill-session", &self.name]).output();
    }
}

/// Path to the built wasm plugin (run `cargo build --release --target wasm32-wasip1` first).
pub fn plugin_wasm_path() -> std::path::PathBuf {
    let root = env!("CARGO_MANIFEST_DIR");
    std::path::Path::new(root).join("target/wasm32-wasip1/release/zj_radar.wasm")
}

/// A minimal layout pinning the plugin in a small left column.
pub fn sidebar_layout(plugin_wasm: &Path) -> String {
    format!(r#"layout {{
    pane size=1 borderless=true {{ plugin location="file:{}" }}
    pane
}}"#, plugin_wasm.display())
}
```

- [ ] **Step 3: Write the smoke test (plugin loads and renders something)**

Create `tests/e2e/main.rs`:

```rust
mod harness;
use harness::*;

#[test]
#[ignore = "e2e: requires zellij + built wasm; run via `just test-e2e`"]
fn plugin_loads_and_renders_status() {
    let wasm = plugin_wasm_path();
    assert!(wasm.exists(), "build the plugin first: cargo build --release --target wasm32-wasip1");
    let session = ZellijSession::start("zjr_smoke", &sidebar_layout(&wasm), &wasm);

    session.pipe_status(r#"{"v":1,"source":"claude","pane":{"type":"terminal","id":99},"status":"running","repo":"web","branch":"main","msg":"building"}"#);

    let screen = session.screen();
    let text: String = (0..40).map(|r| {
        (0..100).map(|c| screen.cell(r, c).map(|x| x.contents()).unwrap_or_default()).collect::<String>()
    }).collect::<Vec<_>>().join("\n");
    assert!(text.contains("web") || text.contains("building"),
        "sidebar should show the piped status; got:\n{text}");
}
```

- [ ] **Step 4: Build the plugin and run the smoke test**

Run:
```bash
cargo build --release --target wasm32-wasip1
cargo test --features e2e --test e2e -- --include-ignored plugin_loads_and_renders_status
```
Expected: PASS. If the plugin doesn't render, debug the layout/pipe name against the README's producer protocol. If flaky on timing, increase the `wait_until_ready`/`pipe_status` sleeps.

- [ ] **Step 5: Commit**

```bash
git add tests/e2e Cargo.toml
git commit -m "test(e2e): PTY-driven Zellij harness + status-render smoke test"
```

### Task 16: Canonical live-E2E scenarios

**Files:**
- Modify: `tests/e2e/main.rs`

- [ ] **Step 1: Add the multi-agent + needs-you-wins scenario**

```rust
#[test]
#[ignore = "e2e"]
fn multi_agent_needs_you_is_visible() {
    let wasm = plugin_wasm_path();
    let s = ZellijSession::start("zjr_multi", &sidebar_layout(&wasm), &wasm);
    s.pipe_status(r#"{"v":1,"source":"claude","pane":{"type":"terminal","id":1},"status":"running","repo":"web","msg":"building"}"#);
    s.pipe_status(r#"{"v":1,"source":"claude","pane":{"type":"terminal","id":2},"status":"pending","repo":"api","msg":"approve?"}"#);
    let screen = s.screen();
    let text: String = (0..40).map(|r| (0..100)
        .map(|c| screen.cell(r,c).map(|x| x.contents()).unwrap_or_default()).collect::<String>())
        .collect::<Vec<_>>().join("\n");
    assert!(text.contains("api") || text.contains("approve"),
        "pending (needs-you) agent must surface; got:\n{text}");
}
```

- [ ] **Step 2: Add the real notify.sh end-to-end scenario**

Add to the harness (`tests/e2e/harness.rs`) a method:

```rust
impl ZellijSession {
    /// Fire the real notify.sh against this session (true hook->pipe->render).
    pub fn run_notify_sh(&self, status: &str, hook_json: &str) {
        let script = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("plugins/zj-radar-claude/scripts/notify.sh");
        use std::io::Write;
        let mut child = std::process::Command::new("bash")
            .arg(&script).arg(status)
            .env("ZELLIJ", "1")
            .env("ZELLIJ_PANE_ID", "terminal_55")
            .stdin(std::process::Stdio::piped())
            .spawn().expect("spawn notify.sh");
        child.stdin.as_mut().unwrap().write_all(hook_json.as_bytes()).unwrap();
        child.wait().unwrap();
        std::thread::sleep(std::time::Duration::from_millis(400));
    }
}
```

Then the test:

```rust
#[test]
#[ignore = "e2e"]
fn notify_sh_end_to_end_updates_sidebar() {
    let wasm = plugin_wasm_path();
    let s = ZellijSession::start("zjr_hook", &sidebar_layout(&wasm), &wasm);
    s.run_notify_sh("running",
        r#"{"hook_event_name":"PostToolUse","cwd":".","tool_name":"Edit","tool_input":{"file_path":"src/auth.rs"}}"#);
    let screen = s.screen();
    let text: String = (0..40).map(|r| (0..100)
        .map(|c| screen.cell(r,c).map(|x| x.contents()).unwrap_or_default()).collect::<String>())
        .collect::<Vec<_>>().join("\n");
    assert!(text.contains("editing") || text.contains("auth.rs"),
        "notify.sh hook should drive the sidebar; got:\n{text}");
}
```

Note: `notify.sh` uses `$ZELLIJ_PANE_ID` (terminal_55) for the pane id; the sidebar shows it as a tab/pane entry. If the layout's tab has no pane 55, the status may attach but not display under a known tab — if so, drive `s.pipe_status` with a matching pane id, or add a pane to the layout. Adjust to make the assertion meaningful.

- [ ] **Step 3: Run the full e2e suite**

Run:
```bash
cargo build --release --target wasm32-wasip1
cargo test --features e2e --test e2e -- --include-ignored
```
Expected: all e2e tests PASS. Stabilize timing if flaky.

- [ ] **Step 4: Commit**

```bash
git add tests/e2e
git commit -m "test(e2e): multi-agent needs-you + real notify.sh hook->render scenarios"
```

---

## Phase 6 — CI & Infrastructure

### Task 17: Restructure CI (lint, OS matrix test, nightly e2e)

**Files:**
- Modify: `.github/workflows/ci.yml`
- Create: `.github/workflows/e2e.yml`

- [ ] **Step 1: Rewrite `ci.yml` with lint + OS-matrix test jobs**

```yaml
name: CI

on:
  push:
  pull_request:

jobs:
  lint:
    runs-on: ubuntu-latest
    steps:
      - uses: actions/checkout@v4
      - uses: cachix/install-nix-action@v30
        with:
          extra_nix_config: |
            experimental-features = nix-command flakes
      - name: clippy + shellcheck
        run: |
          # NOTE: no `cargo fmt --check` — this project does NOT use rustfmt.
          # The codebase is intentionally hand-formatted (aligned one-line
          # multi-field structs etc.) and does not match default rustfmt.
          # Running `cargo fmt` would reformat the entire codebase. Do not add
          # a fmt gate, and never run `cargo fmt --all` in any task.
          nix develop -c cargo clippy --all-features -- -D warnings
          nix develop -c shellcheck plugins/zj-radar-claude/scripts/notify.sh

  test:
    strategy:
      fail-fast: false
      matrix:
        os: [ubuntu-latest, macos-latest]
    runs-on: ${{ matrix.os }}
    env:
      CI: "1"
      INSTA_UPDATE: "no"
    steps:
      - uses: actions/checkout@v4
      - uses: cachix/install-nix-action@v30
        with:
          extra_nix_config: |
            experimental-features = nix-command flakes
      - uses: nix-community/cache-nix-action@v7
        with:
          primary-key: nix-${{ runner.os }}-${{ hashFiles('flake.lock', 'Cargo.lock') }}
          restore-prefixes-first-match: nix-${{ runner.os }}-
      - name: deterministic suite (L1-L4)
        run: nix develop -c cargo test --all-features
      - name: bash hook tests (L3)
        run: nix develop -c just test-bash

  wasm-build:
    runs-on: ubuntu-latest
    steps:
      - uses: actions/checkout@v4
      - uses: cachix/install-nix-action@v30
        with:
          extra_nix_config: |
            experimental-features = nix-command flakes
      - uses: nix-community/cache-nix-action@v7
        with:
          primary-key: nix-${{ runner.os }}-${{ hashFiles('flake.lock', 'Cargo.lock') }}
          restore-prefixes-first-match: nix-${{ runner.os }}-
      - name: nix flake check (wasm build + crane tests)
        run: nix flake check -L
```

- [ ] **Step 2: Create the nightly + release e2e workflow**

Create `.github/workflows/e2e.yml`:

```yaml
name: E2E

on:
  schedule:
    - cron: "0 7 * * *"   # nightly 07:00 UTC
  workflow_dispatch:
  push:
    tags: ["v*"]

jobs:
  e2e:
    strategy:
      fail-fast: false
      matrix:
        os: [ubuntu-latest, macos-latest]
    runs-on: ${{ matrix.os }}
    steps:
      - uses: actions/checkout@v4
      - uses: cachix/install-nix-action@v30
        with:
          extra_nix_config: |
            experimental-features = nix-command flakes
      - uses: nix-community/cache-nix-action@v7
        with:
          primary-key: nix-${{ runner.os }}-${{ hashFiles('flake.lock', 'Cargo.lock') }}
          restore-prefixes-first-match: nix-${{ runner.os }}-
      - name: live e2e (L5)
        run: nix develop -c just test-e2e
```

- [ ] **Step 3: Validate the workflow YAML locally**

Run: `nix develop -c cargo clippy --all-features -- -D warnings`
Expected: clean (fix any clippy issues surfaced by the new test code). Do NOT run `cargo fmt` — this project does not use rustfmt (no rustfmt.toml; the codebase is intentionally hand-formatted). If `nix develop` is unavailable in this environment, run the bare command.

- [ ] **Step 4: Run the full local CI gate**

Run: `just ci`
Expected: `just test` + `just test-bash` PASS.

- [ ] **Step 5: Commit**

```bash
git add .github/workflows/ci.yml .github/workflows/e2e.yml
git commit -m "ci: lint job, macOS+Linux test matrix, nightly/release e2e workflow"
```

---

## Self-Review Notes (for the executor)

- **Spec coverage:** L1 → Tasks 2-3; L2 → Tasks 4-7; L3 → Tasks 8-12; L4 → Tasks 13-14; L5 → Tasks 15-16; CI/infra → Tasks 1, 17. All five layers + the four failure classes (hook plumbing: 8-11; visual/terminal: 2,13,14,16; logic regressions: 3-7; event wiring: 12) are covered.
- **Adapt-to-reality steps:** Several tasks include a "read the real signature/behavior, then align the assertion" note. These are deliberate — assert the *correct* behavior, and when a test exposes a real bug, fix the production code minimally (TDD) rather than weakening the test.
- **Order matters:** Task 1 (tooling) must land first. Tasks 5→6 share `arb_rows`. Task 11 depends on Task 10's `helper.bash`. Task 12 must keep the wasm build green (Step 6). Tasks 15→16 share the harness.
- **Don't add API speculatively:** Task 13 only adds `ColorMode` if no suppression path exists (YAGNI).

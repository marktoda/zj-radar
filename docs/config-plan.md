# Config Surface Implementation Plan

> **⚠ Historical / completed-and-diverged (kept for context).** The config
> surface shipped, but without `stuck_secs` and the long-running `⚠` cue (both
> dropped in commit `d3c6b75`). The shipped knobs are `naming`, `header`,
> `glyphs`, `density`. See `src/config.rs` for the source of truth.

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax.

**Goal:** Replace zj-radar's hardcoded behavior constants with a typed, validated `Config` parsed once from the plugin's KDL config map, exposing `naming` (off/managed/force), `stuck_secs`, and `header`.

**Architecture:** A new pure `src/config.rs` (no `zellij-tile` dep) owns parsing (`Config::from_map`, never fails — defaults on bad input, ignores unknown keys). `render.rs` takes a `RenderOpts { stuck_secs, header }` instead of the `STUCK_SECS` const + always-on header. `lib.rs` parses config in `load()`, drives `apply_renames` by `NamingMode`, and passes `RenderOpts` to `render`. Folds the existing `force_rename` key into `naming`.

**Tech Stack:** Rust, `zellij-tile = "0.44"`, target `wasm32-wasip1`. Build via `cargo build --target wasm32-wasip1`; test via `cargo test` (host).

## Global Constraints

- Pure modules (`config`, `status`, `payload`, `state`, `model`, `render`, `naming`) must NOT import `zellij-tile`. Only `lib.rs` imports it.
- `Config::from_map` NEVER panics/errors: invalid value → that field's default; unknown keys ignored. Bools accept `true|1|yes|on` (case-insensitive); `naming` ∈ `off|managed|force` (case-insensitive); `stuck_secs` parses `u64`, else default.
- Defaults = today's behavior: `naming=Managed`, `stuck_secs=600`, `header=true`.
- `width` is NOT a config key (it's the layout `pane size=N`).
- `cargo test` output must be pristine (0 warnings). Host-test-dead wasm-only items keep the existing `#[cfg_attr(all(not(target_arch="wasm32"),not(test)), allow(dead_code))]` pattern.

## File Structure

- Create: `src/config.rs` — `Config`, `NamingMode`, `from_map` (pure).
- Modify: `src/render.rs` — add `RenderOpts`; thread `stuck_secs`+`header` through `detail_tag`/`header_lines`/`render`; drop `STUCK_SECS` const; update tests.
- Modify: `src/lib.rs` — `mod config;`; `State.config` (replaces `force_names`); parse in `load()`; `apply_renames` by `NamingMode`; pass `RenderOpts`; `header_lines(rows, header)`.
- Modify: `dev/dev.kdl` — `force_rename "true"` → `naming "force"`.

(`naming.rs` already has the `force: bool` param from commit `ab03b3b`; no change needed there.)

---

### Task 1: `config` module

**Files:**
- Create: `src/config.rs`
- Modify: `src/lib.rs` (add `mod config;` with the dead-code cfg_attr)

**Interfaces:**
- Produces: `config::NamingMode { Off, Managed, Force }` (derives `Default`=Managed, Clone/Copy/PartialEq/Eq/Debug); `config::Config { naming: NamingMode, stuck_secs: u64, header: bool }` with `Default` (Managed/600/true) and `pub fn from_map(cfg: &std::collections::BTreeMap<String,String>) -> Config`.

- [ ] **Step 1: Write `src/config.rs` with tests**

```rust
//! Plugin configuration parsed from the KDL `plugin { ... }` block. Pure — no
//! zellij-tile dependency. Parsing never fails: invalid values fall back to the
//! field default and unknown keys are ignored (forward-compatible).

use std::collections::BTreeMap;

#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub enum NamingMode {
    /// Never rename tabs.
    Off,
    /// Rename only default ("Tab #N") or our own prior names (clobber guard).
    #[default]
    Managed,
    /// Rename any tab, overriding user-chosen names.
    Force,
}

#[derive(Clone, PartialEq, Eq, Debug)]
pub struct Config {
    pub naming: NamingMode,
    pub stuck_secs: u64,
    pub header: bool,
}

impl Default for Config {
    fn default() -> Self {
        Config { naming: NamingMode::default(), stuck_secs: 600, header: true }
    }
}

fn parse_bool(v: &str) -> Option<bool> {
    match v.trim().to_ascii_lowercase().as_str() {
        "true" | "1" | "yes" | "on" => Some(true),
        "false" | "0" | "no" | "off" => Some(false),
        _ => None,
    }
}

impl Config {
    pub fn from_map(cfg: &BTreeMap<String, String>) -> Config {
        let d = Config::default();
        let naming = match cfg.get("naming").map(|s| s.trim().to_ascii_lowercase()) {
            Some(s) if s == "off" => NamingMode::Off,
            Some(s) if s == "managed" => NamingMode::Managed,
            Some(s) if s == "force" => NamingMode::Force,
            _ => d.naming,
        };
        let stuck_secs = cfg
            .get("stuck_secs")
            .and_then(|s| s.trim().parse::<u64>().ok())
            .unwrap_or(d.stuck_secs);
        let header = cfg.get("header").and_then(|s| parse_bool(s)).unwrap_or(d.header);
        Config { naming, stuck_secs, header }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn map(pairs: &[(&str, &str)]) -> BTreeMap<String, String> {
        pairs.iter().map(|(k, v)| (k.to_string(), v.to_string())).collect()
    }

    #[test]
    fn empty_map_is_defaults() {
        let c = Config::from_map(&map(&[]));
        assert_eq!(c, Config::default());
        assert_eq!(c.naming, NamingMode::Managed);
        assert_eq!(c.stuck_secs, 600);
        assert!(c.header);
    }

    #[test]
    fn parses_all_keys() {
        let c = Config::from_map(&map(&[("naming", "force"), ("stuck_secs", "120"), ("header", "false")]));
        assert_eq!(c.naming, NamingMode::Force);
        assert_eq!(c.stuck_secs, 120);
        assert!(!c.header);
    }

    #[test]
    fn naming_is_case_insensitive_and_falls_back() {
        assert_eq!(Config::from_map(&map(&[("naming", "OFF")])).naming, NamingMode::Off);
        assert_eq!(Config::from_map(&map(&[("naming", "Force")])).naming, NamingMode::Force);
        // unknown value → default
        assert_eq!(Config::from_map(&map(&[("naming", "wat")])).naming, NamingMode::Managed);
    }

    #[test]
    fn bool_accepts_several_spellings() {
        for t in ["true", "1", "yes", "on", "ON", "Yes"] {
            assert!(Config::from_map(&map(&[("header", t)])).header);
        }
        for f in ["false", "0", "no", "off"] {
            assert!(!Config::from_map(&map(&[("header", f)])).header);
        }
        // garbage → default (true)
        assert!(Config::from_map(&map(&[("header", "maybe")])).header);
    }

    #[test]
    fn stuck_secs_invalid_falls_back() {
        assert_eq!(Config::from_map(&map(&[("stuck_secs", "")])).stuck_secs, 600);
        assert_eq!(Config::from_map(&map(&[("stuck_secs", "abc")])).stuck_secs, 600);
        assert_eq!(Config::from_map(&map(&[("stuck_secs", "0")])).stuck_secs, 0);
    }

    #[test]
    fn unknown_keys_ignored() {
        let c = Config::from_map(&map(&[("totally_unknown", "x"), ("naming", "off")]));
        assert_eq!(c.naming, NamingMode::Off);
        assert_eq!(c.stuck_secs, 600);
    }
}
```

- [ ] **Step 2: Register the module in `src/lib.rs`**

Add alongside the other `mod` declarations (with the same cfg_attr the siblings use):
```rust
#[cfg_attr(all(not(target_arch = "wasm32"), not(test)), allow(dead_code))]
mod config;
```

- [ ] **Step 3: Run tests**

Run: `cargo test config`
Expected: 6 tests pass, 0 warnings.

- [ ] **Step 4: Commit**

```bash
git add src/config.rs src/lib.rs
git commit --no-gpg-sign -m "feat: typed Config parsed from plugin KDL map (naming/stuck_secs/header)"
```

---

### Task 2: thread `RenderOpts` through `render.rs`

**Files:**
- Modify: `src/render.rs`

**Interfaces:**
- Produces: `render::RenderOpts { pub stuck_secs: u64, pub header: bool }` (Clone/Copy, `Default` = 600/true); `render(rows, width, now_tick, opts: RenderOpts)`; `header_lines(rows, header: bool) -> usize`. Removes the `STUCK_SECS` const.

- [ ] **Step 1: Add `RenderOpts` + drop the const**

Replace the `STUCK_SECS` const (lines ~10-12) with:
```rust
/// Render-time options sourced from the plugin config (`config::Config`).
#[derive(Clone, Copy)]
pub struct RenderOpts {
    /// A `running` agent whose elapsed reaches this (secs ≈ ticks) is flagged
    /// long-running / possibly stuck.
    pub stuck_secs: u64,
    /// Whether to render the roll-up summary header line.
    pub header: bool,
}

impl Default for RenderOpts {
    fn default() -> Self {
        RenderOpts { stuck_secs: 600, header: true }
    }
}
```

- [ ] **Step 2: `detail_tag` takes `stuck_secs`**

Change signature + the comparison:
```rust
fn detail_tag(agg: &TabAgg, now_tick: u64, stuck_secs: u64) -> String {
    let Some(d) = &agg.detail else { return String::new() };
    let elapsed = now_tick.saturating_sub(d.since_tick);
    match d.status {
        Status::Done => format!("done {}", format_elapsed(elapsed)),
        Status::Running => {
            let e = format_elapsed(elapsed);
            if elapsed >= stuck_secs {
                format!("{} ⚠", e)
            } else {
                e
            }
        }
        Status::Pending => "needs you".to_string(),
        Status::Error => "error".to_string(),
        Status::Idle => String::new(),
    }
}
```

- [ ] **Step 3: `header_lines` takes `header`**

```rust
/// 1 if a summary header will be rendered, else 0. Single source of truth for
/// the header's vertical span (consumed by click mapping in lib.rs).
pub fn header_lines(rows: &[TabRow], header: bool) -> usize {
    if !header || summary(rows).is_empty() {
        0
    } else {
        1
    }
}
```

- [ ] **Step 4: `render` takes `opts`**

Change signature to `pub fn render(rows: &[TabRow], width: usize, now_tick: u64, opts: RenderOpts) -> String`. Guard the header block with `opts.header &&`:
```rust
    let sum = summary(rows);
    if opts.header && !sum.is_empty() {
        // ... existing header-building block unchanged ...
    }
```
And pass `opts.stuck_secs` to the `detail_tag` call:
```rust
            let tag = detail_tag(&row.agg, now_tick, opts.stuck_secs);
```

- [ ] **Step 5: Update existing tests to the new signatures**

In `src/render.rs` tests: replace every `render(&rows, W, T)` call with `render(&rows, W, T, RenderOpts::default())`, and every `header_lines(&rows)` with `header_lines(&rows, true)`. (Sites: `plain_tab_renders_name_only_no_second_line`, `agent_tab_has_three_lines_with_count_tag_and_msg`, `agent_tab_with_empty_msg_has_two_lines`, `narrow_width_truncates_with_ellipsis`, `no_emitted_line_exceeds_width`, `running_under_threshold_has_no_warning`, `running_at_threshold_shows_warning`, `done_with_long_elapsed_has_no_warning`, `bell_renders_marker`, `no_bell_no_marker`, `summary_empty_when_all_idle`, `header_line_emitted_when_active`.)

- [ ] **Step 6: Add tests for the new knobs**

Append to `src/render.rs` tests:
```rust
    #[test]
    fn header_disabled_suppresses_header_line() {
        let rows = vec![TabRow { number: 1, name: "a".into(), active: false, has_bell: false, agg: agg(Status::Running, 0, 0, None) }];
        assert_eq!(header_lines(&rows, false), 0);
        let s = render(&rows, 24, 0, RenderOpts { stuck_secs: 600, header: false });
        // only the tab row, no summary header line
        assert_eq!(s.matches('\n').count(), 1);
    }

    #[test]
    fn custom_stuck_secs_thresholds_the_warning() {
        let detail = Detail { repo: "r".into(), branch: "b".into(), msg: "".into(), since_tick: 0, status: Status::Running };
        let rows = vec![TabRow { number: 1, name: "t".into(), active: false, has_bell: false, agg: agg(Status::Running, 1, 1, Some(detail)) }];
        let opts = RenderOpts { stuck_secs: 100, header: true };
        assert!(!render(&rows, 30, 99, opts).contains('⚠'));
        assert!(render(&rows, 30, 100, opts).contains('⚠'));
    }
```

- [ ] **Step 7: Run tests**

Run: `cargo test render`
Expected: all render tests pass, 0 warnings.

- [ ] **Step 8: Commit**

```bash
git add src/render.rs
git commit --no-gpg-sign -m "refactor: render takes RenderOpts (stuck_secs, header) instead of a const"
```

---

### Task 3: wire `Config` into `lib.rs` + migrate `dev.kdl`

**Files:**
- Modify: `src/lib.rs`
- Modify: `dev/dev.kdl`

**Interfaces:**
- Consumes: `config::Config`/`NamingMode`, `render::RenderOpts`, `render::header_lines(rows, header)`, `naming::compute_renames(.., force)`.

- [ ] **Step 1: Replace `force_names` with `config` on `State`**

In the `State` struct, replace the `force_names: bool` field (added in `ab03b3b`) with:
```rust
    #[cfg_attr(not(target_arch = "wasm32"), allow(dead_code))]
    config: config::Config,
```
(`Config` derives nothing for `Default` via its manual impl — `State` derives `Default`, which uses `Config::default()`. Confirmed: `Config` has a `Default` impl.)

- [ ] **Step 2: Parse config in `load()`**

Replace the `force_rename` parsing block added in `ab03b3b` with:
```rust
    fn load(&mut self, config: BTreeMap<String, String>) {
        self.config = config::Config::from_map(&config);
        request_permission(&[
```
(rest of `load` unchanged.)

- [ ] **Step 3: Drive `apply_renames` by `NamingMode`**

```rust
    fn apply_renames(&mut self) {
        if self.config.naming == config::NamingMode::Off {
            return;
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
        );
        for (pos, name) in changes {
            rename_tab(pos as u32 + 1, &name);
            self.applied_names.insert(pos, name);
        }
    }
```

- [ ] **Step 4: Pass `header` flag to `header_lines` + `RenderOpts` to `render`**

In `tab_position_at_line`, change `let mut cursor = render::header_lines(&rows);` to:
```rust
        let mut cursor = render::header_lines(&rows, self.config.header);
```
In the wasm `render` method, build opts from config:
```rust
    fn render(&mut self, _rows: usize, cols: usize) {
        let rows = self.build_rows();
        let opts = render::RenderOpts {
            stuck_secs: self.config.stuck_secs,
            header: self.config.header,
        };
        print!("{}", render::render(&rows, cols.max(1), self.tick, opts));
    }
```

- [ ] **Step 5: Migrate `dev/dev.kdl`**

Change the plugin config block from `force_rename "true"` to `naming "force"`:
```kdl
                plugin location="file:/Users/mark.toda/dev/zj-radar/target/wasm32-wasip1/debug/zj_radar.wasm" {
                    // Opt-in: rename tabs even when they have an explicit name.
                    naming "force"
                }
```

- [ ] **Step 6: Add a host test that `NamingMode::Off` yields no renames**

`tab_position_at_line`/`apply_renames` are wasm-gated, but `compute_renames` (pure) is the logic. Add to `src/config.rs` tests is wrong scope; instead verify via `naming` semantics already covered. For lib-level confidence, add to `src/lib.rs` a `#[cfg(test)]` check that a `State` with `config.naming == Off` is constructible and the gate is correct — but since `apply_renames` is wasm-only, assert the decision helper purely. Simplest: confirm `config::NamingMode::Off` maps to "no force, skip" by asserting `compute_renames(.., force=false)` on a default-named tab still renames (managed) — already covered by `naming::tests`. **No new lib test required**; rely on Task 1 config tests + existing naming tests. (This step is a no-op checkpoint — do not invent a test that asserts nothing.)

- [ ] **Step 7: Run full host tests**

Run: `cargo test`
Expected: all pass (Task 1 config tests + render + naming + lib), 0 warnings.

- [ ] **Step 8: Wasm build check**

Run: `cargo build --target wasm32-wasip1`
Expected: compiles, 0 warnings, produces `target/wasm32-wasip1/debug/zj_radar.wasm`.

- [ ] **Step 9: Commit**

```bash
git add src/lib.rs dev/dev.kdl
git commit --no-gpg-sign -m "feat: wire Config into the plugin (naming mode, stuck_secs, header); migrate dev.kdl"
```

---

## Self-Review

**Spec coverage:** `Config`/`NamingMode`/`from_map` + parse rules → Task 1. `RenderOpts` (stuck_secs, header) threading → Task 2. State.config + load parse + apply_renames by NamingMode + render opts + header_lines flag → Task 3. `force_rename`→`naming` migration (incl. dev.kdl) → Task 3 Steps 1/2/5. Defaults=today's behavior → Task 1 Config::default + Task 2 RenderOpts::default. Width-not-a-key → no task creates one (correct). All spec sections covered.

**Placeholder scan:** No TBD/TODO. Task 3 Step 6 is explicitly a no-op checkpoint with reasoning (avoids a vacuous test), not a placeholder.

**Type consistency:** `RenderOpts { stuck_secs, header }` identical in Tasks 2/3. `header_lines(rows, header: bool)` consumed in Task 3 Step 4 as defined in Task 2 Step 3. `Config { naming, stuck_secs, header }` + `NamingMode::{Off,Managed,Force}` consistent across Tasks 1/3. `compute_renames(.., force: bool)` matches the existing `ab03b3b` signature. `config::Config::from_map(&BTreeMap)` consistent.

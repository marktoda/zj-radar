# zj-radar config surface (behavior knobs)

**Status:** design / approved for spec-review
**Date:** 2026-06-26

## Goal

Replace zj-radar's hardcoded behavior constants with a small, typed, validated
config surface read from the plugin's KDL config block — so behavior is tunable
without recompiling, and the designer's eventual visual knobs have a clean home
to slot into. **This pass covers BEHAVIOR knobs only; visual knobs
(colors/glyphs/row-density) are deferred until the design pass lands.**

## How config arrives (Zellij mechanics)

A Zellij plugin is configured by the `plugin location="…" { key "value"; … }`
block that launches it; those key/values arrive in `load(config:
BTreeMap<String,String>)` as **strings**. There is no separate config file. So
the whole surface is: parse that `BTreeMap` once in `load()`.

## Module: `src/config.rs` (new, pure — no `zellij-tile` dep)

```rust
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub enum NamingMode {
    Off,
    #[default]
    Managed,
    Force,
}

#[derive(Clone, PartialEq, Eq, Debug)]
pub struct Config {
    pub naming: NamingMode,   // key "naming":     off | managed | force
    pub stuck_secs: u64,      // key "stuck_secs": integer seconds
    pub header: bool,         // key "header":     true | false
}

impl Default for Config { /* naming=Managed, stuck_secs=600, header=true */ }

impl Config {
    /// Parse the plugin's KDL config map. NEVER fails: unknown keys are
    /// ignored (forward-compatible), and any unparseable/invalid value falls
    /// back to that field's default.
    pub fn from_map(cfg: &std::collections::BTreeMap<String, String>) -> Config;
}
```

**Parsing rules (the only place raw strings are touched):**
- Bools (`header`): `true|1|yes|on` → true; `false|0|no|off` → false; anything
  else → default. Case-insensitive.
- Enum (`naming`): `off|managed|force`, case-insensitive; anything else →
  default (`Managed`).
- Int (`stuck_secs`): parse `u64`; non-numeric or empty → default (`600`).
  (Clamp not needed — any u64 is valid; `0` means "always show the cue".)
- Unknown keys: ignored.

Defaults equal **today's behavior**, so a plugin block with no config changes
nothing: `naming=Managed`, `stuck_secs=600`, `header=true`.

## Integration (thin; the typed values flow outward)

- `State` holds a `config::Config` field, replacing the `force_names: bool` added
  in commit `ab03b3b`. `load()` does `self.config = Config::from_map(&config)`.
- **Naming** (`lib.rs::apply_renames`):
  - `NamingMode::Off` → return early (no renames at all).
  - `Managed` / `Force` → `naming::compute_renames(…, force = self.config.naming
    == NamingMode::Force)`. (This folds the standalone `force_rename` key into
    `naming`.)
  - **Migration:** the `force_rename` key (added in `ab03b3b`) is REMOVED in
    favor of `naming`. Update `dev/dev.kdl`'s `force_rename "true"` → `naming
    "force"`. (No back-compat alias needed — single user, not yet released.)
- **Render** (`render.rs`): introduce `pub struct RenderOpts { pub stuck_secs:
  u64, pub header: bool }` and change `render(rows, width, tick)` →
  `render(rows, width, tick, opts)`. The `STUCK_SECS` const becomes
  `opts.stuck_secs`; the summary header is emitted only when `opts.header`.
  `header_lines()` likewise takes/returns based on `opts.header` so
  `tab_position_at_line`'s offset stays in sync (single source of truth
  preserved).
- `lib.rs` builds `RenderOpts` from `self.config` and passes it to `render` and
  `header_lines`.

## Not in scope / explicitly out

- **Width is NOT a config key.** It's the layout `pane size=N`; the plugin can't
  resize its own pane, it only adapts rendering to the width it's given. Document
  this; do not add a `width` key.
- **Visual knobs** (color roles, glyph set, row density / 2-vs-3-line, header
  format) — deferred until the design pass; `Config` is structured so adding
  them later is a field + a parse line, no architectural change.
- Desktop notifications stay in the shell adapters, unaffected.

## Testing

- `config.rs` pure unit tests: each key parsed (valid, invalid→default,
  missing→default, case-insensitivity, unknown-key-ignored), and `Config::default`
  equals today's constants.
- `render.rs` tests updated to pass `RenderOpts` (cover `header=false` → no header
  line + `header_lines()==0`, and a custom `stuck_secs`).
- `naming` tests already cover `force` true/false; add a `lib`-level check that
  `NamingMode::Off` produces zero renames.

## Implementation note (coordination)

`src/config.rs` is brand-new (no collision). But threading it through
`render.rs` and `naming.rs` edits **agent-B's files** — so implement on a
branch/worktree and merge in (the pattern used for the rename), rather than
editing `main` directly while B is active.

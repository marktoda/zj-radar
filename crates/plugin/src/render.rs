//! Pure renderer: per-tab rows → ANSI string. No zellij-tile dependency.
//!
//! Reading order — the file is five stages, top to bottom:
//! 1. **Primitives** — `Seg` (colored ⇒ RESET-terminated), `LineBg`, `Line`
//!    (the lockstep unit: one text line + its click target + surface class),
//!    and `RenderedRail::from_lines`, the single derive point where ansi,
//!    targets, and footprint all come from one `Vec<Line>`.
//! 2. **Row emission** — `render_row` and the pane/detail line emitters, each
//!    producing `Line`s through `prefixed_line`'s narrow-width guard.
//! 3. **Layout planning** — delegated to `layout.rs` (pure, no ANSI, no
//!    targets): overflow folding, card spacing, per-row line budgets.
//! 4. **Assembly & paint** — body assembly (header, cross-session badge,
//!    cards), `paint_card_line`/`LineBg` tinting.
//! 5. **Bottom region** — ledger lines, footer (rule + tally + hint), filler.

use crate::config::Density;
use crate::rollup::{LedgerLine, Outcome, PaneDisplay, TabDisplay, TabRow};
use crate::sessions::BadgeEntry;
pub use crate::status::GlyphSet;
use crate::status::{Role, Status};
use crate::theme::{DerivedColors, Rgb};
use unicode_width::{UnicodeWidthChar, UnicodeWidthStr};

mod layout;
// Layout planning (overflow folding, card spacing, multi-pane expansion) is
// implementation *behind* the rail seam — only `render.rs` drives it. Import it
// privately here rather than re-exporting crate-wide so the planning
// intermediates (`RowMeta`, `plan_layout`, …) can't leak into new callers.
use layout::*;

const RESET: &str = "\x1b[0m";
const BOLD: &str = "\x1b[1m";

/// Ticks a Running pane/tab must sit at the SAME `since_tick` before its
/// spinner eases from full speed to a slow two-frame blink (see
/// `spin_glyph`). 600 ticks — the tick-driven render clock isn't wall-clock
/// 1:1, but at typical UI tick rates this reads as "on the order of minutes".
/// Honest only because `since_tick` is honest: `StatusStore::apply` preserves
/// `last_change_tick` across a same-status re-broadcast
/// (`status_store.rs::apply_sets_last_change_tick_only_on_status_change`) and
/// `CommandStore` preserves it across a same-command re-promotion
/// (`promotion_preserves_running_since_for_same_command`,
/// `crates/core/src/command.rs`) — so a long-runner's `since_tick` reflects
/// when the work truly started, not the last time it happened to re-announce
/// itself.
pub const EASE_AFTER_TICKS: u64 = 600;

/// Most pane lines a multi-pane tab renders before folding the remainder into
/// a single `+N more` line — the ⟦D6⟧ cap (rail-reference.md: high on
/// purpose, so the common case never folds).
const MAX_PANE_LINES: usize = 6;

/// Columns of the shared tree prefix every child line starts with:
/// spine-or-space (1) + connector `├`/`└` (1) + separating space (1) —
/// exactly what [`child_prefix`] emits. Holding this fixed keeps the glyph
/// aligned across all child lines (see `child_prefix`'s doc).
const TREE_PREFIX_COLS: usize = 3;

/// Spinner frame for a Running glyph: full speed normally; after
/// EASE_AFTER_TICKS a slow two-frame blink (advances every 4th tick) — a
/// long-runner signals "still going, nothing new" instead of anxiety.
fn spin_glyph(now_tick: u64, since_tick: u64) -> char {
    if now_tick.saturating_sub(since_tick) > EASE_AFTER_TICKS {
        crate::status::working_spin(((now_tick / 4) % 2) as usize)
    } else {
        crate::status::working_spin(now_tick as usize)
    }
}

/// A colored (optionally bold) text run that terminates its own SGR with RESET.
///
/// Every colored token in the rail is built through `Seg`, so "colored ⇒
/// RESET-terminated" is structural rather than reviewer-enforced —
/// `paint_card_line` re-arms the card background after each RESET, and a run that
/// forgot its RESET would bleed the foreground color across the rest of the band.
/// `Display` emits `{color}{BOLD?}{text}{RESET}`; a run groups *all* text that
/// shares one color (e.g. line 1's glyph+number+name) so the byte stream matches
/// the hand-built `format!`s it replaces.
struct Seg<'a> {
    color: &'a str,
    bold: bool,
    text: std::borrow::Cow<'a, str>,
}

impl<'a> Seg<'a> {
    fn new(color: &'a str, text: impl Into<std::borrow::Cow<'a, str>>) -> Self {
        Self { color, bold: false, text: text.into() }
    }

    fn bold(color: &'a str, text: impl Into<std::borrow::Cow<'a, str>>) -> Self {
        Self { color, bold: true, text: text.into() }
    }
}

impl std::fmt::Display for Seg<'_> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.color)?;
        if self.bold {
            f.write_str(BOLD)?;
        }
        f.write_str(&self.text)?;
        f.write_str(RESET)
    }
}

#[derive(Clone)]
pub struct RenderOpts {
    pub width: usize,
    pub height: usize,
    pub now_tick: u64,
    pub glyphs: GlyphSet,
    /// Whether to render the " RADAR" identity header block.
    pub header: bool,
    /// Vertical density between tabs.
    pub density: Density,
    /// Terminal-derived colors for card surfaces and readable dim text. These
    /// are the only truecolor values (status hues are ANSI-16). Defaults to a
    /// neutral-dark fallback until the terminal reports its bg/fg via PaneInfo.
    pub theme: DerivedColors,
    /// Wall-clock epoch seconds "now", for ledger age formatting
    /// (`ledger::format_age`). Distinct from `now_tick` (the render/animation
    /// clock) — ages are wall-clock, not tick-relative.
    pub now_epoch_s: u64,
    /// Whether the footer may advertise the `alt-[n] jump` chord. Zellij owns
    /// keybinds, not the plugin, so this is config-driven honesty (the
    /// `GrantHint` pattern) — and strictly opt-in: NO in-tree config sets it.
    /// Even the `run`-owned config, which bakes the Alt-1..9 → GoToTab binds,
    /// deliberately omits it (`run.rs` pins that): Alt+digit is commonly
    /// claimed upstream of Zellij (WM hotkeys, macOS Option typing `¡`) and
    /// the rail can't detect interception. Only a config whose environment
    /// truly delivers the chord may claim it; everything else omits the hint
    /// line rather than promising a chord that does nothing.
    pub jump_hint: bool,
    /// The cross-session badge, current-first (`Sessions::badge()`'s
    /// contract) — rendered by [`render_session_badge`] as zero lines
    /// whenever `len() <= 1` (only this session, or no peer presence has
    /// crossed the shared `/cache` root yet), so every caller that never
    /// populates this (every
    /// pre-existing test, and any host that hasn't wired Task 5/6's session
    /// plumbing) renders byte-identical to before this field existed.
    pub badge: Vec<BadgeEntry>,
}

/// Presentation for the roll-up's `Outcome` tag. The enum itself lives in
/// `rollup` (pure semantics); these methods encode the glyphs and the
/// width-driven roomy/tight forms, which are the renderer's concern.
impl Outcome {
    /// The roomy form. `Ok` is EMPTY — the line-1 status glyph (green ●) is the
    /// one done signal; a tag would double-mark it. Errors carry the info the
    /// line-1 `✗` can't: the exit code.
    fn full(self) -> String {
        match self {
            Outcome::Ok => String::new(),
            Outcome::Failed(Some(code)) => format!("exit {}", code),
            Outcome::Failed(None) => "✗".to_string(),
        }
    }

    /// Whether this outcome renders a visible tag at all — `Ok`'s tag is
    /// empty by design (the line-1 status glyph is the one done signal), so
    /// only failures do. Lets callers ask "is there a tag?" without building
    /// the `full()` string just to test it for emptiness.
    fn renders_tag(self) -> bool {
        !matches!(self, Outcome::Ok)
    }

    /// The irreducible short form, shown when width is too tight for `full`.
    fn minimal(self) -> &'static str {
        match self {
            Outcome::Ok => "",
            Outcome::Failed(_) => "✗",
        }
    }

    /// The hue the tag reads in: success (green) / error (red).
    fn role(self) -> Role {
        match self {
            Outcome::Ok => Role::Success,
            Outcome::Failed(_) => Role::Error,
        }
    }
}

fn truncate(s: &str, max: usize) -> String {
    if UnicodeWidthStr::width(s) <= max {
        s.to_string()
    } else if max == 0 {
        String::new()
    } else {
        // Reserve 1 column for '…' (which itself has display width 1).
        let budget = max.saturating_sub(1);
        let mut kept = String::new();
        let mut used = 0usize;
        for c in s.chars() {
            let w = UnicodeWidthChar::width(c).unwrap_or(0);
            if used + w > budget {
                break;
            }
            kept.push(c);
            used += w;
        }
        // A cut mid-cluster can strand a trailing zero-width joiner, which would
        // try to fuse with the following '…' into mojibake. Drop trailing ZWJs so
        // the ellipsis stands alone. (Combining marks on the last kept base char
        // are harmless and left in place.)
        while kept.ends_with('\u{200d}') {
            kept.pop();
        }
        format!("{}…", kept)
    }
}

/// A click target. `session: None` (the vast majority — tabs and panes in
/// THIS session) behaves exactly as before this field existed: `pane_id`
/// present ⇒ `ShowPane`, else `SwitchTab { position: tab_position }` (see
/// `mouse_click`). `session: Some(name)` marks a cross-session badge line
/// (peer, never the current session — see `render_session_badge`) and routes
/// to `Effect::SwitchSession` instead; its `tab_position` is then read
/// through `session_tab_position`, not directly, because a session target
/// needs to carry "no attention tab" (`Option::None`) losslessly through a
/// field that stays a plain `usize` for every other target — see that
/// method's doc. `session: Option<String>` (owned: a peer's name, read off
/// its `Presence`, isn't a value this module can borrow from) is why
/// the struct can no longer derive `Copy`; every call site that used to rely
/// on implicit-copy semantics now clones explicitly at its last use.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RailTarget {
    pub tab_position: usize,
    pub pane_id: Option<u32>,
    pub session: Option<String>,
}

impl RailTarget {
    /// Sentinel `tab_position` for a session target (`session: Some(_)`)
    /// whose peer has nothing needing attention. Never a legitimate tab
    /// index in practice (`RadarTab` positions come from real tab counts,
    /// nowhere near `usize::MAX`), so it can share the plain `usize` field
    /// every other target already uses instead of growing a 4th field just
    /// for this one case.
    const NO_ATTENTION: usize = usize::MAX;

    /// Build a peer-session click target. `tab_position` is baked in NOW, at
    /// line-build time, from the badge entry's `attention_tab_position` — by
    /// the time a click lands the badge may have moved on, so the target
    /// must carry everything `mouse_click` needs rather than re-deriving it.
    /// Encodes `None` (no attention tab) as [`Self::NO_ATTENTION`] rather
    /// than `unwrap_or(0)`: the latter would collide with a peer whose
    /// attention IS at tab 0, and `mouse_click` would then force-focus that
    /// tab for a peer that has nothing to focus — a silent behavior bug the
    /// plain-`usize` field shape could otherwise hide. See
    /// [`Self::session_tab_position`] for the inverse.
    fn for_session(name: String, attention_tab_position: Option<usize>) -> Self {
        RailTarget {
            tab_position: attention_tab_position.unwrap_or(Self::NO_ATTENTION),
            pane_id: None,
            session: Some(name),
        }
    }

    /// The inverse of [`Self::for_session`]'s sentinel encoding —
    /// `mouse_click` calls this once it has confirmed `session.is_some()`,
    /// to get the `Option<usize>` `Effect::SwitchSession` actually needs.
    /// `None` ⇒ switch session, keep its own last-focused tab (`lib.rs`'s
    /// plain `switch_session`); `Some(pos)` ⇒ switch AND focus
    /// (`switch_session_with_focus`) — mirrors `Sessions::tick`'s
    /// idle-commit `CommitTarget.attention_tab_position` exactly, so
    /// Alt+[/] cycling and a badge click land identically for the same
    /// peer state. Meaningless (and never called) on a target whose
    /// `session` is `None` — those read `tab_position` directly.
    pub(crate) fn session_tab_position(&self) -> Option<usize> {
        (self.tab_position != Self::NO_ATTENTION).then_some(self.tab_position)
    }
}

/// Surface class a line sits on (Cards density only); resolved to a concrete
/// bg escape during assembly using the owning row. `None` = never painted.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum LineBg {
    None,
    Rail,        // dark panel base (rail_bg): header, gaps, idle strip
    Card,        // this row's card surface (card_tint of the owning row)
    ActiveChild, // active multi-pane child line (surface_agent)
}

impl LineBg {
    /// The one home for the surface-class → bg-escape map. `render_rail` resolves
    /// every painted line through here, so the `ActiveChild` vs `Card` split (the
    /// drift `cards_active_more_line_*` guards) lives in a single place. `row` is
    /// the card's owning row (only `Card` consults it); `rail` is the precomputed
    /// panel-base escape. `None` means the line is never painted.
    fn escape(self, row: &TabRow, theme: &DerivedColors, rail: &str) -> Option<String> {
        match self {
            LineBg::None => Option::None,
            LineBg::Rail => Some(rail.to_string()),
            LineBg::Card => Some(card_tint(row, theme)),
            LineBg::ActiveChild => Some(tc_bg(theme.surface_agent)),
        }
    }
}

/// One physical rail line and the click target it resolves to. `text` always
/// ends in exactly one '\n'. The unit of rendering: ansi, targets, and
/// footprint all derive from a `Vec<Line>`, so they cannot drift.
/// Construct via `Line::new` only — the struct literal appears nowhere else.
#[derive(Clone, Debug)]
struct Line {
    text: String,
    target: Option<RailTarget>,
    bg: LineBg,
}

impl Line {
    /// The ONLY way to build a `Line`: owns the trailing-newline invariant
    /// (exactly one `\n`, at the end) that keeps ansi/target lockstep — an
    /// interior newline would render as two physical rows against one click
    /// target. Intake sanitize should make an interior newline unreachable;
    /// the debug_assert flags the offending caller in tests, and the space
    /// replacement keeps lockstep unbreakable in the release wasm (where
    /// debug_assert compiles out) even if a future string source skips
    /// sanitize.
    fn new(text: String, target: Option<RailTarget>, bg: LineBg) -> Self {
        debug_assert!(
            !text.trim_end_matches('\n').contains('\n'),
            "interior newline breaks lockstep: {text:?}"
        );
        let mut text = text;
        while text.ends_with('\n') { text.pop(); }
        if text.contains('\n') {
            text = text.replace('\n', " ");
        }
        text.push('\n');
        Line { text, target, bg }
    }

    /// Repaint the text onto a surface band (Cards), re-normalizing through
    /// [`Line::new`] — the paint paths must not assign `text` raw, or the
    /// constructor's newline invariant becomes discipline-held again.
    fn painted(self, width: usize, bg: &str) -> Line {
        Line::new(paint_card_line(&self.text, width, bg), self.target, self.bg)
    }
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct RenderedRail {
    pub ansi: String,
    targets: Vec<Option<RailTarget>>,
}

impl RenderedRail {
    #[cfg_attr(all(target_arch = "wasm32", not(test)), allow(dead_code))]
    pub fn empty() -> Self {
        Self::default()
    }

    /// The single derive point: `ansi` and `targets` come from one `Vec<Line>`,
    /// so they are always in 1:1 correspondence. `text` is already final
    /// (painted during assembly); `bg` is ignored here. The trailing newline of
    /// the last line is popped to prevent vt100 scroll in the test harness.
    fn from_lines(lines: Vec<Line>) -> RenderedRail {
        let mut ansi = String::new();
        let mut targets = Vec::with_capacity(lines.len());
        for line in lines {
            ansi.push_str(&line.text);
            targets.push(line.target);
        }
        if ansi.ends_with('\n') {
            ansi.pop();
        }
        RenderedRail { ansi, targets }
    }

    /// Build a targetless panel face from raw ANSI, clamped to `height` lines
    /// and with the final newline popped — the same discipline `from_lines`
    /// applies to the rail. Without the clamp+pop a face taller than the pane
    /// scrolls its header (" RADAR", the permission warning) off the top.
    fn from_ansi_without_targets(ansi: &str, height: usize) -> Self {
        let mut clamped = String::new();
        let mut targets = Vec::new();
        for line in ansi.split_inclusive('\n').take(height) {
            clamped.push_str(line);
            targets.push(None);
        }
        if clamped.ends_with('\n') {
            clamped.pop();
        }
        RenderedRail { ansi: clamped, targets }
    }

    pub fn target_at_line(&self, line: isize) -> Option<RailTarget> {
        if line < 0 {
            return None;
        }
        self.targets.get(line as usize).cloned().flatten()
    }

    #[cfg_attr(all(target_arch = "wasm32", not(test)), allow(dead_code))]
    pub fn line_count(&self) -> usize {
        self.targets.len()
    }
}

/// Color of the active-tab spine (col-0 `▌`), by the tab's dominant status:
/// accent (mauve) normally, attention (peach) when the tab is waiting/error so
/// the focus cue carries the alarm. Single source for all three spine sites
/// (line-1 header, `+N more`, active pane lines).
fn spine_role(status: Status) -> Role {
    match status {
        Status::Pending | Status::Error => Role::Attention,
        _ => Role::Accent,
    }
}

/// The rendered col-0 spine cell: status-hued `▌` on the active tab, a plain
/// space otherwise. The one call path from `spine_role` to emitted ANSI — the
/// three colored spine sites (line-1 header, pane detail line, child prefix)
/// all route through here, so the color rule can't be applied at one site and
/// forgotten at another. (The SGR-free width closures keep their own literal
/// `if active { "▌" } else { " " }` — they measure, they don't color.)
fn spine_seg(active: bool, tab_status: Status) -> String {
    if active {
        Seg::new(spine_role(tab_status).ansi(), "▌").to_string()
    } else {
        " ".to_string()
    }
}

/// A tab is "multi-pane" for tree purposes when it has more than one tracked pane.
/// Single-pane tabs keep the chunk-1 line-2 behavior; multi-pane tabs use
/// the line-per-pane design.
fn is_multi_pane(display: &TabDisplay) -> bool {
    display.panes.iter().filter(|p| p.is_tracked()).count() > 1
}

/// The rail's identity header. Single source of truth for the header's vertical
/// span. Headerless only when there's truly nothing to show — no rows AND no
/// ledger history (`has_content` is false) — or when `header` is false, which
/// suppresses the identity block so rows start at line 0.
///
/// In Cards density the carded hero is just the " RADAR …" title (1 line) — the
/// `═` rule is dropped so cards begin immediately under the title. Compact and
/// Comfortable keep the two-line title+rule header. `render_rail()` uses the
/// same emitted header lines for ANSI and targets, so the count stays in lockstep.
fn header_lines(header: bool, density: Density, has_content: bool) -> usize {
    if !has_content || !header {
        0
    } else if density == Density::Cards {
        1
    } else {
        2
    }
}

/// Push one full-width, click-inert line (`role`-colored, clamped to `w`) into a
/// targetless panel face. The truncation acts on the visible text, never the SGR
/// codes (color is purely additive), so the panel honors the same width
/// discipline as the rail. Shared by `onboarding` and `needs_permission`.
fn push_panel_line(out: &mut String, role: &str, text: &str, w: usize) {
    out.push_str(&format!("{}\n", Seg::new(role, truncate(text, w))));
}

/// The rail's resting face when zero tabs are tracked (cold start, or every
/// tab has closed with no completion history — see `PluginRuntime::render`'s
/// routing). Not a permission interceptor. Deliberately minimal per spec §7/
/// rail-reference.md rule 8 ("not a marketing screen"): title + rule + one
/// muted status line, no status-glyph legend and no click hint (the panel is
/// click-inert — there is nothing here to jump to).
pub fn onboarding(opts: &RenderOpts) -> RenderedRail {
    let w = opts.width;
    let mut out = String::new();
    let accent = Role::Accent.ansi();
    let muted = Role::Muted.ansi();
    push_panel_line(&mut out, accent, " RADAR", w);
    push_panel_line(&mut out, accent, &"═".repeat(w), w);
    out.push('\n');
    push_panel_line(&mut out, muted, " scanning… no agents yet", w);
    RenderedRail::from_ansi_without_targets(&out, opts.height)
}

/// Rail face shown when permission has NOT been granted. Distinct from
/// `onboarding` (which is the granted-but-idle face) so a blocked install is
/// never mistaken for a working one. Points at the `Ctrl-y` keybind (baked into
/// the owned `config.kdl`) rather than "press y here": the borderless rail can't
/// host Zellij's prompt legibly (Zellij #4749), and on an attached session no
/// onboarding float was ever opened — `Ctrl-y` summons that legible float from
/// any session state.
pub fn needs_permission(opts: &RenderOpts, grant_hint: crate::config::GrantHint) -> RenderedRail {
    let w = opts.width;
    let mut out = String::new();
    let accent = Role::Accent.ansi();
    let needs = Role::Attention.ansi(); // Attention (bright orange/red) for the warning line
    let muted = Role::Muted.ansi();
    push_panel_line(&mut out, accent, " RADAR", w);
    push_panel_line(&mut out, accent, &"═".repeat(w), w);
    push_panel_line(&mut out, needs, " ⚠ needs permission", w);
    out.push('\n');
    // The escape-hatch hint must only promise what this install actually
    // bound: run-owned configs bake the Ctrl-y float keybind; a setup-injected
    // rail has no such bind, so it gets the universally true wording (Zellij's
    // own prompt is bound to a rail pane — focus it and answer).
    let hint: [&str; 3] = match grant_hint {
        crate::config::GrantHint::CtrlY => [" press Ctrl-y to", " open the grant", " prompt."],
        crate::config::GrantHint::Generic => [" focus this pane;", " press y when the", " prompt appears."],
    };
    for line in hint {
        push_panel_line(&mut out, muted, line, w);
    }
    RenderedRail::from_ansi_without_targets(&out, opts.height)
}

/// Split an agent pane's text into its identity-line text and an optional
/// subordinate detail line. The identity is the sticky task when one is known
/// (falling back to the msg — so task-less panes, i.e. commands and old
/// producers, render bit-identically to the pre-task rail). The detail is the
/// actionable question, emitted only for Pending/Error when a distinct,
/// non-blank msg exists — calm states never spend a second line.
fn identity_and_detail<'a>(status: Status, task: &'a str, msg: &'a str) -> (&'a str, Option<&'a str>) {
    if task.trim().is_empty() {
        return (msg, None);
    }
    let trimmed_msg = msg.trim();
    let detail = match status {
        Status::Pending | Status::Error if !trimmed_msg.is_empty() && trimmed_msg != task.trim() => Some(trimmed_msg),
        _ => None,
    };
    (task, detail)
}

/// The `· 12m` wait tag for a pane blocked on the user: whole minutes since
/// the waiting-on-you edge (`pending_epoch_s`), frozen at `1h+` once saturated
/// — the same freeze the ledger uses, so the Slow cadence can disarm. `None`
/// under a minute (a fresh ask needs no clock), for every non-Pending status,
/// and for unstamped rows (pre-upgrade snapshots). Per ⟦D-timer⟧ this lives on
/// the pane's identity line, never the tab line.
fn wait_tag(status: Status, pending_epoch_s: Option<u64>, now_epoch_s: u64) -> Option<String> {
    if status != Status::Pending {
        return None;
    }
    let age = now_epoch_s.saturating_sub(pending_epoch_s?);
    if age < 60 {
        None
    } else if age < crate::ledger::SATURATE_S {
        Some(format!("{}m", age / 60))
    } else {
        Some("1h+".to_string())
    }
}

/// Append the wait tag to an identity line: `migrate schema · 12m`. Identity
/// passes through untouched when no tag applies, keeping calm rows
/// bit-identical to the tagless rail.
fn with_wait_tag(identity: &str, status: Status, pending_epoch_s: Option<u64>, now_epoch_s: u64) -> String {
    match wait_tag(status, pending_epoch_s, now_epoch_s) {
        Some(tag) => format!("{identity} · {tag}"),
        None => identity.to_string(),
    }
}

/// The row's line 1: spine + glyph + tab number + name + bell, every column
/// budget clamped so the emitted line never exceeds `width`. This is the
/// densest width-math in the file, self-contained here so `render_row` reads
/// as pane-roster logic.
fn tab_header_line(row: &TabRow, opts: &RenderOpts, tab_target: &RailTarget) -> Line {
    let width = opts.width;
    let now_tick = opts.now_tick;
    let st = row.display.status;

    // Status HUES are always ANSI-16 role codes so the terminal renders them in
    // its OWN theme (any theme, zero config): attention `\x1b[91m` (waiting),
    // error `\x1b[31m`, working `\x1b[33m`, success `\x1b[32m`, accent `\x1b[35m`
    // (spine). ANSI has no orange/peach, so waiting (also bright-red-family)
    // stays distinct from the error row via shape + bold (◆ + bold vs ✗), not
    // hue. Only the dark panel surfaces + dim greys are truecolor (terminal-bg/fg
    // derived), so those match the terminal's theme too.
    let hue = |r: Role| -> String { r.ansi().to_string() };

    // col 0: spine column — ALWAYS reserved so every row's glyph/number/name
    // start at fixed columns; `▌` when active, plain space otherwise.
    let bar = spine_seg(row.active, st);

    // Internal left padding: `pad_x` cells after the col-0 spine/space, before
    // the glyph. At extreme-narrow widths clamp pad_x then num so the prefix
    // never exceeds `width`.
    let pad_x = card_spacing(opts.density).pad_x;

    // col 1: status glyph (working spins; eases to a slow blink for
    // long-runners — see `spin_glyph`).
    let glyph_char = if st == Status::Running {
        let since_tick = row.display.detail.as_ref().map(|d| d.since_tick).unwrap_or(now_tick);
        spin_glyph(now_tick, since_tick)
    } else {
        st.glyph_for(opts.glyphs)
    };
    // The whole label (glyph + number + name) shares the status color so each
    // row reads as its state at a glance — design: "4 web" is green, "5 infra"
    // red, "3 pinky" peach+bold. Idle recedes to the dim idle_text; waiting is
    // also bold (it's the alarm). Active is shown by the spine + brighter card,
    // NOT bold, so the two cues stay independent.
    let label_color = if st == Status::Idle {
        tc_fg(opts.theme.idle_text)
    } else {
        hue(st.role())
    };
    // Bold encodes *activity*: every non-idle row's glyph+number+name is bold so
    // state reads at a glance. Idle stays light/recessed. (Focus is a separate
    // cue — the accent spine + brighter card — so the two stay independent.)
    let label_bold = st != Status::Idle;

    // left visible prefix is "X[pad]<glyph> <num> " — bar/glyph are 1 cell each;
    // `pad_len` is the Cards-only internal left pad (1 col, else 0).
    // Bare minimum: bar(1, always reserved) + glyph(1) + sp(1) + num. Clamp pad first, then num.
    let num_full = row.number.to_string();
    let bar_width = 1;
    let bare_min = bar_width + 1 + 1; // bar + glyph + sp (before num)
    let pad_len = pad_x.min(width.saturating_sub(bare_min + 1)); // keep 1 col for at least '1'
    let num_budget = width.saturating_sub(bare_min + pad_len);
    let num = truncate(&num_full, num_budget);
    let num_w = UnicodeWidthStr::width(num.as_str());
    // Trailing sp after num only if it fits.
    let has_trailing_sp = bare_min + pad_len + num_w < width;
    let pad = " ".repeat(pad_len);
    let prefix_len = bare_min + pad_len + num_w + if has_trailing_sp { 1 } else { 0 };
    // Trailing bell marker (⚑ + space, 2 cols). The prefix
    // clamp only guarantees the prefix itself fits `width`; suppress the bell at
    // extreme-narrow widths where it wouldn't fit beside the prefix, or it would
    // be emitted past the column edge — breaking the "no line exceeds width"
    // invariant and the card-padding math (name_budget would still saturate to 0).
    const BELL_W: usize = 2; // ⚑ + trailing space
    let show_bell = row.has_bell && prefix_len + BELL_W <= width;
    let bell_len = if show_bell { BELL_W } else { 0 };
    let bell = if show_bell {
        format!("{} ", Seg::new(&hue(Role::Working), "⚑"))
    } else {
        String::new()
    };
    // At extreme-narrow widths name_budget saturates to 0 → name = ""; no
    // .max(1) so we never force an extra `…` that would push past `width`.
    let name_budget = width.saturating_sub(prefix_len + bell_len);
    let name = truncate(&row.name, name_budget);

    // gap can be 0 at extreme-narrow widths; saturating_sub prevents underflow.
    let used = prefix_len + UnicodeWidthStr::width(name.as_str()) + bell_len;
    let gap = width.saturating_sub(used);
    let sp_after_num = if has_trailing_sp { " " } else { "" };
    let label_text = format!("{glyph_char} {num}{sp_after_num}{name}");
    let label = Seg {
        color: &label_color,
        bold: label_bold,
        text: label_text.into(),
    };
    Line::new(
        format!("{}{}{}{}{}\n", bar, pad, label, " ".repeat(gap), bell),
        Some(tab_target.clone()),
        LineBg::Card,
    )
}

/// Emit one row's body into `out`, respecting `max_lines`.
///
/// Line 1 (gutter+glyph+num+name+slot) is ALWAYS emitted (via
/// [`tab_header_line`]). PrimaryDetail/roster lines are emitted in priority
/// order. Returns the full untruncated set of lines; caller applies
/// `.take(max_lines)` for overflow.
fn render_row(row: &TabRow, opts: &RenderOpts) -> Vec<Line> {
    let mut lines: Vec<Line> = Vec::new();
    let width = opts.width;
    let st = row.display.status;
    let tab_target = target_for_row(row);

    lines.push(tab_header_line(row, opts, &tab_target));

    // Theme-derived detail text colors: dim_strong for activity text on non-pending rows,
    // idle_text for the muted tree chars and identity mark glyph (neutral/vendor color).
    // Both are truecolor foreground escapes derived from the bg/fg palette blend.
    let dim_strong = tc_fg(opts.theme.dim_strong);
    let idle_color = tc_fg(opts.theme.idle_text);

    // Shared per-pane emission: the identity line (wait tag folded in) plus the
    // optional subordinate `↳ question` line, both click-targeting the pane
    // itself. The multi-pane roster and the single-pane branch below differ
    // only in which panes earn lines and which Branch connector they carry.
    // The one surface class for everything under this row's header (pane
    // lines, detail lines, `+N more`): agent surface when the tab is active,
    // card tint otherwise.
    //
    // `skip_silent` is the single-pane emptiness gate (see the single-pane
    // branch below): when set, a pane that has nothing to say — idle, or an
    // empty identity with no rendering outcome tag — earns no lines at all.
    // It lives here, on the same `identity` that gets emitted, so the gate
    // can never judge a different value than the one drawn.
    let child_bg = if row.active { LineBg::ActiveChild } else { LineBg::Card };
    let pane_lines = |pane: &PaneDisplay, branch: Branch, skip_silent: bool| -> Vec<Line> {
        let pane_status = pane.render_status();
        let (identity, detail) = identity_and_detail(pane_status, pane.task(), pane.msg());
        if skip_silent {
            let says_something = !identity.trim().is_empty()
                || pane.outcome().is_some_and(|o| o.renders_tag());
            if pane_status == Status::Idle || !says_something {
                return Vec::new();
            }
        }
        let identity =
            with_wait_tag(identity, pane_status, pane.pending_epoch_s(), opts.now_epoch_s);
        let pane_target = RailTarget { tab_position: tab_target.tab_position, pane_id: Some(pane.pane_id()), session: None };
        let text = emit_pane_line(pane, &identity, detail.is_some(), opts, row.active, st, &dim_strong, &idle_color, branch);
        // `pane_target` is cloned here because a Pending/Error pane also emits
        // the subordinate `↳ question` line below, targeting the SAME pane —
        // `RailTarget` dropped `Copy` when `session` (a `String`) joined it.
        let mut out = vec![Line::new(text, Some(pane_target.clone()), child_bg)];
        if let Some(q) = detail {
            let text = emit_pane_detail_line(
                q, row.active, st, pane_status, branch, &idle_color, width,
            );
            out.push(Line::new(text, Some(pane_target), child_bg));
        }
        out
    };

    // ── Multi-pane line-per-pane tree (new design) ────────────────────────────
    // A tab with >1 tracked pane renders as: header (line 1, above) + one line
    // per tracked pane (in position order), up to MAX_PANE_LINES, joined by tree
    // connectors (`├` for every child that has a sibling/`+N more` below it, `└`
    // for the last visible child); if more exist, a final `+N more` line is the
    // `└`. No collapse — the tree is purely a visual affordance for "these panes
    // belong to the tab above."
    if is_multi_pane(&row.display) {
        let tracked_panes: Vec<&PaneDisplay> = row.display.panes.iter()
            .filter(|p| p.is_tracked())
            .collect();
        let total_tracked = tracked_panes.len();
        let show = total_tracked.min(MAX_PANE_LINES);
        let remaining = total_tracked - show;

        for (i, &pane) in tracked_panes.iter().take(show).enumerate() {
            // The final pane line is the `└` only when no `+N more` line follows
            // it; otherwise that trailing line carries the elbow.
            let branch = if i + 1 == show && remaining == 0 {
                Branch::Elbow
            } else {
                Branch::Tee
            };
            lines.extend(pane_lines(pane, branch, false));
        }

        if remaining > 0 {
            let more_text = format!("+{} more", remaining);
            // The prefix is the TREE_PREFIX_COLS-wide tree prefix (spine/space +
            // connector + space), so reserve those columns before clamping the
            // text to avoid overflow.
            let clamped = truncate(&more_text, opts.width.saturating_sub(TREE_PREFIX_COLS));
            let text = format!(
                "{}{}\n",
                child_prefix(row.active, st, Branch::Elbow, &idle_color),
                Seg::new(&idle_color, clamped),
            );
            lines.push(Line::new(text, Some(tab_target), child_bg));
        }
        return lines;
    }

    // ── Single-pane pane line (chunk 1) ──────────────────────────────────────
    // The tab's one tracked pane renders through the SAME tree machinery as
    // multi-pane children — `└` elbow, status glyph, identity mark, activity
    // (wait tag included), and the subordinate `↳ question` line — so single-
    // and multi-pane tabs scan identically: same columns, same glyphs, same
    // click semantics (the line targets the pane directly; a click routes
    // straight to `Effect::ShowPane`) and the same subordinate surface band
    // when the tab is active (`LineBg::ActiveChild`).
    //
    // Unlike the multi-pane roster (where every tracked pane earns a line),
    // the single pane's line is emitted only when it says something: a
    // non-empty identity OR an outcome tag that actually renders (`Ok`'s tag
    // is empty by design — see `Outcome::renders_tag` — so an empty-msg Ok
    // completion earns no line at all). Idle stays header-only. The gate
    // itself lives inside `pane_lines` (`skip_silent`), judged on the very
    // identity it emits.
    if let Some(pane) = row.display.panes.iter().find(|p| p.is_tracked()) {
        lines.extend(pane_lines(pane, Branch::Elbow, true));
    }
    lines
}

/// Compose the styled activity segment for a detail/pane line: the command text
/// (in `cmd_color`) plus, when the pane has finished with a nonempty tag, its
/// outcome tag in the outcome's role hue. `Ok` renders no tag at all — the
/// line-1 status glyph is the one done signal. The tag is reserved FIRST so it
/// always survives — the command absorbs any truncation (degrading to `…`,
/// then vanishing), while the outcome shrinks only from its full form
/// (`exit 1`) to the irreducible glyph (`✗`). The returned string fits within
/// `avail` columns and carries its own color escapes (each segment
/// RESET-terminated).
fn compose_activity(cmd: &str, outcome: Option<Outcome>, avail: usize, cmd_color: &str) -> String {
    let Some(oc) = outcome else {
        return Seg::new(cmd_color, truncate(cmd, avail)).to_string();
    };
    // An outcome that renders no tag (Ok) is "no tag at all": no separator
    // space, no empty SGR pair — the status glyph already carries the signal.
    if !oc.renders_tag() {
        return Seg::new(cmd_color, truncate(cmd, avail)).to_string();
    }
    let role = oc.role().ansi();
    let cmd = cmd.trim();
    // Outcome with no command (e.g. an exit with no recorded command string):
    // show the largest form that fits.
    if cmd.is_empty() {
        let full = oc.full();
        let tag = if UnicodeWidthStr::width(full.as_str()) <= avail {
            full
        } else {
            truncate(oc.minimal(), avail)
        };
        return Seg::new(role, tag).to_string();
    }
    // Command + tag: prefer the full tag, falling back to the minimal tag, as
    // long as ≥1 command column remains (tag width + 1 separating space + ≥1
    // command col). The `+ 2` reserves the space and that one command column.
    let full = oc.full();
    let min = oc.minimal();
    for tag in [full.as_str(), min] {
        let tag_w = UnicodeWidthStr::width(tag);
        if tag_w + 2 <= avail {
            return format!(
                "{} {}",
                Seg::new(cmd_color, truncate(cmd, avail - tag_w - 1)),
                Seg::new(role, tag),
            );
        }
    }
    // Too tight for any command: show the outcome glyph alone (clip if even that
    // overflows the extreme-narrow width).
    if UnicodeWidthStr::width(min) <= avail {
        Seg::new(role, min).to_string()
    } else {
        Seg::new(role, truncate(min, avail)).to_string()
    }
}

/// The shared fixed-prefix line shape — sole owner of the narrow-width
/// invariant "no emitted line exceeds `width`". Every colored row emitter
/// writes an unconditional `prefix_cols`-wide styled prefix and budgets its
/// tail with what's left; below the floor (`width < prefix_cols`) the colored
/// path would overflow, so color is dropped whole and ONE plain line is
/// clamped to `width`. `plain` builds that fallback (full, untruncated —
/// clamping happens here); `styled` receives the tail budget
/// (`width - prefix_cols`) and returns the full styled line. Both omit the
/// trailing newline; it is appended here so "exactly one `\n`" rides the same
/// owner. Used by the pane, detail, and ledger rows — a new fixed-prefix row
/// type should route through this rather than re-deriving the guard.
fn prefixed_line(
    width: usize,
    prefix_cols: usize,
    plain: impl FnOnce() -> String,
    styled: impl FnOnce(usize) -> String,
) -> String {
    if width < prefix_cols {
        format!("{}\n", truncate(&plain(), width))
    } else {
        format!("{}\n", styled(width - prefix_cols))
    }
}

/// Emit one pane line in the line-per-pane / tree design:
/// Inactive: ` {connector} {glyph} {mark} {msg}` (space + `├`/`└` + space)
/// Active:   `▌{connector} {glyph} {mark} {msg}` (spine + `├`/`└` + space)
// The `identity`/`has_detail` params push this past clippy's 7-arg default; the
// params are tightly coupled render-context pieces (no natural sub-struct)
// shared with `emit_pane_detail_line`'s sibling call in `render_row`.
#[allow(clippy::too_many_arguments)]
fn emit_pane_line(
    pane: &PaneDisplay,
    identity: &str,
    has_detail: bool,
    opts: &RenderOpts,
    tab_active: bool,
    tab_status: Status,
    dim_strong: &str,
    conn_color: &str,
    branch: Branch,
) -> String {
    let width = opts.width;
    let mark = pane.kind().mark(opts.glyphs);
    let mark_w = UnicodeWidthChar::width(mark).unwrap_or(1);
    let status = pane.render_status();
    let glyph = if status == Status::Running {
        let since_tick = pane.since_tick().unwrap_or(opts.now_tick);
        spin_glyph(opts.now_tick, since_tick)
    } else {
        status.glyph_for(opts.glyphs)
    };
    let glyph_w = UnicodeWidthChar::width(glyph).unwrap_or(1);
    // Prefix: the tree prefix (spine/space + connector + space) + glyph + 1 space + mark + 1 space
    let prefix_vis = TREE_PREFIX_COLS + glyph_w + 1 + mark_w + 1;
    prefixed_line(
        width,
        prefix_vis,
        || {
            let spine = if tab_active { "▌" } else { " " };
            format!("{spine}{} {glyph} {mark} {}", branch.glyph(), identity)
        },
        |avail| {
            let role_ansi = |r: Role| -> &'static str { r.ansi() };
            // Glyph bold on non-idle (matches line 1); mark bold + the stronger dim
            // (vendor-neutral, heavier than the faint idle_text).
            let glyph_color = role_ansi(status.role());
            let cmd_color = if status == Status::Pending && !has_detail {
                role_ansi(Role::Attention).to_string()
            } else {
                dim_strong.to_string()
            };
            let activity = compose_activity(identity, pane.outcome(), avail, &cmd_color);
            // The glyph carries the status color (bold on non-idle, matching line 1); the
            // mark is the vendor-neutral stronger dim, always bold.
            let glyph_seg = Seg {
                color: glyph_color,
                bold: status != Status::Idle,
                text: glyph.to_string().into(),
            };
            let mark_seg = Seg::bold(dim_strong, mark.to_string());
            format!(
                "{}{} {} {}",
                child_prefix(tab_active, tab_status, branch, conn_color),
                glyph_seg,
                mark_seg,
                activity,
            )
        },
    )
}

/// Emit the subordinate `↳ question` line under a Pending/Error pane line.
/// Prefix (7 cols): spine/space + connector continuation (`│` when siblings
/// follow, space under the last child) + 3 spaces + `↳` + space. The question
/// reads in the pane's status hue (attention/error), not bold — subordinate to
/// the identity line above it.
fn emit_pane_detail_line(
    question: &str,
    tab_active: bool,
    tab_status: Status,
    pane_status: Status,
    branch: Branch,
    conn_color: &str,
    width: usize,
) -> String {
    let cont = match branch {
        Branch::Tee => "│",
        Branch::Elbow => " ",
    };
    // The shared tree prefix (spine + connector continuation + its space) + 4
    // more cols: two further indent spaces + `↳` + its trailing space.
    const PREFIX_VIS: usize = TREE_PREFIX_COLS + 4;
    prefixed_line(
        width,
        PREFIX_VIS,
        || {
            let spine = if tab_active { "▌" } else { " " };
            format!("{spine}{cont}   ↳ {question}")
        },
        |avail| {
            format!(
                "{}{}   {}",
                spine_seg(tab_active, tab_status),
                Seg::new(conn_color, cont),
                Seg::new(pane_status.role().ansi(), format!("↳ {}", truncate(question, avail))),
            )
        },
    )
}

/// Which tree connector a multi-pane child line draws at column 1.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Branch {
    /// `├` — a child that has more siblings (or a `+N more` line) below it.
    Tee,
    /// `└` — the last *visible* child under the tab (the `+N more` line when
    /// present, otherwise the final pane line).
    Elbow,
}

impl Branch {
    fn glyph(self) -> &'static str {
        match self {
            Branch::Tee => "├",
            Branch::Elbow => "└",
        }
    }
}

/// The [`TREE_PREFIX_COLS`]-column (3) left prefix shared by multi-pane child
/// / `+N more` lines:
///   col 0  — active-tab spine `▌` (status-hued: peach when waiting/error,
///            mauve accent otherwise) or a plain space when inactive;
///   col 1  — the tree connector (`├`/`└`), in the muted `conn_color`;
///   col 2  — a separating space before the glyph.
///
/// Holding the connector at a fixed column (1) whether or not the tab is active
/// keeps the glyph aligned at column 3 across all child lines, so the per-line
/// truncation budget is constant (`prefix_vis` in [`emit_pane_line`]).
fn child_prefix(active: bool, tab_status: Status, branch: Branch, conn_color: &str) -> String {
    format!("{}{} ", spine_seg(active, tab_status), Seg::new(conn_color, branch.glyph()))
}

/// Measure visible (display) width of a string that may contain ANSI SGR escapes.
fn visible_width(s: &str) -> usize {
    let mut width = 0usize;
    let mut chars = s.chars().peekable();
    while let Some(c) = chars.next() {
        if c == '\x1b' {
            if chars.peek() == Some(&'[') {
                for inner in chars.by_ref() {
                    if inner == 'm' {
                        break;
                    }
                }
            }
        } else {
            width += UnicodeWidthChar::width(c).unwrap_or(0);
        }
    }
    width
}

/// Emit an ANSI truecolor background escape for a given (r, g, b) triple.
fn tc_bg(c: Rgb) -> String {
    format!("\x1b[48;2;{};{};{}m", c.0, c.1, c.2)
}

/// Emit an ANSI truecolor foreground escape for a given (r, g, b) triple.
fn tc_fg(c: Rgb) -> String {
    format!("\x1b[38;2;{};{};{}m", c.0, c.1, c.2)
}

/// The truecolor surface tint for a card, by class: a flashing row (the
/// flip-to-pending ping) outranks everything else — it's the glance-catcher —
/// then the focused tab, then agent rows (active status), then idle/plain
/// panes as the dimmest surface. Returns an owned ANSI escape string.
fn card_tint(row: &TabRow, theme: &DerivedColors) -> String {
    let rgb = if row.flash {
        theme.surface_flash
    } else if row.active {
        theme.surface_active
    } else if row.display.status.is_active() {
        theme.surface_agent
    } else {
        theme.surface_idle
    };
    tc_bg(rgb)
}

/// Paint a single content line with a truecolor (24-bit) surface background
/// band (`bg`). Terminals without truecolor silently ignore the `48;2;…m`
/// escape (it is well-formed SGR), so the card surface tint is simply absent
/// there — the character grid and status hues (ANSI-16) are unaffected.
///
/// Steps:
/// 1. Replace every `RESET` (`\x1b[0m`) in the line with `RESET + bg` so that
///    colored tokens re-arm the background after they reset.
/// 2. Strip the trailing newline (if present), measure, pad to `width`, restore.
/// 3. Wrap: `bg + transformed_line + pad + "\x1b[49m\x1b[0m"`.
///
/// The returned string ends with `\n`.
fn paint_card_line(line: &str, width: usize, bg: &str) -> String {
    const BG_RESET: &str = "\x1b[49m";

    // Strip trailing newline; we'll add it back at the end.
    let bare = line.strip_suffix('\n').unwrap_or(line);

    // Re-arm bg after every reset token inside the line.
    let rearmed = bare.replace(RESET, &format!("{}{}", RESET, bg));

    // Measure visible width of the re-armed content.
    let vis = visible_width(&rearmed);

    // Pad to fill the band up to `width`.
    let pad = if vis < width {
        " ".repeat(width - vis)
    } else {
        String::new()
    };

    format!("{}{}{}{}{}\n", bg, rearmed, pad, BG_RESET, RESET)
}

fn target_for_row(row: &TabRow) -> RailTarget {
    RailTarget {
        tab_position: row.number.saturating_sub(1) as usize,
        pane_id: None,
        session: None,
    }
}

/// Produce the identity header lines as raw (unpainted) `Line` values.
/// Returns 0 lines if `!opts.header` or `!has_content` (no rows AND no ledger
/// history); 1 line (title only) in Cards density; 2 lines (title + `═` rule)
/// in all other densities. `overflow` is computed by the caller and passed in.
/// The `·N`/`N▲` count is always `rows.len()` — with zero tracked tabs and a
/// non-empty ledger it reads `·0`, which is honest: the header counts tabs,
/// not history.
///
/// The right slot appends a needs-you badge (`{n}!`, `Role::Attention`, bold)
/// whenever any row's `Status::needs_you()`, space-joined after the census —
/// `·{tabs} {n}!`. Narrow-width priority (`Some(_) if …` guards below, tried
/// top to bottom): the overflow marker always wins over the badge; the badge
/// always wins over the plain census. So a tight budget drops the census
/// first, keeping the badge alone; only once there's no badge (or the
/// overflow marker itself needs the room) does the lone primary token stand,
/// clamped to width.
///
/// `working` drives the header-rule heartbeat: in Compact/
/// Comfortable density (Cards has no `═` rule to carry it) the rule swaps one
/// `═` for a `◆` (`Role::Accent`, bold) at column `now_tick % width`, a pure
/// function of the render tick — see [`header_rule`] and [`render_body`]'s
/// call site for how `working` is derived from `rows`.
fn render_header(rows: &[TabRow], opts: &RenderOpts, overflow: bool, has_content: bool, working: bool) -> Vec<Line> {
    if !opts.header || !has_content {
        return vec![];
    }
    let width = opts.width;
    let accent = Role::Accent.ansi();
    let primary = if overflow {
        format!("{}▲", rows.len())
    } else {
        format!("·{}", rows.len())
    };
    let title = " RADAR";
    let title_w = UnicodeWidthStr::width(title);
    // Title in accent; primary muted (accent when overflowing, so the ▲
    // marker stays loud).
    let primary_color = if overflow { accent } else { Role::Muted.ansi() };
    let need_you = rows.iter().filter(|r| r.display.status.needs_you()).count();
    let badge = (need_you > 0).then(|| format!("{}!", need_you));
    // Budget available to the right slot before the title itself has to give
    // up any columns — the priority decision below is made against this,
    // independent of the later title-squeeze clamp.
    let avail = width.saturating_sub(title_w);
    let combined_w = |b: &str| UnicodeWidthStr::width(primary.as_str()) + 1 + UnicodeWidthStr::width(b);
    // (text, color, bold) run(s) that make up the right slot, in emission order.
    let right_segs: Vec<(String, &str, bool)> = match &badge {
        Some(b) if combined_w(b) <= avail => {
            vec![(primary.clone(), primary_color, false), (format!(" {b}"), Role::Attention.ansi(), true)]
        }
        // Combined doesn't fit: the overflow marker always wins, but a plain
        // census loses to the badge — drop it and keep the badge alone.
        Some(b) if !overflow => vec![(b.clone(), Role::Attention.ansi(), true)],
        _ => vec![(primary.clone(), primary_color, false)],
    };
    let plain_right: String = right_segs.iter().map(|(t, _, _)| t.as_str()).collect();
    let right_full_w = UnicodeWidthStr::width(plain_right.as_str());
    let right_w = right_full_w.min(width);
    // At extreme-narrow widths the gap can be 0 (no `.max(1)`) so the
    // assembled visible content never exceeds `width`.
    let gap = width.saturating_sub(title_w + right_w);
    // Clamp visible portions to width before assembling the ANSI line.
    let title_budget = width.saturating_sub(right_w + gap);
    let title_clamped = truncate(title, title_budget);
    let right_rendered = if right_full_w > width {
        // Extreme-narrow clamp: the composite doesn't even fit alone — fall
        // back to one plain (uncolored-per-segment) truncated run, same as
        // the pre-badge single-token clamp.
        Seg::new(primary_color, truncate(&plain_right, right_w)).to_string()
    } else {
        right_segs
            .into_iter()
            .map(|(t, c, b)| if b { Seg::bold(c, t).to_string() } else { Seg::new(c, t).to_string() })
            .collect::<String>()
    };
    let mut title_line = String::new();
    title_line.push_str(&format!(
        "{}{}{}\n",
        Seg::new(accent, title_clamped),
        " ".repeat(gap),
        right_rendered,
    ));

    let mut lines = vec![Line::new(title_line, None, LineBg::Rail)];
    // Header line 2: rule across the full width — only in non-Cards densities.
    if opts.density != Density::Cards {
        lines.push(Line::new(
            format!("{}\n", header_rule(width, opts.now_tick, working, accent)),
            None,
            LineBg::Rail,
        ));
    }
    lines
}

/// The header rule (`═` × width), or — while `working` is true — the same
/// rule with one column swapped for a bold-accent `◆` at `now_tick % width`:
/// three `Seg`s (`═`×pos, `◆`, `═`×rest) so the heartbeat is purely additive
/// over the plain rule's layout, exactly like every other colored token in
/// this file. Pure function of `now_tick`: no state, just a different column
/// each tick, wrapping at `width`. `width == 0` degenerates to an empty rule
/// (nothing to place the diamond in) — `render_rail` never calls with 0
/// (`cols.max(1)`), but this stays total rather than panicking if a test does.
fn header_rule(width: usize, now_tick: u64, working: bool, accent: &str) -> String {
    if !working || width == 0 {
        return Seg::new(accent, "═".repeat(width)).to_string();
    }
    let pos = (now_tick % width as u64) as usize;
    let before = "═".repeat(pos);
    let after = "═".repeat(width - pos - 1);
    format!(
        "{}{}{}",
        Seg::new(accent, before),
        Seg::bold(accent, "◆"),
        Seg::new(accent, after),
    )
}

/// The cross-session badge: one line per session `Sessions::badge()` tracks
/// (current-first, per its ordering contract — this function trusts that
/// order and does not re-sort), inserted between the header and the first
/// card. Renders ZERO lines when `entries.len() <= 1` — only the current
/// session, or no peer presence has crossed the shared `/cache` root yet —
/// so the feature is invisible until there's genuinely something
/// cross-session to show, and
/// every existing single-session snapshot/test stays byte-identical (the
/// badge is additive, never a subtraction from the render surface).
///
/// The current session's own line carries NO click target at all — you
/// can't "switch to" the session you're already in, and unlike a peer line
/// there is no `attention_tab_position` to jump to either — so its `Line`
/// gets `None`, exactly like a header line. Every peer line targets that
/// peer via [`RailTarget::for_session`], which bakes in `tab_position` NOW
/// (at line-build time, from the entry's `attention_tab_position`) rather
/// than leaving `mouse_click` to re-derive it later against a badge that may
/// have moved on by the time the click lands.
///
/// Text is `name` plus, only when nonzero, a running count and an attention
/// count — each paired with the SAME glyph the per-tab rows already use for
/// that status (`Status::Running`/`Status::Pending`, run through
/// `opts.glyphs`) rather than inventing a parallel icon vocabulary the nerd/
/// plain config wouldn't know about. `selected` (the pending Alt+[/] cycle
/// target) renders bold+accent; everything else — including the current
/// line — renders in the muted `idle_text`, so the badge reads as a status
/// strip, not a second row of cards.
fn render_session_badge(entries: &[BadgeEntry], opts: &RenderOpts) -> Vec<Line> {
    if entries.len() <= 1 {
        return vec![];
    }
    let width = opts.width;
    let idle = tc_fg(opts.theme.idle_text);
    let accent = Role::Accent.ansi();
    let running_glyph = Status::Running.glyph_for(opts.glyphs);
    let attention_glyph = Status::Pending.glyph_for(opts.glyphs);
    entries
        .iter()
        .map(|entry| {
            let mut label = entry.name.clone();
            if entry.running > 0 {
                label.push_str(&format!(" {}{}", entry.running, running_glyph));
            }
            if entry.attention > 0 {
                label.push_str(&format!(" {}{}", entry.attention, attention_glyph));
            }
            // A dim `•` marks "you are here" on the current line, independent
            // of `selected` — a plain space keeps every other line's label
            // aligned under it.
            let marker = if entry.is_current { "•" } else { " " };
            const PREFIX_VIS: usize = 2; // marker + its separating space
            let text = prefixed_line(
                width,
                PREFIX_VIS,
                || format!("{marker} {label}"),
                |avail| {
                    let clamped = truncate(&label, avail);
                    let label_seg = if entry.selected {
                        Seg::bold(accent, clamped)
                    } else {
                        Seg::new(&idle, clamped)
                    };
                    format!("{} {}", Seg::new(&idle, marker), label_seg)
                },
            );
            let target = (!entry.is_current)
                .then(|| RailTarget::for_session(entry.name.clone(), entry.attention_tab_position));
            Line::new(text, target, LineBg::Rail)
        })
        .collect()
}

/// Produce the idle-strip line as a raw (unpainted) `Line` value.
/// Returns 0 lines if `strip_folded == 0`; else 1 line tagged `LineBg::Rail`.
fn render_strip(strip_folded: usize, opts: &RenderOpts) -> Vec<Line> {
    if strip_folded == 0 {
        return vec![];
    }
    vec![Line::new(
        format!(
            "{}\n",
            Seg::new(
                Role::Accent.ansi(),
                truncate(&format!("+{} idle ▾", strip_folded), opts.width),
            ),
        ),
        None,
        LineBg::Rail,
    )]
}

/// Header + one card block per kept row + idle strip — everything in
/// `render_rail` EXCEPT the bottom region (spec §9). Split out so the bottom
/// region's `leftover` can be measured against this body's real footprint,
/// and so a test harness can ask "how tall is this session's content alone"
/// (see `body_line_count`) without triggering the pinned-footer fill.
fn render_body(rows: &[TabRow], ledger: &[LedgerLine], opts: &RenderOpts) -> Vec<Line> {
    let width = opts.width;
    let cards = opts.density == Density::Cards;
    let rail = tc_bg(opts.theme.rail_bg);
    // Zero tabs with a non-empty ledger still has something to show (spec §9's
    // floor: header + bottom region, no cards) — the header must not vanish
    // just because `rows` is empty.
    let has_content = !rows.is_empty() || !ledger.is_empty();

    // Built (and its line count reserved out of `body_budget`) BEFORE
    // `plan_layout` runs, exactly like `header_lines` above it — otherwise
    // overflow folding would plan against a taller budget than the rows
    // actually get once the badge lines land above them, and the final
    // `flat.truncate(opts.height)` in `render_rail` would silently eat into
    // the footer/last row rather than the folding math accounting for it.
    let badge_lines = render_session_badge(&opts.badge, opts);

    let blocks: Vec<Vec<Line>> = rows.iter().map(|r| render_row(r, opts)).collect();
    let metas: Vec<RowMeta> = rows.iter().zip(&blocks)
        .map(|(r, b)| RowMeta { status: r.display.status, full_lines: b.len() })
        .collect();
    let body_budget = opts
        .height
        .saturating_sub(header_lines(opts.header, opts.density, has_content))
        .saturating_sub(badge_lines.len());
    let (plan, strip_folded, spacing) = plan_layout(&metas, body_budget, opts.density);
    let overflow = plan.len() < rows.len();
    // Drives the header-rule heartbeat — a plain `any()`, not a
    // count, so it's a distinct question from `footer_tally`'s "how many are
    // working" tally computed later in `render_bottom`; the two live in
    // separate pipeline stages (body vs. bottom region) and sharing one
    // number across that seam would cost more plumbing than the one `filter`
    // it saves.
    let working = rows.iter().any(|r| r.display.status == Status::Running);

    let mut flat: Vec<Line> = Vec::new();

    // Header.
    for line in render_header(rows, opts, overflow, has_content, working) {
        flat.push(if cards { line.painted(width, &rail) } else { line });
    }

    // Cross-session badge (zero lines with ≤1 session — see
    // `render_session_badge`), between the header and the first card.
    for line in badge_lines {
        flat.push(if cards { line.painted(width, &rail) } else { line });
    }

    // Body: one card block per kept row.
    for &(i, budget) in &plan {
        let row = &rows[i];
        let row_target = target_for_row(row);

        // Resolve a raw line's surface through the one `LineBg::escape` map and
        // paint it (Cards only). The emitted line is final, so it carries
        // `LineBg::None`; its footprint is exactly the lines pushed here.
        let finalize = |bg: LineBg, text: String, target: Option<RailTarget>| -> Line {
            let text = match bg.escape(row, &opts.theme, &rail) {
                Some(esc) if cards => paint_card_line(&text, width, &esc),
                _ => text,
            };
            Line::new(text, target, LineBg::None)
        };

        // pad_y internal top padding — belongs to this card's click span.
        // Cloned each iteration (`RailTarget` isn't `Copy`; `session` is a
        // `String`) — `pad_y` can be >1, so a plain move would only survive
        // the first pass.
        for _ in 0..spacing.pad_y {
            flat.push(finalize(LineBg::Card, "\n".to_string(), Some(row_target.clone())));
        }

        // content (truncated to the planned budget == today's compression).
        for line in blocks[i].iter().take(budget) {
            flat.push(finalize(line.bg, line.text.clone(), line.target.clone()));
        }

        // gap external separation (dark panel base in Cards).
        for _ in 0..spacing.gap {
            flat.push(finalize(LineBg::Rail, "\n".to_string(), None));
        }
    }

    // Idle strip.
    for line in render_strip(strip_folded, opts) {
        flat.push(if cards { line.painted(width, &rail) } else { line });
    }

    flat
}

/// Test-only: the height `render_rail` needs to show every row (+ idle strip)
/// with no overflow folding and no bottom region (leftover 0) at this
/// width/density — i.e. the session's "natural" content height. `opts.height`
/// only needs to be large enough to avoid overflow folding; the result is
/// otherwise independent of it. The reference-doc harness uses this to
/// preserve pre-footer scenario heights: passing a merely-large height (the
/// old "enough to fit" sentinel) would now land in the bottom region's
/// unbounded-filler branch.
#[cfg(test)]
pub(crate) fn body_line_count(rows: &[TabRow], ledger: &[LedgerLine], opts: &RenderOpts) -> usize {
    render_body(rows, ledger, opts).len()
}

/// One filler line: blank, rail-based, click-inert. Shared by the bottom
/// region's leading pad and (via the ledger branch) its interior pad.
fn bottom_filler() -> Line {
    Line::new("\n".to_string(), None, LineBg::Rail)
}

/// The footer's top rule: a full-width ghost-colored `─` line.
fn footer_rule(opts: &RenderOpts) -> Line {
    let ghost = tc_fg(opts.theme.idle_text);
    Line::new(
        format!("{}\n", Seg::new(&ghost, "─".repeat(opts.width))),
        None,
        LineBg::Rail,
    )
}

/// The footer's bottom hint line: "alt-[n] jump", clamped to width. Starts at
/// column 0 like the tally above it — a hand-tuned leading space here once put
/// the two footer lines one column out of step.
fn footer_hint(opts: &RenderOpts) -> Line {
    let idle = tc_fg(opts.theme.idle_text);
    Line::new(
        format!("{}\n", Seg::new(&idle, truncate("alt-[n] jump", opts.width))),
        None,
        LineBg::Rail,
    )
}

/// The footer's tally line: `{n} working · {m} need you`. `n` counts
/// `Status::Running` rows; `m` counts `Status::needs_you` rows (loud + bold —
/// and the ` · {m} need you` segment only exists when `m > 0`: a zero is
/// noise, not signal, so the line recedes to just `{n} working`). No spinner:
/// the count is the information, and motion for "something is running" already
/// lives in the row glyphs and the header-rule heartbeat — a third animation
/// glued to the digit read as a corrupted number, not a signal. Truncated to
/// width; too-tight-for-both-colors degrades to one run colored by whether the
/// tally is still loud (`m > 0`).
fn footer_tally(rows: &[TabRow], opts: &RenderOpts) -> Line {
    let width = opts.width;
    let idle = tc_fg(opts.theme.idle_text);
    let working = rows.iter().filter(|r| r.display.status == Status::Running).count();
    let need_you = rows.iter().filter(|r| r.display.status.needs_you()).count();
    if need_you == 0 {
        let text = truncate(&format!("{working} working"), width);
        return Line::new(format!("{}\n", Seg::new(&idle, text)), None, LineBg::Rail);
    }
    let left = format!("{working} working · ");
    let right = format!("{} need you", need_you);
    let fits = UnicodeWidthStr::width(left.as_str()) + UnicodeWidthStr::width(right.as_str()) <= width;
    // `need_you > 0` is guaranteed past the early return, so the loud (bold
    // attention) form is the only one either branch renders.
    let text = if fits {
        format!("{}{}\n", Seg::new(&idle, left), Seg::bold(Role::Attention.ansi(), right))
    } else {
        let full = format!("{left}{right}");
        format!("{}\n", Seg::bold(Role::Attention.ansi(), truncate(&full, width)))
    };
    Line::new(text, None, LineBg::Rail)
}

/// The ledger section's own rule: `─ earlier ` then `─` fill to width.
fn ledger_rule(opts: &RenderOpts) -> Line {
    let ghost = tc_fg(opts.theme.idle_text);
    let prefix = "─ earlier ";
    let prefix_w = UnicodeWidthStr::width(prefix);
    let text = if opts.width <= prefix_w {
        truncate(prefix, opts.width)
    } else {
        format!("{}{}", prefix, "─".repeat(opts.width - prefix_w))
    };
    Line::new(format!("{}\n", Seg::new(&ghost, text)), None, LineBg::Rail)
}

/// Max columns a ledger row's tab name may take before truncating — the
/// label absorbs whatever remains. rail-reference.md §AB documents the
/// same 12 ("a tab name past 12 columns truncates with `…`").
const LEDGER_NAME_COLS: usize = 12;

/// One ledger row: `{age} {glyph} {tab_name} {label}`. Exact truncation rule
/// (spec §9): the fixed 3-col `age space glyph space` prefix is reserved
/// first, `tab_name` gets up to [`LEDGER_NAME_COLS`] cols of what's left, and
/// `label` absorbs whatever remains — omitted (with its separating space)
/// entirely when nothing remains. Click-inert once its tab has closed
/// (`tab_position` is `None`).
fn ledger_entry_line(line: &LedgerLine, opts: &RenderOpts) -> Line {
    let width = opts.width;
    let idle = tc_fg(opts.theme.idle_text);
    let dim_strong = tc_fg(opts.theme.dim_strong);
    let age = crate::ledger::format_age(line.at_epoch_s, opts.now_epoch_s);
    let age_w = UnicodeWidthStr::width(age.as_str());
    let (glyph, glyph_role) = if line.error {
        ("✗", Role::Error)
    } else {
        ("●", Role::Success)
    };
    let prefix = age_w + 1 + 1 + 1; // age, space, glyph, space
    let text = prefixed_line(
        width,
        prefix,
        || format!("{age} {glyph} {} {}", line.tab_name, line.label),
        |avail| {
            let name_budget = LEDGER_NAME_COLS.min(avail);
            let name = truncate(&line.tab_name, name_budget);
            let name_w = UnicodeWidthStr::width(name.as_str());
            let label_budget = avail.saturating_sub(name_w + 1);
            let mut text = format!(
                "{} {} {}",
                Seg::new(&idle, age.as_str()),
                Seg::new(glyph_role.ansi(), glyph),
                Seg::new(&dim_strong, name),
            );
            // Gate on the label too, not just budget: an empty command-origin
            // label would otherwise emit a trailing space plus an SGR pair
            // wrapped around nothing.
            if label_budget > 0 && !line.label.is_empty() {
                let label = truncate(&line.label, label_budget);
                text.push(' ');
                text.push_str(&Seg::new(&idle, label).to_string());
            }
            text
        },
    );
    Line::new(
        text,
        line.tab_position.map(|p| RailTarget { tab_position: p, pane_id: None, session: None }),
        LineBg::Rail,
    )
}

/// Most `─ earlier` entries the rail ever shows, no matter how tall the pane
/// is. The rail is a status surface, not a log: past this, spare height reads
/// better as blank filler than as ever-deeper history (the sidebar should
/// never be more history than not-history). Display-only — the storage ring
/// keeps `ledger::LEDGER_CAP` (32) entries for cross-instance merge/dedup.
const LEDGER_DISPLAY_CAP: usize = 10;

/// The bottom region per the spec §9 budget table. `leftover` = height minus
/// everything already in `flat` (i.e. `render_body`'s footprint). Returns
/// lines ordered top→bottom (filler … ledger rule, entries newest-first,
/// spacer … footer rule, tally, hint?). INVARIANT: when it returns any lines,
/// `flat.len() + returned.len() == opts.height` exactly.
///
/// The footer is `f` lines: rule + tally, plus the `alt-[n] jump` hint line
/// only when `opts.jump_hint` claims the chord actually exists (f = 2 or 3).
/// A shown ledger always ends with one blank spacer line before the footer
/// rule — history gets air above the pinned floor instead of running into it.
///
/// | leftover | region |
/// |---|---|
/// | 0–1 | nothing |
/// | 2 | rule + tally (hint dropped even when configured) |
/// | 3..=f | full footer(f) |
/// | >f, ledger empty or too tight | (leftover−f) filler + footer(f) |
/// | ≥f+3, ledger non-empty | filler + ledger rule + `min(len, leftover−f−2, LEDGER_DISPLAY_CAP)` entries + spacer + footer(f) |
fn render_bottom(rows: &[TabRow], ledger: &[LedgerLine], leftover: usize, opts: &RenderOpts) -> Vec<Line> {
    // Build the footer once and derive `f` from what was actually built, so
    // the budget math and the emitted lines can never disagree on the
    // footer's height — a future footer line re-budgets the filler/ledger
    // arithmetic automatically instead of underflowing it.
    let mut footer = vec![footer_rule(opts), footer_tally(rows, opts)];
    if opts.jump_hint {
        footer.push(footer_hint(opts));
    }
    let f = footer.len();
    match leftover {
        0 | 1 => vec![],
        // Squeezed (n < f): pin the top of the footer — rule + tally survive,
        // the hint is the first line dropped.
        n if n <= f => {
            footer.truncate(n);
            footer
        }
        n => {
            let mut v = Vec::with_capacity(n);
            // Ledger needs its rule, ≥1 entry, and the spacer to be worth
            // showing; below that (saturating: n can be as small as f+1) the
            // space reads better as blank filler.
            let entries_n = ledger.len().min(n.saturating_sub(f + 2)).min(LEDGER_DISPLAY_CAP);
            if entries_n == 0 {
                for _ in 0..n - f {
                    v.push(bottom_filler());
                }
            } else {
                let filler_n = n - f - 2 - entries_n;
                for _ in 0..filler_n {
                    v.push(bottom_filler());
                }
                v.push(ledger_rule(opts));
                for line in ledger.iter().take(entries_n) {
                    v.push(ledger_entry_line(line, opts));
                }
                v.push(bottom_filler());
            }
            v.extend(footer);
            v
        }
    }
}

pub fn render_rail(rows: &[TabRow], ledger: &[LedgerLine], opts: &RenderOpts) -> RenderedRail {
    // Truly nothing to show only when there are no rows AND no ledger history
    // — the caller routes that case to `onboarding` instead (spec §7/§9). Zero
    // rows with a non-empty ledger still renders: header + bottom region, no
    // cards.
    if rows.is_empty() && ledger.is_empty() {
        return RenderedRail::from_lines(vec![]);
    }
    let cards = opts.density == Density::Cards;
    let width = opts.width;
    let rail = tc_bg(opts.theme.rail_bg);

    let mut flat = render_body(rows, ledger, opts);

    // Bottom region (spec §9): whatever height `render_body` didn't use, up to
    // and including the pinned footer at the floor. The trailing gap line the
    // body already emits after the last card simply becomes part of the space
    // `leftover` measures — no special-casing needed.
    let leftover = opts.height.saturating_sub(flat.len());
    for line in render_bottom(rows, ledger, leftover, opts) {
        flat.push(if cards { line.painted(width, &rail) } else { line });
    }

    // Final height clamp. The body is budgeted against `height - header_lines`, so
    // this only bites when the header ALONE exceeds `height` (e.g. height 1 with a
    // 2-line Compact/Comfortable header) — a degenerate size Zellij would clip
    // anyway. Truncating the single `flat` Vec keeps ansi/targets/line-count in
    // lockstep (they all derive from it), so the "no rail exceeds `height` lines"
    // invariant now holds at every density, not just Cards (1-line header).
    flat.truncate(opts.height);

    RenderedRail::from_lines(flat)
}

#[cfg(test)]
fn render(rows: &[TabRow], opts: &RenderOpts) -> String {
    render_rail(rows, &[], opts).ansi
}

#[cfg(test)]
mod tests;

// Shared vt100 grid oracle — used by `tests` above AND `crate::reference_tests`,
// so the snapshot suite and the executable spec can never drift apart.
#[cfg(test)]
pub(crate) mod test_util;

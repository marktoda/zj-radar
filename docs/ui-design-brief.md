# zj-radar — UI Design Brief

**For:** a designer crafting the visual language of the sidebar
**From:** Mark (with Claude)
**Date:** 2026-06-26
**Status:** functional v1 exists; we want it to look great

---

## 1. TL;DR

`zj-radar` is a **left sidebar inside the terminal** that shows, at a glance, the
status of every tab — and especially of AI coding agents (Claude Code, Codex)
running in those tabs: which are *working*, which are *waiting for me*, which are
*done*, which *errored*. You can click a row to jump to that tab. Think of the
"agent list / activity rail" in apps like Cmux or a CI dashboard, but rendered as
text inside a terminal multiplexer.

We have a working but bare v1. **We want you to design the visual language**:
layout, hierarchy, the state vocabulary (glyphs + color), the active/idle
treatment, headers/separators, density, and how it behaves as tabs and agents
come and go.

---

## 2. The medium — please read this first

This is **not** a web/app UI. It is a **character grid**. Designing for it is
closer to typography + ASCII/Unicode art than to Figma rectangles. The hard
constraints:

- **Monospace grid.** Everything is fixed-width cells (one cell per character).
  No sub-pixel positioning, no arbitrary spacing — only whole columns/rows.
- **Width is precious and fixed.** The sidebar is a vertical strip, currently
  **~32 columns** wide, full terminal height. You can propose a different width
  (and an optional collapsed/expanded mode), but every column counts. Long names
  must truncate (we use `…`).
- **Color = the terminal theme.** We can use the 16 ANSI colors, 256-color, or
  truecolor, but the *tasteful* choice is to use the user's **active theme
  palette** (so it matches their terminal). Assume a dark theme by default; it
  must also be legible on light. Don't rely on exact hex — design in terms of
  roles (e.g. "success/green", "attention/yellow", "muted/dim", "accent").
- **Glyphs:** the user has **Nerd Fonts** (JetBrains Mono Nerd Font), so we can
  use Nerd Font icons + Unicode box-drawing/geometric shapes (`● ◐ ◑ ○ ✓ ✗ ▌ ▸
  ╭─╮│╰╯` etc.). Provide a **plain-Unicode fallback** set too (no Nerd Font) in
  case we ship it broadly.
- **No images, no animation frames** beyond what a 1-second redraw allows. We
  *can* do a simple spinner (cycle a glyph each second) and elapsed timers, since
  the plugin re-renders ~once per second while agents are active.
- **Interaction is limited:** mouse **click** works (→ switch tab). There's no
  reliable hover. Keyboard navigation is possible but the user mostly drives tabs
  with their existing keybindings, not the sidebar.

A good mental model: you're designing a **dense, glanceable status rail in text**,
where color + a single icon + tight typography carry the meaning.

---

## 3. What it's for (context)

- The user runs **many terminal tabs**, several of which have **AI agents**
  working autonomously. The pain: you lose track of which agent finished, which
  is blocked asking a question, which is still grinding.
- The sidebar makes that **ambient and glanceable** without switching tabs:
  color + icon per tab, plus (for agent tabs) repo/branch, elapsed time, and the
  agent's last message.
- It complements (doesn't replace) OS notifications that already fire when an
  agent finishes. The sidebar is the *persistent at-a-glance* layer.

---

## 4. The data we have to show (per tab)

For **every** tab:
- **tab number** (1-based) and **tab name** (e.g. `dotfiles`, `pinky`, `api`).
- **active?** — is this the currently-focused tab.

For tabs that contain **one or more agents** (additive — plain tabs show none of
this):
- **status**, one of: `working` · `waiting-for-you` · `done` · `error` · (idle).
- **repo** and **branch** (e.g. `pinky` / `fix/x`).
- **elapsed time** in the current status (e.g. `0:14`, `2m`, `1h3m`).
- **last message** — a short line of what the agent just said/did
  (e.g. `"running tests…"`), truncated.
- **count** when a tab holds multiple agents: `done/total` (e.g. `2/4`).

Not every field is always present (an agent may have no message yet; a plain
terminal tab has none of the agent fields).

---

## 5. States to design (the heart of the brief)

Please give each of these a clear, distinct, glanceable treatment:

| State | Meaning | Today's placeholder |
|---|---|---|
| **plain / idle** | tab with no agent (or agent gone idle) — should *recede* | dim `○` + number + name |
| **working** | agent actively running | yellow `◐` |
| **waiting-for-you** | agent blocked on a question/permission — *most urgent to surface* | orange `◑` |
| **done** | agent finished its turn — should be noticeable until you visit it | green `●` |
| **error** | agent/turn failed | red `✗` |
| **active tab** | the tab you're currently in | bold name (weak) |
| **multi-agent tab** | several agents in one tab | `2/4` count |

Cross-cutting:
- **Hierarchy:** "waiting-for-you" and "error" should draw the eye first; "done"
  next; "working" is informative but calm; "idle/plain" should be quiet.
- **Active-tab indicator:** today it's just bold — we'd like something stronger
  and unmistakable (a left bar `▌`, an inverse row, an accent — your call).
- **Header / identity:** there's currently no header. Consider a compact title or
  rule so the rail reads as a deliberate panel, not stray text. Must be cheap on
  vertical space.
- **Row anatomy:** today an agent tab is up to 3 lines (line 1: icon + number +
  name + count; line 2: `repo/branch · elapsed`; line 3: `"last message"`). You
  may redesign this — fewer/more lines, different grouping — but mind vertical
  space when many tabs exist.

---

## 6. Flows / storyboards we'd like to see

Please mock these as a sequence of sidebar states (a few "frames" each):

1. **Fresh session, no agents.** A handful of plain tabs. Show how quiet/clean it
   looks at rest, and how the active tab reads.

2. **An agent's lifecycle (the core flow).** One tab goes:
   `idle → working (spinner + elapsed counting) → waiting-for-you (you need to act)
   → working → done (✓, stays noticeable) → you click/visit it → clears to idle`.
   Show each frame. This is the most important storyboard.

3. **Many tabs, mixed states.** ~8–12 tabs, a realistic mix (some plain, 3
   working, 1 waiting, 2 done, 1 error). Show the scan-ability: can you find the
   "needs me" one instantly? Consider what happens when there are more tabs than
   vertical room (scroll? compress to one line each? group?).

4. **Multi-agent tab.** One tab running 4 agents (`2/4 done`, one waiting). How do
   we represent an aggregate + the count, and which agent's detail/message wins
   the row?

5. **First-run permission (onboarding).** On first launch the plugin must ask the
   OS-multiplexer for permission; until granted it should present a clear,
   friendly "press `y` to allow zj-radar to read tab state" affordance in the
   rail, then transition to the normal sidebar once granted. Design this
   onboarding moment.

6. **Collapsed / expanded (optional, nice-to-have).** A thin collapsed mode
   (~4–6 cols: just colored dots + numbers) toggled by a keybind, expanding to the
   full rich rail. Show both.

---

## 7. What exists today (your starting point)

A bare functional render: per row, `<colored dot> <number> <name>`, the active
tab's name bolded, agent tabs add `repo/branch · elapsed` and a quoted last
message line. Glyphs today: `● ◐ ◑ ○ ✗`. No header, weak active treatment, no
separators, ~32 cols. (Screenshot/recording available.) Everything here is
changeable — treat it as a wireframe, not a constraint.

---

## 8. Deliverables we'd love

- **A state spec:** for each state in §5, the glyph (Nerd Font + plain fallback),
  the color role, and weight/treatment.
- **Row layouts:** plain tab, agent tab (all fields), agent tab (minimal fields),
  multi-agent tab — as monospace mockups at the target width.
- **Active-tab + hierarchy treatment.**
- **Header/separator** design (if any).
- **Width + truncation rules:** target width(s), what truncates first, ellipsis
  style, behavior at very narrow widths.
- **The storyboards in §6** as monospace frame sequences.
- **Overflow behavior** when tabs exceed vertical space.
- Optional: collapsed mode, a spinner glyph sequence, light-theme variant.

Deliver as **monospace text mockups** (in a fixed-width font, so we see exact
columns) — e.g. fenced code blocks — plus notes on color roles and any Nerd Font
glyph codepoints. We'll translate your mockups into the renderer.

---

## 9. Constraints & non-goals

- **Must stay within the character grid** (no graphics). Color + glyph + spacing
  only.
- **Theme-aware:** prefer palette roles over fixed hex; legible on dark and light.
- **Performance:** redraws ~1/sec while agents are active; keep it simple (no
  heavy per-frame animation).
- **Don't redesign the agent-detection or notifications** — only the sidebar's
  look/layout/states/flows.
- Width default ~32 cols, but propose what's right (with a collapsed option).

---

## 10. Reference & inspiration

- **Cmux** (native macOS terminal for AI agents): its vertical tab rail / agent
  list + "needs attention" treatment is the closest inspiration. Note we are a
  *text* rail, not a GPU UI — borrow the information design, not the pixels.
- This repo's `docs/design.md` (technical design — data model, states, the
  per-pane→per-tab aggregation) and `docs/plan.md`.
- Existing terminal status bars for tone: zjstatus, tmux status lines, lualine.

Questions welcome — especially on which states matter most to surface and how far
to push density vs. calm.

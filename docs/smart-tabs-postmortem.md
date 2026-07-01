# smart-tabs meltdown under a many-agent zellij workload — postmortem

**Environment:** zellij 0.44.3, zellij-smart-tabs v0.2.1, macOS.
**Workload:** a single zellij session running many Claude Code agents, each in its own
pane, all producing continuous terminal output. ~30 terminal panes at peak.

## Symptom

- Opening any new pane (the `room` switcher plugin, or a plain Run command pane) took ~5s.
- `zellij pipe` calls timed out.
- The attached client repeatedly crashed/disconnected (the whole window vanished), though
  the server kept running headless.
- At peak it was unrecoverable: `delete-session main --force` reported success but left a
  306MB zombie server process still running, which had to be `kill -9`'d.

## Root cause

smart-tabs' `handle_timer()` calls `get_pane_running_command()` and `get_pane_cwd()` for
every non-command pane, **unconditionally** — it is not gated on whether the configured
format template references `{program}`/`{cwd}`. (Confirmed by reading src: removing
`{program}` from our format changed nothing.)

These are **blocking host calls** into the zellij server. The server services them on its
single main/screen thread — which is the same thread busy parsing the high-volume PTY
output from all the active agent panes. So the host calls can't be answered within the
server's 1s timeout and fail. The plugin re-polls a pane whenever it's "dirty" (i.e.
produced output) at debounce (0.2s) granularity, so continuously-outputting agent panes
get re-polled ~6×/sec each, and `poll_interval` (5s) is only the idle fallback.

The result is a self-feeding storm:

```
agent emits output → pane marked dirty → re-poll (every ~0.2s)
   → blocking get_pane_running_command() → server main thread (busy with that same output)
   → 1s timeout → plugin-exec thread blocked, serialized → more output arrives → repeat
```

The single plugin-exec thread spends ~all its time blocked on timing-out host calls,
starving every other plugin job: tab/status-bar rendering, new plugin/pane creation, and
CliPipe (incoming `zellij pipe`). That's the 5s pane spawns and pipe timeouts. Under enough
backlog the client's message queue overflows — log shows `Client sent over 1000 consecutive
unknown messages, this is probably an infinite loop, logging client out` — and the client
dies.

## Evidence

- 35,225 `… timed out for plugin 0` lines in one server log; sustained ~19/sec.
  `plugin 0` = smart-tabs (sole `load_plugins` entry).
- Storm persisted even at 2 tabs / 3 active panes (~6 poll rounds/sec) → **not a pane-count
  ceiling, it's per-pane-per-output polling.**
- Not memory: freed 8GB (closed Chrome) → storm unchanged.
- Not the server being a hog: zellij server was at 4.7% CPU — it was **starved, not busy**;
  the contention is the single main thread + the 1s blocking host-call timeout.
- Scaling: a poll round ≈ (panes × up to 1s), serialized. At ~30 panes a single round can
  stall the thread for tens of seconds → unbounded backlog → total meltdown. Same bug at 3
  panes, just below the cliff.

## What we did

Removed smart-tabs entirely: deleted it from `load_plugins`, removed the plugin alias,
removed the two `MessagePlugin` "smart-tabs" rename hooks in the keybindings, and stripped
the hook plumbing that pushed per-pane status to it. Agent status (`*`/`?`/`✓`) had been
pushed in via `zellij pipe` from Claude Code hooks; that push path is cheap and fine — it
was smart-tabs' additional polling for naming/program detection that melted down.

## Recommendations for the plugin

1. **Gate the queries on the template.** Only call `get_pane_running_command` /
   `get_pane_cwd` if the active format actually references `program` / `cwd`/`git_root`.
2. **Don't re-poll on output.** A pane producing output rarely changes its running command
   or cwd. Re-resolve only on events that can actually change them (e.g. `CwdChanged`, or a
   `PaneUpdate` manifest delta), not on every dirty tick.
3. **Cache aggressively and cap concurrent in-flight host calls;** never let a poll round
   issue N blocking calls that can each eat 1s on one thread.
4. **Tolerate timeouts without tight retry** so a slow server can't induce a feedback loop.
5. **Prefer push over poll for status** — which is the model we're moving to.

**Net:** it's an architecture/workload mismatch — a poll-every-pane-on-every-change design
against a high-output, many-pane agent workload — not a config bug. No
`poll_interval`/`format` setting avoids it because the polling is unconditional and
output-triggered.

---

## Implications for zj-radar

zj-radar is architecturally immune to this specific meltdown, and the postmortem
crystallizes *why* — these are constraints to preserve, not just happy accidents:

- **Push, not poll (recommendation #5 is already the design).** State arrives via the
  `zj_radar.status.v1` broadcast pipe from Claude/Codex hooks. The plugin never asks the
  server "what's running in this pane?" — the hook tells it. There is no per-pane,
  per-output query path, so there is no storm to feed.
- **No *polled* blocking host calls on the hot path.** The lethal pattern was
  `get_pane_running_command()` / `get_pane_cwd()` re-issued *on every output tick*, per pane —
  unbounded calls driven by output volume. zj-radar must never reintroduce that. Prefer push
  (`PaneUpdate` manifest, `CwdChanged`, hook payload) for anything that changes over a pane's
  life.
  - **Narrow exception (added with cwd-bootstrap naming):** a blocking call is allowed *once
    per pane id, at first sighting*, when it can't be re-triggered by output. The cwd-bootstrap
    path issues `get_pane_cwd` exactly once per new terminal pane to name a freshly-opened tab,
    then lets the `CwdChanged` push path own every change after. It is **gated on attempts, not
    successes** (a pane that returns no cwd is never re-polled), **capped per `PaneUpdate`**
    (`MAX_CWD_BOOTSTRAP_PER_UPDATE`, focused panes first), and **off entirely when naming is
    `Off`**. Call count is bounded by pane-*creation* rate, not output — the exact dimension
    that melted smart-tabs is untouched. This is precisely recommendations #2 (resolve on a
    manifest delta, not a dirty tick) and #3 (cap concurrent in-flight calls). Decision logic
    lives in the pure `RadarState::cwd_bootstrap_targets`; the blocking call itself is isolated
    to `lib.rs::resolve_cwd` (wasm glue). Do not widen this to per-output or uncapped use.
- **The one-shot timer must stay event-gated.** zj-radar re-arms its timer only while there is
  *animating* work (a `Running` glyph that spins) or an un-carried completion edge still to
  settle — a backgrounded `done`/`error`/`pending` row does not keep it awake
  (`PluginRuntime::timer_should_continue`). Even while armed, a tick does no work proportional to
  pane count. Keep it that way; never let a timer tick fan out into N host calls.
- **Output volume is irrelevant to us.** Because we key off discrete hook events
  (UserPromptSubmit / Pre/PostToolUse / Notification / Stop), a pane spewing megabytes of
  output costs us nothing. That is the exact dimension that melted smart-tabs.

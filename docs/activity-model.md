# The activity model: states, classes, and presentation

`design.md` documents the pipeline (how state moves); this documents the
semantics (what a state means and how it looks). Everything here is shipped;
§7 lists what remains.

## 1. The core principle

> **A spinner means bounded work in progress.** Animation is a promise:
> "this will complete, and you will want to know when."

Everything else follows from one observation: the rail used to conflate *"a
foreground process exists"* with *"work is happening."* The user-meaningful
axis is **attention direction** — who is waiting on whom:

- You wait on **it** → activity. Show progress, notify on completion.
- **It** waits on you → attention. Show urgency (pending/error).
- Neither (unending by design, or interactive) → context. Show identity,
  quietly, or nothing. Never a spinner, never a completion promise.

An open editor is not activity. A dev server is not "still running" in any
sense the user needs animated. A `cargo build` is exactly what the spinner
was made for.

## 2. The three orthogonal axes

Every tracked pane's presentation is a function of three independent facts.
Keeping them orthogonal — rather than growing one enum sideways — is what
keeps the model extensible:

| Axis | Type | Values | Who owns it |
|---|---|---|---|
| **Origin** | `ObservationOrigin` (exists) | `StatusPipe` \| `Command` | intake: pushed payload vs `CommandChanged` |
| **Kind** | `Kind` (exists) | Claude, Codex, Opencode, Pi, Gemini, Test, Build, Deploy, Server, Remote, Command, Other | classification (`Kind::from_source` on the pushed source token / `command::classify`) |
| **Class** | semantic vocabulary — **not** a stored or derived Rust type | `Job` \| `Service` \| `Remote` \| `Companion` | this document |

The classes are a semantic model, not an `AttentionClass` enum. No call site
would ever match all four variants: `Companion` is consumed entirely at intake
(§5) and never becomes an observation; `Service` and `Remote` share the
*steady* presentation and its four readers (the glyph split in
`render::running_glyph`, the cadence term `TrackedObservation::animating`,
the run-tag exclusion in `render::run_tag`, and the roll-up tie-break); `Job`
is the default everywhere. So the code follows its existing pattern of
predicates rather than parallel enums (`Kind::is_agent()`,
`Status::{is_active, needs_attention, is_completion}`): the interactive set is
intake policy, and `Kind::is_steady()` (`is_service() || is_remote()`) sits
behind the four readers.

- **`Job`** — bounded work with an end. Kinds: `Test`, `Build`, `Deploy`,
  `Command`, `Other`.
- **`Service`** — unending by design. Kind: `Server` (`npm run dev`,
  `cargo watch`). Completion is not expected; an *exit* is news (often bad).
- **`Remote`** — a live connection to another machine. Kind: `Remote` (`ssh`,
  `mosh`, and their `DEFAULT_REMOTE` siblings — §4). Steady like `Service`
  (nothing is progressing toward a completion the user is waiting on), but
  its ending is phrased as a disconnect and carries its own notify toggle
  (§3, §4) rather than a routine exit.
- **`Companion`** — interactive; it waits on the user. Editors, pagers,
  monitors, git TUIs, file managers, REPLs. Reachable only via the
  interactive classification (§4); it is a class, not a `Kind` — `nvim` and
  `htop` don't need distinct kinds to share a presentation.
- **Agents are outside this axis.** They are push-owned: their producer
  reports `Running`/`Pending`/`Done` directly, so no class needs deriving —
  which is also why "class is a function of Kind" holds (an agent turn being
  a bounded job is the *producer's* contract, not a classification).

## 3. The state × class presentation matrix

`Status` (Idle < Done < Running < Pending < Error, severity-ordered) is
unchanged — it is the *wire and lifecycle* vocabulary and it is correct.
Presentation is `f(Status, class)`:

All cells below are **implemented** (the executable examples live in
`docs/rail-reference.md` — scenarios AD/AE/AF/AG); only the upstream detection
signal (§4 layer 4) remains future work.

| | `Job` | `Service` | `Remote` | `Companion` |
|---|---|---|---|---|
| **Running** | `⠋` spinner + activity string; `· 4m` run tag once ≥ 1 minute (whole minutes, frozen at `1h+`; the true start survives re-promotions) | steady non-animated `▸` mark + name, **no spinner, no fast cadence** | steady `▸` + identity, same as `Service`; a right-aligned `⇄` also marks the tab line while connected | *never enters Running* — suppressed at intake (§5); renders the muted identity label (`○ $ nvim README.md`) |
| **Done** | `●` + ledger hand-off, TTL recede to Idle, notify | exit of a service — `●`/`✗` per exit code, notify: a dead dev server is news | `●` phrased "disconnected", notify (`notify_remote`, independent of `notify_done`); tab flashes; identity label stays so the user sees *which* connection dropped | labeled completion (`● $ nvim`) via preserved identity for held/run panes; recede as Job |
| **Error** | `✗`, persists until re-run, notify | same | `✗` phrased "connection lost", notify (`notify_remote`) | same |
| **Pending** | agent-origin only: `◆` waiting-for-you + wait-age tag | n/a | n/a | n/a |
| **Idle** | muted row if `ever_active` | muted row | muted row, `⇄` marker cleared (the marker tracks *connected*, not *was remote*) | muted identity label while the program is foreground (it also replaces a stale finished-command echo); nothing once it exits |

**Background tasks** (the payload's `tasks`, today Claude only) render as
`┊` sub-lines under their agent's pane line, reusing this matrix's
vocabulary: running work the agent waits on spins with a `· 4m` age, a
running service holds the steady `▸` with no age, and finished tasks show
`●` completed / `✗` failed / muted `○` killed / muted `·` ended (outcome
unknown — never a false green) with their frozen duration. While the agent's
turn is over and it waits on that work, its own pane line holds a steady `⋯`
and the spinner moves down to the task lines (the tab header keeps spinning:
the tab as a whole is still working). Task state never feeds the
roll-up, counts, or notifications: the agent reports its own outcome. At
most three task lines per pane (two plus `┊ +N more` beyond that, failures
sorted first after running work). Below 20 columns, or under height
pressure, a card drops them for a `+N` count on the pane line (finished tasks
included; error-coloured when one failed). The height squeeze order is:
inter-card gaps go, idle tabs fold into the strip, the strip line goes, then
task lines fold, and only then do pane rows compress.

Ties in the tab-level roll-up: on equal severity a bounded job outranks a
steady row (`Service` or `Remote`) as the tab's primary detail — a spinning
build summarizes the tab better than a server or an ssh session that is
merely up.

Cadence rule, restated per class: only *animating* work
(`TrackedObservation::animating` = Running minus steady (`Service` ∪
`Remote`), so agent rows animate too) plus **scheduled
one-shots** keep the 1 Hz timer armed — the one-shots being promotable
pendings awaiting debounce, tentative-Dones awaiting confirm
(`pending_done`), stale-Running grace clocks (`suspect_running`), unsettled
notifications, flashes, and Done-awaiting-recede. The one-shots must count
explicitly *because* the steady exclusion exists: pre-exclusion, "some row
is Running" was an implicit tick source for all of them, and a Ctrl-C'd dev
server still needs its Done confirm (and a killed `server`-source producer
its ghost-expiry) within seconds, not on the ≤60s Slow heartbeat. `Service`,
`Remote`, and `Companion` rows themselves never pin fast cadence — a dev
server, a live ssh session, or an open editor left overnight all cost zero
ticks. Note the long-runner *easing* does not save cadence — an eased
spinner still repaints every 4th tick — so steady-row relief comes only from
the steady mark plus the `animating` term.

One behavior delta reviewers should expect: closing nvim in a shell pane no
longer fires a "done — nvim" notification (no observation ever exists); a
held `zellij run -- nvim` exit still notifies, because pane death is a
genuine event. (A tab holding [agent, nvim] stays a multi-pane tree — the
Interactive label earns a pane line via `earns_pane_line`.)

Notification rule per class: `Job` notifies on Done/Error (the core product);
`Service` notifies on exit (state *changed*, not state *continuing*);
`Companion` never notifies except a labeled exit of a held pane.

## 4. Detection: how a command earns its class

Detection is layered, and the layers are **replaceable classifiers feeding one
mechanism** — this is the anti-cat-and-mouse structure. The mechanism (§5)
never knows how the classification was made.

1. **Identity** (exists): exe name is an agent's wire-source token → agent
   kinds, owned by the push pipe (`AGENT_NAMES`).
2. **Structured argv** (exists): `TOOL_RULES` table → `Test`/`Build`/
   `Deploy`/`Server`. Verb- and word-bounded, sees through `sudo`/`env`/
   wrappers via the single `effective_program` peel — which also means
   *children* classify: `git commit` spawning `$EDITOR` fires its own
   `CommandChanged` and is classified as the editor. `ssh`/`mosh`-family
   launchers (`DEFAULT_REMOTE`) get their own dedicated argv shape
   (`display_remote`, routed ahead of the table since ssh's value-taking
   short options don't fit `ToolRule`'s columns) — `Remote` when the
   destination has no trailing command, `Command` (still a Job) when it does
   (`ssh box cargo build`). `remote_commands` extras are a different, simpler
   contract: matched by program name alone, arguments ignored, exactly like
   `interactive_commands` — an extra is a session launcher, period.
3. **Interactive names** (the issue-#13 fix): a conservative built-in set of
   unambiguous TUIs — editors, pagers, `man`, monitors, git TUIs, file
   managers, `fzf`; the authoritative list is `DEFAULT_INTERACTIVE` in
   `crates/core/src/command.rs` (this doc deliberately doesn't copy it — the
   list grows and a doc copy has no guard) — housed beside `TOOL_RULES`. It
   is classification data, not an ignore list, extended by the
   `interactive_commands` config key (comma/space-separated exe names,
   live-applied via `config.v1`). The config field holds the user's *extras
   only* (default: empty); the effective set is `DEFAULT_INTERACTIVE ∪
   extras`, composed in the setter — because the `config_fields!` macro
   assigns wholesale, a field seeded with the defaults would let
   `interactive_commands "k9s"` silently *replace* them and un-quiet nvim.
   Override semantics stay consistent with every other key
   (wholesale-replace of the extras). Matched on `program_name` after the
   peel.
4. **Terminal signals** (future, upstream): `is_alternate_screen` /
   `is_raw_mode` on Zellij's `PaneInfo` — the genuinely general detector.
   Alt-screen sees through `ssh`/`docker exec -it`; raw-mode covers REPLs
   (`python` bare vs `python train.py` — undecidable by name, decidable by
   termios). Verified absent from zellij-tile 0.44.3 *and* unreleased main;
   Zellij tracks both internally, so this is a feasible contribution. When it
   lands, layer 4 outranks layer 3 and the name list decays into a fallback
   for older Zellij — no state-machine change, it just becomes another
   producer of the same classification.

**Known accepted gaps** (status-quo noise, never broken semantics): bare
REPLs and remote TUIs over ssh until layer 4 exists; Debian-style variant
binaries (`vim.basic`) until configured. A miss is always cosmetic and
self-describing — the noisy row displays the exact string the user pastes
into the config key.

**Four lists, four contracts** — the roles must never be merged:

| List | Meaning | Consulted by |
|---|---|---|
| `IGNORE_NAMES` | "this pane is back at a shell prompt" | `is_shell_prompt` → exit-clears pushed status, arms the Running grace clock |
| `AGENT_NAMES` | "owned by the push pipe; never command-track" | intake suppression + grace-clock cancel; pinned equal to the CLI's adapters |
| interactive set | "a real command that never earns a Running row" | promotion policy only (§5) — **not** consulted by `is_shell_prompt` |
| `DEFAULT_REMOTE` | "this pane's shell is on another machine" | `classify`'s dedicated branch (Kind + `display_remote`) and the intake `remote_commands` extras override |

Putting an editor in `IGNORE_NAMES` would make "opened nvim" read as
"returned to shell" and wrongly exit-clear a finished agent's pushed status.
That conflation is the trap issue #13's reporter flagged. The table's
enforced home is the doc comments in `command.rs` plus a guard test
(`interactive_set_disjoint_from_prompt_and_agent_names`: all four lists
pairwise disjoint, same style as `agent_names_match_push_adapter_sources`) —
a name drifting into two lists silently re-creates the trap.

## 5. The mechanism: quiet pendings

One state-machine change carries the whole model. In `CommandStore`, a
`Pending` entry gains `promotable: bool`:

- **Intake** of an interactive-classified command: arm the
  tentative-Done for any prior Running row (opening nvim after `cargo build`
  finished still flips the build to Done), clear the exit dedup (a re-run's
  exit must apply fresh), and insert a **non-promotable pending** carrying
  the classified identity. The `is_foreground` flag is NOT consulted for an
  interactive argv: a childless interactive ROOT (`zellij run -- nvim`)
  reports `false`, and that report is the editor alive, not a prompt return —
  shell panes can't reach the override because their childless root IS the
  shell, which the ignore set catches.
- **Promotion** (`on_timer`) filters on `promotable` — the Running row never
  materializes.
- **`on_exit` is unchanged** — the surviving pending's identity labels the
  completion, so `zellij run -- nvim` closing reads `● $ nvim`, never a blank
  row. (The identity-less untracked-pane fallback stays load-bearing for
  fast run-pane commands; do not guard it.)
- **`needs_ticks` counts only promotable pendings** — an open
  editor must not pin the 1 Hz timer (the v0.3.1 cadence guarantee).
- Prompt contract, agent grace clocks, prune grace (`tracked_pane_ids`
  already includes pendings): all untouched.

**Level-triggered application.** One function,
`CommandStore::set_interactive_extras` (reached via
`RadarState::set_interactive_commands`): store the composed set + sweep
already-promoted state — demote matching Running command-origin rows to Idle
without ledgering, reconstructing their quiet pending from the observation so
the swept state is identical to the intake state (muted label, labeled exit,
symmetric un-quiet), and re-judge every pending against the new set. Called
from two places — the end of `PluginRuntime::load` and after
`apply_overrides` on the `config.v1` pipe.
`load` assigns config *before* `load_snapshot`, so the single post-load call
covers both a mid-session config change and a stale Running-nvim row
rehydrated from another instance's snapshot; no ordering special cases.

The sweep is what makes the fix apply to already-promoted rows: a mid-session
TUI fires no further `CommandChanged` until it exits, so without it a config
change (or a restart whose snapshot rehydrates the Running row) leaves the
spinner and its cadence pin live until the editor closes. Dropping all
command-origin Running rows and letting re-promotion recover would be unsound:
re-reports are not guaranteed, so a legitimate build row could vanish for good.
A row whose tentative-Done is already armed is exempt; its command has left the
foreground, and the armed confirm flips it Done. Pendings re-judge on the
intake-stamped peeled program name; the observation sweep matches on the
display's first token, which equals the exe basename by construction (guard
test, §7).

Identity-preserving suppression rather than an ignore branch: the preserved
identity is what the labeled exit needs (without it a held `zellij run -- nvim`
exit inserts a blank Done row and a blank notification), and a future
alt-screen classifier produces the identical mark.

**The muted label is a roll-up feature.** Quiet pendings live in
`CommandStore.pending`; `rows()` reads only observations.
`CommandStore::quiet_identity` feeds `roll_up`'s `quiet` lookup, which builds
`PaneDisplay::Interactive`, shown only where no live observation outranks it
and included in the roster via `earns_pane_line`. Promoting quiet commands to
an Idle observation instead would either count an open editor in `done/total`
forever (`ever_active: true`) or never show the identity (`ever_active: false`
renders as Untracked).

## 6. Extension guide

- **New tool classifies wrong** → one `TOOL_RULES` row (kind + display).
- **New TUI shows a spinner** → user: one `interactive_commands` entry,
  live; maintainer: one name in the built-in set if it's ubiquitous.
- **New presentation-relevant distinction** (the `Remote` class is the worked
  example) → a row in the §3 matrix first, then a `Kind` predicate added
  *with its first consumer* (`is_agent()`-style) — never a speculative enum
  ahead of a call site that matches it.
- **New detector** (alt-screen, raw-mode, anything) → a new producer of the
  existing classification; zero changes to stores, roll-up, or render.
- **New pushed agent** → unchanged: `Agent` variant + `AGENT_NAMES` entry;
  the guard test walks you through it.

## 7. Status

Shipped: the quiet-pending mechanism, the built-in interactive set and
`interactive_commands` key with its level-triggered sweep, the Companion muted
label, the Service steady `▸` and cadence exclusion, the long-Job run tag, the
job-over-steady tie-break, and the Remote class (steady mark, tab `⇄` marker,
disconnect notification with its own `notify_remote` toggle, `remote_commands`
extras). Guard tests pin the seams: display first token equals exe basename,
the four name lists pairwise disjoint, and rail-reference scenarios AD/AE/AF/AG.

Remaining: propose `is_alternate_screen`/`is_raw_mode` on Zellij's `PaneInfo`
(§4 layer 4), the general detector for REPLs and TUIs behind ssh/docker.
For `Remote` specifically, this observed-only change has two honest gaps
until the pushed modality lands (a `zj-radar ssh` wrapper reporting the real
exit code — see `docs/troubleshooting.md`): a clean `logout` and a dropped
link are indistinguishable (an ordinary shell pane reports no exit code, so
both notify as a disconnect), and a *hung* ssh shows as connected until it
gives up (no termios/heartbeat signal to detect a stalled link).

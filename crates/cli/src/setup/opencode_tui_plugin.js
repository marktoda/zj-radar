// ZJ_RADAR_OPENCODE_PLUGIN=v2
//
// zj-radar bridge for opencode 2.x — a *TUI* plugin. Vendored by
// `zj-radar setup opencode` into opencode's auto-discovered global plugins dir
// as `~/.config/opencode/plugins/zj-radar/tui.js` (a directory: opencode's TUI
// only discovers plugin directories, and resolves `tui.js` inside them). No
// `opencode.json` edit; a clean uninstall is one directory delete.
//
// WHY THE TUI SIDE (load-bearing — do not port this back to a server plugin):
// opencode 2.x runs one shared, detached background server per machine
// (`opencode serve --service`). Server plugins live in that daemon, whose
// environment is whatever pane happened to launch it first, so from the
// server `$ZELLIJ_PANE_ID` is one stale pane forever (or unset). The TUI runs
// in the pane's own process: its env IS the pane, and it knows which sessions
// it is showing. So the bridge runs here and spawns `zj-radar notify` with
// the pane's env, exactly like a Claude hook does.
//
// Spawn discipline (see CONTEXT.md → Status contract / Bounded sends): each
// event spawns `zj-radar notify opencode --status <s>` with the payload JSON
// on stdin. The `zellij pipe` send is bounded by the CLI path, so the bridge
// adds no second pipe client. Spawns are never concurrent: status edges
// (pending/done/error/idle) and the task-carrying prompt go out strictly
// FIFO, while tool-activity `running` refreshes coalesce to the latest unsent
// one (latest-wins is the project's ordering rule). Each child has a hard
// ~10s kill timer; a rejected child never breaks the queue.
// ASYNC SPAWN ONLY — never a synchronous spawn: this runs inside the TUI's
// event loop, so a wedged rail must not freeze the UI.
//
// The bridge picks the status class (it knows the event); the Rust adapter
// (crates/cli/src/agents/opencode.rs) owns the refinements keyed off the
// payload's `event` field. The `event` names below are zj-radar's own wire
// vocabulary (kept from the 1.x bridge), NOT opencode's event types.
//
// Event shapes are opencode 2.0.x (`@opencode/schema`): every bus event is
// `{ type, data }`; every `session.*` data carries `sessionID`.

// Session cwd (forwarded as `cwd` on every spawn so the rail resolves
// repo/branch without a host probe). Resolved once at setup.
let CWD = "";

// Root sessions this pane created or prompted. On the shared server every
// pane's events arrive here, so an event may paint this pane's row only if
// its root session is one this TUI is *showing right now* (router route or
// an open tab — recomputed per event) or one it *owns*: created or prompted
// from here. Owned roots stay owned until deleted, so a run the user
// switched away from (or backgrounded) keeps reporting to the pane that
// launched it; a session merely browsed via the picker reports only while
// it is on screen, and never hijacks this pane's row afterwards.
const ownedRoots = new Set();

// The last assistant text per root session, so a `session.execution.succeeded`
// (which carries no text) can emit the turn's final assistant text for the
// adapter's trailing-question Done→Pending remap.
const lastAssistantText = new Map();

// Tool call id → { name, root }. Only `session.tool.input.started` carries
// the name; `called` (which carries the input) and `success`/`failed` carry
// just the call `id`. `success`/`failed` are load-bearing: a permission is
// asked inside the tool, between called and success, so success is what
// brings a ◆ back to running. `root` lets a turn's end drop the calls that
// never settled (an interrupted turn), so the map cannot grow unbounded.
const toolNames = new Map();

// `Bun.which` is a PATH scan; probe once and re-probe only while missing, so
// an install that lands mid-session is picked up without a per-event scan.
let zjRadarOnPath = false;

// Queue: edges FIFO, one droppable slot for the latest tool-activity running.
let pendingRunning = null;
const queue = [];
let processing = false;

function enqueue(status, payload) {
  // Only tool-activity refreshes are droppable. The prompt-carrying running
  // becomes the sticky task label — coalescing it away under a backlog would
  // lose the label for the whole turn.
  const droppable = status === "running" && payload.event !== "chat.message";
  if (droppable) {
    pendingRunning = payload;
  } else {
    if (pendingRunning !== null) {
      queue.push({ status: "running", payload: pendingRunning });
      pendingRunning = null;
    }
    queue.push({ status, payload });
  }
  processQueue();
}

async function processQueue() {
  if (processing) return;
  processing = true;
  while (queue.length > 0 || pendingRunning !== null) {
    let item = queue.shift();
    if (item === undefined) {
      item = { status: "running", payload: pendingRunning };
      pendingRunning = null;
    }
    try {
      await notify(item.status, item.payload);
    } catch {}
  }
  processing = false;
}

// Bounded async spawn: write the JSON payload to the child's stdin, then close
// it; a hard kill timer reaps a wedged child so the queue keeps moving.
function notify(status, payload) {
  // Gate: not Bun (a non-Bun host has no Bun.spawn), not under Zellij (the CLI
  // no-ops anyway, but a process per event is a pointless cost), or zj-radar
  // missing from PATH (a partial install must not throw inside the TUI).
  if (typeof Bun === "undefined") return Promise.resolve();
  if (!process.env.ZELLIJ) return Promise.resolve();
  if (!zjRadarOnPath) zjRadarOnPath = Boolean(Bun.which("zj-radar"));
  if (!zjRadarOnPath) return Promise.resolve();

  const data = JSON.stringify({ ...payload, cwd: CWD });
  let child;
  try {
    child = Bun.spawn(["zj-radar", "notify", "opencode", "--status", status], {
      stdin: "pipe",
      stdout: "ignore",
      stderr: "ignore",
    });
  } catch {
    return Promise.resolve(); // spawn failed — never throw in the TUI
  }
  try {
    child.stdin.write(data);
    child.stdin.end();
  } catch {
    // A broken stdin pipe is the child's problem; the kill timer reaps it.
  }
  const timer = setTimeout(() => {
    try { child.kill(); } catch {}
  }, 10_000);
  return child.exited
    .then(() => clearTimeout(timer))
    .catch(() => clearTimeout(timer));
}

// `permission.asked` is the flattened Request — no title; derive one from
// `action` (e.g. "bash") + `resources` (e.g. "cargo test").
function permissionMessage(data) {
  const name = typeof data.action === "string" && data.action ? data.action : "permission";
  const resources = Array.isArray(data.resources) ? data.resources.filter((r) => r !== "*").join(", ") : "";
  return resources ? `${name}: ${resources}` : name;
}

// `form.created`: the built-in `question` tool asks through a Form titled
// "Questions" whose fields carry the question text in `description` (the
// header in `title`). Other forms (MCP elicitation, auth) fall back to the
// form title. An MCP elicitation owned by no session carries the `"global"`
// sentinel as its sessionID; it is not any pane's, so it is dropped by the
// ownership filter rather than painted on every pane.
function formMessage(form) {
  if (!form) return "question";
  const fields = Array.isArray(form.fields) ? form.fields : [];
  const kind = form.metadata && form.metadata.kind;
  if (kind === "question") {
    const first = fields[0];
    if (first && typeof first.description === "string" && first.description) return first.description;
    if (first && typeof first.title === "string" && first.title) return first.title;
  }
  return typeof form.title === "string" && form.title ? form.title : "question";
}

export default {
  id: "zj-radar",
  setup(ctx) {
    CWD = (ctx.location && typeof ctx.location.directory === "string" && ctx.location.directory) || process.cwd();

    function rootOf(sessionID) {
      try {
        const root = ctx.data.session.root(sessionID);
        if (typeof root === "string" && root) return root;
      } catch {}
      return sessionID;
    }

    // Is `root` on screen right now (the router's session, or an open tab)?
    function showing(root) {
      try {
        const route = ctx.ui.router.current();
        if (route && route.type === "session" && typeof route.sessionID === "string" && rootOf(route.sessionID) === root) {
          return true;
        }
      } catch {}
      try {
        for (const tab of ctx.ui.tabs.list()) {
          if (tab && typeof tab.sessionID === "string" && rootOf(tab.sessionID) === root) return true;
        }
      } catch {}
      return false;
    }

    // May this event paint this pane's row? Owned (created/prompted here) or
    // currently showing.
    function concerns(root) {
      return ownedRoots.has(root) || showing(root);
    }

    function endTurn(root) {
      lastAssistantText.delete(root);
      for (const [id, call] of toolNames) {
        if (call.root === root) toolNames.delete(id);
      }
    }

    // Classify one bus event into a (status, payload) send, or nothing.
    function handle(event) {
      const type = event && event.type;
      const data = (event && event.data) || {};
      const sessionID = typeof data.sessionID === "string" ? data.sessionID : (data.form && data.form.sessionID);
      if (typeof sessionID !== "string" || !sessionID) return;
      const root = rootOf(sessionID);
      const isChild = sessionID !== root;
      // Claim a root this pane created or prompted: the TUI navigates to a new
      // session optimistically before the create round-trip, so its
      // `session.created` arrives while the route already points at it; a
      // prompt submitted here arrives as an inbox item while it is on screen.
      if (!isChild && (type === "session.created" || (type === "session.inbox.enqueued" && data.item && data.item.type === "user")) && showing(root)) {
        ownedRoots.add(root);
      }
      if (!concerns(root)) return;

      switch (type) {
        // Needs-you prompts block this TUI whichever session in the family
        // raised them (a subagent's `bash` asks through the parent's UI), so
        // they are never filtered by child-ness. The user answering brings the
        // row back to running now — a denied permission fails the tool, so a
        // `session.tool.success` may never come.
        case "permission.asked":
          enqueue("pending", { event: "permission.ask", message: permissionMessage(data) });
          return;
        case "form.created":
          enqueue("pending", { event: "question.ask", message: formMessage(data.form) });
          return;
        case "permission.replied":
        case "form.replied":
        case "form.cancelled":
          enqueue("running", { event: "needs_you.replied" });
          return;
      }
      // Subagent (task-tool) sessions run their own prompts, tools and
      // executions; a subagent finishing must not paint Done (or clobber the
      // task label) mid-turn.
      if (isChild) return;

      switch (type) {
        // User submitted a prompt → running, with the prompt text for task capture.
        case "session.inbox.enqueued": {
          const item = data.item;
          if (!item || item.type !== "user") return;
          const text = item.payload && typeof item.payload.text === "string" ? item.payload.text.trim() : "";
          lastAssistantText.delete(root);
          enqueue("running", { event: "chat.message", prompt: text });
          return;
        }
        // A run began without a prompt we saw (resumed/attached session, a
        // queued steer) → a plain running refresh.
        case "session.execution.started":
          enqueue("running", { event: "session.execution" });
          return;

        // Tool activity → running, with the live tool action.
        case "session.tool.input.started":
          if (typeof data.id === "string" && typeof data.name === "string") toolNames.set(data.id, { name: data.name, root });
          return;
        case "session.tool.called": {
          const call = toolNames.get(data.id);
          enqueue("running", { event: "tool.execute", tool: call && call.name, tool_input: data.input });
          return;
        }
        case "session.tool.success":
        case "session.tool.failed": {
          const call = toolNames.get(data.id);
          toolNames.delete(data.id);
          enqueue("running", { event: "tool.execute", tool: call && call.name });
          return;
        }

        // Track the final assistant text so succeeded can emit it.
        case "session.text.ended":
          if (typeof data.text === "string") lastAssistantText.set(root, data.text);
          return;

        // Turn complete → done, with the tracked final assistant text (the
        // adapter remaps to pending if it ends in a question).
        case "session.execution.succeeded":
          enqueue("done", { event: "session.idle", message: lastAssistantText.get(root) || "" });
          endTurn(root);
          return;
        // A real failure → error (a signal Claude's hook model lacks).
        case "session.execution.failed": {
          const err = data.error || {};
          enqueue("error", { event: "session.error", message: typeof err.message === "string" ? err.message : "" });
          endTurn(root);
          return;
        }
        // Esc is the user's own action → the turn is over, not broken: Done
        // (1.x parity). `superseded` means a new prompt already took over
        // (its running follows), `shutdown`/`inactivity` mean the run went
        // away → the row recedes.
        case "session.execution.interrupted":
          if (data.reason === "user") {
            enqueue("done", { event: "session.idle", message: "" });
          } else if (data.reason !== "superseded") {
            enqueue("idle", { event: "session.lifecycle" });
          }
          endTurn(root);
          return;

        // Session lifecycle → idle (row recedes; new/deleted session).
        case "session.created":
        case "session.deleted":
          enqueue("idle", { event: "session.lifecycle" });
          endTurn(root);
          if (type === "session.deleted") ownedRoots.delete(root);
          return;
      }
    }

    const types = [
      "permission.asked", "permission.replied",
      "form.created", "form.replied", "form.cancelled",
      "session.inbox.enqueued", "session.execution.started",
      "session.tool.input.started", "session.tool.called", "session.tool.success", "session.tool.failed",
      "session.text.ended",
      "session.execution.succeeded", "session.execution.failed", "session.execution.interrupted",
      "session.created", "session.deleted",
    ];
    const offs = [];
    for (const type of types) {
      try {
        offs.push(ctx.data.on(type, (event) => {
          try { handle(event); } catch {}
        }));
      } catch {}
    }
    return () => {
      for (const off of offs) {
        try { off(); } catch {}
      }
    };
  },
};

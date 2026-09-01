// ZJ_RADAR_OPENCODE_PLUGIN=v1
//
// zj-radar bridge plugin for opencode. vendored by `zj-radar setup opencode`
// into opencode's auto-loaded global plugins dir (~/.config/opencode/plugins/),
// so no `opencode.json` edit is needed and a clean uninstall is one file delete.
//
// Spawn discipline (load-bearing, see CONTEXT.md → Status contract / Bounded
// sends): this bridge spawns `zj-radar notify opencode --status <s>` per event
// with the payload JSON on stdin. The `zellij pipe` send is bounded by the CLI
// path (self_limiting_pipe_argv's watchdog survives this bridge's own death),
// so the bridge adds no second pipe client. Spawns are never concurrent:
// status edges (pending/done/error/idle) and the task-carrying `chat.message`
// go out strictly FIFO, while tool-activity `running` refreshes coalesce to
// the latest unsent one (latest-wins is the project's ordering rule, so a
// stale running is free to drop and can never head-of-line block an edge). Each child has a hard ~10s kill timer as an outer backstop;
// a rejected child never breaks the queue.
// ASYNC SPAWN ONLY — never a synchronous spawn: the plugin runs in opencode's
// process, so a wedged rail must not freeze the TUI event loop.
//
// The bridge picks the status class (it knows the event); the Rust adapter
// (crates/cli/src/agents/opencode.rs) owns the refinements keyed off the
// payload's `event` field.

// Resolved once at plugin init. `directory` is the session cwd (forwarded as
// `cwd` on every spawn so the rail resolves repo/branch without a host probe).
let CWD = "";

// Subagent (task-tool) sessions. opencode forks them with `parentID`, and
// their prompts/tools/idle fire the same hooks and bus events as the user's
// session — so a subagent finishing would paint Done (and clobber the task
// label) mid-turn. Children always announce themselves via `session.created`
// (info.parentID) before anything else, so the bridge learns the set from
// the stream and drops their events; every *other* session is the user's.
// Deliberately NOT a "pin the root" latch: switching to an existing session
// (picker, `-c`, `--session`, attach) fires no `session.created`, and a latch
// would freeze the rail on the old session forever.
const childSessions = new Set();

// The current user message ID: opencode publishes `message.part.updated` for
// the user's own prompt parts too (Part carries no role), so parts of this
// message must never land in `lastAssistantText`.
let lastUserMessageID = null;

// messageID -> role, learned from `message.updated` (which does carry role);
// cleared at each turn boundary so it stays bounded.
let messageRoles = new Map();

// The last assistant text part seen via `message.part.updated`, tracked so a
// `session.idle` (which carries no message text) can emit the turn's final
// assistant text for the adapter's trailing-question Done→Pending remap.
let lastAssistantText = "";

// Queue with latest-wins coalescing for `running` events: running refreshes are
// droppable so a backlog under slow/wedged sends never blocks pending/done edges.
let pendingRunning = null;
let queue = [];
let processing = false;

function enqueue(status, payload) {
  // Only tool-activity refreshes are droppable. The `chat.message` running
  // carries the prompt that becomes the sticky task label — coalescing it
  // away under a backlog would lose the label for the whole turn.
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
    let item;
    if (queue.length > 0) {
      item = queue.shift();
    } else {
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
// it; a hard kill timer reaps a wedged child so the queue keeps moving. Returns
// a promise that resolves when the child exits (cleanly or killed).
function notify(status, payload) {
  // Gate: skip entirely when not running under Zellij (the CLI no-ops anyway,
  // but spawning it pointlessly burns a process per event).
  if (!process.env.ZELLIJ) return Promise.resolve();
  // Gate: skip spawn when zj-radar isn't on PATH (Bun.which returns undefined).
  // Wiring only happens via `zj-radar setup opencode`, which implies the binary
  // — but a partial install or a moved binary must not throw inside the TUI.
  if (!Bun.which("zj-radar")) return Promise.resolve();

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

// Extract the user's prompt text from a chat.message `output.parts` array
// (UserMessage itself carries no text — the prompt is in its TextParts).
function promptText(parts) {
  if (!Array.isArray(parts)) return "";
  return parts
    .filter((p) => p && p.type === "text" && typeof p.text === "string")
    .map((p) => p.text)
    .join("\n")
    .trim();
}

// Extract a human message from opencode's error union ({ name, data: { message? } }).
function errorMessage(error) {
  if (!error) return "";
  if (error.data && typeof error.data.message === "string" && error.data.message) {
    return error.data.message;
  }
  return typeof error.name === "string" ? error.name : "";
}

// Extract a permission description from `permission.asked` properties
// ({ id, sessionID, permission, patterns, metadata, always, tool? } — no title).
function permMessage(props) {
  if (!props) return "Permission requested";
  const name = typeof props.permission === "string" && props.permission ? props.permission : "permission";
  const patterns = Array.isArray(props.patterns) && props.patterns.length > 0
    ? props.patterns.join(", ")
    : "";
  return patterns ? `${name}: ${patterns}` : name;
}

// Whether a payload declares itself a child/subagent session (Session.Info
// carries `parentID`), or names a session already learned as a child.
function isChildSession(props) {
  if (!props) return false;
  if (props.parentID || props.parent_id || (props.info && (props.info.parentID || props.info.parent_id))) {
    return true;
  }
  const sid = eventSession(props);
  return sid !== null && childSessions.has(sid);
}

// The sessionID an event belongs to. Current opencode stamps a top-level
// `sessionID` on every event; older shapes carried only `{ info }` (whose
// session key is `id`) or `{ part }` (which carries its own `sessionID`).
function eventSession(props) {
  if (!props) return null;
  return (
    props.sessionID ||
    props.session_id ||
    (props.info && (props.info.sessionID || props.info.session_id || props.info.id)) ||
    (props.part && props.part.sessionID) ||
    null
  );
}

export const ZjRadarPlugin = async ({ directory }) => {
  CWD = typeof directory === "string" ? directory : "";
  return {
    // User submitted a prompt → running, with the prompt text for task capture.
    "chat.message": async (input, output) => {
      if (!output || isChildSession(input) || isChildSession(output)) return;
      lastAssistantText = "";
      const msgID = output.id || output.messageID || (output.message && output.message.id);
      if (msgID) {
        lastUserMessageID = msgID;
        messageRoles.set(msgID, "user");
      }
      const prompt = promptText(output && (output.parts || (output.message && output.message.parts)));
      enqueue("running", { event: "chat.message", prompt });
    },

    // Tool about to run / just ran → running, with the live tool activity.
    "tool.execute.before": async (input, output) => {
      if (isChildSession(input) || isChildSession(output)) return;
      enqueue("running", {
        event: "tool.execute",
        tool: input && input.tool,
        tool_input: output && output.args,
      });
    },
    "tool.execute.after": async (input) => {
      if (isChildSession(input)) return;
      enqueue("running", {
        event: "tool.execute",
        tool: input && input.tool,
        tool_input: input && input.args,
      });
    },

    // Legacy (< 1.14) permission hook → pending. Current opencode no longer
    // triggers it (the signal is the `permission.asked` bus event below);
    // kept so older installs still get the needs-you moment.
    "permission.ask": async (input) => {
      enqueue("pending", {
        event: "permission.ask",
        message: input && input.title,
      });
    },

    // The catch-all event stream: session lifecycle + assistant-text tracking.
    event: async ({ event }) => {
      const type = event && event.type;
      const props = (event && event.properties) || {};
      const eventSessionID = eventSession(props);

      // Permission prompts block the user's TUI whichever session raised them
      // (a subagent's `bash` asks through the parent's UI), so they are the one
      // class of event that must NOT be filtered by session.
      if (type === "permission.asked") {
        enqueue("pending", { event: "permission.ask", message: permMessage(props) });
        return;
      }
      if (type === "permission.replied") {
        // The user answered → back to running now, rather than on the next
        // tool/idle event (a denied permission throws inside the tool, so
        // `tool.execute.after` may never come).
        enqueue("running", { event: "permission.replied" });
        return;
      }

      // Learn (and forget) subagent sessions from their lifecycle events.
      if ((type === "session.created" || type === "session.updated") && eventSessionID) {
        if (props.parentID || (props.info && props.info.parentID)) childSessions.add(eventSessionID);
      }
      if (type === "session.deleted" && eventSessionID && childSessions.has(eventSessionID)) {
        childSessions.delete(eventSessionID);
        return;
      }
      if (isChildSession(props)) return;

      switch (type) {
        // Track message role updates so we can accurately ignore user parts.
        case "message.updated": {
          const msg = props.message || props.info || props;
          const id = msg && (msg.id || msg.messageID);
          const role = msg && msg.role;
          if (id && role) {
            messageRoles.set(id, role);
          }
          break;
        }
        // Track the latest assistant text part so session.idle can emit it.
        case "message.part.updated": {
          const part = props.part;
          const msgID = (part && part.messageID) || props.messageID || (props.message && props.message.id);
          const role = (part && part.role) || props.role || (props.message && props.message.role) || (msgID && messageRoles.get(msgID));
          if (role === "user" || (msgID && lastUserMessageID && msgID === lastUserMessageID)) {
            break;
          }
          if (part && part.type === "text" && typeof part.text === "string") {
            lastAssistantText = part.text;
          }
          break;
        }
        // Turn complete → done, with the tracked final assistant text (the
        // adapter remaps to pending if it ends in a question).
        case "session.idle":
          enqueue("done", { event: "session.idle", message: lastAssistantText });
          lastAssistantText = "";
          messageRoles.clear();
          break;
        // A real failure signal Claude's hook model lacks → error.
        case "session.error":
          enqueue("error", { event: "session.error", message: errorMessage(props.error) });
          lastAssistantText = "";
          break;
        // Session lifecycle → idle (row recedes; /clear, new/deleted session).
        case "session.created":
        case "session.deleted":
          enqueue("idle", { event: "session.lifecycle" });
          lastAssistantText = "";
          break;
      }
    },
  };
};

export default ZjRadarPlugin;

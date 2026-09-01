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
// stale running is free to drop and can never head-of-line block an edge).
// Each child has a hard ~10s kill timer as an outer backstop; a rejected child
// never breaks the queue.
// ASYNC SPAWN ONLY — never a synchronous spawn: the plugin runs in opencode's
// process, so a wedged rail must not freeze the TUI event loop.
//
// The bridge picks the status class (it knows the event); the Rust adapter
// (crates/cli/src/agents/opencode.rs) owns the refinements keyed off the
// payload's `event` field.
//
// Event shapes are opencode ≥ 1.18 (`packages/schema/src/v1/*.ts`): every bus
// event carries a top-level `sessionID`; `session.*` also carry `info`
// (Session.Info, whose own key is `id`), `message.part.updated` carries `part`.

// Resolved once at plugin init. `directory` is the session cwd (forwarded as
// `cwd` on every spawn so the rail resolves repo/branch without a host probe).
let CWD = "";

// Subagent (task-tool) sessions. opencode forks them with `parentID`, and
// their prompts/tools/idle fire the same hooks and bus events as the user's
// session — so a subagent finishing would paint Done (and clobber the task
// label) mid-turn. Children announce themselves via `session.created`
// (info.parentID) before anything else, so the bridge learns the set from the
// stream and drops their events; every *other* session is the user's.
// Deliberately NOT a "pin the root" latch: switching to an existing session
// (picker, `-c`, `--session`, attach) fires no `session.created`, and a latch
// would freeze the rail on the old session forever.
const childSessions = new Set();

// Sessions already classified (child or not). The one gap in the stream is a
// subagent *resumed* by `task_id` after an opencode restart: no
// `session.created`, and its `chat.message` fires before `session.updated`.
// For a never-seen session, `chat.message` asks the SDK once instead; an
// unreachable SDK classifies it as the user's (today's behavior, never worse).
const classifiedSessions = new Set();
let sdk = null;

async function classify(sessionID) {
  if (typeof sessionID !== "string" || classifiedSessions.has(sessionID)) return;
  classifiedSessions.add(sessionID);
  try {
    const { data } = await sdk.session.get({ sessionID });
    if (data && data.parentID) childSessions.add(sessionID);
  } catch {}
}

// messageID → role for the current turn. Part carries no role, and opencode
// publishes `message.part.updated` for the user's own prompt parts too (and
// for compaction's synthetic user message, which never passes `chat.message`)
// — so only parts of a known-assistant message may become `lastAssistantText`.
const messageRoles = new Map();

// The last assistant text part seen, so a `session.idle` (which carries no
// message text) can emit the turn's final assistant text for the adapter's
// trailing-question Done→Pending remap.
let lastAssistantText = "";

// opencode's `halt()` publishes `session.error` and then sets the session idle
// in the same tick, so a real error is always followed by `session.idle`.
// Latched, that idle must not paint Done over the error; the latch clears on
// the next non-error send (the user's next prompt, a tool, a permission).
let errorLatched = false;

// Queue: edges FIFO, one droppable slot for the latest tool-activity running.
let pendingRunning = null;
const queue = [];
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
  errorLatched = status === "error";
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

// The user's prompt text from a chat.message `output.parts` array
// (UserMessage itself carries no text — the prompt is in its TextParts).
function promptText(parts) {
  if (!Array.isArray(parts)) return "";
  return parts
    .filter((p) => p && p.type === "text" && typeof p.text === "string")
    .map((p) => p.text)
    .join("\n")
    .trim();
}

// A human message from opencode's error union ({ name, data: { message? } }).
function errorMessage(error) {
  if (!error) return "";
  if (error.data && typeof error.data.message === "string" && error.data.message) {
    return error.data.message;
  }
  return typeof error.name === "string" ? error.name : "";
}

// `permission.asked` is the flattened Request — no title; derive one from
// `permission` (e.g. "bash") + `patterns` (e.g. "cargo test").
function permissionMessage(props) {
  const name = typeof props.permission === "string" && props.permission ? props.permission : "permission";
  const patterns = Array.isArray(props.patterns) ? props.patterns.join(", ") : "";
  return patterns ? `${name}: ${patterns}` : name;
}

// `question.asked` is the flattened Request: `questions[].question`.
function questionMessage(props) {
  const first = Array.isArray(props.questions) ? props.questions[0] : null;
  return first && typeof first.question === "string" ? first.question : "question";
}

// The sessionID a bus event belongs to (top-level on every current event;
// `info.id` / `part.sessionID` cover the older wrapped shapes).
function eventSession(props) {
  return props.sessionID || (props.info && props.info.id) || (props.part && props.part.sessionID) || null;
}

function isChild(sessionID) {
  return typeof sessionID === "string" && childSessions.has(sessionID);
}

function endTurn() {
  lastAssistantText = "";
  messageRoles.clear();
}

export const ZjRadarPlugin = async ({ directory, client }) => {
  CWD = typeof directory === "string" ? directory : "";
  sdk = client;
  return {
    // User submitted a prompt → running, with the prompt text for task capture.
    // Fires before the message is stored, so the role is known before any of
    // its parts arrive via `message.part.updated`.
    "chat.message": async (input, output) => {
      const sessionID = input && input.sessionID;
      await classify(sessionID);
      if (!output || isChild(sessionID)) return;
      lastAssistantText = "";
      if (output.message && output.message.id) messageRoles.set(output.message.id, "user");
      enqueue("running", { event: "chat.message", prompt: promptText(output.parts) });
    },

    // Tool about to run / just ran → running, with the live tool activity.
    // `after` is load-bearing: a permission is asked inside the tool, between
    // `before` and `after`, so `after` is what brings a ◆ back to running.
    "tool.execute.before": async (input, output) => {
      if (isChild(input && input.sessionID)) return;
      enqueue("running", { event: "tool.execute", tool: input.tool, tool_input: output && output.args });
    },
    "tool.execute.after": async (input) => {
      if (isChild(input && input.sessionID)) return;
      enqueue("running", { event: "tool.execute", tool: input.tool, tool_input: input.args });
    },

    // The bus: needs-you prompts, session lifecycle, assistant-text tracking.
    event: async ({ event }) => {
      const type = event && event.type;
      const props = (event && event.properties) || {};
      const sessionID = eventSession(props);

      switch (type) {
        // Needs-you prompts block the user's TUI whichever session raised them
        // (a subagent's `bash` asks through the parent's UI), so they are never
        // filtered by session. The user answering brings the row back to
        // running now — a denied permission throws inside the tool, so
        // `tool.execute.after` may never come.
        case "permission.asked":
          enqueue("pending", { event: "permission.ask", message: permissionMessage(props) });
          return;
        case "question.asked":
          enqueue("pending", { event: "question.ask", message: questionMessage(props) });
          return;
        case "permission.replied":
        case "question.replied":
        case "question.rejected":
          enqueue("running", { event: "needs_you.replied" });
          return;

        // Learn subagent sessions from their lifecycle; forget them on delete.
        case "session.created":
        case "session.updated":
          if (!sessionID) break;
          classifiedSessions.add(sessionID);
          if (props.info && props.info.parentID) {
            childSessions.add(sessionID);
            return;
          }
          break;
        case "session.deleted":
          classifiedSessions.delete(sessionID);
          if (childSessions.delete(sessionID)) return;
          break;
      }
      if (isChild(sessionID)) return;

      switch (type) {
        case "message.updated":
          if (props.info && props.info.id && props.info.role) messageRoles.set(props.info.id, props.info.role);
          break;
        // Track the latest assistant text part so session.idle can emit it.
        case "message.part.updated": {
          const part = props.part;
          if (!part || part.type !== "text" || typeof part.text !== "string") break;
          if (messageRoles.get(part.messageID) === "assistant") lastAssistantText = part.text;
          break;
        }
        // Turn complete → done, with the tracked final assistant text (the
        // adapter remaps to pending if it ends in a question). Skipped while
        // an error is latched: that idle is the tail of the error, not a Done.
        case "session.idle":
          if (!errorLatched) enqueue("done", { event: "session.idle", message: lastAssistantText });
          endTurn();
          break;
        // A real failure → error (a signal Claude's hook model lacks). An Esc
        // interrupt also arrives here as `MessageAbortedError`; that is the
        // user's own action, so it falls through to the idle → Done path.
        case "session.error":
          if (props.error && props.error.name === "MessageAbortedError") break;
          enqueue("error", { event: "session.error", message: errorMessage(props.error) });
          endTurn();
          break;
        // Session lifecycle → idle (row recedes; /clear, new/deleted session).
        case "session.created":
        case "session.deleted":
          enqueue("idle", { event: "session.lifecycle" });
          endTurn();
          break;
      }
    },
  };
};

export default ZjRadarPlugin;

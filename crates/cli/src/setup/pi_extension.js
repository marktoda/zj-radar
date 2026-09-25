// ZJ_RADAR_PI_EXTENSION=v1
//
// zj-radar bridge extension for pi (@earendil-works/pi-coding-agent ≥ 0.80.4).
// Vendored by `zj-radar setup pi` into pi's auto-loaded global extensions dir
// (~/.pi/agent/extensions/zj-radar.js, or $PI_CODING_AGENT_DIR/extensions), so
// no settings edit is needed and a clean uninstall is one file delete.
//
// Spawn discipline (load-bearing, see CONTEXT.md → Status contract / Bounded
// sends): this bridge spawns `zj-radar notify pi --status <s>` per event with
// the payload JSON on stdin. The `zellij pipe` send is bounded by the CLI path,
// so the bridge adds no second pipe client. Spawns are never concurrent:
// status edges go out strictly FIFO, while tool-activity `running` refreshes
// coalesce to the latest unsent one (latest-wins is the project's ordering
// rule). Each child has a hard 10 s kill timer; a failed child never breaks
// the queue.
// ASYNC SPAWN ONLY — never a synchronous spawn: the extension runs in pi's
// process, and pi awaits every handler in turn, so handlers enqueue and
// return at once. The one exception is `session_shutdown`, which drains the
// queue (bounded): pi awaits it before tearing the session down — via
// process.exit on `quit`, or in-process on new/resume/fork/reload.
//
// pi loads extensions with moduleCache:false — every runtime (/new, reload)
// gets a fresh instance of this module — so the queue lives on globalThis:
// the old runtime's trailing `settled` can never be overtaken by the new
// runtime's `session.new`.
//
// The bridge picks the status class; the Rust adapter (crates/cli/src/agents/
// pi.rs) owns the refinements keyed off the payload's `event` field, which is
// zj-radar's own vocabulary so a pi API change lands here only.
import { spawn as nodeSpawn } from "node:child_process";

const KILL_AFTER_MS = 10_000;
const DRAIN_BOUND_MS = 1_500;

// The only tool_input keys the Rust side ever reads: `tool_activity` and
// `bash_activity` (crates/cli/src/agents.rs) read `file_path`/`notebook_path`/
// `command`; `normalize_tool_args` (crates/cli/src/agents/pi.rs) renames pi's
// `path` to `file_path` before that lookup. Keep this list in sync with those
// readers — a `write` tool's `content` arg alone can run hundreds of KB, and
// nothing downstream ever looks past this set.
const TOOL_INPUT_KEYS = ["path", "file_path", "notebook_path", "command"];

// Trim a tool call's args down to the keys the Rust adapter reads, dropping
// everything else (large payload fields, non-string values), and cap each
// kept value like the free text below — a heredoc `command` can be as large
// as a `write`'s content, and its readers only look at the head. Non-object
// args (or none) become `undefined` so the field is omitted entirely.
function trimToolInput(args) {
  if (!args || typeof args !== "object" || Array.isArray(args)) return undefined;
  const out = {};
  for (const key of TOOL_INPUT_KEYS) {
    if (typeof args[key] === "string") out[key] = capHead(args[key]);
  }
  return out;
}

// Free-text cap for `prompt`/`message` (and each kept tool arg) before they
// are serialized and piped.
// The Rust side bounds every wire field at MAX_WIRE_FIELD_CHARS (512) anyway;
// this only stops a 20 MB paste from being stringified and written to a child
// on every event. Generous so neither reader loses what it looks at.
const TEXT_CAP = 4096;

// A string slice that never leaves a lone UTF-16 surrogate at either cut:
// JSON.stringify would escape it as `\udXXX`, which serde_json rejects — the
// whole payload would fail to parse and the event would be silently lost.
function sliceWhole(s, start, end) {
  let a = start;
  let b = end;
  if (a > 0 && a < s.length && isLow(s.charCodeAt(a))) a++;
  if (b > a && b < s.length && isLow(s.charCodeAt(b))) b--;
  return s.slice(a, b);
}
function isLow(c) { return c >= 0xdc00 && c <= 0xdfff; }

// The prompt's reader (`task_from_prompt`) takes its first non-empty line:
// keep the head.
function capHead(s) {
  return s.length <= TEXT_CAP ? s : sliceWhole(s, 0, TEXT_CAP);
}

// A message has two readers: the rail's msg (the head) and the trailing-
// question remap (`trailing_question`, the last non-empty line): keep both
// ends, joined on a line break so the tail's last line stays the last line.
function capHeadTail(s) {
  if (s.length <= TEXT_CAP) return s;
  const half = TEXT_CAP / 2;
  return `${sliceWhole(s, 0, half)}\n…\n${sliceWhole(s, s.length - half, s.length)}`;
}

function shared() {
  const key = Symbol.for("zj-radar.pi");
  const s = (globalThis[key] ??= {});
  s.spawn ??= nodeSpawn;
  s.queue ??= [];
  if (s.pendingRunning === undefined) s.pendingRunning = null;
  if (s.processing === undefined) s.processing = null;
  // The last settled status (done/error/idle), for a dialog closed while
  // pi is idle: the row returns to where it was, error included.
  s.lastSettled ??= { status: "idle", message: "" };
  return s;
}

function enqueue(status, payload) {
  const s = shared();
  // Only tool-activity refreshes are droppable; the prompt carries the task.
  if (status === "running" && payload.event === "tool") {
    s.pendingRunning = payload;
  } else {
    if (s.pendingRunning !== null) {
      s.queue.push({ status: "running", payload: s.pendingRunning });
      s.pendingRunning = null;
    }
    s.queue.push({ status, payload });
  }
  if (s.processing === null) s.processing = processQueue(s);
}

async function processQueue(s) {
  while (s.queue.length > 0 || s.pendingRunning !== null) {
    let item = s.queue.shift();
    if (item === undefined) {
      item = { status: "running", payload: s.pendingRunning };
      s.pendingRunning = null;
    }
    try {
      await notify(s, item.status, item.payload);
    } catch {}
  }
  s.processing = null;
}

// Bounded async spawn: write the JSON payload to stdin, close it; a hard kill
// timer reaps a wedged child. Resolves when the child exits, errors, or is
// killed — never rejects.
function notify(s, status, payload) {
  return new Promise((resolve) => {
    let child;
    try {
      child = s.spawn("zj-radar", ["notify", "pi", "--status", status], {
        stdio: ["pipe", "ignore", "ignore"],
      });
    } catch {
      resolve();
      return;
    }
    let timer = null;
    const done = () => {
      if (timer !== null) clearTimeout(timer);
      resolve();
    };
    // ENOENT (zj-radar not on PATH) arrives here asynchronously; an
    // unhandled `error` event would crash pi.
    child.on("error", done);
    child.on("exit", done);
    timer = setTimeout(() => {
      try { child.kill(); } catch {}
      done();
    }, KILL_AFTER_MS);
    // A reaper must never keep pi's process alive on its own.
    timer.unref?.();
    // EPIPE (writing to a child that already exited without reading stdin)
    // arrives asynchronously as an `error` event on child.stdin, not a
    // synchronous throw from write()/end() — unhandled, it's an uncaught
    // exception that tears down pi's whole TUI. Attach before writing.
    if (child.stdin && typeof child.stdin.on === "function") child.stdin.on("error", () => {});
    try {
      child.stdin.write(JSON.stringify(payload));
      child.stdin.end();
    } catch {}
  });
}

// Wait for the queue to empty, at most DRAIN_BOUND_MS.
function drain() {
  const s = shared();
  if (s.processing === null) return Promise.resolve();
  // Deliberately NOT unref'd: pi awaits this, and a sole unref'd timer would
  // let the event loop exit mid-await. It is bounded, so it can't hold pi.
  return Promise.race([s.processing, new Promise((r) => setTimeout(r, DRAIN_BOUND_MS))]);
}

// The joined text parts of an assistant message.
function textOf(message) {
  if (!message || !Array.isArray(message.content)) return "";
  return message.content
    .filter((c) => c && c.type === "text" && typeof c.text === "string")
    .map((c) => c.text)
    .join("\n")
    .trim();
}

export default function (pi) {
  let armed = false;
  let cwd = "";
  // Prompt text stashed by `input` until the run actually starts (`input`
  // fires before model/API-key validation, and another extension can swallow
  // it — sending running there would strand the row).
  let stashedPrompt = null;
  // The run's last assistant message: decides settled vs error vs abort.
  let last = null;

  const send = (status, payload) => {
    if (!armed) return;
    const out = { ...payload, cwd };
    if (typeof out.prompt === "string") out.prompt = capHead(out.prompt);
    if (typeof out.message === "string") out.message = capHeadTail(out.message);
    enqueue(status, out);
  };
  // Every handler is wrapped: a bridge bug must never throw into pi.
  const on = (name, fn) => pi.on(name, (event, ctx) => {
    try {
      return fn(event || {}, ctx || {});
    } catch {}
  });

  on("session_start", (event, ctx) => {
    armed = ctx.mode === "tui" && Boolean(process.env.ZELLIJ);
    cwd = typeof ctx.cwd === "string" && ctx.cwd ? ctx.cwd : process.cwd();
    // A different session (new/resume/fork) must not inherit the previous
    // one's settled state: an idle dialog closed before its first turn would
    // otherwise restore another session's done/error. Only startup (nothing
    // to inherit) and reload (same session) keep it.
    if (event.reason !== "startup" && event.reason !== "reload") {
      shared().lastSettled = { status: "idle", message: "" };
    }
    // Only /new resets the row (Claude wires only SessionStart{clear});
    // startup/resume/fork/reload wait for the first real event.
    if (event.reason === "new") send("idle", { event: "session.new" });
  });

  on("input", (event) => {
    if (event.source === "extension" || typeof event.text !== "string") return;
    if (event.streamingBehavior) {
      // steer/followUp join the current run: no new agent_start is coming.
      send("running", { event: "prompt", prompt: event.text });
    } else {
      stashedPrompt = event.text;
    }
  });

  on("agent_start", () => {
    last = null;
    const payload = { event: "prompt" };
    if (stashedPrompt !== null) payload.prompt = stashedPrompt;
    stashedPrompt = null;
    send("running", payload);
  });

  on("tool_execution_start", (event) => {
    send("running", { event: "tool", tool: event.toolName, tool_input: trimToolInput(event.args) });
  });

  on("message_end", (event) => {
    const m = event.message;
    if (m && m.role === "assistant") last = m;
  });

  on("ui_prompt_start", (event) => {
    const title = typeof event.title === "string" ? event.title.trim() : "";
    send("pending", { event: "ui_prompt.start", message: title || "needs input" });
  });

  on("ui_prompt_end", (_event, ctx) => {
    const idle = typeof ctx.isIdle === "function" ? ctx.isIdle() : false;
    if (!idle) {
      send("running", { event: "ui_prompt.end" });
    } else {
      const { status, message } = shared().lastSettled;
      send(status, { event: "ui_prompt.end", message });
    }
  });

  on("agent_settled", () => {
    const stop = last && last.stopReason;
    let settled;
    if (stop === "error") {
      const message = typeof last.errorMessage === "string" ? last.errorMessage : "";
      settled = { status: "error", message };
      send("error", { event: "error", message });
    } else {
      // An abort (Esc) is the user's own action: done, and no
      // trailing-question remap (blank message).
      const message = stop === "aborted" ? "" : textOf(last);
      settled = { status: "done", message };
      send("done", { event: "settled", message });
    }
    shared().lastSettled = { status: settled.status, message: capHeadTail(settled.message) };
    last = null;
  });

  on("session_shutdown", (event) => {
    if (event.reason === "quit") send("idle", { event: "session.end" });
    // pi awaits this before proceeding: on `quit` it then calls process.exit;
    // on new/resume/fork/reload it tears this session down instead. Either
    // way the drain (bounded) runs first, so a slow child can delay e.g.
    // /new by up to DRAIN_BOUND_MS.
    return drain();
  });
}

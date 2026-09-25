// node --test harness for the vendored pi bridge (crates/cli/src/setup/pi_extension.js).
// Loads a fresh module instance per "runtime" (pi loads extensions with
// moduleCache:false), drives a fake `pi` event bus, and records every
// `zj-radar notify pi` spawn through the shared-state spawn seam.
import { test, beforeEach } from "node:test";
import assert from "node:assert/strict";
import { EventEmitter } from "node:events";
import { copyFileSync, mkdtempSync } from "node:fs";
import { tmpdir } from "node:os";
import { join, dirname } from "node:path";
import { fileURLToPath, pathToFileURL } from "node:url";

const SRC = join(dirname(fileURLToPath(import.meta.url)), "../src/setup/pi_extension.js");
const KEY = Symbol.for("zj-radar.pi");
const DIR = mkdtempSync(join(tmpdir(), "zjr-pi-"));
let loads = 0;

// A fresh ESM instance each call (copied to a unique .mjs so node treats it
// as ESM and never serves it from the module cache).
async function loadRuntime() {
  const file = join(DIR, `ext-${loads++}.mjs`);
  copyFileSync(SRC, file);
  const mod = await import(pathToFileURL(file).href);
  const handlers = new Map();
  const pi = { on: (name, fn) => handlers.set(name, fn) };
  mod.default(pi);
  let idle = true;
  const ctx = { mode: "tui", cwd: "/repo", isIdle: () => idle };
  return {
    ctx,
    setIdle: (v) => { idle = v; },
    emit: (name, event = {}) => handlers.get(name)?.({ type: name, ...event }, ctx),
  };
}

// Fake spawn: records {status, payload}; `hang` keeps the child alive.
let sent;
let behavior;
function fakeSpawn(cmd, args) {
  const child = new EventEmitter();
  let buf = "";
  if (behavior === "epipe") {
    // A real dead child's stdin fires `error` asynchronously — never as a
    // synchronous throw from write()/end() — after the write is attempted.
    const stdin = new EventEmitter();
    stdin.write = (d) => { buf += d; };
    stdin.end = () => {
      queueMicrotask(() => stdin.emit("error", Object.assign(new Error("write EPIPE"), { code: "EPIPE" })));
    };
    child.stdin = stdin;
    child.kill = () => child.emit("exit", null, "SIGTERM");
    queueMicrotask(() => child.emit("exit", 1, null));
    return child;
  }
  child.stdin = { write: (d) => { buf += d; }, end: () => {} };
  child.kill = () => child.emit("exit", null, "SIGTERM");
  if (behavior === "enoent") {
    queueMicrotask(() => child.emit("error", Object.assign(new Error("spawn zj-radar ENOENT"), { code: "ENOENT" })));
    return child;
  }
  assert.equal(cmd, "zj-radar");
  assert.deepEqual(args.slice(0, 3), ["notify", "pi", "--status"]);
  const mode = behavior;
  queueMicrotask(() => {
    sent.push({ status: args[3], payload: JSON.parse(buf) });
    // "slow" keeps each child alive 5 ms so a backlog forms (coalescing,
    // cross-runtime ordering); "hang" never exits (bounded-drain test).
    if (mode !== "hang") setTimeout(() => child.emit("exit", 0, null), mode === "slow" ? 5 : 0);
  });
  return child;
}

const settle = () => new Promise((r) => setTimeout(r, 20));
const events = () => sent.map((s) => `${s.status}:${s.payload.event}`);

beforeEach(() => {
  sent = [];
  behavior = "ok";
  process.env.ZELLIJ = "0";
  globalThis[KEY] = { spawn: fakeSpawn };
});

const assistant = (text, extra = {}) => ({
  message: { role: "assistant", content: [{ type: "text", text }], stopReason: "stop", ...extra },
});

test("prompt → tool → settled", async () => {
  const rt = await loadRuntime();
  rt.emit("session_start", { reason: "startup" });
  rt.emit("input", { text: "fix the flaky test", source: "interactive" });
  rt.setIdle(false);
  rt.emit("agent_start");
  rt.emit("tool_execution_start", { toolCallId: "1", toolName: "read", args: { path: "/repo/a.rs" } });
  rt.emit("message_end", assistant("All green."));
  rt.setIdle(true);
  rt.emit("agent_settled");
  await settle();
  assert.deepEqual(events(), ["running:prompt", "running:tool", "done:settled"]);
  assert.equal(sent[0].payload.prompt, "fix the flaky test");
  assert.equal(sent[0].payload.cwd, "/repo");
  assert.equal(sent[1].payload.tool, "read");
  assert.deepEqual(sent[1].payload.tool_input, { path: "/repo/a.rs" });
  assert.equal(sent[2].payload.message, "All green.");
});

test("tool_input is trimmed to the keys the Rust adapter reads", async () => {
  const rt = await loadRuntime();
  rt.emit("session_start", { reason: "startup" });
  rt.emit("agent_start");
  const content = "x".repeat(500_000);
  rt.emit("tool_execution_start", {
    toolCallId: "1",
    toolName: "write",
    args: { path: "/repo/a.rs", content, encoding: "utf8" },
  });
  await settle();
  assert.deepEqual(events(), ["running:prompt", "running:tool"]);
  assert.deepEqual(sent[1].payload.tool_input, { path: "/repo/a.rs" });
  assert.ok(!("content" in sent[1].payload.tool_input));
});

test("session_start on startup/resume/fork/reload sends nothing", async () => {
  const rt = await loadRuntime();
  for (const reason of ["startup", "resume", "fork", "reload"]) rt.emit("session_start", { reason });
  await settle();
  assert.deepEqual(events(), []);
});

test("input swallowed by another extension, or rejected pre-run, sends nothing", async () => {
  const rt = await loadRuntime();
  rt.emit("session_start", { reason: "startup" });
  rt.emit("input", { text: "hello", source: "interactive" }); // no agent_start follows
  await settle();
  assert.deepEqual(events(), []);
});

test("extension-sourced input is never a task", async () => {
  const rt = await loadRuntime();
  rt.emit("session_start", { reason: "startup" });
  rt.emit("input", { text: "injected", source: "extension" });
  rt.emit("agent_start");
  await settle();
  assert.deepEqual(events(), ["running:prompt"]);
  assert.equal(sent[0].payload.prompt, undefined);
});

test("steer mid-run sends prompt immediately", async () => {
  const rt = await loadRuntime();
  rt.emit("session_start", { reason: "startup" });
  rt.emit("agent_start");
  rt.emit("input", { text: "also update the docs", source: "interactive", streamingBehavior: "steer" });
  await settle();
  assert.deepEqual(events(), ["running:prompt", "running:prompt"]);
  assert.equal(sent[1].payload.prompt, "also update the docs");
});

test("Esc abort is done with a blank message", async () => {
  const rt = await loadRuntime();
  rt.emit("session_start", { reason: "startup" });
  rt.emit("agent_start");
  rt.emit("message_end", assistant("Should I continue?", { stopReason: "aborted" }));
  rt.emit("agent_settled");
  await settle();
  assert.deepEqual(events(), ["running:prompt", "done:settled"]);
  assert.equal(sent[1].payload.message, "");
});

test("provider error is error; a successful retry is done", async () => {
  const rt = await loadRuntime();
  rt.emit("session_start", { reason: "startup" });
  rt.emit("agent_start");
  rt.emit("message_end", assistant("", { stopReason: "error", errorMessage: "429 rate limited" }));
  rt.emit("agent_settled");
  await settle();
  assert.deepEqual(events(), ["running:prompt", "error:error"]);
  assert.equal(sent[1].payload.message, "429 rate limited");
  rt.emit("agent_start");
  rt.emit("message_end", assistant("", { stopReason: "error", errorMessage: "overloaded" }));
  rt.emit("message_end", assistant("Recovered and done."));
  rt.emit("agent_settled");
  await settle();
  assert.equal(events().at(-1), "done:settled");
});

test("dialog while running returns to running", async () => {
  const rt = await loadRuntime();
  rt.emit("session_start", { reason: "startup" });
  rt.setIdle(false);
  rt.emit("agent_start");
  rt.emit("ui_prompt_start", { reason: "ui_prompt", kind: "confirm", title: "Allow rm -rf build?" });
  rt.emit("ui_prompt_end", { reason: "ui_prompt", kind: "confirm" });
  await settle();
  assert.deepEqual(events(), ["running:prompt", "pending:ui_prompt.start", "running:ui_prompt.end"]);
  assert.equal(sent[1].payload.message, "Allow rm -rf build?");
});

test("untitled dialog gets a generic label", async () => {
  const rt = await loadRuntime();
  rt.emit("session_start", { reason: "startup" });
  rt.emit("ui_prompt_start", { reason: "ui_prompt", kind: "custom" });
  await settle();
  assert.equal(sent[0].payload.message, "needs input");
});

test("dialog while idle after an error restores the error", async () => {
  const rt = await loadRuntime();
  rt.emit("session_start", { reason: "startup" });
  rt.emit("agent_start");
  rt.emit("message_end", assistant("", { stopReason: "error", errorMessage: "boom" }));
  rt.emit("agent_settled");
  rt.setIdle(true);
  rt.emit("ui_prompt_start", { reason: "ui_prompt", kind: "select", title: "Pick a model" });
  rt.emit("ui_prompt_end", { reason: "ui_prompt", kind: "select" });
  await settle();
  assert.deepEqual(events().slice(-2), ["pending:ui_prompt.start", "error:ui_prompt.end"]);
  assert.equal(sent.at(-1).payload.message, "boom");
});

test("/new mid-turn keeps cross-runtime ordering", async () => {
  behavior = "slow";
  const a = await loadRuntime();
  // Pre-load b's module before scheduling any of a's (slow) spawns, so
  // nothing below the next block depends on module-load wall-clock timing.
  const b = await loadRuntime();
  a.emit("session_start", { reason: "startup" });
  a.emit("agent_start");
  a.emit("message_end", assistant("partial", { stopReason: "aborted" }));
  a.emit("agent_settled");
  // Fire the shutdown and the next runtime's session_start back-to-back with
  // NO await between them: a's `done:settled` is still sitting in the queue
  // (its slow spawn hasn't resolved) when b's `session.new` is pushed right
  // behind it. A per-module (non-shared) queue would let b's item cut the
  // line — the shared queue must keep it FIFO regardless.
  a.emit("session_shutdown", { reason: "new" });
  b.emit("session_start", { reason: "new" });
  await new Promise((r) => setTimeout(r, 40));
  assert.deepEqual(events(), ["running:prompt", "done:settled", "idle:session.new"]);
});

test("quit sends session.end and drains before resolving", async () => {
  const rt = await loadRuntime();
  rt.emit("session_start", { reason: "startup" });
  rt.emit("agent_start");
  await rt.emit("session_shutdown", { reason: "quit" });
  assert.deepEqual(events(), ["running:prompt", "idle:session.end"]);
});

test("shutdown drain is bounded when a child never exits", async () => {
  const rt = await loadRuntime();
  rt.emit("session_start", { reason: "startup" });
  behavior = "hang";
  rt.emit("agent_start");
  const t0 = Date.now();
  await rt.emit("session_shutdown", { reason: "quit" });
  const took = Date.now() - t0;
  assert.ok(took >= 1400 && took < 3000, `drain took ${took}ms`);
});

test("stdin EPIPE does not crash pi and the queue keeps moving", async () => {
  const rt = await loadRuntime();
  rt.emit("session_start", { reason: "startup" });
  behavior = "epipe";
  rt.emit("agent_start");
  await settle();
  // If the async stdin `error` event went unhandled, node would have raised
  // an uncaught exception by now and failed this test process outright.
  behavior = "ok";
  rt.emit("agent_settled");
  await settle();
  assert.deepEqual(events(), ["done:settled"], "a following event must still send");
});

test("spawn error does not throw or stall the queue", async () => {
  const rt = await loadRuntime();
  rt.emit("session_start", { reason: "startup" });
  behavior = "enoent";
  rt.emit("agent_start");
  await settle();
  behavior = "ok";
  rt.emit("agent_settled");
  await settle();
  assert.deepEqual(events(), ["done:settled"]);
});

test("handlers tolerate malformed events", async () => {
  const rt = await loadRuntime();
  rt.emit("session_start", {});
  for (const [name, ev] of [
    ["input", { text: 42 }],
    ["tool_execution_start", { toolName: null, args: null }],
    ["message_end", { message: { role: "assistant", content: "not-an-array" } }],
    ["message_end", {}],
    ["ui_prompt_start", {}],
    ["agent_settled", undefined],
  ]) {
    assert.doesNotThrow(() => rt.emit(name, ev), name);
  }
  await settle();
});

test("non-tui mode and no ZELLIJ are silent", async () => {
  const rt = await loadRuntime();
  rt.ctx.mode = "json";
  rt.emit("session_start", { reason: "startup" });
  rt.emit("agent_start");
  delete process.env.ZELLIJ;
  const rt2 = await loadRuntime();
  rt2.emit("session_start", { reason: "startup" });
  rt2.emit("agent_start");
  await settle();
  assert.deepEqual(events(), []);
});

test("coalesces tool refreshes but never drops edges", async () => {
  behavior = "slow";
  const rt = await loadRuntime();
  rt.emit("session_start", { reason: "startup" });
  rt.emit("agent_start");
  for (let i = 0; i < 5; i++) rt.emit("tool_execution_start", { toolName: "read", args: { path: `/f${i}` } });
  rt.emit("agent_settled");
  await settle();
  const ev = events();
  assert.equal(ev[0], "running:prompt");
  assert.equal(ev.at(-1), "done:settled");
  assert.ok(ev.filter((e) => e === "running:tool").length <= 2, ev.join(","));
});

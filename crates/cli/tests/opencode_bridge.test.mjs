// node --test harness for the vendored opencode bridges
// (crates/cli/src/setup/opencode_plugin.js, the 1.x server plugin, and
// crates/cli/src/setup/opencode_tui_plugin.js, the 2.x TUI plugin). Both are
// Bun-hosted, so a fake `Bun` global records every `zj-radar notify opencode`
// spawn; each test loads a fresh module instance (the bridges keep
// module-level state).
import { test, beforeEach } from "node:test";
import assert from "node:assert/strict";
import { copyFileSync, mkdtempSync } from "node:fs";
import { tmpdir } from "node:os";
import { join, dirname } from "node:path";
import { fileURLToPath, pathToFileURL } from "node:url";

const SETUP = join(dirname(fileURLToPath(import.meta.url)), "../src/setup");
const DIR = mkdtempSync(join(tmpdir(), "zjr-opencode-"));
let loads = 0;

// A fresh ESM instance each call (copied to a unique .mjs so node treats it
// as ESM and never serves it from the module cache).
async function load(name) {
  const file = join(DIR, `${loads++}-${name.replace(/\.js$/, ".mjs")}`);
  copyFileSync(join(SETUP, name), file);
  return import(pathToFileURL(file).href);
}

let sent;
beforeEach(() => {
  sent = [];
  process.env.ZELLIJ = "0";
  globalThis.Bun = {
    which: (bin) => (bin === "zj-radar" ? "/usr/bin/zj-radar" : undefined),
    spawn: (argv) => {
      assert.deepEqual(argv.slice(0, 4), ["zj-radar", "notify", "opencode", "--status"]);
      let buf = "";
      let exit;
      const exited = new Promise((r) => { exit = r; });
      return {
        stdin: {
          write: (d) => { buf += d; },
          end: () => {
            sent.push({ status: argv[4], payload: JSON.parse(buf) });
            queueMicrotask(() => exit(0));
          },
        },
        kill: () => exit(null),
        exited,
      };
    },
  };
});

// Poll `pred` until true or `ms` elapses (then fail loudly): no fixed sleeps.
async function until(pred, ms = 5000) {
  const t0 = Date.now();
  while (!pred()) {
    if (Date.now() - t0 > ms) throw new Error(`condition not met within ${ms}ms`);
    await new Promise((r) => setTimeout(r, 2));
  }
}

const BIG = "x".repeat(1_000_000);
// A prompt whose cut point would split a surrogate pair: "😀" is two UTF-16
// units, so 4095 ASCII chars put its high half at index 4095.
const SPLIT = `${"a".repeat(4095)}😀${"b".repeat(10_000)}`;
const lone = /[\ud800-\udbff](?![\udc00-\udfff])|(?<![\ud800-\udbff])[\udc00-\udfff]/;

function assertTrimmed(input, expected) {
  assert.deepEqual(Object.keys(input).sort(), Object.keys(expected).sort());
  for (const [k, v] of Object.entries(expected)) assert.equal(input[k], v, k);
}

// ── 1.x server plugin ───────────────────────────────────────────────────────

async function serverPlugin() {
  const mod = await load("opencode_plugin.js");
  const client = { session: { get: async () => ({ data: {} }) } };
  return mod.ZjRadarPlugin({ directory: "/repo", client });
}

test("1.x: tool_input is trimmed to the keys the Rust adapter reads", async () => {
  const hooks = await serverPlugin();
  const args = { filePath: "/p/big.rs", content: BIG, oldString: BIG, newString: BIG, replaceAll: true };
  await hooks["tool.execute.before"]({ tool: "write", sessionID: "s1" }, { args });
  await until(() => sent.length === 1);
  await hooks["tool.execute.after"]({ tool: "write", sessionID: "s1", args });
  await until(() => sent.length === 2);
  for (const { status, payload } of sent) {
    assert.equal(status, "running");
    assert.equal(payload.tool, "write");
    assertTrimmed(payload.tool_input, { filePath: "/p/big.rs" });
  }
});

test("1.x: a huge command is capped at the head; absent args are omitted", async () => {
  const hooks = await serverPlugin();
  await hooks["tool.execute.before"]({ tool: "bash", sessionID: "s1" }, { args: { command: `cargo test ${BIG}` } });
  await until(() => sent.length === 1);
  const cmd = sent[0].payload.tool_input.command;
  assert.ok(cmd.length <= 4096 && cmd.startsWith("cargo test "), `command ${cmd.length}`);
  await hooks["tool.execute.after"]({ tool: "bash", sessionID: "s1" });
  await until(() => sent.length === 2);
  assert.ok(!("tool_input" in sent[1].payload), "missing args leave tool_input out");
});

test("1.x: the prompt and the final message are capped without a lone surrogate", async () => {
  const hooks = await serverPlugin();
  await hooks["chat.message"]({ sessionID: "s1" }, { message: { id: "m1" }, parts: [{ type: "text", text: `fix the flaky test\n${SPLIT}` }] });
  await until(() => sent.length === 1);
  const p = sent[0].payload.prompt;
  assert.ok(p.length <= 4096 && p.startsWith("fix the flaky test\n") && !lone.test(p), `prompt ${p.length}`);

  await hooks.event({ event: { type: "message.updated", properties: { sessionID: "s1", info: { id: "a1", role: "assistant" } } } });
  const text = `${SPLIT}\nShall I push?`;
  await hooks.event({ event: { type: "message.part.updated", properties: { sessionID: "s1", part: { type: "text", messageID: "a1", text } } } });
  await hooks.event({ event: { type: "session.idle", properties: { sessionID: "s1" } } });
  await until(() => sent.length === 2);
  const m = sent[1].payload.message;
  assert.equal(sent[1].status, "done");
  assert.ok(m.length <= 4096 + 3 && !lone.test(m), `message ${m.length}`);
  assert.ok(m.endsWith("\nShall I push?"), "the tail's last line survives for the trailing-question remap");
});

// ── 2.x TUI plugin ──────────────────────────────────────────────────────────

async function tuiPlugin() {
  const mod = await load("opencode_tui_plugin.js");
  const handlers = new Map();
  const ctx = {
    location: { directory: "/repo" },
    data: {
      on: (type, fn) => { handlers.set(type, fn); return () => handlers.delete(type); },
      session: { root: (id) => id },
    },
    ui: {
      router: { current: () => ({ type: "session", sessionID: "s1" }) },
      tabs: { list: () => [] },
    },
  };
  mod.default.setup(ctx);
  return (type, data) => handlers.get(type)?.({ type, data: { sessionID: "s1", ...data } });
}

test("2.x: tool_input is trimmed to the keys the Rust adapter reads", async () => {
  const emit = await tuiPlugin();
  emit("session.tool.input.started", { id: "c1", name: "edit" });
  emit("session.tool.called", { id: "c1", input: { filePath: "/p/mod.rs", oldString: BIG, newString: BIG } });
  await until(() => sent.length === 1);
  assert.equal(sent[0].payload.tool, "edit");
  assertTrimmed(sent[0].payload.tool_input, { filePath: "/p/mod.rs" });

  emit("session.tool.input.started", { id: "c2", name: "shell" });
  emit("session.tool.called", { id: "c2", input: { command: `git status ${BIG}`, description: BIG } });
  await until(() => sent.length === 2);
  const input = sent[1].payload.tool_input;
  assert.deepEqual(Object.keys(input), ["command"]);
  assert.ok(input.command.length <= 4096 && input.command.startsWith("git status "), `command ${input.command.length}`);
});

test("2.x: the prompt and the final message are capped without a lone surrogate", async () => {
  const emit = await tuiPlugin();
  emit("session.inbox.enqueued", { item: { type: "user", payload: { text: `fix the flaky test\n${SPLIT}` } } });
  await until(() => sent.length === 1);
  const p = sent[0].payload.prompt;
  assert.equal(sent[0].payload.event, "chat.message");
  assert.ok(p.length <= 4096 && p.startsWith("fix the flaky test\n") && !lone.test(p), `prompt ${p.length}`);

  emit("session.text.ended", { text: `${SPLIT}\nShall I push?` });
  emit("session.execution.succeeded", {});
  await until(() => sent.length === 2);
  const m = sent[1].payload.message;
  assert.equal(sent[1].status, "done");
  assert.ok(m.length <= 4096 + 3 && !lone.test(m), `message ${m.length}`);
  assert.ok(m.endsWith("\nShall I push?"));
});

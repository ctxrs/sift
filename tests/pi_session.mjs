// Synthetic adapter transport only: no Pi host, model, or workload commands.
import { test } from "node:test";
import assert from "node:assert/strict";
import { readFile, writeFile, mkdtemp, rm, chmod } from "node:fs/promises";
import { tmpdir } from "node:os";
import { join } from "node:path";

const root = new URL("../", import.meta.url);
const runtime = await readFile(new URL("integrations/runtime.js", root), "utf8");
const adapter = await readFile(new URL("integrations/pi_session.js", root), "utf8");
const unix = {skip:process.platform === "win32"};
const event = (command, text = "original output") => ({toolName:"bash", input:{command},
  isError:true, details:{truncation:{truncated:true},opaque:7}, content:[{type:"text",text,custom:9}]});

async function fixture(mode, body) {
  const dir = await mkdtemp(join(tmpdir(), "retok-pi-adapter-"));
  const executable = join(dir, "retok-test"), log = join(dir, "calls.jsonl");
  const handlers = {};
  const calls = async () => (await readFile(log, "utf8").catch(() => "")).trim().split("\n")
    .filter(Boolean).map(line => JSON.parse(line));
  try {
    await writeFile(executable, `#!${process.execPath}
const fs = require("node:fs"), readline = require("node:readline");
const mode = ${JSON.stringify(mode)}, log = ${JSON.stringify(log)};
const session = process.argv.includes("--protocol=session-v1");
const record = row => fs.appendFileSync(log, JSON.stringify(row) + "\\n");
record({kind:"spawn",pid:process.pid,session});
if (session && mode === "old") process.exit(7);
if (session) process.stdout.write('{"version":1,"session":1}\\n');
const lines = readline.createInterface({input:process.stdin});
lines.on("line", line => {
  const item = JSON.parse(line), request = session ? item.request : item;
  record({kind:"request",session,item});
  if (mode === "hang" && session) return;
  const reply = () => {
    const id = item.id + (mode === "wrong-id" ? 1 : 0);
    process.stdout.write(JSON.stringify({version:1,...(session ? {id} : {}),
      text:"compact 🦀",encoding:"raw",input_tokens:100,output_tokens:2,
      ...(mode === "semantic" ? {semantic:true} : mode === "bad-semantic" ? {semantic:"true"} : {})}) + "\\n");
    if (session) process.stdout.write(JSON.stringify({version:1,id,done:true}) + "\\n");
  };
  if (mode === "delay" && session) setTimeout(reply, 80); else reply();
});
`);
    await chmod(executable, 0o700);
    const source = runtime.replace("__RETOK_EXECUTABLE__", JSON.stringify(executable))
      .replace("__RETOK_SOURCE__", '"pi"') + adapter;
    const plugin = (await import(`data:text/javascript;base64,${Buffer.from(source).toString("base64")}`)).default;
    plugin({on:(name, handler) => {handlers[name] = handler;}});
    await body(handlers, calls);
  } finally {
    await handlers.session_shutdown?.();
    const survivors = (await calls()).filter(row => row.kind === "spawn").filter(row => {
      try { process.kill(row.pid, 0); return true; } catch (error) {
        if (error.code !== "ESRCH") throw error;
        return false;
      }
    });
    // Test-owned synthetic processes only; never inspect/kill unrelated peers.
    for (const row of survivors) { try {process.kill(row.pid, "SIGKILL");} catch {} }
    await rm(dir, {recursive:true, force:true});
    assert.deepEqual(survivors, [], "all synthetic children must close after shutdown");
  }
}

test("command metadata travels in each envelope while event metadata stays opaque", unix, async () => {
  await fixture("normal", async (handlers, calls) => {
    const original = event("cat 'file\\name'; true");
    original.content.push({type:"image",data:"AA==",mimeType:"image/png"}, {type:"text",text:"second",custom:10});
    const before = structuredClone(original);
    const patch = await handlers.tool_result(original);
    assert.deepEqual(original, before);
    assert.deepEqual(Object.keys(patch), ["content"]);
    assert.deepEqual(patch.content, [{type:"text",text:"compact 🦀",custom:9}, before.content[1],
      {type:"text",text:"compact 🦀",custom:10}]);
    const requests = (await calls()).filter(row => row.kind === "request");
    assert.deepEqual(requests.map(row => row.item.id), [1,2]);
    assert(requests.every(row => row.item.command === original.input.command));
    assert(requests.every(row => row.item.delivered_view === true));
    assert.deepEqual(requests.map(row => row.item.request), [{version:1,text:"original output"}, {version:1,text:"second"}]);
    await handlers.tool_result(event(undefined));
    const last = (await calls()).filter(row => row.kind === "request").at(-1);
    assert.equal(Object.hasOwn(last.item, "command"), false);
    assert.equal(Object.hasOwn(last.item, "delivered_view"), false);
  });
});

test("contextual busy fallback is original; legacy busy and PowerShell remain one-shot", unix, async () => {
  await fixture("delay", async (handlers, calls) => {
    const first = handlers.tool_result(event("cat fixture"));
    assert.equal(await handlers.tool_result(event("retok proxy -- cat fixture")), undefined);
    const legacy = await handlers.tool_result(event(undefined));
    assert.equal(legacy.content[0].text, "compact 🦀");
    await first;
    const powershell = event("retok proxy -- type fixture"); powershell.toolName = "powershell";
    assert.equal((await handlers.tool_result(powershell)).content[0].text, "compact 🦀");
    const rows = await calls();
    assert.equal(rows.filter(row => row.kind === "spawn").length, 3);
    assert.equal(rows.filter(row => row.kind === "request").length, 3);
    assert(rows.filter(row => row.kind === "request" && !row.session).every(row =>
      !Object.hasOwn(row.item,"command") && !Object.hasOwn(row.item,"delivered_view")));
  });
});

for (const mode of ["old", "wrong-id", "hang", "bad-semantic"]) {
  test(`contextual ${mode} failure never retries through command-blind compaction`, unix, async () => {
    await fixture(mode, async (handlers, calls) => {
      assert.equal(await handlers.tool_result(event("retok run --raw -- cat fixture")), undefined);
      if (mode === "old") {
        assert.equal(await handlers.tool_result(event("retok proxy -- cat fixture")), undefined);
      }
      assert((await calls()).filter(row => row.kind === "spawn").every(row => row.session));
      assert.equal((await calls()).filter(row => row.kind === "spawn").length, 1);
    });
  });
}

test("semantic response is accepted only for an opted-in contextual request", unix, async () => {
  await fixture("semantic", async handlers => {
    const original = event("cargo test");
    const before = structuredClone(original);
    const patch = await handlers.tool_result(original);
    assert.deepEqual(patch, {content:[{...original.content[0],text:"compact 🦀"}]});
    assert.deepEqual(original, before);
    assert.equal(await handlers.tool_result(event(undefined)), undefined);
  });
});

test("combined UTF-8 command bytes include every block; oversized callbacks never spawn", unix, async () => {
  await fixture("normal", async (handlers, calls) => {
    const original = event("🦀".repeat(1024 * 1024));
    original.content.push({type:"text",text:"second"});
    assert.equal(await handlers.tool_result(original), undefined);
    assert.deepEqual(await calls(), []);
    await handlers.session_shutdown();
    assert.equal(await handlers.tool_result(event("cat fixture")), undefined);
    assert.deepEqual(await calls(), []);
  });
});

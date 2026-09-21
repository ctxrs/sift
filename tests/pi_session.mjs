// Synthetic adapter transport only: no Pi host, model, or workload commands.
import { test } from "node:test";
import assert from "node:assert/strict";
import { readFile, writeFile, mkdtemp, rm } from "node:fs/promises";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { pathToFileURL } from "node:url";

const root = new URL("../", import.meta.url);
const runtime = await readFile(new URL("integrations/runtime.js", root), "utf8");
const adapter = await readFile(new URL("integrations/pi_session.js", root), "utf8");
const isolated = {concurrency:false};
const grep = (text = "a.js:1: alpha\nb.js:2: beta\nc.js:3: gamma\n", details) => ({
  toolName:"grep", input:{pattern:"private-pattern",path:"private-path"}, isError:false,
  details, opaque:7, content:[{type:"text",text,custom:9}],
});
const bash = (command = "cargo test", texts = ["original output"]) => ({
  toolName:"bash", input:{command}, isError:true, details:{opaque:7}, opaque:9,
  content:texts.flatMap((text, index) => [
    {type:"text",text,custom:index},
    ...(index === 0 ? [{type:"image",data:"AA==",mimeType:"image/png"}] : []),
  ]),
});

function commonJs(source) {
  for (const [from, to] of [
    ['import { execFile } from "node:child_process";', 'const { execFile } = require("node:child_process");'],
    ['import { spawn } from "node:child_process";', 'const { spawn } = require("node:child_process");'],
    ['import { statSync } from "node:fs";', 'const { statSync } = require("node:fs");'],
    ['import { StringDecoder } from "node:string_decoder";', 'const { StringDecoder } = require("node:string_decoder");'],
    ['export default function sift(pi) {', 'module.exports = function sift(pi) {'],
  ]) {
    assert.equal(source.split(from).length - 1, 1);
    source = source.replace(from, to);
  }
  return source;
}

async function waitFor(check) {
  for (let i = 0; i < 100; i++) {
    const value = await check();
    if (value) return value;
    await new Promise(resolve => setTimeout(resolve, 10));
  }
  assert.fail("timed out waiting for fixture observation");
}

async function fixture(mode, body, moduleType = "module") {
  const dir = await mkdtemp(join(tmpdir(), "sift-pi-adapter-"));
  const previousCwd = process.cwd();
  const executable = process.execPath, program = join(dir, "compact"), log = join(dir, "calls.jsonl");
  const handlers = {};
  const calls = async () => (await readFile(log, "utf8").catch(() => "")).trim().split("\n")
    .filter(Boolean).map(line => JSON.parse(line));
  try {
    await writeFile(program, `const fs = require("node:fs"), readline = require("node:readline");
const mode = ${JSON.stringify(mode)}, log = ${JSON.stringify(log)};
const session = process.argv.includes("--protocol=session-v2");
const record = row => fs.appendFileSync(log, JSON.stringify(row) + "\\n");
record({kind:"spawn",pid:process.pid,session,args:process.argv.slice(2)});
if (session && mode === "old") process.exit(7);
if (session) process.stdout.write('{"version":1,"session":2}\\n');
const lines = readline.createInterface({input:process.stdin});
lines.on("line", line => {
  const item = JSON.parse(line), request = session ? item.request : item;
  const priorRequests = fs.readFileSync(log, "utf8").split("\\n")
    .filter(row => row.includes('"kind":"request"')).length;
  record({kind:"request",session,item});
  if (session && (mode === "hang" || (mode === "hang-once" && priorRequests === 0))) return;
  if (session && mode === "malformed") { process.stdout.write('{"broken":\\n'); return; }
  const reply = () => {
    const id = item.id + (mode === "wrong-id" ? 1 : 0);
    const text = mode === "no-key" ? request.text : "compact 🦀";
    process.stdout.write(JSON.stringify({version:1,...(session ? {id} : {}),text,encoding:"raw",
      input_tokens:100,output_tokens:2,...(session && (mode === "semantic" || mode === "bad-semantic") ? {semantic:true} : {})}) + "\\n");
    if (session) process.stdout.write(JSON.stringify({version:1,id,done:true}) + "\\n");
  };
  if (session && mode === "delay") setTimeout(reply, 80); else reply();
});
`);
    process.chdir(dir);
    const source = runtime.replace("__SIFT_EXECUTABLE__", JSON.stringify(executable))
      .replace("__SIFT_SOURCE__", '"pi"') + adapter;
    let plugin;
    if (moduleType === "commonjs") {
      const installed = join(dir, "index.cjs");
      await writeFile(installed, commonJs(source));
      plugin = (await import(pathToFileURL(installed))).default;
    } else {
      plugin = (await import(`data:text/javascript;base64,${Buffer.from(source).toString("base64")}`)).default;
    }
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
    for (const row of survivors) { try { process.kill(row.pid, "SIGKILL"); } catch {} }
    process.chdir(previousCwd);
    await rm(dir, {recursive:true, force:true});
    assert.deepEqual(survivors, [], "all synthetic children must close after shutdown");
  }
}

test("CommonJS uses one exact session-v2 child for repeated native grep", isolated, async () => {
  await fixture("normal", async (handlers, calls) => {
    handlers.before_agent_start({prompt:"find alpha"});
    assert.equal((await handlers.tool_result(grep())).content[0].text, "compact 🦀");
    assert.equal((await handlers.tool_result(grep())).content[0].text, "compact 🦀");
    const rows = await calls(), spawns = rows.filter(row => row.kind === "spawn");
    assert.equal(spawns.length, 1);
    assert.match(adapter, /spawn\(siftExecutable, \["compact", "--protocol=session-v2",\s*\n\s*"--record-source", "pi", "--record-tool", "pi"\]/);
    assert.deepEqual(spawns[0].args, ["--protocol=session-v2","--record-source","pi","--record-tool","pi"]);
    assert.deepEqual(rows.filter(row => row.kind === "request").map(row => row.item.id), [1,2]);
  }, "commonjs");
});

test("task lifecycle sends only the current prompt and clears it", isolated, async () => {
  await fixture("normal", async (handlers, calls) => {
    const task = "Find `src/alpha.js` and line 42";
    handlers.before_agent_start({prompt:task,systemPrompt:"SYSTEM-SECRET",images:[{data:"IMAGE-SECRET"}]});
    await handlers.tool_result(grep(), {signal:new AbortController().signal});
    let requests = (await calls()).filter(row => row.kind === "request");
    assert.equal(requests[0].item.selection.task, task);
    assert.equal(requests[0].item.selection.path, "private-path");
    assert.deepEqual(Object.keys(requests[0].item), ["id","request","tool","selection"]);
    assert.equal(requests[0].item.tool, "grep");
    const wire = JSON.stringify(requests[0].item);
    for (const absent of ["SYSTEM-SECRET","IMAGE-SECRET","private-pattern",process.cwd()]) {
      assert.equal(wire.includes(absent), false);
    }
    handlers.agent_settled();
    await handlers.tool_result(grep());
    requests = (await calls()).filter(row => row.kind === "request");
    assert.equal(Object.hasOwn(requests[1].item, "selection"), false);
    handlers.before_agent_start({prompt:"replacement task"});
    await handlers.session_shutdown();
    const before = (await calls()).length;
    assert.equal(await handlers.tool_result(grep()), undefined);
    assert.equal((await calls()).length, before);
  });
});

test("passages use exact UTF-8 byte ranges, full coverage, and stable grouping", isolated, async () => {
  await fixture("normal", async (handlers, calls) => {
    handlers.before_agent_start({prompt:"Locate grouped results"});
    const text = Array.from({length:83}, (_, i) => `src/🦀-${i}.js:${i + 1}: value ${i}\r\n`).join("");
    await handlers.tool_result(grep(text));
    const selection = (await calls()).find(row => row.kind === "request").item.selection;
    assert.equal(selection.passages.length, 40);
    assert.deepEqual(selection.passages.map(p => p.id), Array.from({length:40}, (_, i) => `p${i + 1}`));
    const bytes = Buffer.from(text, "utf8");
    assert.equal(selection.passages[0].start, 0);
    assert.equal(selection.passages.at(-1).end, bytes.length);
    selection.passages.forEach((passage, i) => {
      if (i) assert.equal(passage.start, selection.passages[i - 1].end);
      assert.equal(bytes[passage.end - 1], 10);
      assert(Buffer.from(bytes.subarray(passage.start, passage.end)).toString("utf8").endsWith("\n"));
    });
    assert(selection.passages.some(p => bytes.subarray(p.start, p.end).toString("utf8").split("\n").length > 3));
  });
});

test("quoted, backticked, path, numeric, and notice passages are required", isolated, async () => {
  await fixture("normal", async (handlers, calls) => {
    handlers.before_agent_start({prompt:'Find "needle value", `src/alpha.js`, build/target.log, and 42'});
    const text = ["plain unrelated row","contains needle value","path src/alpha.js here",
      "artifact build/target.log here","number 42 here","[20 matches limit reached]"].join("\n") + "\n";
    await handlers.tool_result(grep(text, {matchLimitReached:20,truncation:{truncated:true},linesTruncated:true}));
    const passages = (await calls()).find(row => row.kind === "request").item.selection.passages;
    const bytes = Buffer.from(text);
    const passage = needle => passages.find(p => bytes.subarray(p.start,p.end).toString().includes(needle));
    assert.equal(passage("plain").required, false);
    for (const anchor of ["needle value","src/alpha.js","build/target.log","42","matches limit"]) {
      assert.equal(passage(anchor).required, true, anchor);
    }
  });
});

test("only successful one-text native grep can use the session", isolated, async () => {
  await fixture("normal", async (handlers, calls) => {
    const ignored = [
      {...grep(),toolName:"read"}, {...grep(),toolName:"find"}, {...grep(),toolName:"other"},
      {...grep(),isError:true}, {...grep(),isError:undefined},
      {...grep(),content:[grep().content[0],{type:"image",data:"AA==",mimeType:"image/png"}]},
      {...grep(),content:[{type:"image",data:"AA==",mimeType:"image/png"}]},
    ];
    for (const value of ignored) assert.equal(await handlers.tool_result(value), undefined);
    const shell = bash("grep secret", ["shell text","tail"]);
    const shellBefore = structuredClone(shell), shellPatch = await handlers.tool_result(shell);
    assert.deepEqual(shell, shellBefore);
    assert.deepEqual(shellPatch.content, [{...shell.content[0],text:"compact 🦀"},shell.content[1],
      {...shell.content[2],text:"compact 🦀"}]);
    const power = structuredClone(shell); power.toolName = "powershell";
    assert.equal((await handlers.tool_result(power)).content[0].text, "compact 🦀");
    const original = grep(), before = structuredClone(original);
    assert.equal((await handlers.tool_result(original)).content[0].text, "compact 🦀");
    assert.deepEqual(original, before);
    const rows = await calls();
    const sessionRows = rows.filter(row => row.kind === "request" && row.session);
    assert.equal(sessionRows.length, 3);
    assert.deepEqual(sessionRows.map(row => row.item.tool), ["bash","bash","grep"]);
    assert(sessionRows.slice(0,2).every(row => row.item.command === "grep secret"
      && row.item.delivered_view === true));
    assert.equal(rows.filter(row => row.kind === "request" && !row.session).length, 2);
  });
});

test("Bash keeps contextual views, multi-block metadata, and command-blind fallback", isolated, async () => {
  await fixture("normal", async (handlers, calls) => {
    const original = bash("cat 'file\\name'; true", ["first","second"]), before = structuredClone(original);
    const patch = await handlers.tool_result(original);
    assert.deepEqual(original, before);
    assert.deepEqual(patch.content, [{...original.content[0],text:"compact 🦀"},original.content[1],
      {...original.content[2],text:"compact 🦀"}]);
    const rows = (await calls()).filter(row => row.kind === "request");
    assert.deepEqual(rows.map(row => row.item.id), [1,2]);
    assert(rows.every(row => row.item.tool === "bash" && row.item.command === original.input.command
      && row.item.delivered_view === true && !Object.hasOwn(row.item,"selection")));
    const generic = bash(undefined,["generic"]); delete generic.input.command;
    assert.equal((await handlers.tool_result(generic)).content[0].text, "compact 🦀");
  });
});

test("busy contextual Bash stays original while generic Bash and PowerShell remain one-shot", isolated, async () => {
  await fixture("delay", async (handlers, calls) => {
    const first = handlers.tool_result(bash("cat fixture",["first"]));
    assert.equal(await handlers.tool_result(bash("sift proxy -- cat fixture",["busy"])), undefined);
    const generic = bash(undefined,["generic"]); delete generic.input.command;
    assert.equal((await handlers.tool_result(generic)).content[0].text, "compact 🦀");
    const power = bash("Get-Content fixture",["powershell"]); power.toolName = "powershell";
    assert.equal((await handlers.tool_result(power)).content[0].text, "compact 🦀");
    assert.equal((await first).content[0].text, "compact 🦀");
    const rows = await calls();
    assert.equal(rows.filter(row => row.kind === "request" && row.session).length, 1);
    assert.equal(rows.filter(row => row.kind === "request" && !row.session).length, 2);
  });
});

test("no task stays generic and no-key fallback is exact with no retry", isolated, async () => {
  await fixture("normal", async (handlers, calls) => {
    assert.equal((await handlers.tool_result(grep())).content[0].text, "compact 🦀");
    const item = (await calls()).find(row => row.kind === "request").item;
    assert.equal(Object.hasOwn(item, "selection"), false);
  });
  await fixture("no-key", async (handlers, calls) => {
    handlers.before_agent_start({prompt:"find useful rows"});
    const original = grep(), before = structuredClone(original);
    assert.equal(await handlers.tool_result(original), undefined);
    assert.deepEqual(original, before);
    const rows = await calls();
    assert.equal(rows.filter(row => row.kind === "spawn").length, 1);
    assert.equal(rows.filter(row => row.kind === "request").length, 1);
    assert(rows.find(row => row.kind === "request").item.selection);
  });
});

test("semantic true is accepted only when that request carried selection", isolated, async () => {
  await fixture("semantic", async handlers => {
    handlers.before_agent_start({prompt:"find useful rows"});
    assert.equal((await handlers.tool_result(grep())).content[0].text, "compact 🦀");
  });
  await fixture("bad-semantic", async (handlers, calls) => {
    const original = grep(), before = structuredClone(original);
    assert.equal(await handlers.tool_result(original), undefined);
    assert.deepEqual(original, before);
    assert.equal((await calls()).filter(row => row.kind === "spawn").length, 1);
  });
});

test("cancellation retires only that child and a later request can restart", isolated, async () => {
  await fixture("hang-once", async (handlers, calls) => {
    handlers.before_agent_start({prompt:"find useful rows"});
    const controller = new AbortController(), original = grep(), before = structuredClone(original);
    const pending = handlers.tool_result(original, {signal:controller.signal});
    await waitFor(async () => (await calls()).some(row => row.kind === "request"));
    controller.abort();
    assert.equal(await pending, undefined);
    assert.deepEqual(original, before);
    assert.equal((await handlers.tool_result(grep())).content[0].text, "compact 🦀");
    assert.equal((await calls()).filter(row => row.kind === "spawn").length, 2);
  });
});

for (const mode of ["old","wrong-id","malformed","hang"]) {
  test(`${mode} session failure returns exact original without retry`, isolated, async () => {
    await fixture(mode, async (handlers, calls) => {
      handlers.before_agent_start({prompt:"find useful rows"});
      const original = grep(), before = structuredClone(original);
      assert.equal(await handlers.tool_result(original), undefined);
      assert.deepEqual(original, before);
      assert.equal(await handlers.tool_result(grep()), undefined);
      assert.equal((await calls()).filter(row => row.kind === "spawn").length, 1);
    });
  });
}

test("small, unsafe, and oversized semantic inputs bypass safely", isolated, async () => {
  await fixture("normal", async (handlers, calls) => {
    handlers.before_agent_start({prompt:"find rows"});
    assert.equal((await handlers.tool_result(grep("one\ntwo\n"))).content[0].text, "compact 🦀");
    const missingPath = grep(); delete missingPath.input.path;
    assert.equal((await handlers.tool_result(missingPath)).content[0].text, "compact 🦀");
    let rows = await calls();
    assert(rows.filter(row => row.kind === "request")
      .every(row => !Object.hasOwn(row.item, "selection")));
    const unsafe = grep("one\ntwo\nthree\ud800\n");
    assert.equal(await handlers.tool_result(unsafe), undefined);
    const oversized = grep("x".repeat(8 * 1024 * 1024 + 1));
    assert.equal(await handlers.tool_result(oversized), undefined);
    rows = await calls();
    assert.equal(rows.filter(row => row.kind === "request").length, 2);
  });
});

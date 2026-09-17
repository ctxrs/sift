// Node fixtures for the native adapter; these do not load OpenClaw itself.
import assert from "node:assert/strict";
import { mkdtemp, writeFile, readFile, chmod, rm, access } from "node:fs/promises";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { pathToFileURL } from "node:url";

if (process.platform === "win32") {
  console.log("OpenClaw POSIX executable fixtures skipped on Windows (rewrites disabled).");
  process.exit(0);
}
const root = await mkdtemp(join(tmpdir(), "retok openclaw "));
const executable = join(root, "retok ' 🦄");
const receipt = join(root, "argv.json");
const savedShell = process.env.SHELL;
const adapter = await readFile(new URL("../integrations/openclaw/index.mjs", import.meta.url), "utf8");
let sequence = 0;
let checks = 0;
async function binary(body = "console.log(JSON.stringify({changed:true,command:'rewritten'}));") {
  await writeFile(executable, `#!${process.execPath}\nconst fs = require('node:fs');\n`
    + `fs.writeFileSync(${JSON.stringify(receipt)}, JSON.stringify(process.argv.slice(2)));\n${body}\n`);
  await chmod(executable, 0o700);
}
async function handler(options = {}) {
  const file = join(root, `adapter-${sequence++}.mjs`);
  await writeFile(file, adapter.replaceAll("__RETOK_EXECUTABLE_JSON__", JSON.stringify(executable)));
  const { default: register } = await import(pathToFileURL(file));
  const hooks = [];
  register({ ...options, on: (...args) => hooks.push(args) });
  assert.ok(hooks.length <= 1);
  if (hooks.length) assert.equal(hooks[0][0], "before_tool_call");
  return hooks[0]?.[1];
}
try {
  process.env.SHELL = "/bin/bash";
  assert.equal(await handler({ pluginConfig: { enabled: false } }), undefined);
  const hook = await handler({ config: { enabled: false } }); // Root config is not plugin config.
  assert.equal(typeof hook, "function");
  const marker = join(root, "never execute");
  const command = `git status; touch '${marker}'; $(echo nope)`;
  const params = { command, workdir: "/work space", timeout: 7, ask: "always", security: "allowlist",
    elevated: false, host: "gateway", metadata: { fixture: true } };
  const original = structuredClone(params);
  await binary();
  const result = await hook({ toolName: "exec", params, toolCallId: "fixture-call" });
  assert.deepEqual(result, { params: { ...params, command: "rewritten" } });
  assert.deepEqual(params, original);
  assert.deepEqual(JSON.parse(await readFile(receipt, "utf8")),
    ["rewrite", "--json", "--shell", "posix", "--", command]);
  await assert.rejects(access(marker));
  checks++;

  await rm(receipt);
  const unsupported = [null, { toolName: "read_file", params }, { toolName: "exec", params: [] },
    { toolName: "exec", params: { command: 3 } },
    { toolName: "exec", params: { command: " " } },
    { toolName: "exec", params: { command: "a\0b" } },
    { toolName: "exec", params: { command: "\ud800" } },
    { toolName: "exec", params: { command: "x".repeat(32769) } },
    { toolName: "exec", params: { command: "git status", pty: true } },
    { toolName: "exec", params: { command: "git status", background: true } },
    { toolName: "exec", toolKind: "code_mode_exec", params },
    { toolName: "exec", toolInputKind: "javascript", params }];
  for (const host of ["sandbox", "node", "unknown"]) {
    unsupported.push({ toolName: "exec", params: { ...params, host } });
  }
  for (const event of unsupported) assert.equal(await hook(event), undefined);
  assert.equal(await hook({ toolName: "exec", params }, { toolKind: "code_mode_exec" }), undefined);
  const sandboxHook = await handler({ config: { agents: { defaults: { sandbox: { mode: "all" } } } } });
  assert.equal(await sandboxHook({ toolName: "exec", params: { command: "git status" } }), undefined);
  const nodeHook = await handler({ config: { tools: { exec: { host: "node" } } } });
  assert.equal(await nodeHook({ toolName: "exec", params: { command: "git status" } }), undefined);
  for (const shell of ["/bin/fish", "/bin/sh", "/bin/dash", "/bin/unknown"]) {
    process.env.SHELL = shell;
    assert.equal(await hook({ toolName: "exec", params }), undefined);
  }
  delete process.env.SHELL;
  assert.equal(await hook({ toolName: "exec", params }), undefined);
  await assert.rejects(access(receipt));
  process.env.SHELL = "/bin/bash"; // Re-evaluated per call; failures aren't cached.
  checks++;

  for (const body of ["console.log('not-json')", "console.log('null')", "console.log('[]')",
    "console.log('{}')", "console.log(JSON.stringify({changed:false,command:'wrong'}))",
    "console.log(JSON.stringify({changed:1,command:'wrong'}))",
    "console.log(JSON.stringify({changed:true,command:42}))",
    "console.log(JSON.stringify({changed:true,command:' '}))",
    "console.log(JSON.stringify({changed:true,command:'a\\0b'}))",
    "console.log(JSON.stringify({changed:true,command:'\\ud800'}))",
    "process.stdout.write(Buffer.concat([Buffer.from('{\"changed\":true,\"command\":\"'),Buffer.from([255]),Buffer.from('\"}')]))",
    `console.log(JSON.stringify({changed:true,command:${JSON.stringify(command)}}))`,
    "console.log('x'.repeat(128 * 1024 + 1))",
    "process.stderr.write('x'.repeat(128 * 1024 + 1));console.log(JSON.stringify({changed:true,command:'wrong'}))",
    ...[1, 2, 3, 7].map(status => `console.log(JSON.stringify({changed:true,command:'wrong'}));process.exitCode=${status};`)]) {
    await binary(body);
    assert.equal(await hook({ toolName: "exec", params }), undefined, body);
    assert.deepEqual(params, original);
    checks++;
  }
  await binary("setTimeout(() => {}, 30000);");
  const start = Date.now();
  assert.equal(await hook({ toolName: "exec", params }), undefined);
  assert.ok(Date.now() - start < 6000, "rewrite timeout must be bounded");
  await rm(executable);
  assert.equal(await hook({ toolName: "exec", params }), undefined);
  await binary();
  assert.ok(await hook({ toolName: "exec", params }));
  const controller = new AbortController();
  controller.abort();
  assert.equal(await hook({ toolName: "exec", params }, { abortSignal: controller.signal }), undefined);
  checks++;
  console.log(`OpenClaw adapter: ${checks} fixture checks passed (not native runtime qualification).`);
} finally {
  if (savedShell === undefined) delete process.env.SHELL;
  else process.env.SHELL = savedShell;
  await rm(root, { recursive: true, force: true });
}

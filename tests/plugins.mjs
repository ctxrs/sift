// Run with: node --test tests/plugins.mjs
import { test } from "node:test";
import assert from "node:assert/strict";
import { readFile, writeFile, mkdtemp, rm, chmod } from "node:fs/promises";
import { tmpdir } from "node:os";
import { join } from "node:path";

const root = new URL("../", import.meta.url);
const runtime = await readFile(new URL("integrations/runtime.js", root), "utf8");
async function plugin(host, executable) {
  const source = runtime.replace("__RETOK_EXECUTABLE__", JSON.stringify(executable))
    .replace("__RETOK_SOURCE__", JSON.stringify(host))
    + await readFile(new URL(`integrations/${host}.js`, root), "utf8");
  return (await import(`data:text/javascript;base64,${Buffer.from(source).toString("base64")}`)).default;
}
async function fixture(mode, body) {
  const dir = await mkdtemp(join(tmpdir(), "retok-plugin-test-"));
  try {
    const executable = join(dir, "retok-test");
    await writeFile(executable, `#!${process.execPath}
let input = "";
process.stdin.setEncoding("utf8");
process.stdin.on("data", data => input += data);
process.stdin.on("end", () => {
  const mode = ${JSON.stringify(mode)};
  if (mode === "exit") process.exit(7);
  if (mode === "malformed") { process.stdout.write("not-json\\n"); return; }
  for (const line of input.trimEnd().split("\\n")) {
    const request = JSON.parse(line);
    process.stdout.write(JSON.stringify({version: 1, encoding:"text-runs-v1",
      text: mode === "unchanged" ? request.text : "framed(" + request.text + ")",
      input_tokens:10, output_tokens:mode === "expansion" ? 11 : 4}) + "\\n");
  }
});
`);
    await chmod(executable, 0o700);
    await body(executable);
  } finally { await rm(dir, {recursive:true, force:true}); }
}
const unix = {skip:process.platform === "win32"};

test("Pi replaces only text, preserving error flag, details, image, and original command", unix, async () => {
  await fixture("valid", async executable => {
    const handlers = {};
    (await plugin("pi", executable))({on:(name, fn) => handlers[name] = fn});
    assert.deepEqual(Object.keys(handlers), ["tool_result"]);
    const image = {type:"image", data:"AA==", mimeType:"image/png"};
    const event = {toolName:"bash", input:{command:"native command | tail -1"}, isError:true,
      content:[{type:"text", text:"failed\r\n", custom:7},image,{type:"text",text:"more"}],
      details:{exitCode:9}};
    const saved = structuredClone(event);
    const patch = await handlers.tool_result(event);
    assert.deepEqual(event, saved);
    assert.deepEqual(Object.keys(patch), ["content"]);
    assert.deepEqual(patch.content, [{type:"text",text:"framed(failed\r\n)",custom:7},image,
      {type:"text",text:"framed(more)"}]);
    assert.equal(await handlers.tool_result({...event,toolName:"read"}), undefined);
  });
});

for (const host of ["opencode", "kilo"]) {
  test(`${host} changes only final output field`, unix, async () => {
    await fixture("valid", async executable => {
      const exported = await plugin(host, executable);
      const hooks = await (typeof exported === "function" ? exported() : exported.server());
      assert.deepEqual(Object.keys(hooks), ["tool.execute.after"]);
      const input = {tool:"bash",args:{command:"echo original"}};
      const output = {title:"title",output:"result",metadata:{exit:3}};
      await hooks["tool.execute.after"](input, output);
      assert.equal(output.output, "framed(result)");
      assert.deepEqual(input, {tool:"bash",args:{command:"echo original"}});
      assert.deepEqual(output.metadata, {exit:3});
      assert.equal(output.title, "title");
      await hooks["tool.execute.after"]({tool:"read"}, output);
      assert.equal(output.output, "framed(result)");
    });
  });
}

for (const mode of ["exit", "malformed", "expansion", "unchanged"]) {
  test(`compression ${mode} leaves result intact`, unix, async () => {
    await fixture(mode, async executable => {
      const hooks = await (await plugin("opencode", executable))();
      const output = {title:"t",output:"untouched",metadata:{exit:0}};
      await hooks["tool.execute.after"]({tool:"bash"}, output);
      assert.deepEqual(output, {title:"t",output:"untouched",metadata:{exit:0}});
    });
  });
}

test("missing executable and oversized text fail open", async () => {
  const hooks = await (await plugin("opencode", join(tmpdir(), "missing-retok-executable")))();
  for (const text of ["unchanged", "x".repeat(8 * 1024 * 1024 + 1)]) {
    const output = {output:text};
    await hooks["tool.execute.after"]({tool:"bash"}, output);
    assert.equal(output.output, text);
  }
});

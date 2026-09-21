// One Pi-session-owned child. Contextual requests never fall back without context.
import { spawn } from "node:child_process";
import { statSync } from "node:fs";
import { StringDecoder } from "node:string_decoder";

function createPiSession() {
  const LIMIT = 8 * 1024 * 1024, OUTPUT = 32 * 1024 * 1024;
  let current = null, busy = false, stopped = false, disabled = null, sequence = 0;
  function refs(slot, active) {
    for (const handle of [slot.child, slot.child.stdin, slot.child.stdout, slot.child.stderr]) {
      handle?.[active ? "ref" : "unref"]?.();
    }
  }
  function retire(slot, force = false) {
    if (!slot) return Promise.resolve();
    if (slot.retiring) return slot.retiring;
    clearTimeout(slot.idle);
    slot.dead = true;
    slot.pending?.finish(null);
    refs(slot, true);
    slot.retiring = new Promise(resolve => {
      let term, kill, end;
      const finish = () => {
        clearTimeout(term); clearTimeout(kill); clearTimeout(end);
        slot.child.removeListener("close", finish);
        refs(slot, false);
        if (slot.closed && current === slot) current = null;
        if (!slot.closed) disabled = slot.key;
        resolve();
      };
      const signal = name => { try { slot.child.kill(name); } catch {} };
      if (slot.closed) { finish(); return; }
      slot.child.once("close", finish);
      try { slot.child.stdin.end(); } catch {}
      if (force) signal("SIGTERM");
      term = setTimeout(() => signal("SIGTERM"), 100);
      kill = setTimeout(() => signal("SIGKILL"), 500);
      end = setTimeout(finish, 750);
    });
    return slot.retiring;
  }
  function idle(slot) {
    if (slot.dead) return;
    refs(slot, false);
    slot.idle = setTimeout(() => { void retire(slot); }, 30_000);
    slot.idle.unref();
  }
  function start(key) {
    const child = spawn(siftExecutable, ["compact", "--protocol=session-v1",
      "--record-source", "pi", "--record-tool", "bash"], {windowsHide:true});
    const slot = {child, key, pending:null, buffer:"", decoder:new StringDecoder("utf8"),
      ready:false, dead:false, closed:false, stderr:0, idle:null, retiring:null};
    current = slot;
    const fail = () => {
      if (!slot.ready) disabled = key;
      void retire(slot, true);
    };
    child.on("error", fail);
    child.stdin.on("error", fail);
    child.stderr.on("error", fail);
    child.stdout.on("error", fail);
    child.on("close", () => {
      slot.closed = true;
      if (!slot.dead) fail();
      if (current === slot) current = null;
    });
    child.stderr.on("data", chunk => {
      slot.stderr += chunk.length;
      if (slot.stderr > OUTPUT) fail();
    });
    child.stdout.on("data", chunk => {
      if (slot.dead) return;
      const pending = slot.pending;
      if (!pending) { fail(); return; }
      pending.bytes += chunk.length;
      if (pending.bytes > OUTPUT) { fail(); return; }
      slot.buffer += slot.decoder.write(chunk);
      try {
        let newline;
        while ((newline = slot.buffer.indexOf("\n")) !== -1) {
          const line = slot.buffer.slice(0, newline);
          slot.buffer = slot.buffer.slice(newline + 1);
          const item = JSON.parse(line);
          if (!slot.ready) {
            if (item.version !== 1 || item.session !== 1 || Object.keys(item).length !== 2) throw Error();
            slot.ready = true;
            continue;
          }
          const index = pending.results.length;
          if (item.version !== 1 || !Number.isSafeInteger(item.id)
            || item.id !== pending.ids[index]) throw Error();
          if (pending.response) {
            if (item.done !== true || Object.keys(item).length !== 3) throw Error();
            pending.results.push(pending.response.text);
            pending.response = null;
          } else {
            if (typeof item.text !== "string" || !Number.isSafeInteger(item.input_tokens)
              || !Number.isSafeInteger(item.output_tokens) || item.output_tokens < 0
              || item.output_tokens > item.input_tokens || item.done !== undefined) throw Error();
            pending.response = item;
          }
        }
        if (pending.results.length === pending.ids.length) {
          if (slot.buffer.length || slot.decoder.lastNeed) throw Error();
          pending.finish(pending.results);
        }
      } catch { fail(); }
    });
    return slot;
  }
  return {
    async compact(texts, tool, command) {
      if (stopped) return texts;
      if (tool !== "bash") return compactTexts(texts, tool);
      const fallback = () => command === undefined ? compactTexts(texts, tool) : texts;
      if (busy) return fallback();
      if (!texts.length || texts.some(t => typeof t !== "string")
        || texts.reduce((n,t) => n + Buffer.byteLength(t)
          + (command === undefined ? 0 : Buffer.byteLength(command)), 0) > LIMIT) return texts;
      busy = true;
      const started = performance.now();
      try {
        const stat = statSync(siftExecutable, {bigint:true});
        const key = JSON.stringify([String(stat.dev), String(stat.ino), String(stat.size),
          String(stat.mtimeNs), process.cwd(), Object.entries(process.env).sort()]);
        if (key === disabled) return await fallback();
        if (current && (current.key !== key || current.dead)) await retire(current);
        if (stopped) return texts;
        if (current?.dead) return await fallback();
        const slot = current || start(key);
        clearTimeout(slot.idle);
        refs(slot, true);
        slot.stderr = 0;
        if (sequence + texts.length > Number.MAX_SAFE_INTEGER) { await retire(slot); return texts; }
        const ids = texts.map(() => ++sequence);
        return await new Promise(resolve => {
          let settled = false;
          const timer = setTimeout(() => { void retire(slot, true); }, Math.max(0, 3000 - (performance.now() - started)));
          slot.pending = {ids, results:[], response:null, bytes:0, finish(value) {
            if (settled) return;
            settled = true;
            clearTimeout(timer);
            slot.pending = null;
            if (!slot.dead) idle(slot);
            resolve(value || texts);
          }};
          try {
            slot.child.stdin.write(texts.map((text,i) => JSON.stringify({id:ids[i],
              request:{version:1,text}, ...(command === undefined ? {} : {command})})).join("\n") + "\n");
          } catch { void retire(slot, true); }
        });
      } catch {
        await retire(current, true);
        return texts;
      } finally { busy = false; }
    },
    async shutdown() {
      stopped = true;
      await retire(current);
    },
  };
}


export default function sift(pi) {
  const session = true ? createPiSession() : null;
  if (session) pi.on("session_shutdown", () => session.shutdown());
  pi.on("tool_result", async event => {
    if (!["bash", "powershell"].includes(event.toolName) || !Array.isArray(event.content)) return;
    const indices = [];
    const texts = [];
    event.content.forEach((block, index) => {
      if (block?.type === "text" && typeof block.text === "string") {
        indices.push(index);
        texts.push(block.text);
      }
    });
    const command = event.toolName === "bash" && typeof event.input?.command === "string"
      ? event.input.command : undefined;
    const compacted = await (session ? session.compact(texts, event.toolName, command) : compactTexts(texts, event.toolName));
    if (compacted.every((text, i) => text === texts[i])) return;
    const content = event.content.slice();
    indices.forEach((index, i) => { content[index] = {...content[index], text: compacted[i]}; });
    return {content};
  });
}

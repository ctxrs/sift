// One Pi-session-owned child handles native grep; ordinary tools stay lossless.
import { spawn } from "node:child_process";
import { statSync } from "node:fs";
import { StringDecoder } from "node:string_decoder";

function safeUtf8(text) {
  for (let i = 0; i < text.length; i++) {
    const code = text.charCodeAt(i);
    if (code >= 0xd800 && code <= 0xdbff) {
      if (i + 1 === text.length) return false;
      const next = text.charCodeAt(++i);
      if (next < 0xdc00 || next > 0xdfff) return false;
    } else if (code >= 0xdc00 && code <= 0xdfff) return false;
  }
  return true;
}

function taskAnchors(task) {
  const anchors = new Set();
  for (const expression of [/"([^"\r\n]+)"/g, /'([^'\r\n]+)'/g, /`([^`\r\n]+)`/g]) {
    for (const match of task.matchAll(expression)) anchors.add(match[1]);
  }
  for (const token of task.match(/[^\s"'`<>(){}\[\],;]+/g) || []) {
    const value = token.replace(/[.:!?]+$/u, "");
    if (value.length > 1 && (/[\\/]/.test(value)
      || /^[\w@+.-]+\.[\p{L}][\p{L}\p{N}]{0,9}$/u.test(value))) anchors.add(value);
  }
  for (const match of task.matchAll(/(?:^|[^\p{L}\p{N}_])(\d+(?:\.\d+)*)(?=$|[^\p{L}\p{N}_])/gu)) {
    anchors.add(match[1]);
  }
  return [...anchors];
}

function semanticSelection(text, task, details, path, limit) {
  if (typeof task !== "string" || !task.trim() || !safeUtf8(text) || !safeUtf8(task)
    || task.includes("\0") || Buffer.byteLength(task, "utf8") > 16 * 1024
    || typeof path !== "string" || !path || !safeUtf8(path) || path.includes("\0")
    || Buffer.byteLength(path, "utf8") > 4096
    || Buffer.byteLength(text, "utf8") + Buffer.byteLength(task, "utf8") > limit) return null;
  const lines = [];
  let character = 0, byte = 0;
  while (character < text.length) {
    if (lines.length >= 100_000) return null;
    const newline = text.indexOf("\n", character);
    const endCharacter = newline === -1 ? text.length : newline + 1;
    const value = text.slice(character, endCharacter);
    const end = byte + Buffer.byteLength(value, "utf8");
    lines.push({start:byte, end, startCharacter:character, endCharacter,
      useful:value.trim().length > 0});
    character = endCharacter;
    byte = end;
  }
  const units = [];
  let first = 0;
  for (let i = 0; i < lines.length; i++) {
    if (lines[i].useful) {
      units.push({first, last:i});
      first = i + 1;
    }
  }
  if (units.length < 3) return null;
  if (first < lines.length) units.at(-1).last = lines.length - 1;
  const count = Math.min(40, units.length), anchors = taskAnchors(task);
  const notice = details?.truncation?.truncated === true
    || (Number.isSafeInteger(details?.matchLimitReached) && details.matchLimitReached > 0)
    || details?.linesTruncated === true;
  const passages = [];
  for (let i = 0; i < count; i++) {
    const firstUnit = units[Math.floor(i * units.length / count)];
    const lastUnit = units[Math.floor((i + 1) * units.length / count) - 1];
    const startLine = lines[firstUnit.first], endLine = lines[lastUnit.last];
    const value = text.slice(startLine.startCharacter, endLine.endCharacter);
    passages.push({id:`p${i + 1}`, start:startLine.start, end:endLine.end,
      required:(notice && i === count - 1) || anchors.some(anchor => value.includes(anchor))});
  }
  return {policy:"sift-semantic-v1", task, path, kind:"pi-grep-v1", passages};
}

function createPiSession() {
  const LIMIT = 8 * 1024 * 1024, OUTPUT = 32 * 1024 * 1024;
  let current = null, busy = false, stopped = false, disabled = null, sequence = 0;
  function refs(slot, active) {
    for (const handle of [slot.child, slot.child.stdin, slot.child.stdout, slot.child.stderr]) {
      handle?.[active ? "ref" : "unref"]?.();
    }
  }
  function retire(slot, force = false, disable = false) {
    if (!slot) return Promise.resolve();
    if (disable) disabled = slot.key;
    if (slot.retiring) return slot.retiring;
    clearTimeout(slot.idle);
    slot.dead = true;
    refs(slot, true);
    const pending = slot.pending;
    slot.retiring = new Promise(resolve => {
      let term, kill, end, finished = false;
      const finish = () => {
        if (finished) return;
        finished = true;
        clearTimeout(term); clearTimeout(kill); clearTimeout(end);
        slot.child.removeListener("close", finish);
        refs(slot, false);
        if (current === slot) current = null;
        pending?.finish(null);
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
    const child = spawn(siftExecutable, ["compact", "--protocol=session-v2",
      "--record-source", "pi", "--record-tool", "pi"], {windowsHide:true});
    const slot = {child, key, pending:null, buffer:"", decoder:new StringDecoder("utf8"),
      ready:false, dead:false, closed:false, stderr:0, idle:null, retiring:null};
    current = slot;
    const fail = () => { void retire(slot, true, true); };
    child.on("error", fail);
    child.stdin.on("error", fail);
    child.stderr.on("error", fail);
    child.stdout.on("error", fail);
    child.on("close", () => {
      slot.closed = true;
      if (!slot.dead) fail();
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
            if (item.version !== 1 || item.session !== 2 || Object.keys(item).length !== 2) throw Error();
            slot.ready = true;
            continue;
          }
          if (item.version !== 1 || !Number.isSafeInteger(item.id) || item.id !== pending.id) throw Error();
          if (pending.response) {
            if (item.done !== true || Object.keys(item).length !== 3
              || slot.buffer.length || slot.decoder.lastNeed) throw Error();
            const result = pending.response.text;
            pending.response = null;
            pending.finish({ok:true, value:result});
            return;
          }
          if (typeof item.text !== "string" || !Number.isSafeInteger(item.input_tokens)
            || !Number.isSafeInteger(item.output_tokens) || item.output_tokens < 0
            || item.output_tokens > item.input_tokens || item.done !== undefined
            || (item.semantic !== undefined && (item.semantic !== true || !pending.contextual))) throw Error();
          pending.response = item;
        }
      } catch { fail(); }
    });
    return slot;
  }
  function request(slot, id, envelope, contextual, abortSignal, deadline) {
    return new Promise(resolve => {
      let settled = false;
      const cancel = () => { void retire(slot, true, false); };
      const timeout = () => { void retire(slot, true, true); };
      const timer = setTimeout(timeout, Math.max(0, deadline - performance.now()));
      const finish = value => {
        if (settled) return;
        settled = true;
        clearTimeout(timer);
        abortSignal?.removeEventListener?.("abort", cancel);
        if (slot.pending?.id === id) slot.pending = null;
        resolve(value || {ok:false});
      };
      slot.pending = {id, response:null, bytes:0, contextual, finish};
      abortSignal?.addEventListener?.("abort", cancel, {once:true});
      if (abortSignal?.aborted) { cancel(); return; }
      try { slot.child.stdin.write(JSON.stringify(envelope) + "\n"); }
      catch { void retire(slot, true, true); }
    });
  }
  return {
    selection(text, task, details, path) {
      return semanticSelection(text, task, details, path, LIMIT);
    },
    async compact(texts, tool, options = {}) {
      const fallback = async () => tool === "bash" && options.command === undefined
        ? compactTexts(texts, tool) : texts;
      if (stopped || !["bash", "grep"].includes(tool)) return fallback();
      if (busy) return fallback();
      if (!texts.length || texts.some(text => typeof text !== "string" || !safeUtf8(text))
        || texts.reduce((size, text) => size + Buffer.byteLength(text, "utf8"), 0)
          + (typeof options.command === "string" ? Buffer.byteLength(options.command, "utf8") : 0)
          > LIMIT) return texts;
      busy = true;
      const started = performance.now();
      try {
        const stat = statSync(siftExecutable, {bigint:true});
        const key = JSON.stringify([String(stat.dev), String(stat.ino), String(stat.size),
          String(stat.mtimeNs), process.cwd(), Object.entries(process.env).sort()]);
        if (key === disabled) return await fallback();
        if (current && (current.key !== key || current.dead)) await retire(current);
        if (stopped || key === disabled) return await fallback();
        const slot = current || start(key);
        clearTimeout(slot.idle);
        refs(slot, true);
        slot.stderr = 0;
        if (sequence + texts.length > Number.MAX_SAFE_INTEGER) {
          await retire(slot, true, true);
          return texts;
        }
        const deadline = started + 3000, results = [];
        for (const text of texts) {
          const id = ++sequence;
          const contextual = tool === "grep" ? options.selection !== null
            : options.command !== undefined;
          const envelope = {id, request:{version:1,text}, tool,
            ...(tool === "grep" && options.selection !== null
              ? {selection:options.selection} : {}),
            ...(tool === "bash" && options.command !== undefined
              ? {command:options.command,delivered_view:true} : {})};
          const response = await request(slot, id, envelope, contextual, options.signal, deadline);
          if (!response.ok) return texts;
          results.push(response.value);
        }
        if (!slot.dead) idle(slot);
        return results;
      } catch {
        await retire(current, true, true);
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
  const session = createPiSession();
  let task = null;
  const nativeGrep = () => {
    try {
      const tools = pi.getAllTools?.();
      const matches = Array.isArray(tools) ? tools.filter(tool => tool?.name === "grep") : [];
      return matches.length === 1 && matches[0]?.sourceInfo?.source === "builtin";
    } catch { return false; }
  };
  pi.on("before_agent_start", event => { task = typeof event?.prompt === "string" ? event.prompt : null; });
  pi.on("agent_settled", () => { task = null; });
  pi.on("session_shutdown", async () => { task = null; await session.shutdown(); });
  pi.on("tool_result", async (event, ctx) => {
    if (event.toolName === "grep" && nativeGrep() && event.isError === false && Array.isArray(event.content)
      && event.content.length === 1 && event.content[0]?.type === "text"
      && typeof event.content[0].text === "string") {
      const original = event.content[0].text;
      const compacted = (await session.compact([original], "grep", {
        selection:session.selection(original, task, event.details, event.input?.path),
        signal:ctx?.signal,
      }))[0];
      if (compacted === original) return;
      return {content:[{...event.content[0], text:compacted}]};
    }
    if (!["bash", "powershell"].includes(event.toolName) || !Array.isArray(event.content)) return;
    const indices = [], texts = [];
    event.content.forEach((block, index) => {
      if (block?.type === "text" && typeof block.text === "string") {
        indices.push(index);
        texts.push(block.text);
      }
    });
    const command = event.toolName === "bash" && typeof event.input?.command === "string"
      ? event.input.command : undefined;
    const compacted = event.toolName === "bash"
      ? await session.compact(texts, "bash", {command,signal:ctx?.signal})
      : await compactTexts(texts, event.toolName);
    if (compacted.every((text, i) => text === texts[i])) return;
    const content = event.content.slice();
    indices.forEach((index, i) => { content[index] = {...content[index], text:compacted[i]}; });
    return {content};
  });
}

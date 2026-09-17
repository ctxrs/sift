// Retok output adapter. Installation supplies the absolute executable path.
// Execution and permissions remain with the host; no command is rewritten.
import { execFile } from "node:child_process";
const retokExecutable = __RETOK_EXECUTABLE__;
const retokSource = __RETOK_SOURCE__;

async function compactTexts(texts, tool) {
  if (!texts.length || texts.some(text => typeof text !== "string")) return texts;
  if (texts.reduce((n, text) => n + Buffer.byteLength(text, "utf8"), 0) > 8 * 1024 * 1024) return texts;
  return new Promise(resolve => {
    let settled = false;
    const finish = value => { if (!settled) { settled = true; resolve(value); } };
    try {
      const child = execFile(retokExecutable,
        ["compact", "--protocol=json-v1", "--record-source", retokSource,
          ...(tool ? ["--record-tool", tool] : [])],
        { timeout: 3000, maxBuffer: 32 * 1024 * 1024, windowsHide: true },
        (error, stdout) => {
          if (error) return finish(texts);
          try {
            const lines = stdout.trimEnd().split("\n");
            if (lines.length !== texts.length) return finish(texts);
            const results = lines.map(line => JSON.parse(line));
            if (results.some(item => item.version !== 1 || typeof item.text !== "string"
              || !Number.isSafeInteger(item.input_tokens) || !Number.isSafeInteger(item.output_tokens)
              || item.output_tokens < 0 || item.output_tokens > item.input_tokens)) return finish(texts);
            finish(results.map(item => item.text));
          } catch { finish(texts); }
        });
      child.on("error", () => finish(texts));
      child.stdin.on("error", () => finish(texts));
      child.stdin.end(texts.map(text => JSON.stringify({version: 1, text})).join("\n") + "\n");
    } catch { finish(texts); }
  });
}

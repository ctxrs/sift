// Retok's native OpenClaw pre-tool adapter. The CLI owns rewrite decisions.
import { execFile } from "node:child_process";
import { basename } from "node:path";

const retokExecutable = __RETOK_EXECUTABLE_JSON__;
const maxOutputBytes = 128 * 1024;
const maxCommandBytes = 32 * 1024;

export default function register(api) {
  if (api.pluginConfig?.enabled === false) return;
  api.on("before_tool_call", async (event, context = {}) => {
    try {
      if (event?.toolName !== "exec" || event.toolKind || event.toolInputKind
        || context.toolKind || context.toolInputKind) return;
      const params = event.params;
      if (!params || typeof params !== "object" || Array.isArray(params)) return;
      if (params.pty || params.background) return;
      const command = params.command;
      if (typeof command !== "string" || !command.trim() || command.includes("\0")
        || Buffer.byteLength(command, "utf8") > maxCommandBytes
        || Buffer.from(command, "utf8").toString("utf8") !== command) return;
      // The installed absolute executable belongs to this gateway, not a node
      // or sandbox. Never assume a remote machine shares its path or shell.
      const agent = api.config?.agents?.list?.find(item => item.id === context.agentId);
      const host = params.host ?? agent?.tools?.exec?.host ?? api.config?.tools?.exec?.host ?? "auto";
      if (host !== "gateway" && host !== "auto") return;
      const sandbox = agent?.sandbox?.mode ?? api.config?.agents?.defaults?.sandbox?.mode ?? "off";
      if (host === "auto" && sandbox !== "off") return;
      // Native Windows and non-pipefail shells need independent qualification.
      if (process.platform === "win32") return;
      const shell = basename(process.env.SHELL ?? "sh");
      if (!["bash", "zsh", "ksh"].includes(shell)) return;
      const rewritten = await rewrite(command, context.abortSignal);
      if (rewritten !== undefined) return { params: { ...params, command: rewritten } };
    } catch {
      // Errors may contain commands; fail open without logging them.
    }
  });
}

function rewrite(command, signal) {
  return new Promise(resolve => {
    try {
      const child = execFile(retokExecutable,
        ["rewrite", "--json", "--shell", "posix", "--", command],
        { timeout: 2000, maxBuffer: maxOutputBytes, encoding: "buffer", windowsHide: true,
          killSignal: "SIGKILL", signal },
        (error, stdout) => {
          if (error) return resolve();
          try {
            const result = JSON.parse(new TextDecoder("utf-8", { fatal: true }).decode(stdout));
            const rewritten = result?.command;
            if (result?.changed === true && typeof rewritten === "string" && rewritten.trim()
              && !rewritten.includes("\0") && rewritten !== command
              && Buffer.from(rewritten, "utf8").toString("utf8") === rewritten) return resolve(rewritten);
          } catch { /* malformed responses leave the host command intact */ }
          resolve();
        });
      child.stdin.on("error", () => {});
      child.stdin.end();
    } catch { resolve(); }
  });
}

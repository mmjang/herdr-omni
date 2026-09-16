import { createConnection } from "node:net";
import type { CommandResult } from "./types";

/** Use the injected session socket; never guess the default session. */
export function requestHerdr(method: string, params: Record<string, unknown>, path = process.env.HERDR_SOCKET_PATH, timeoutMs = 3000): Promise<CommandResult> {
  if (!path) return Promise.resolve({ ok: false, message: "Herdr did not provide its session socket path." });
  return new Promise(resolve => {
    const id = crypto.randomUUID();
    const socket = createConnection(path);
    let buffer = "";
    let finished = false;
    const finish = (result: CommandResult) => {
      if (finished) return;
      finished = true;
      clearTimeout(timer);
      socket.destroy();
      resolve(result);
    };
    const timer = setTimeout(() => finish({ ok: false, message: "Herdr API timed out; the action may not have completed." }), timeoutMs);
    socket.setEncoding("utf8");
    socket.on("connect", () => socket.write(JSON.stringify({ id, method, params }) + "\n"));
    socket.on("data", chunk => {
      buffer += chunk;
      if (buffer.length > 1024 * 1024) return finish({ ok: false, message: "Herdr returned an oversized API response." });
      let newline: number;
      while ((newline = buffer.indexOf("\n")) >= 0) {
        const line = buffer.slice(0, newline);
        buffer = buffer.slice(newline + 1);
        try {
          const response = JSON.parse(line);
          if (response.id !== id) continue;
          if (response.error) return finish({ ok: false, message: String(response.error.message ?? "Herdr rejected the action.") });
          return finish(response.result ? { ok: true, message: "" } : { ok: false, message: "Herdr returned an invalid API response." });
        } catch { return finish({ ok: false, message: "Herdr returned unreadable API data." }); }
      }
    });
    socket.on("error", error => finish({ ok: false, message: `Cannot reach Herdr: ${error.message}` }));
    socket.on("close", () => finish({ ok: false, message: "Herdr closed the connection before confirming the action." }));
  });
}

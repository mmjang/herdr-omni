import { spawn, type ChildProcessWithoutNullStreams } from "node:child_process";
import type { SavedSession } from "./types";
import { OpenCodeHistory } from "./opencode-history";
import { SESSION_REQUEST_TIMEOUT_MS, TRANSCRIPT_RESULT_LIMIT, transcriptExcerpt, type SessionEvent, type SessionRequest } from "./sessions";

/** Private read-only app-server connection; never starts or resumes a turn. */
export class CodexHistory {
  private child?: ChildProcessWithoutNullStreams;
  private sequence = 0;
  private pending = new Map<number, { resolve: (value: any) => void; reject: (error: Error) => void }>();
  constructor(private command = ["codex", "app-server", "--listen", "stdio://"], private timeoutMs = SESSION_REQUEST_TIMEOUT_MS) {}
  async start() {
    if (this.child) return;
    const child = spawn(this.command[0]!, this.command.slice(1), { stdio: "pipe" });
    this.child = child;
    let buffer = "";
    child.stdout.setEncoding("utf8");
    child.stderr.resume();
    const disconnected = () => { if (this.child === child) this.close(); };
    child.on("error", disconnected);
    child.on("exit", disconnected);
    child.stdin.on("error", disconnected);
    child.stdout.on("data", (chunk: string) => {
      buffer += chunk;
      if (buffer.length > 64 * 1024 * 1024) return this.close();
      let newline: number;
      while ((newline = buffer.indexOf("\n")) >= 0) {
        const line = buffer.slice(0, newline); buffer = buffer.slice(newline + 1);
        try {
          const response = JSON.parse(line);
          const pending = this.pending.get(response.id);
          if (!pending) continue;
          this.pending.delete(response.id);
          if (response.error) pending.reject(new Error("Codex history request failed"));
          else pending.resolve(response.result);
        } catch { this.close(); }
      }
    });
    await this.call("initialize", { clientInfo: { name: "herdr_omni_history", version: "1.0" } });
    child.stdin.write(JSON.stringify({ method: "initialized" }) + "\n");
  }
  call(method: string, params: unknown): Promise<any> {
    if (!this.child) return Promise.reject(new Error("Codex history unavailable"));
    const id = ++this.sequence;
    return new Promise((resolve, reject) => {
      const timeout = setTimeout(() => { this.pending.delete(id); reject(new Error("Codex history timed out")); this.close(); }, this.timeoutMs);
      this.pending.set(id, { resolve: value => { clearTimeout(timeout); resolve(value); }, reject: error => { clearTimeout(timeout); reject(error); } });
      this.child!.stdin.write(JSON.stringify({ id, method, params }) + "\n");
    });
  }
  close() {
    const child = this.child; this.child = undefined;
    child?.kill();
    for (const pending of this.pending.values()) pending.reject(new Error("Codex history unavailable"));
    this.pending.clear();
  }
}

/** Extract only documented visible text/tool fields; never stringify raw metadata or reasoning. */
export function claudeText(messages: any[]): string[] {
  const content = (value: any): string[] => {
    if (typeof value === "string") return [value];
    if (!Array.isArray(value)) return [];
    return value.flatMap(block => block?.type === "text" && typeof block.text === "string" ? [block.text]
      : block?.type === "tool_result" ? content(block.content) : []);
  };
  return messages.flatMap(message => content(message.message?.content));
}

export function codexText(thread: any): string[] {
  return (thread?.turns ?? []).flatMap((turn: any) => (turn.items ?? []).flatMap((item: any) => {
    if (item.type === "agentMessage" && typeof item.text === "string") return [item.text];
    if (item.type === "userMessage") return (item.content ?? []).filter((part: any) => part.type === "text" && typeof part.text === "string").map((part: any) => part.text);
    if (item.type === "commandExecution") return [item.command, item.aggregatedOutput].filter(value => typeof value === "string");
    return [];
  }));
}

async function work(request: SessionRequest, publish: (event: SessionEvent) => void) {
  const codex = new CodexHistory();
  const opencode = new OpenCodeHistory();
  const stop = () => { codex.close(); opencode.close(); process.exit(0); };
  process.on("SIGTERM", stop);
  process.on("SIGINT", stop);
  try {
    if (request.type === "list") {
      await Promise.all([
        (async () => {
          if (!Bun.which("opencode")) return;
          try { publish({ type: "sessions", sessions: await opencode.list() }); }
          catch { publish({ type: "error", message: "OpenCode history unavailable (requires the global session API)." }); }
        })(),
        (async () => {
          if (!Bun.which("codex")) return;
          try {
            await codex.start();
            let cursor: string | null = null;
            const seen = new Set<string>();
            do {
              const result = await codex.call("thread/list", { limit: 100, cursor, sortKey: "updated_at", sourceKinds: ["cli", "vscode", "appServer", "exec"] });
              const sessions: SavedSession[] = (result.data ?? []).filter((thread: any) => typeof thread.id === "string").map((thread: any) => ({
                provider: "codex", id: thread.id, title: thread.name || thread.preview || "Codex session", cwd: thread.cwd || "", updatedAt: (thread.updatedAt ?? 0) * 1000,
              }));
              publish({ type: "sessions", sessions });
              cursor = result.nextCursor;
              if (cursor && seen.has(cursor)) throw new Error();
              if (cursor) seen.add(cursor);
            } while (cursor);
          } catch { publish({ type: "error", message: "Codex history unavailable (requires a compatible Codex app-server)." }); }
        })(),
        (async () => {
          if (!Bun.which("claude")) return;
          try {
            const { listSessions } = await import("@anthropic-ai/claude-agent-sdk");
            const sessions = await listSessions();
            publish({ type: "sessions", sessions: sessions.map(session => ({ provider: "claude", id: session.sessionId,
              title: session.customTitle || session.summary || session.firstPrompt || "Claude session", cwd: session.cwd || "", updatedAt: session.lastModified })) });
          } catch { publish({ type: "error", message: "Claude history unavailable." }); }
        })(),
      ]);
    } else {
      let hits = 0, scanned = 0, failures = 0;
      for (const session of request.sessions) {
        try {
          let messages: string[];
          if (session.provider === "codex") {
            await codex.start();
            messages = codexText((await codex.call("thread/read", { threadId: session.id, includeTurns: true })).thread);
          } else if (session.provider === "opencode") {
            messages = await opencode.read(session.id);
          } else {
            const { getSessionMessages } = await import("@anthropic-ai/claude-agent-sdk");
            messages = claudeText(await getSessionMessages(session.id, { dir: session.cwd || undefined }));
          }
          const excerpt = transcriptExcerpt(messages, request.query);
          if (excerpt) { publish({ type: "hit", session, excerpt }); hits++; }
        } catch { failures++; }
        publish({ type: "progress", scanned: ++scanned, total: request.sessions.length });
        if (hits >= TRANSCRIPT_RESULT_LIMIT) { publish({ type: "done", limited: scanned < request.sessions.length }); return; }
      }
      if (failures) publish({ type: "error", message: `${failures} transcript(s) unavailable.` });
    }
    publish({ type: "done" });
  } finally {
    codex.close();
    opencode.close();
    process.removeListener("SIGTERM", stop);
    process.removeListener("SIGINT", stop);
  }
}

if (import.meta.main) {
  // The UI owns this worker. Its SIGTERM also closes the private Codex child.
  const timeout = setTimeout(() => process.kill(process.pid, "SIGTERM"), 10 * 60_000);
  try {
    await work(JSON.parse(await Bun.stdin.text()), event => console.log(JSON.stringify(event)));
  } finally { clearTimeout(timeout); }
}

import { spawn, type ChildProcess, type ChildProcessWithoutNullStreams } from "node:child_process";
import { homedir } from "node:os";
import { basename, join } from "node:path";
import type { SavedSession } from "./types";
import { OpenCodeHistory } from "./opencode-history";
import { SESSION_REQUEST_TIMEOUT_MS, TRANSCRIPT_RESULT_LIMIT, transcriptExcerpt, transcriptTerms, type SessionEvent, type SessionRequest } from "./sessions";

type RgResult = { code: number; stdout: string };

export interface TranscriptPrefilterRunner {
  run(args: string[]): Promise<RgResult>;
  close(): void;
}

/** A bounded, cancellable runner for read-only ripgrep probes. */
export class RgRunner implements TranscriptPrefilterRunner {
  private readonly pending = new Map<ChildProcess, { resolve: (result: RgResult) => void; reject: (error: Error) => void }>();
  private closed = false;

  constructor(private readonly command = "rg") {}

  run(args: string[]): Promise<RgResult> {
    if (this.closed) return Promise.reject(new Error("Transcript prefilter unavailable"));
    return new Promise((resolve, reject) => {
      let stdout = "";
      let child: ChildProcess;
      try {
        child = spawn(this.command, args, { stdio: ["ignore", "pipe", "ignore"] });
      } catch (error) {
        reject(error instanceof Error ? error : new Error("Transcript prefilter unavailable"));
        return;
      }
      const finish = (error?: Error, result?: RgResult) => {
        if (!this.pending.delete(child)) return;
        if (error) reject(error);
        else resolve(result!);
      };
      this.pending.set(child, { resolve: result => finish(undefined, result), reject: error => finish(error) });
      child.stdout?.setEncoding("utf8");
      child.stdout?.on("data", (chunk: string) => {
        stdout += chunk;
        if (stdout.length > 64 * 1024 * 1024) {
          child.kill();
          finish(new Error("Transcript prefilter response too large"));
        }
      });
      child.once("error", error => finish(error instanceof Error ? error : new Error("Transcript prefilter unavailable")));
      child.once("close", code => finish(undefined, { code: code ?? -1, stdout }));
    });
  }

  close() {
    this.closed = true;
    for (const [child, pending] of this.pending) {
      child.kill();
      pending.reject(new Error("Transcript prefilter cancelled"));
    }
    this.pending.clear();
  }
}

const transcriptRoots = (provider: "codex" | "claude", home = homedir()): string[] => provider === "codex"
  ? [join(home, ".codex", "sessions"), join(home, ".codex", "archived_sessions")]
  : [join(home, ".claude", "projects")];

const splitNull = (stdout: string) => stdout.split("\0").filter(Boolean);

/**
 * Find sessions whose JSONL files are coarse candidates for a literal query.
 * `undefined` deliberately means "scan all": path/layout assumptions or rg
 * failures must never turn into a silent false negative.
 */
export async function prefilterTranscriptSessions(
  provider: "codex" | "claude",
  sessions: SavedSession[],
  query: string,
  runner: TranscriptPrefilterRunner,
  roots = transcriptRoots(provider),
): Promise<Set<string> | undefined> {
  const phrase = transcriptTerms(query)[0];
  if (!phrase || !sessions.length || !roots.length) return undefined;

  const glob = provider === "codex" ? "rollout-*.jsonl" : "*.jsonl";
  const existingRoots: string[] = [];
  const files: string[] = [];
  try {
    for (const root of roots) {
      const listed = await runner.run(["--files", "--null", "--hidden", "--no-ignore", "--glob", glob, "--", root]);
      // rg uses 2 for an absent/unreadable root. Other roots can still be
      // valid (archived_sessions is commonly absent), so defer fallback until
      // we know whether any usable corpus exists.
      if (listed.code === 0) {
        existingRoots.push(root);
        files.push(...splitNull(listed.stdout));
      } else if (listed.code === 1) {
        existingRoots.push(root);
      } else if (listed.code !== 2) {
        return undefined;
      }
    }
    // No roots or no JSONL files means the provider's layout may have moved.
    if (!existingRoots.length || !files.length) return undefined;

    // Search both the normalized phrase and its JSON representation. The
    // latter catches quotes/backslashes that JSONL escaped on disk, while -F
    // keeps all user punctuation literal and -- prevents option injection.
    const compactPhrase = phrase.replace(/\s*([{}\[\],:])\s*/g, "$1");
    const encodedPhrase = JSON.stringify(phrase);
    const encodedCompactPhrase = JSON.stringify(compactPhrase);
    // JSONL escapes quotes, slashes and control characters inside strings.
    // Keep the quoted form for an exact JSON-string hit and the unquoted form
    // for a phrase embedded inside a larger JSON string value.
    const patterns = [...new Set([
      phrase,
      compactPhrase,
      encodedPhrase,
      encodedPhrase.slice(1, -1),
      encodedCompactPhrase,
      encodedCompactPhrase.slice(1, -1),
    ])];
    const args = ["--files-with-matches", "--null", "--hidden", "--no-ignore", "--ignore-case", "--fixed-strings", "--glob", glob];
    for (const pattern of patterns) args.push("-e", pattern);
    const matches = await runner.run([...args, "--", ...existingRoots]);
    if (matches.code === 1) return new Set();
    if (matches.code !== 0) return undefined;

    const matchedFiles = splitNull(matches.stdout);
    if (!matchedFiles.length) return new Set();
    const byClaudeFilename = new Map(sessions.map(session => [`${session.id}.jsonl`, session.id]));
    const ids = new Set<string>();
    for (const path of matchedFiles) {
      const file = basename(path);
      const id = provider === "claude"
        ? byClaudeFilename.get(file)
        : sessions.find(session => file === `${session.id}.jsonl` || file.endsWith(`-${session.id}.jsonl`))?.id;
      // A hit that cannot be mapped is evidence our filename assumption is
      // stale; retain the old full scan rather than dropping a real result.
      if (!id) return undefined;
      ids.add(id);
    }
    return ids;
  } catch {
    return undefined;
  }
}

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

/** Search conversation text only, excluding tool calls/results, system data and reasoning. */
export function claudeText(messages: any[]): string[] {
  const content = (value: any): string[] => {
    if (typeof value === "string") return [value];
    if (!Array.isArray(value)) return [];
    return value.flatMap(block => block?.type === "text" && typeof block.text === "string" ? [block.text] : []);
  };
  return messages.flatMap(message => ["user", "assistant"].includes(message.type) ? content(message.message?.content) : []);
}

export function codexText(thread: any): string[] {
  return (thread?.turns ?? []).flatMap((turn: any) => (turn.items ?? []).flatMap((item: any) => {
    if (item.type === "agentMessage" && typeof item.text === "string") return [item.text];
    if (item.type === "userMessage") return (item.content ?? []).filter((part: any) => part.type === "text" && typeof part.text === "string").map((part: any) => part.text);
    return [];
  }));
}

async function work(request: SessionRequest, publish: (event: SessionEvent) => void) {
  const codex = new CodexHistory();
  const opencode = new OpenCodeHistory();
  const rg = new RgRunner();
  const stop = () => { codex.close(); opencode.close(); rg.close(); process.exit(0); };
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
      // Keep OpenCode on its existing provider path. Its SQLite corpus is
      // small, while Codex/Claude JSONL files benefit substantially from a
      // cancellable coarse probe before the exact SDK reads below.
      const prefiltered = await Promise.all(["codex", "claude"] as const).then(async providers => {
        const entries = await Promise.all(providers.map(async provider => {
          const sessions = request.sessions.filter(session => session.provider === provider);
          if (!Bun.which("rg") || !sessions.length) return [provider, undefined] as const;
          return [provider, await prefilterTranscriptSessions(provider, sessions, request.query, rg)] as const;
        }));
        return new Map(entries);
      });
      const sessionsToScan = request.sessions.filter(session => {
        const allowed = prefiltered.get(session.provider as "codex" | "claude");
        return !allowed || allowed.has(session.id);
      });
      let hits = 0, scanned = 0, failures = 0;
      for (const session of sessionsToScan) {
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
        publish({ type: "progress", scanned: ++scanned, total: sessionsToScan.length });
        if (hits >= TRANSCRIPT_RESULT_LIMIT) { publish({ type: "done", limited: scanned < sessionsToScan.length }); return; }
      }
      if (failures) publish({ type: "error", message: `${failures} transcript(s) unavailable.` });
    }
    publish({ type: "done" });
  } finally {
    codex.close();
    opencode.close();
    rg.close();
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

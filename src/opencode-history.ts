import { spawn } from "node:child_process";
import { randomBytes } from "node:crypto";
import { SESSION_REQUEST_TIMEOUT_MS } from "./sessions";
import type { SavedSession } from "./types";

const validId = (value: unknown): value is string => typeof value === "string" && /^ses_[a-zA-Z0-9_-]{1,196}$/.test(value);
const record = (value: unknown): value is Record<string, any> => value !== null && typeof value === "object" && !Array.isArray(value);

/** Read-only history access; never starts a model turn or interactive picker. */
export class OpenCodeHistory {
  private pending = new Set<() => void>();
  private closed = false;
  constructor(private command = ["opencode"], private timeoutMs = SESSION_REQUEST_TIMEOUT_MS) {}

  private run(args: string[]): Promise<string> {
    if (this.closed) return Promise.reject(new Error("OpenCode history closed"));
    return new Promise((resolve, reject) => {
      const child = spawn(this.command[0]!, [...this.command.slice(1), ...args], { stdio: ["ignore", "pipe", "ignore"] });
      let output = "", size = 0, settled = false;
      const finish = (error?: Error) => {
        if (settled) return;
        settled = true;
        clearTimeout(timeout);
        this.pending.delete(cancel);
        if (error) { child.kill("SIGKILL"); reject(error); }
        else resolve(output);
      };
      const cancel = () => finish(new Error("OpenCode history closed"));
      const timeout = setTimeout(() => finish(new Error("OpenCode history timed out")), this.timeoutMs);
      this.pending.add(cancel);
      child.stdout.setEncoding("utf8");
      child.stdout.on("data", (chunk: string) => {
        size += Buffer.byteLength(chunk);
        if (size > 64 * 1024 * 1024) finish(new Error("OpenCode history output too large"));
        else if (!settled) output += chunk;
      });
      child.on("error", () => finish(new Error("OpenCode history unavailable")));
      child.on("close", code => finish(code === 0 ? undefined : new Error("OpenCode history command failed")));
    });
  }

  async list(): Promise<SavedSession[]> {
    // `session list` is project-scoped. Use the global API, including sessions
    // whose original project/worktree directory no longer exists.
    const rows = await this.listGlobal();
    if (!Array.isArray(rows)) throw new Error("Invalid OpenCode session list");
    const seen = new Set<string>();
    return rows.flatMap(row => {
      if (!record(row) || !validId(row.id) || typeof row.title !== "string" || typeof row.directory !== "string"
        || !record(row.time) || typeof row.time.updated !== "number" || !Number.isFinite(row.time.updated) || row.time.updated < 0 || seen.has(row.id)) return [];
      seen.add(row.id);
      return [{ provider: "opencode", id: row.id, title: row.title || "OpenCode session", cwd: row.directory, updatedAt: row.time.updated }];
    });
  }

  private listGlobal(): Promise<unknown[]> {
    if (this.closed) return Promise.reject(new Error("OpenCode history closed"));
    return new Promise((resolve, reject) => {
      const password = randomBytes(24).toString("hex");
      const child = spawn(this.command[0]!, [...this.command.slice(1), "serve", "--pure", "--hostname", "127.0.0.1", "--port", "0"], {
        stdio: ["ignore", "pipe", "ignore"],
        env: { ...process.env, OPENCODE_SERVER_USERNAME: "omni", OPENCODE_SERVER_PASSWORD: password },
      });
      const controller = new AbortController();
      let settled = false, started = false, output = "";
      const finish = (error?: Error, rows: unknown[] = []) => {
        if (settled) return;
        settled = true;
        clearTimeout(timeout);
        this.pending.delete(cancel);
        controller.abort();
        child.kill("SIGKILL");
        if (error) reject(error); else resolve(rows);
      };
      const cancel = () => finish(new Error("OpenCode history closed"));
      const timeout = setTimeout(() => finish(new Error("OpenCode history timed out")), this.timeoutMs);
      this.pending.add(cancel);
      child.on("error", () => finish(new Error("OpenCode history unavailable")));
      child.on("close", () => finish(new Error("OpenCode history command failed")));
      child.stdout.setEncoding("utf8");
      child.stdout.on("data", (chunk: string) => {
        if (started || settled) return;
        output = (output + chunk).slice(-8192);
        const match = /server listening on (http:\/\/127\.0\.0\.1:\d+)/.exec(output);
        if (!match) return;
        started = true;
        void (async () => {
          const rows: unknown[] = [];
          const cursors = new Set<string>();
          let cursor: string | undefined;
          do {
            const url = new URL("/experimental/session", match[1]);
            url.searchParams.set("roots", "true");
            url.searchParams.set("limit", "1000");
            if (cursor) url.searchParams.set("cursor", cursor);
            const response = await fetch(url, {
              headers: { Authorization: `Basic ${Buffer.from(`omni:${password}`).toString("base64")}` },
              signal: controller.signal,
              redirect: "error",
            });
            if (!response.ok) throw new Error("OpenCode global history unavailable");
            const page: unknown = await response.json();
            if (!Array.isArray(page)) throw new Error("Invalid OpenCode session list");
            rows.push(...page);
            cursor = response.headers.get("x-next-cursor") ?? undefined;
            if (cursor && cursors.has(cursor)) throw new Error("Invalid OpenCode history pagination");
            if (cursor) cursors.add(cursor);
          } while (cursor);
          finish(undefined, rows);
        })().catch(() => finish(new Error("OpenCode global history unavailable")));
      });
    });
  }

  async read(id: string): Promise<string[]> {
    if (!validId(id)) throw new Error("Invalid OpenCode session ID");
    const data: unknown = JSON.parse(await this.run(["export", id]));
    if (!record(data) || !record(data.info) || data.info.id !== id || !Array.isArray(data.messages)) {
      throw new Error("Invalid OpenCode session export");
    }
    return data.messages.flatMap(message => {
      if (!record(message) || !record(message.info) || !["user", "assistant"].includes(message.info.role) || !Array.isArray(message.parts)) return [];
      return message.parts.flatMap((part: unknown) => {
        if (!record(part)) return [];
        if (part.type === "text" && !part.ignored && !part.synthetic && typeof part.text === "string") return [part.text];
        return [];
      });
    });
  }

  close(): void {
    this.closed = true;
    for (const cancel of this.pending) cancel();
  }
}

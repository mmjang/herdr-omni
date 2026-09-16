import { basename } from "node:path";
import type { PaletteItem, SavedSession } from "./types";

export const TRANSCRIPT_DEBOUNCE_MS = 200;
export const TRANSCRIPT_RESULT_LIMIT = 100;
export const SESSION_REQUEST_TIMEOUT_MS = 20_000;
export const TRANSCRIPT_CONTEXT_BEFORE = 300;
export const TRANSCRIPT_CONTEXT_AFTER = 900;

export type SessionEvent =
  | { type: "sessions"; sessions: SavedSession[] }
  | { type: "hit"; session: SavedSession; excerpt: string }
  | { type: "progress"; scanned: number; total: number }
  | { type: "error"; message: string }
  | { type: "done"; limited?: boolean };
export type SessionRequest = { type: "list" } | { type: "search"; sessions: SavedSession[]; query: string };
export type SessionJob = (request: SessionRequest, signal: AbortSignal, publish: (event: SessionEvent) => void) => Promise<void>;

export const sessionKey = (session: Pick<SavedSession, "provider" | "id">) => `${session.provider}:${session.id}`;
export const cleanText = (value: string) => value.replace(/[\x00-\x1f\x7f-\x9f]/g, " ").replace(/\s+/g, " ").trim();

export function agentActivity(item: PaletteItem): number {
  return Math.max(...[item.lastActiveAt, item.session?.updatedAt, 0]
    .map(value => typeof value === "number" && Number.isFinite(value) && value > 0 ? value : 0));
}

export function savedSessionItem(session: SavedSession): PaletteItem {
  const title = `${cleanText(session.title).slice(0, 160) || session.provider} - ${cleanText(basename(session.cwd)) || "Unknown project"}`;
  return { id: `saved:agent:${sessionKey(session)}`, title, category: "Agents", icon: "◈",
    description: session.cwd, aliases: [session.provider], searchPaths: [session.cwd], shortcuts: [],
    priority: 5, session, savedSession: true, invocation: { kind: "resume-session", session } };
}

/** Never infer identity from titles: two conversations can have identical names. */
export function mergeSessions(live: PaletteItem[], sessions: SavedSession[]): PaletteItem[] {
  const known = new Set(live.flatMap(item => item.session ? [sessionKey(item.session)] : []));
  const activity = new Map<string, number>();
  for (const session of sessions) {
    const key = sessionKey(session);
    const updated = Number.isFinite(session.updatedAt) ? session.updatedAt : 0;
    activity.set(key, Math.max(activity.get(key) ?? 0, updated));
  }
  const enriched = live.map(item => item.session ? { ...item,
    lastActiveAt: Math.max(agentActivity(item), activity.get(sessionKey(item.session)) ?? 0) } : item);
  return [...enriched, ...sessions.filter(session => !known.has(sessionKey(session))).map(savedSessionItem)];
}

/** AND of literal tokens/quoted phrases, within one message, not scattered fuzzy letters. */
export function transcriptTerms(query: string): string[] {
  return [...query.matchAll(/"([^"]+)"|([^\s"]+)/g)].map(match => cleanText(match[1] ?? match[2]!).toLowerCase()).filter(Boolean);
}

export function transcriptExcerpt(messages: string[], query: string): string | undefined {
  const terms = transcriptTerms(query);
  if (!terms.length) return;
  for (const [index, message] of messages.entries()) {
    const text = cleanText(message);
    const lower = text.toLowerCase();
    if (!terms.every(term => lower.includes(term))) continue;
    const first = Math.min(...terms.map(term => lower.indexOf(term)));
    const start = Math.max(0, first - TRANSCRIPT_CONTEXT_BEFORE);
    const end = Math.min(text.length, first + Math.max(...terms.map(term => term.length)) + TRANSCRIPT_CONTEXT_AFTER);
    const before = start === 0 && index > 0 ? cleanText(messages[index - 1]!).slice(-TRANSCRIPT_CONTEXT_BEFORE) : "";
    const after = end === text.length && index + 1 < messages.length ? cleanText(messages[index + 1]!).slice(0, TRANSCRIPT_CONTEXT_AFTER) : "";
    return [before, `${start ? "…" : ""}${text.slice(start, end)}${end < text.length ? "…" : ""}`, after].filter(Boolean).join("\n\n");
  }
}

/** Isolate filesystem scans/SDK parsing from the UI. Killing this worker cancels stale work. */
export const runSessionJob: SessionJob = async (request, signal, publish) => {
  if (signal.aborted) return;
  const child = Bun.spawn([process.execPath, `${import.meta.dir}/session-worker.ts`], { stdin: "pipe", stdout: "pipe", stderr: "ignore" });
  const abort = () => { child.kill(); };
  signal.addEventListener("abort", abort, { once: true });
  child.stdin.write(JSON.stringify(request));
  child.stdin.end();
  let completed = false;
  try {
    const reader = child.stdout.getReader();
    const decoder = new TextDecoder();
    let buffer = "";
    while (!signal.aborted) {
      const { value, done } = await reader.read();
      if (done) break;
      buffer += decoder.decode(value, { stream: true });
      let newline: number;
      while ((newline = buffer.indexOf("\n")) >= 0) {
        const event: SessionEvent = JSON.parse(buffer.slice(0, newline));
        buffer = buffer.slice(newline + 1);
        if (event.type === "done") completed = true;
        if (!signal.aborted) publish(event);
      }
      if (buffer.length > 16 * 1024 * 1024) throw new Error("Session metadata response too large");
    }
    await child.exited;
    if (!signal.aborted && !completed) throw new Error("Session reader stopped unexpectedly");
  } finally {
    signal.removeEventListener("abort", abort);
    child.kill();
  }
};

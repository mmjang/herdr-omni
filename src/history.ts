import { mkdirSync, readFileSync, renameSync, unlinkSync, writeFileSync } from "node:fs";
import { homedir } from "node:os";
import { dirname, join } from "node:path";

const MAX_ENTRIES = 200;
const LIVE_PREFIX = "live:";
const SCOPED_LIVE_PREFIX = "live@";

type History = Record<string, number>;

/** The on-disk location used when a caller does not provide one explicitly. */
function defaultHistoryPath(): string {
  const stateHome = process.env.XDG_STATE_HOME || join(process.env.HOME || homedir(), ".local", "state");
  return join(stateHome, "herdr-palette", "history.json");
}

function liveScope(): string {
  // encodeURIComponent is injective for the path we use as the namespace and
  // keeps arbitrary socket paths from introducing separators in a history key.
  return encodeURIComponent(process.env.HERDR_SOCKET_PATH || "default");
}

/**
 * Convert a palette ID to its persisted history key. Static IDs are shared by
 * every Herdr session; live IDs are namespaced by the Herdr socket path.
 */
export function historyKey(id: string): string {
  return id.startsWith(LIVE_PREFIX) ? `${SCOPED_LIVE_PREFIX}${liveScope()}:${id}` : id;
}

function isRecord(value: unknown): value is Record<string, unknown> {
  return typeof value === "object" && value !== null && !Array.isArray(value);
}

/** Keep only the JSON shape we can safely use, and retain the newest entries. */
function validatedHistory(value: unknown): History {
  if (!isRecord(value)) return {};

  const entries: Array<[string, number]> = Object.entries(value).filter(
    (entry): entry is [string, number] => typeof entry[1] === "number" && Number.isFinite(entry[1]),
  );
  entries.sort((left, right) => right[1] - left[1]);

  const result: History = {};
  for (const [key, timestamp] of entries.slice(0, MAX_ENTRIES)) result[key] = timestamp;
  return result;
}

function readHistory(path: string): History {
  try {
    return validatedHistory(JSON.parse(readFileSync(path, "utf8")));
  }
  catch {
    return {};
  }
}

function writeHistory(path: string, history: History): void {
  const temporaryPath = `${path}.${process.pid}.${Math.random().toString(36).slice(2)}.tmp`;
  try {
    mkdirSync(dirname(path), { recursive: true });
    writeFileSync(temporaryPath, `${JSON.stringify(history)}\n`, { encoding: "utf8", mode: 0o600 });
    renameSync(temporaryPath, path);
  }
  catch {
    // History is an enhancement. A read-only or otherwise unavailable state
    // directory must never prevent the palette from opening or running items.
    try { unlinkSync(temporaryPath); } catch {}
  }
}

/** Load valid history entries for the current session. */
export function loadHistory(path = defaultHistoryPath()): History {
  const history = readHistory(path);
  const currentLivePrefix = `${SCOPED_LIVE_PREFIX}${liveScope()}:`;
  const visible: History = {};
  for (const [key, timestamp] of Object.entries(history)) {
    if (!key.startsWith(SCOPED_LIVE_PREFIX) || key.startsWith(currentLivePrefix)) visible[key] = timestamp;
  }
  return visible;
}

/** Record a successful selection, without allowing persistence failures to escape. */
export function recordSelection(id: string, path = defaultHistoryPath()): void {
  if (!id) return;

  const history = readHistory(path);
  const key = historyKey(id);
  const latest = Object.values(history).reduce((maximum, timestamp) => Math.max(maximum, timestamp), 0);
  const timestamp = Math.max(Date.now(), latest + 1);
  history[key] = timestamp;

  const bounded = validatedHistory(history);
  writeHistory(path, bounded);
}

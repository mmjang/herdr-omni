import { readFileSync, statSync } from "node:fs";
import { homedir } from "node:os";
import { dirname, join } from "node:path";

/**
 * Keep the scan bounded. Herdr's server log is append-only and can become
 * large over time; only a recent tail can affect the ordering we present.
 */
export const MAX_SERVER_LOG_CHARS = 8 * 1024 * 1024;

type LogCache = {
  path: string;
  size: number;
  mtimeMs: number;
  visits: Map<string, number>;
};

let cache: LogCache | undefined;

/** Resolve Herdr's server log next to its active configuration directory. */
export function defaultWorkspaceHistoryPath(): string {
  const configPath = process.env.HERDR_CONFIG_PATH;
  if (configPath) return join(dirname(configPath), "herdr-server.log");
  return join(process.env.HOME || homedir(), ".config", "herdr", "herdr-server.log");
}

/**
 * Extract the most recent successful workspace-focus event for each workspace.
 * We intentionally ignore tab focus events and failed requests: only Herdr's
 * own workspace focus signal is a recency source for workspace destinations.
 */
export function parseWorkspaceFocusLog(source: string): Map<string, number> {
  const visits = new Map<string, number>();
  for (const line of source.split(/\r?\n/)) {
    const timestampText = line.match(/^(\S+)/)?.[1];
    if (!timestampText) continue;
    const timestamp = Date.parse(timestampText);
    if (!Number.isFinite(timestamp) || timestamp <= 0) continue;
    if (!/event="workspace\.focus"/.test(line) || !/outcome="ok"/.test(line)) continue;
    const workspaceId = line.match(/\bworkspace_id="([^"]+)"/)?.[1];
    if (workspaceId && timestamp > (visits.get(workspaceId) ?? 0)) visits.set(workspaceId, timestamp);
  }
  return visits;
}

/** Load and cache workspace visit times until Herdr appends to the log. */
export function loadWorkspaceVisitTimes(path = defaultWorkspaceHistoryPath()): ReadonlyMap<string, number> {
  let stat: ReturnType<typeof statSync>;
  try {
    stat = statSync(path);
  }
  catch {
    cache = undefined;
    return new Map();
  }

  if (cache?.path === path && cache.size === stat.size && cache.mtimeMs === stat.mtimeMs) return cache.visits;

  try {
    let source = readFileSync(path, "utf8");
    if (source.length > MAX_SERVER_LOG_CHARS) source = source.slice(-MAX_SERVER_LOG_CHARS);
    const visits = parseWorkspaceFocusLog(source);
    cache = { path, size: stat.size, mtimeMs: stat.mtimeMs, visits };
    return visits;
  }
  catch {
    cache = undefined;
    return new Map();
  }
}

/** Reset the process-local cache; useful for tests and config changes. */
export function clearWorkspaceVisitCache(): void {
  cache = undefined;
}

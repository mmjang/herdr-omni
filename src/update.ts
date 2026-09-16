import { mkdirSync, openSync, readFileSync, renameSync, statSync, unlinkSync, writeFileSync, closeSync } from "node:fs";
import { homedir } from "node:os";
import { join } from "node:path";
import * as herdr from "./herdr";
import type { CommandResult } from "./types";

const OWNER = "mmjang";
const REPOSITORY = "herdr-omni";
const PLUGIN = "herdr-omni";
const CHECK_TIMEOUT_MS = 5_000;
const CACHE_MS = 60 * 60 * 1_000;
const SNOOZE_MS = 24 * 60 * 60 * 1_000;
const LOCK_STALE_MS = 30 * 60 * 1_000;
const STATE_FILE = "update.json";
const LOCK_FILE = "update.lock";

export interface UpdateOffer {
  version: string;
  ref: string;
}

type Plugin = {
  plugin_id?: unknown;
  version?: unknown;
  enabled?: unknown;
  source?: unknown;
};

type Eligibility = {
  plugin: Plugin;
  source: { kind: "github"; owner: string; repo: string; resolved_commit: string; requested_ref?: string };
  state: State;
  stateDir: string;
};

type State = {
  cache?: { checkedAt: number; currentVersion: string; offer?: UpdateOffer };
  snooze?: { version: string; dismissedAt: number };
  updaterInstall?: { ref: string; resolved_commit: string };
};

type HerdrResult = { code: number; stdout: string; stderr: string };
type Runner = (argv: string[], timeoutMs?: number, cwd?: string) => Promise<HerdrResult>;
type GitRunner = (argv: string[], timeoutMs?: number, cwd?: string, signal?: AbortSignal) => Promise<HerdrResult>;
type UpdateOverrides = {
  runHerdr?: Runner;
  runGit?: GitRunner;
  stateDir?: string;
  now?: () => number;
};

let overrides: UpdateOverrides = {};

/** Test-only dependency injection; returns a restore function. */
export function setUpdateDependenciesForTests(next: UpdateOverrides): () => void {
  const previous = overrides;
  overrides = { ...overrides, ...next };
  return () => { overrides = previous; };
}

const run: Runner = (argv, timeoutMs, cwd = homedir()) => (overrides.runHerdr ?? herdr.runHerdr)(argv, timeoutMs, cwd);
async function runGit(argv: string[], timeoutMs = CHECK_TIMEOUT_MS, cwd = homedir(), signal?: AbortSignal): Promise<HerdrResult> {
  if (overrides.runGit) return overrides.runGit(argv, timeoutMs, cwd, signal);
  if (signal?.aborted) return { code: 1, stdout: "", stderr: "aborted" };
  const child = Bun.spawn(["git", ...argv], {
    cwd,
    env: { ...process.env, GIT_TERMINAL_PROMPT: "0" },
    stdout: "pipe",
    stderr: "pipe",
  });
  const timeout = setTimeout(() => child.kill(), timeoutMs);
  const onAbort = () => child.kill();
  signal?.addEventListener("abort", onAbort, { once: true });
  try {
    const [stdout, stderr, code] = await Promise.all([new Response(child.stdout).text(), new Response(child.stderr).text(), child.exited]);
    return { code, stdout, stderr };
  } finally {
    clearTimeout(timeout);
    signal?.removeEventListener("abort", onAbort);
  }
}
const now = () => overrides.now?.() ?? Date.now();

function stateDir(): string {
  return overrides.stateDir ?? process.env.HERDR_PLUGIN_STATE_DIR ?? join(process.env.HOME ?? homedir(), ".local", "state", "herdr-omni");
}

function statePath(dir: string): string { return join(dir, STATE_FILE); }
function lockPath(dir: string): string { return join(dir, LOCK_FILE); }

function readState(dir: string): State {
  try {
    const value: unknown = JSON.parse(readFileSync(statePath(dir), "utf8"));
    if (!value || typeof value !== "object") return {};
    const record = value as Record<string, unknown>;
    const result: State = {};
    const cache = record.cache;
    if (cache && typeof cache === "object") {
      const c = cache as Record<string, unknown>;
      if (typeof c.checkedAt === "number" && typeof c.currentVersion === "string") {
        const offer = c.offer;
        result.cache = { checkedAt: c.checkedAt, currentVersion: c.currentVersion };
        if (offer && typeof offer === "object" && typeof (offer as Record<string, unknown>).version === "string" && typeof (offer as Record<string, unknown>).ref === "string") {
          result.cache.offer = { version: (offer as Record<string, string>).version, ref: (offer as Record<string, string>).ref };
        }
      }
    }
    const snooze = record.snooze;
    if (snooze && typeof snooze === "object") {
      const s = snooze as Record<string, unknown>;
      if (typeof s.version === "string" && typeof s.dismissedAt === "number") result.snooze = { version: s.version, dismissedAt: s.dismissedAt };
    }
    const marker = record.updaterInstall;
    if (marker && typeof marker === "object") {
      const m = marker as Record<string, unknown>;
      if (typeof m.ref === "string" && typeof m.resolved_commit === "string") result.updaterInstall = { ref: m.ref, resolved_commit: m.resolved_commit };
    }
    return result;
  } catch { return {}; }
}

function writeState(dir: string, state: State): void {
  try {
    mkdirSync(dir, { recursive: true, mode: 0o700 });
    const temporary = join(dir, `${STATE_FILE}.${process.pid}.${Math.random().toString(16).slice(2)}.tmp`);
    writeFileSync(temporary, `${JSON.stringify(state)}\n`, { encoding: "utf8", mode: 0o600 });
    renameSync(temporary, statePath(dir));
  } catch {
    // Cache and snooze data are conveniences. A read-only state directory must not break updates.
  }
}

function parseVersion(value: string): bigint[] | undefined {
  const match = /^v(0|[1-9]\d*)\.(0|[1-9]\d*)\.(0|[1-9]\d*)$/.exec(value);
  if (!match) return undefined;
  return [BigInt(match[1]!), BigInt(match[2]!), BigInt(match[3]!)];
}

function compareVersions(left: bigint[], right: bigint[]): number {
  for (let index = 0; index < 3; index++) if (left[index]! !== right[index]!) return left[index]! > right[index]! ? 1 : -1;
  return 0;
}

function normalizeTag(value: unknown): string | undefined {
  if (typeof value !== "string") return undefined;
  return value.startsWith("refs/tags/") ? value.slice("refs/tags/".length) : value;
}

function isOffer(value: unknown): value is UpdateOffer {
  if (!value || typeof value !== "object") return false;
  const offer = value as Record<string, unknown>;
  return typeof offer.version === "string" && typeof offer.ref === "string" && offer.ref === `v${offer.version}` && !!parseVersion(offer.ref);
}

async function abortable<T>(promise: Promise<T>, signal?: AbortSignal): Promise<T | undefined> {
  if (!signal) return promise;
  if (signal.aborted) return undefined;
  return new Promise(resolve => {
    const finish = (value: T | undefined) => { signal.removeEventListener("abort", onAbort); resolve(value); };
    const onAbort = () => finish(undefined);
    signal.addEventListener("abort", onAbort, { once: true });
    promise.then(value => finish(value), () => finish(undefined));
  });
}

function pluginFrom(stdout: string): Plugin | undefined {
  try {
    const plugins = JSON.parse(stdout)?.result?.plugins;
    if (!Array.isArray(plugins)) return undefined;
    const plugin = plugins.find((entry: unknown) => {
      if (!entry || typeof entry !== "object") return false;
      const value = entry as Plugin;
      return value.plugin_id === PLUGIN;
    });
    return plugin as Plugin | undefined;
  } catch { return undefined; }
}

function officialSource(plugin: Plugin): Eligibility["source"] | undefined {
  if (plugin.enabled !== true || !plugin.source || typeof plugin.source !== "object") return undefined;
  const source = plugin.source as Record<string, unknown>;
  if (source.kind !== "github" || source.owner !== OWNER || source.repo !== REPOSITORY || typeof source.resolved_commit !== "string" || !source.resolved_commit) return undefined;
  if (source.subdir !== undefined && source.subdir !== null && source.subdir !== "") return undefined;
  const requested = source.requested_ref;
  if (requested !== undefined && requested !== null && typeof requested !== "string") return undefined;
  return { kind: "github", owner: OWNER, repo: REPOSITORY, resolved_commit: source.resolved_commit, ...(typeof requested === "string" ? { requested_ref: requested } : {}) };
}

function bareVersion(value: unknown): string | undefined {
  if (typeof value !== "string") return undefined;
  const normalized = value.startsWith("v") ? value.slice(1) : value;
  return parseVersion(`v${normalized}`) ? normalized : undefined;
}

function githubSource(plugin: Plugin, state: State): Eligibility["source"] | undefined {
  const source = officialSource(plugin);
  if (!source) return undefined;
  if (source.requested_ref !== undefined && (!state.updaterInstall || state.updaterInstall.ref !== source.requested_ref || state.updaterInstall.resolved_commit !== source.resolved_commit)) return undefined;
  return source;
}

async function inspect(signal?: AbortSignal): Promise<Eligibility | undefined> {
  const dir = stateDir();
  const state = readState(dir);
  const result = await abortable(run(["plugin", "list", "--plugin", PLUGIN, "--json"], CHECK_TIMEOUT_MS, homedir()), signal);
  if (!result || result.code !== 0) return undefined;
  const plugin = pluginFrom(result.stdout);
  if (!plugin) return undefined;
  const source = githubSource(plugin, state);
  return source ? { plugin, source, state, stateDir: dir } : undefined;
}

/** Read the post-install metadata without applying the pre-install pin check. */
async function inspectInstalled(): Promise<Eligibility | undefined> {
  const dir = stateDir();
  const state = readState(dir);
  const result = await run(["plugin", "list", "--plugin", PLUGIN, "--json"], CHECK_TIMEOUT_MS, homedir());
  if (!result || result.code !== 0) return undefined;
  const plugin = pluginFrom(result.stdout);
  const source = plugin ? officialSource(plugin) : undefined;
  return source ? { plugin: plugin!, source, state, stateDir: dir } : undefined;
}

function snoozed(state: State, offer: UpdateOffer): boolean {
  return !!state.snooze && state.snooze.version === offer.version && now() - state.snooze.dismissedAt < SNOOZE_MS;
}

async function discoverTags(signal?: AbortSignal): Promise<{ ok: true; offer?: UpdateOffer } | { ok: false }> {
  try {
    const result = await abortable(runGit(["ls-remote", "--tags", `https://github.com/${OWNER}/${REPOSITORY}.git`, "refs/tags/v*"], CHECK_TIMEOUT_MS, homedir(), signal), signal);
    if (!result || result.code !== 0) return { ok: false };
    let best: UpdateOffer | undefined;
    for (const line of result.stdout.split(/\r?\n/)) {
      const tag = normalizeTag(line.split(/\s+/)[1]);
      const parsed = tag ? parseVersion(tag) : undefined;
      if (tag && parsed && (!best || compareVersions(parsed, parseVersion(best.ref)!) > 0)) best = { version: tag.slice(1), ref: tag };
    }
    return { ok: true, ...(best ? { offer: best } : {}) };
  } catch { return { ok: false }; }
}

function cachedOffer(state: State, currentVersion: string): UpdateOffer | undefined | null {
  if (!state.cache || state.cache.currentVersion !== currentVersion || now() - state.cache.checkedAt >= CACHE_MS) return null;
  return state.cache.offer && isOffer(state.cache.offer) ? state.cache.offer : undefined;
}

export async function checkForUpdate(currentVersion: string, signal?: AbortSignal): Promise<UpdateOffer | undefined> {
  if (signal?.aborted || !parseVersion(`v${currentVersion.replace(/^v/, "")}`)) return undefined;
  const eligibility = await inspect(signal);
  if (!eligibility || signal?.aborted) return undefined;
  const cached = cachedOffer(eligibility.state, currentVersion);
  const discovered = cached === null ? await discoverTags(signal) : undefined;
  const candidate = cached === null ? (discovered?.ok ? discovered.offer : undefined) : cached;
  if (signal?.aborted) return undefined;
  const current = parseVersion(`v${currentVersion.replace(/^v/, "")}`)!;
  const offer = candidate && isOffer(candidate) && compareVersions(parseVersion(candidate.ref)!, current) > 0 ? candidate : undefined;
  if (cached === null && discovered?.ok) {
    // Merge with a fresh read: an install or dismiss in another popup may have
    // written updaterInstall/snooze since eligibility was inspected.
    const fresh = readState(eligibility.stateDir);
    fresh.cache = { checkedAt: now(), currentVersion, ...(offer ? { offer } : {}) };
    writeState(eligibility.stateDir, fresh);
  }
  return offer && !snoozed(readState(eligibility.stateDir), offer) ? offer : undefined;
}

function staleLock(path: string): boolean {
  try {
    const lock = JSON.parse(readFileSync(path, "utf8")) as { pid?: unknown; createdAt?: unknown };
    if (typeof lock.createdAt !== "number" || now() - lock.createdAt < LOCK_STALE_MS) return false;
    if (typeof lock.pid !== "number" || lock.pid <= 0) return true;
    try { process.kill(lock.pid, 0); return false; } catch (error) { return (error as NodeJS.ErrnoException).code === "ESRCH"; }
  } catch {
    // An active owner can be observed between open(O_EXCL) and its metadata
    // write. Use mtime so that only an old, malformed orphan is reclaimed.
    try { return now() - statSync(path).mtimeMs >= LOCK_STALE_MS; } catch { return false; }
  }
}

function acquireLock(dir: string): string | undefined {
  try { mkdirSync(dir, { recursive: true, mode: 0o700 }); } catch { return undefined; }
  const path = lockPath(dir);
  for (let attempt = 0; attempt < 2; attempt++) {
    try {
      const fd = openSync(path, "wx", 0o600);
      try { writeFileSync(fd, JSON.stringify({ pid: process.pid, createdAt: now(), token: Math.random().toString(16) })); } finally { closeSync(fd); }
      return path;
    } catch (error) {
      if ((error as NodeJS.ErrnoException).code !== "EEXIST") return undefined;
      let before: string;
      try { before = readFileSync(path, "utf8"); } catch { continue; }
      if (!staleLock(path)) return undefined;
      // Do not unlink a lock that changed while stale ownership was checked.
      try { if (readFileSync(path, "utf8") !== before) continue; } catch { continue; }
      try { unlinkSync(path); } catch { return undefined; }
    }
  }
  return undefined;
}

function releaseLock(path: string): void { try { unlinkSync(path); } catch {} }

export async function installUpdate(offer: UpdateOffer): Promise<CommandResult> {
  if (!isOffer(offer)) return { ok: false, message: "Invalid update offer." };
  const dir = stateDir();
  const lock = acquireLock(dir);
  if (!lock) return { ok: false, message: "Another update is already in progress." };
  try {
    const eligibility = await inspect();
    if (!eligibility) return { ok: false, message: "herdr-omni is no longer eligible for automatic updates." };
    const installed = bareVersion(eligibility.plugin.version);
    const offered = parseVersion(offer.ref);
    if (!installed) return { ok: false, message: "Unable to determine the installed version." };
    if (installed && offered && compareVersions(offered, parseVersion(`v${installed}`)!) <= 0) return { ok: false, message: "This update is no longer newer than the installed version." };
    const result = await run(["plugin", "install", `${OWNER}/${REPOSITORY}`, "--ref", offer.ref, "--yes"], undefined, homedir());
    if (result.code !== 0) return { ok: false, message: herdr.explain(result.stderr, result.code) };
    const after = await inspectInstalled();
    if (!after || after.source.requested_ref !== offer.ref || bareVersion(after.plugin.version) !== offer.version) {
      return { ok: false, message: "Update finished but could not be verified. Check herdr plugin list and reopen Omni." };
    }
    if (after.source.requested_ref === offer.ref) {
      const fresh = readState(dir);
      fresh.updaterInstall = { ref: offer.ref, resolved_commit: after.source.resolved_commit };
      writeState(dir, fresh);
    }
    return { ok: true, message: "" };
  } catch { return { ok: false, message: "Unable to install the update." }; }
  finally { releaseLock(lock); }
}

export function dismissUpdate(offer: UpdateOffer): void {
  if (!isOffer(offer)) return;
  const dir = stateDir();
  const state = readState(dir);
  state.snooze = { version: offer.version, dismissedAt: now() };
  // Preserve a marker/cache written by an updater between the first read and this write.
  const fresh = readState(dir);
  fresh.snooze = state.snooze;
  writeState(dir, fresh);
}

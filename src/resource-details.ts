/** A concrete resource for which the palette may request a small detail string. */
export type ResourceDetailTarget =
  | { kind: "workspace"; workspaceId: string; path?: string }
  | { kind: "pane"; paneId: string };

export type ResourceDetailsReader = (
  target: ResourceDetailTarget,
  signal: AbortSignal,
) => Promise<string | undefined>;

export interface ResourceDetailsReaderOptions {
  /** Override the Herdr executable (primarily useful for tests). */
  readonly command?: string;
  /** Alias for command for callers that describe the executable as a binary. */
  readonly binary?: string;
  readonly timeoutMs?: number;
  readonly maxBytes?: number;
}

export const RESOURCE_DETAILS_TIMEOUT_MS = 3_000;
export const RESOURCE_DETAILS_MAX_BYTES = 256 * 1024;

/** A stable, non-sensitive error for command, timeout, and output failures. */
export class ResourceDetailsUnavailableError extends Error {
  constructor(reason?: "timed out" | "response too large") {
    super(`Resource details unavailable${reason ? ` (${reason})` : ""}.`);
    this.name = "ResourceDetailsUnavailableError";
  }
}

function abortError(): DOMException {
  return new DOMException("Resource details request was aborted.", "AbortError");
}

function isAbortError(error: unknown): error is DOMException {
  return error instanceof DOMException && error.name === "AbortError";
}

function record(value: unknown): Record<string, unknown> | undefined {
  return typeof value === "object" && value !== null ? value as Record<string, unknown> : undefined;
}

function stringValue(value: unknown): string | undefined {
  return typeof value === "string" && value.length > 0 ? value : undefined;
}

async function readLimited(
  stream: ReadableStream<Uint8Array>,
  maxBytes: number,
  overflow: () => void,
): Promise<string> {
  const reader = stream.getReader();
  const decoder = new TextDecoder();
  let bytes = 0;
  let text = "";
  try {
    while (true) {
      const { done, value } = await reader.read();
      if (done) return text + decoder.decode();
      bytes += value.byteLength;
      if (bytes > maxBytes) {
        overflow();
        throw new ResourceDetailsUnavailableError("response too large");
      }
      text += decoder.decode(value, { stream: true });
    }
  } finally {
    reader.releaseLock();
  }
}

function commandFor(options: ResourceDetailsReaderOptions): string {
  return options.command ?? options.binary ?? process.env.HERDR_BIN_PATH ?? "herdr";
}

function positiveOption(value: number | undefined, fallback: number): number {
  return typeof value === "number" && Number.isFinite(value) ? Math.max(1, value) : fallback;
}

function sourceCheckoutPath(payload: Record<string, unknown>, result: Record<string, unknown>): string | undefined {
  const source = record(result.source) ?? record(payload.source);
  return stringValue(source?.source_checkout_path);
}

function workspaceDetail(payload: unknown, target: Extract<ResourceDetailTarget, { kind: "workspace" }>): string | undefined {
  const root = record(payload);
  const result = record(root?.result);
  if (!result) return undefined;

  const rows = Array.isArray(result.worktrees)
    ? result.worktrees.map(record).filter((row): row is Record<string, unknown> => row !== undefined)
    : [];
  const sourcePath = sourceCheckoutPath(root ?? {}, result);

  // open_workspace_id is the authoritative identity. Path matching is only a
  // fallback for older Herdr responses that did not include that field.
  const row = rows.find(candidate => candidate.open_workspace_id === target.workspaceId)
    ?? (target.path ? rows.find(candidate => candidate.path === target.path) : undefined)
    ?? (sourcePath ? rows.find(candidate => candidate.path === sourcePath) : undefined);
  if (!row) return undefined;
  if (row.is_detached === true) return "detached HEAD";
  return stringValue(row.branch);
}

function processNameValues(process: Record<string, unknown>): string[] {
  const names = process.names;
  if (Array.isArray(names)) {
    const values = names.filter((value): value is string => typeof value === "string" && value.length > 0);
    if (values.length > 0) return values;
  } else if (typeof names === "string" && names.length > 0) {
    return [names];
  }
  const name = stringValue(process.name);
  if (name) return [name];
  const argv0 = stringValue(process.argv0);
  return argv0 ? [argv0] : [];
}

function paneDetail(payload: unknown, target: Extract<ResourceDetailTarget, { kind: "pane" }>): string | undefined {
  const processInfo = record(record(payload)?.result)?.process_info;
  const info = record(processInfo);
  if (!info || info.pane_id !== target.paneId || !Array.isArray(info.foreground_processes)) return undefined;

  const names: string[] = [];
  for (const value of info.foreground_processes) {
    const process = record(value);
    if (!process) continue;
    for (const name of processNameValues(process)) {
      if (!names.includes(name)) names.push(name);
    }
  }
  return names.length > 0 ? names.join(" | ") : undefined;
}

function parseDetail(payload: unknown, target: ResourceDetailTarget): string | undefined {
  return target.kind === "workspace" ? workspaceDetail(payload, target) : paneDetail(payload, target);
}

async function readWithOptions(
  target: ResourceDetailTarget,
  signal: AbortSignal,
  options: ResourceDetailsReaderOptions,
): Promise<string | undefined> {
  if (signal.aborted) throw abortError();

  const id = target.kind === "workspace" ? target.workspaceId : target.paneId;
  if (!id) return undefined;

  const timeoutMs = positiveOption(options.timeoutMs, RESOURCE_DETAILS_TIMEOUT_MS);
  const maxBytes = positiveOption(options.maxBytes, RESOURCE_DETAILS_MAX_BYTES);
  const argv = target.kind === "workspace"
    ? ["worktree", "list", "--workspace", id]
    : ["pane", "process-info", "--pane", id];

  let child: ReturnType<typeof Bun.spawn>;
  try {
    child = Bun.spawn([commandFor(options), ...argv], { stdout: "pipe", stderr: "ignore" });
  } catch {
    throw new ResourceDetailsUnavailableError();
  }

  let timedOut = false;
  let aborted = false;
  let timeout: ReturnType<typeof setTimeout> | undefined;
  const kill = () => { try { child.kill(); } catch {} };
  const onAbort = () => { aborted = true; kill(); };
  signal.addEventListener("abort", onAbort, { once: true });
  if (signal.aborted) onAbort();
  timeout = setTimeout(() => { timedOut = true; kill(); }, timeoutMs);

  const exited = child.exited;
  try {
    if (!child.stdout || typeof child.stdout === "number") throw new ResourceDetailsUnavailableError();
    const output = await readLimited(child.stdout, maxBytes, kill);
    const code = await exited;
    if (aborted) throw abortError();
    if (timedOut) throw new ResourceDetailsUnavailableError("timed out");
    if (code !== 0) throw new ResourceDetailsUnavailableError();
    let payload: unknown;
    try {
      payload = JSON.parse(output);
    } catch {
      throw new ResourceDetailsUnavailableError();
    }
    return parseDetail(payload, target);
  } catch (error) {
    kill();
    await exited.catch(() => -1);
    if (isAbortError(error) || error instanceof ResourceDetailsUnavailableError) throw error;
    if (aborted) throw abortError();
    if (timedOut) throw new ResourceDetailsUnavailableError("timed out");
    throw new ResourceDetailsUnavailableError();
  } finally {
    if (timeout) clearTimeout(timeout);
    signal.removeEventListener("abort", onAbort);
  }
}

/** Build a reader with bounded process/output behavior; the default reader uses Herdr. */
export function createResourceDetailsReader(options: ResourceDetailsReaderOptions = {}): ResourceDetailsReader {
  return (target, signal) => readWithOptions(target, signal, options);
}

/** Read a resource detail using Herdr's default executable. */
export function readResourceDetails(
  target: ResourceDetailTarget,
  signal: AbortSignal,
  options: ResourceDetailsReaderOptions = {},
): Promise<string | undefined> {
  return readWithOptions(target, signal, options);
}

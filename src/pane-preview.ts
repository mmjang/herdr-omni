/** The small, read-only contract used by pane preview consumers. */
export type PanePreviewReader = (paneId: string, signal: AbortSignal) => Promise<string>;

export interface PanePreviewReaderOptions {
  /** Override the Herdr executable (primarily useful for tests). */
  readonly command?: string;
  readonly timeoutMs?: number;
  readonly maxBytes?: number;
}

export const PANE_PREVIEW_TIMEOUT_MS = 3_000;
export const PANE_PREVIEW_MAX_BYTES = 256 * 1024;

/** A stable, non-sensitive error for command, timeout, and output failures. */
export class PanePreviewUnavailableError extends Error {
  constructor(reason?: "timed out" | "response too large") {
    super(`Pane preview unavailable${reason ? ` (${reason})` : ""}.`);
    this.name = "PanePreviewUnavailableError";
  }
}

function abortError(): DOMException {
  return new DOMException("Pane preview request was aborted.", "AbortError");
}

function isAbortError(error: unknown): error is DOMException {
  return error instanceof DOMException && error.name === "AbortError";
}

function skipCsi(text: string, start: number): number {
  let index = start;
  while (index < text.length) {
    const code = text.charCodeAt(index++);
    if (code >= 0x40 && code <= 0x7e) return index;
  }
  return index;
}

function skipEscape(text: string, start: number): number {
  let index = start;
  while (index < text.length) {
    const code = text.charCodeAt(index++);
    // ESC intermediates are followed by one final byte in the 0x30–0x7e range.
    if (code >= 0x30 && code <= 0x7e) return index;
    if (code < 0x20 || code > 0x2f) return index;
  }
  return index;
}

function skipStringControl(text: string, start: number): number {
  let index = start;
  while (index < text.length) {
    const code = text.charCodeAt(index++);
    if (code === 0x07 || code === 0x9c) return index;
    if (code === 0x1b && text.charCodeAt(index) === 0x5c) return index + 1;
  }
  return index;
}

/** Remove terminal decoration/control sequences while retaining screen whitespace. */
export function stripTerminalSequences(text: string): string {
  let clean = "";
  let index = 0;
  while (index < text.length) {
    const code = text.charCodeAt(index);
    if (code === 0x1b) {
      const next = text.charCodeAt(index + 1);
      if (next === 0x5b) { index = skipCsi(text, index + 2); continue; }
      if (next === 0x5d || next === 0x50 || next === 0x58 || next === 0x5e || next === 0x5f) {
        index = skipStringControl(text, index + 2); continue;
      }
      index = skipEscape(text, index + 1);
      continue;
    }
    if (code === 0x9b) { index = skipCsi(text, index + 1); continue; }
    if (code === 0x9d || code === 0x90 || code === 0x98 || code === 0x9e || code === 0x9f) {
      index = skipStringControl(text, index + 1); continue;
    }
    // Newlines and tabs are part of the visible layout. Other C0/C1 controls are not.
    if (code === 0x0a || code === 0x09) clean += text[index]!;
    else if (code >= 0x20 && code !== 0x7f && !(code >= 0x80 && code <= 0x9f)) clean += text[index]!;
    index++;
  }
  return clean;
}

async function readLimited(stream: ReadableStream<Uint8Array>, maxBytes: number, overflow: () => void): Promise<string> {
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
        throw new PanePreviewUnavailableError("response too large");
      }
      text += decoder.decode(value, { stream: true });
    }
  } finally {
    reader.releaseLock();
  }
}

function commandFor(options: PanePreviewReaderOptions): string {
  return options.command ?? process.env.HERDR_BIN_PATH ?? "herdr";
}

async function readWithOptions(paneId: string, signal: AbortSignal, options: PanePreviewReaderOptions): Promise<string> {
  if (signal.aborted) throw abortError();
  if (!paneId) throw new PanePreviewUnavailableError();

  const timeoutMs = Math.max(1, options.timeoutMs ?? PANE_PREVIEW_TIMEOUT_MS);
  const maxBytes = Math.max(1, options.maxBytes ?? PANE_PREVIEW_MAX_BYTES);
  let child: ReturnType<typeof Bun.spawn>;
  try {
    child = Bun.spawn([commandFor(options), "pane", "read", paneId, "--source", "visible", "--format", "text"], {
      stdout: "pipe",
      stderr: "ignore",
    });
  } catch {
    throw new PanePreviewUnavailableError();
  }

  let timedOut = false;
  let aborted = false;
  let timeout: ReturnType<typeof setTimeout> | undefined;
  const kill = () => { try { child.kill(); } catch {} };
  const onAbort = () => { aborted = true; kill(); };
  signal.addEventListener("abort", onAbort, { once: true });
  timeout = setTimeout(() => { timedOut = true; kill(); }, timeoutMs);

  const exited = child.exited;
  try {
    if (!child.stdout || typeof child.stdout === "number") throw new PanePreviewUnavailableError();
    const output = await readLimited(child.stdout, maxBytes, kill);
    const code = await exited;
    if (aborted) throw abortError();
    if (timedOut) throw new PanePreviewUnavailableError("timed out");
    if (code !== 0) throw new PanePreviewUnavailableError();
    return stripTerminalSequences(output);
  } catch (error) {
    kill();
    await exited.catch(() => -1);
    if (isAbortError(error) || error instanceof PanePreviewUnavailableError) throw error;
    if (aborted) throw abortError();
    if (timedOut) throw new PanePreviewUnavailableError("timed out");
    throw new PanePreviewUnavailableError();
  } finally {
    if (timeout) clearTimeout(timeout);
    signal.removeEventListener("abort", onAbort);
  }
}

/** Build a reader with bounded process/output behavior; the default reader uses Herdr. */
export function createPanePreviewReader(options: PanePreviewReaderOptions = {}): PanePreviewReader {
  return (paneId, signal) => readWithOptions(paneId, signal, options);
}

export const readPanePreview: PanePreviewReader = createPanePreviewReader();

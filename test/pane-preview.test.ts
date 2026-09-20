import { mkdtemp, rm } from "node:fs/promises";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { expect, test } from "bun:test";
import {
  createPanePreviewReader,
  stripTerminalSequences,
} from "../src/pane-preview";

async function mockHerdr(body: string) {
  const directory = await mkdtemp(join(tmpdir(), "herdr-pane-preview-"));
  const command = join(directory, "herdr");
  await Bun.write(command, `#!/bin/sh\n${body}\n`);
  const chmod = Bun.spawn(["chmod", "+x", command]);
  if (await chmod.exited !== 0) throw new Error("could not make mock executable");
  return { command, dispose: () => rm(directory, { recursive: true, force: true }) };
}

test("reads visible text with the explicit read argv and preserves layout", async () => {
  const mock = await mockHerdr("printf '%s\\n' \"$@\"");
  try {
    const read = createPanePreviewReader({ command: mock.command });
    const result = await read("pane; echo should-not-run", new AbortController().signal);
    expect(result).toBe("pane\nread\npane; echo should-not-run\n--source\nvisible\n--format\ntext\n");
  } finally {
    await mock.dispose();
  }
});

test("strips terminal sequences without trimming visible whitespace", async () => {
  expect(stripTerminalSequences("\x1b[31m  title\x1b[0m\n\tsecond\n\x1b]0;ignored\x07third"))
    .toBe("  title\n\tsecond\nthird");
  expect(stripTerminalSequences("before\x1b(Bafter\n")).toBe("beforeafter\n");

  const mock = await mockHerdr("printf '\\033[32m  visible\\033[0m\\n\\tline\\n'");
  try {
    const read = createPanePreviewReader({ command: mock.command });
    await expect(read("pane", new AbortController().signal)).resolves.toBe("  visible\n\tline\n");
  } finally {
    await mock.dispose();
  }
});

test("cancelling a read kills the child and rejects as aborted", async () => {
  const mock = await mockHerdr("sleep 10");
  try {
    const controller = new AbortController();
    const read = createPanePreviewReader({ command: mock.command, timeoutMs: 2_000 });
    const pending = read("pane", controller.signal);
    await new Promise(resolve => setTimeout(resolve, 20));
    controller.abort();
    await expect(pending).rejects.toMatchObject({ name: "AbortError" });
  } finally {
    await mock.dispose();
  }
});

test("bounds timeout and response size", async () => {
  const slow = await mockHerdr("sleep 10");
  try {
    const read = createPanePreviewReader({ command: slow.command, timeoutMs: 30 });
    await expect(read("pane", new AbortController().signal)).rejects.toThrow("Pane preview unavailable (timed out).");
  } finally {
    await slow.dispose();
  }

  const large = await mockHerdr("head -c 33 /dev/zero");
  try {
    const read = createPanePreviewReader({ command: large.command, maxBytes: 32 });
    await expect(read("pane", new AbortController().signal)).rejects.toThrow("response too large");
  } finally {
    await large.dispose();
  }
});

test("hides stderr and reports nonzero exits generically", async () => {
  const mock = await mockHerdr("printf 'secret backend details\\n' >&2; exit 9");
  try {
    const read = createPanePreviewReader({ command: mock.command });
    const error = await read("pane", new AbortController().signal).catch(value => value as Error);
    const message = error instanceof Error ? error.message : String(error);
    expect(message).toBe("Pane preview unavailable.");
    expect(message).not.toContain("secret backend details");
  } finally {
    await mock.dispose();
  }
});

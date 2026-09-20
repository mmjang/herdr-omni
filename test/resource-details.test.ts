import { mkdtemp, rm } from "node:fs/promises";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { expect, test } from "bun:test";
import {
  createResourceDetailsReader,
  readResourceDetails,
} from "../src/resource-details";

async function mockHerdr(body: string) {
  const directory = await mkdtemp(join(tmpdir(), "herdr-resource-details-"));
  const command = join(directory, "herdr");
  await Bun.write(command, `#!/bin/sh\n${body}\n`);
  const chmod = Bun.spawn(["chmod", "+x", command]);
  if (await chmod.exited !== 0) throw new Error("could not make mock executable");
  return { command, dispose: () => rm(directory, { recursive: true, force: true }) };
}

test("reads a workspace branch using the exact workspace identity", async () => {
  const mock = await mockHerdr("printf '%s' '{\"result\":{\"worktrees\":[{\"path\":\"/unrelated\",\"branch\":\"wrong\",\"open_workspace_id\":\"other\"},{\"path\":\"/repo\",\"branch\":\"feature/api\",\"open_workspace_id\":\"workspace-1\"}]}}'");
  try {
    const read = createResourceDetailsReader({ command: mock.command });
    await expect(read({ kind: "workspace", workspaceId: "workspace-1" }, new AbortController().signal))
      .resolves.toBe("feature/api");
  } finally {
    await mock.dispose();
  }
});

test("uses path and source checkout metadata only as identity fallbacks", async () => {
  const pathMock = await mockHerdr("printf '%s' '{\"result\":{\"worktrees\":[{\"path\":\"/repo\",\"branch\":\"main\"},{\"path\":\"/other\",\"branch\":\"do-not-use\"}]}}'");
  try {
    const read = createResourceDetailsReader({ command: pathMock.command });
    await expect(read({ kind: "workspace", workspaceId: "missing", path: "/repo" }, new AbortController().signal))
      .resolves.toBe("main");
  } finally {
    await pathMock.dispose();
  }

  const sourceMock = await mockHerdr("printf '%s' '{\"result\":{\"source\":{\"source_checkout_path\":\"/checkout\"},\"worktrees\":[{\"path\":\"/checkout\",\"branch\":\"source-branch\"},{\"path\":\"/other\",\"branch\":\"do-not-use\"}]}}'");
  try {
    const read = createResourceDetailsReader({ command: sourceMock.command });
    await expect(read({ kind: "workspace", workspaceId: "missing" }, new AbortController().signal))
      .resolves.toBe("source-branch");
  } finally {
    await sourceMock.dispose();
  }
});

test("reports detached worktrees and leaves unrelated worktrees unknown", async () => {
  const mock = await mockHerdr("printf '%s' '{\"result\":{\"worktrees\":[{\"path\":\"/repo\",\"is_detached\":true,\"open_workspace_id\":\"w1\"},{\"path\":\"/other\",\"branch\":\"unrelated\"}]}}'");
  try {
    const read = createResourceDetailsReader({ command: mock.command });
    await expect(read({ kind: "workspace", workspaceId: "w1" }, new AbortController().signal))
      .resolves.toBe("detached HEAD");
    await expect(read({ kind: "workspace", workspaceId: "not-open", path: "/unknown" }, new AbortController().signal))
      .resolves.toBeUndefined();
  } finally {
    await mock.dispose();
  }
});

test("validates pane identity and returns unique foreground names without arguments", async () => {
  const mock = await mockHerdr("printf '%s' '{\"result\":{\"process_info\":{\"pane_id\":\"pane-1\",\"foreground_processes\":[{\"name\":\"node\",\"argv\":[\"node\",\"--token=secret\"]},{\"name\":\"node\",\"argv0\":\"ignored\"},{\"argv0\":\"zsh\",\"argv\":[\"zsh\",\"-lc\",\"secret\"]}]}}}'");
  try {
    const read = createResourceDetailsReader({ command: mock.command });
    const detail = await read({ kind: "pane", paneId: "pane-1" }, new AbortController().signal);
    expect(detail).toBe("node | zsh");
    expect(detail).not.toContain("secret");
    await expect(read({ kind: "pane", paneId: "other-pane" }, new AbortController().signal))
      .resolves.toBeUndefined();
  } finally {
    await mock.dispose();
  }
});

test("uses explicit argv and supports the direct options overload", async () => {
  const mock = await mockHerdr("[ \"$1\" = pane ] && [ \"$2\" = process-info ] && [ \"$3\" = --pane ] && [ \"$4\" = 'p; echo no' ] || exit 9; printf '%s' '{\"result\":{\"process_info\":{\"pane_id\":\"p; echo no\",\"foreground_processes\":[{\"name\":\"bash\"}]}}}'");
  try {
    const detail = await readResourceDetails({ kind: "pane", paneId: "p; echo no" }, new AbortController().signal, { command: mock.command });
    expect(detail).toBe("bash");
  } finally {
    await mock.dispose();
  }
});

test("cancels, times out, and caps output with generic errors", async () => {
  const slow = await mockHerdr("sleep 10");
  try {
    const controller = new AbortController();
    const pending = readResourceDetails({ kind: "pane", paneId: "pane" }, controller.signal, { command: slow.command, timeoutMs: 2_000 });
    await new Promise(resolve => setTimeout(resolve, 20));
    controller.abort();
    await expect(pending).rejects.toMatchObject({ name: "AbortError" });

    await expect(readResourceDetails({ kind: "pane", paneId: "pane" }, new AbortController().signal, { command: slow.command, timeoutMs: 20 }))
      .rejects.toThrow("Resource details unavailable (timed out).");
  } finally {
    await slow.dispose();
  }

  const large = await mockHerdr("head -c 33 /dev/zero");
  try {
    await expect(readResourceDetails({ kind: "pane", paneId: "pane" }, new AbortController().signal, { command: large.command, maxBytes: 32 }))
      .rejects.toThrow("response too large");
  } finally {
    await large.dispose();
  }
});

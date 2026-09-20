import { expect, test } from "bun:test";
import { createServer, type Socket } from "node:net";
import { mkdtempSync, rmSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { requestHerdr } from "../src/socket";
import { execute } from "../src/execute";
import { defaultItems } from "../src/catalog";
import { itemsFromSnapshot } from "../src/live";

async function withServer(handle: (socket: Socket, request: any) => void, run: (path: string) => Promise<void>) {
  const directory = mkdtempSync(join(tmpdir(), "omni-api-"));
  const path = join(directory, "api.sock");
  const server = createServer(socket => {
    let data = "";
    socket.on("data", chunk => {
      data += chunk.toString();
      if (data.includes("\n")) handle(socket, JSON.parse(data.trim()));
    });
  });
  await new Promise<void>(resolve => server.listen(path, resolve));
  try { await run(path); }
  finally {
    await new Promise<void>(resolve => server.close(() => resolve()));
    rmSync(directory, { recursive: true, force: true });
  }
}

test("Edit scrollback invokes the API for the launch pane, accepting chunked responses", async () => {
  await withServer((socket, request) => {
    expect(request.method).toBe("pane.edit_scrollback");
    expect(request.params).toEqual({ pane_id: "w2:p4" });
    const response = JSON.stringify({ id: request.id, result: { type: "ok" } }) + "\n";
    socket.write(response.slice(0, 12));
    setTimeout(() => socket.end(response.slice(12)), 5);
  }, async path => {
    const previousPath = process.env.HERDR_SOCKET_PATH;
    const previousContext = process.env.HERDR_PLUGIN_CONTEXT_JSON;
    process.env.HERDR_SOCKET_PATH = path;
    process.env.HERDR_PLUGIN_CONTEXT_JSON = JSON.stringify({ workspace_id: "w2", tab_id: "w2:t1", focused_pane_id: "w2:p4" });
    try {
      expect(await execute(defaultItems().find(item => item.id === "edit_scrollback")!)).toEqual({ ok: true, message: "" });
    } finally {
      if (previousPath === undefined) delete process.env.HERDR_SOCKET_PATH;
      else process.env.HERDR_SOCKET_PATH = previousPath;
      if (previousContext === undefined) delete process.env.HERDR_PLUGIN_CONTEXT_JSON;
      else process.env.HERDR_PLUGIN_CONTEXT_JSON = previousContext;
    }
  });
});

test("API errors are shown without claiming success or retrying mutations", async () => {
  await withServer((socket, request) => socket.end(JSON.stringify({ id: request.id, error: { message: "pane is no longer focused" } }) + "\n"), async path => {
    expect(await requestHerdr("pane.edit_scrollback", { pane_id: "w2:p4" }, path)).toEqual({ ok: false, message: "pane is no longer focused" });
  });
});

test("selecting an agent focuses its pane through the API that moves the visible client", async () => {
  const [agent] = itemsFromSnapshot({ agents: [{ pane_id: "wR:p1", workspace_id: "wR", tab_id: "wR:t1", title: "KDI-5013 App inbox push notification" }] }, "w1V");
  for (const rejected of [false, true]) {
    let calls = 0;
    await withServer((socket, request) => {
      calls++;
      expect(request.method).toBe("pane.focus");
      expect(request.params).toEqual({ pane_id: "wR:p1" });
      socket.end(JSON.stringify({ id: request.id, ...(rejected
        ? { error: { message: "pane no longer exists" } }
        : { result: { type: "ok" } }) }) + "\n");
    }, async path => {
      const previousPath = process.env.HERDR_SOCKET_PATH;
      process.env.HERDR_SOCKET_PATH = path;
      try {
        expect(await execute(agent!)).toEqual(rejected
          ? { ok: false, message: "pane no longer exists" }
          : { ok: true, message: "" });
        expect(calls).toBe(1);
      } finally {
        if (previousPath === undefined) delete process.env.HERDR_SOCKET_PATH;
        else process.env.HERDR_SOCKET_PATH = previousPath;
      }
    });
  }
});

test("legacy shortcut-only invocations fail explicitly instead of claiming execution", async () => {
  const action = { ...defaultItems()[0]!, invocation: { kind: "shortcut" as const } };
  const result = await execute({ ...action, shortcuts: ["ctrl+alt+c"] });
  expect(result.ok).toBe(false);
  expect(result.message).toContain("Close Omni (Esc), then press ctrl+alt+c");
  expect(result.message).toContain("does not expose");
  expect((await execute({ ...action, shortcuts: [] })).message).toContain("Configure its Herdr keybinding");
});

test("socket requests fail safely on missing path, malformed data, disconnect, and timeout", async () => {
  expect((await requestHerdr("ping", {}, "")).ok).toBe(false);
  for (const handle of [(socket: Socket) => { socket.end("bad json\n"); }, (socket: Socket) => { socket.end(); }, (_socket: Socket) => {}]) {
    await withServer(handle, async path => {
      expect((await requestHerdr("ping", {}, path, 25)).ok).toBe(false);
    });
  }
});

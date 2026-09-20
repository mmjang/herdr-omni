import { expect, test } from "bun:test";
import { createServer } from "node:net";
import { mkdtempSync, readFileSync, rmSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { defaultItems } from "../src/catalog";
import { execute } from "../src/execute";

test("every searchable action executes the expected command or API against the popup host", async () => {
  const directory = mkdtempSync(join(tmpdir(), "omni-actions-"));
  const binary = join(directory, "herdr");
  const log = join(directory, "calls.jsonl");
  const socketPath = join(directory, "api.sock");
  const rpc: { method: string; params: unknown }[] = [];
  const server = createServer(socket => {
    let data = "";
    socket.on("data", chunk => {
      data += chunk.toString();
      if (!data.includes("\n")) return;
      const request = JSON.parse(data.trim());
      rpc.push({ method: request.method, params: request.params });
      socket.end(JSON.stringify({ id: request.id, result: { type: "ok" } }) + "\n");
    });
  });
  const lists = {
    "tab list --workspace w2": { tabs: [{ tab_id: "w2:t0" }, { tab_id: "w2:t1" }, { tab_id: "w2:t2" }] },
    "workspace list": { workspaces: [{ workspace_id: "w1" }, { workspace_id: "w2" }, { workspace_id: "w3" }] },
    "agent list": { agents: [{ pane_id: "w1:p1" }, { pane_id: "w2:p4" }, { pane_id: "w3:p1" }] },
  };
  writeFileSync(binary, `#!${process.execPath}
import { appendFileSync } from "node:fs";
const args = process.argv.slice(2);
appendFileSync(${JSON.stringify(log)}, JSON.stringify(args) + "\\n");
const lists = ${JSON.stringify(lists)};
console.log(JSON.stringify({result: lists[args.join(" ")] ?? {type: "ok"}}));
`, { mode: 0o755 });
  const env = {
    HERDR_BIN_PATH: binary,
    HERDR_SOCKET_PATH: socketPath,
    HERDR_PLUGIN_CONTEXT_JSON: JSON.stringify({ workspace_id: "w2", tab_id: "w2:t1", focused_pane_id: "w2:p4" }),
    HERDR_PANE_ID: undefined,
  };
  const previous = Object.fromEntries(Object.keys(env).map(key => [key, process.env[key]]));
  const commands: Record<string, string[]> = {
    new_workspace: ["workspace", "create", "--focus"],
    new_tab: ["tab", "create", "--workspace", "w2", "--focus"],
    rename_workspace: ["workspace", "rename", "w2", "A name"],
    rename_tab: ["tab", "rename", "w2:t1", "A name"],
    rename_pane: ["pane", "rename", "w2:p4", "A name"],
    clear_pane_name: ["pane", "rename", "w2:p4", "--clear"],
    close_workspace: ["workspace", "close", "w2"],
    close_tab: ["tab", "close", "w2:t1"],
    close_pane: ["pane", "close", "w2:p4"],
    previous_workspace: ["workspace", "focus", "w1"],
    next_workspace: ["workspace", "focus", "w3"],
    previous_tab: ["tab", "focus", "w2:t0"],
    next_tab: ["tab", "focus", "w2:t2"],
    split_vertical: ["pane", "split", "--pane", "w2:p4", "--direction", "right", "--focus"],
    split_horizontal: ["pane", "split", "--pane", "w2:p4", "--direction", "down", "--focus"],
    zoom: ["pane", "zoom", "--pane", "w2:p4"],
    move_pane_new_tab: ["pane", "move", "w2:p4", "--new-tab", "--focus"],
    move_pane_new_workspace: ["pane", "move", "w2:p4", "--new-workspace", "--focus"],
    new_worktree: ["worktree", "create", "--workspace", "w2", "--branch", "feature/topic", "--focus"],
    open_worktree: ["worktree", "open", "--workspace", "w2", "--branch", "feature/topic", "--focus"],
    remove_worktree: ["worktree", "remove", "--workspace", "w2"],
  };
  for (const verb of ["focus", "resize", "swap"]) {
    for (const direction of ["left", "right", "up", "down"]) {
      commands[`${verb}_pane_${direction}`] = ["pane", verb, "--direction", direction, "--pane", "w2:p4"];
    }
  }
  const methods: Record<string, { method: string; params: unknown }> = {
    previous_agent: { method: "pane.focus", params: { pane_id: "w1:p1" } },
    next_agent: { method: "pane.focus", params: { pane_id: "w3:p1" } },
    edit_scrollback: { method: "pane.edit_scrollback", params: { pane_id: "w2:p4" } },
  };
  await new Promise<void>(resolve => server.listen(socketPath, resolve));
  try {
    for (const [key, value] of Object.entries(env)) {
      if (value === undefined) delete process.env[key]; else process.env[key] = value;
    }
    expect(defaultItems().map(item => item.id).sort()).toEqual([...Object.keys(commands), ...Object.keys(methods)].sort());
    for (const item of defaultItems()) {
      writeFileSync(log, "");
      rpc.length = 0;
      const input = item.id === "remove_worktree" ? "yes" : item.id.endsWith("worktree") ? " feature/topic " : " A name ";
      expect(await execute(item, input)).toEqual({ ok: true, message: "" });
      const calls: string[][] = readFileSync(log, "utf8").trim().split("\n").filter(Boolean).map(line => JSON.parse(line));
      if (methods[item.id]) expect(rpc).toEqual([methods[item.id]!]);
      else {
        expect(calls.at(-1)).toEqual(commands[item.id]!);
        expect(rpc).toEqual([]);
      }
      expect(calls.flat()).not.toContain("--current");
    }
    for (const id of ["rename_workspace", "rename_tab", "rename_pane", "open_worktree", "remove_worktree"]) {
      writeFileSync(log, "");
      expect((await execute(defaultItems().find(item => item.id === id)!, " ")).ok).toBe(false);
      expect(readFileSync(log, "utf8")).toBe("");
    }
  } finally {
    for (const [key, value] of Object.entries(previous)) {
      if (value === undefined) delete process.env[key]; else process.env[key] = value;
    }
    await new Promise<void>(resolve => server.close(() => resolve()));
    rmSync(directory, { recursive: true, force: true });
  }
});

import { expect, test } from "bun:test";
import { mkdtempSync, writeFileSync, rmSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { defaultItems } from "../src/catalog";
import { execute, resolve } from "../src/execute";

test("pane actions resolve the popup launch pane without HERDR_PANE_ID", async () => {
  const previousContext = process.env.HERDR_PLUGIN_CONTEXT_JSON;
  const previousPane = process.env.HERDR_PANE_ID;
  process.env.HERDR_PLUGIN_CONTEXT_JSON = JSON.stringify({ workspace_id: "w2", tab_id: "w2:t1", focused_pane_id: "w2:p4" });
  delete process.env.HERDR_PANE_ID;
  try {
    const actions = defaultItems().filter(item => item.invocation.kind === "herdr" && item.invocation.argv.includes("--current"));
    expect(actions.map(item => item.id)).toContain("split_vertical");
    expect(actions.map(item => item.id)).toContain("split_horizontal");
    for (const item of actions) {
      if (item.invocation.kind !== "herdr") throw new Error("unexpected invocation");
      const result = await resolve(item.invocation);
      expect(result).toEqual({ argv: item.invocation.argv.flatMap(arg => arg === "--current" ? ["--pane", "w2:p4"] : [arg]) });
      expect(item.invocation.argv).toContain("--current");
    }
    const explicit = { kind: "herdr" as const, argv: ["pane", "focus", "w9:p1"] };
    expect(await resolve(explicit)).toEqual({ argv: explicit.argv });
  } finally {
    if (previousContext === undefined) delete process.env.HERDR_PLUGIN_CONTEXT_JSON;
    else process.env.HERDR_PLUGIN_CONTEXT_JSON = previousContext;
    if (previousPane === undefined) delete process.env.HERDR_PANE_ID;
    else process.env.HERDR_PANE_ID = previousPane;
  }
});

test("split executes both directions against the launch pane and reports CLI errors", async () => {
  const directory = mkdtempSync(join(tmpdir(), "omni-split-"));
  const binary = join(directory, "herdr");
  const previous = { binary: process.env.HERDR_BIN_PATH, context: process.env.HERDR_PLUGIN_CONTEXT_JSON, pane: process.env.HERDR_PANE_ID };
  // Reject --current exactly as the real split CLI does in a popup, and check
  // target/direction/focus at the process boundary without creating any panes.
  writeFileSync(binary, '#!/bin/sh\nif [ "$1" = pane ] && [ "$2" = split ] && [ "$3" = --pane ] && [ "$4" = w2:p4 ] && [ "$5" = --direction ] && { [ "$6" = right ] || [ "$6" = down ]; } && [ "$7" = --focus ]; then exit 0; fi\necho "wrong split target" >&2\nexit 1\n', { mode: 0o755 });
  process.env.HERDR_BIN_PATH = binary;
  process.env.HERDR_PLUGIN_CONTEXT_JSON = JSON.stringify({ workspace_id: "w2", tab_id: "w2:t1", focused_pane_id: "w2:p4" });
  delete process.env.HERDR_PANE_ID;
  try {
    for (const id of ["split_vertical", "split_horizontal"]) {
      expect(await execute(defaultItems().find(item => item.id === id)!)).toEqual({ ok: true, message: "" });
    }
    writeFileSync(binary, '#!/bin/sh\necho \'{"error":{"message":"pane no longer exists"}}\' >&2\nexit 1\n', { mode: 0o755 });
    expect(await execute(defaultItems().find(item => item.id === "split_vertical")!)).toEqual({ ok: false, message: "pane no longer exists" });
    delete process.env.HERDR_PLUGIN_CONTEXT_JSON;
    expect(await execute(defaultItems().find(item => item.id === "split_vertical")!)).toEqual({ ok: false, message: "Herdr did not report the pane that opened the palette." });
  } finally {
    for (const [key, value] of [["HERDR_BIN_PATH", previous.binary], ["HERDR_PLUGIN_CONTEXT_JSON", previous.context], ["HERDR_PANE_ID", previous.pane]] as const) {
      if (value === undefined) delete process.env[key]; else process.env[key] = value;
    }
    rmSync(directory, { recursive: true, force: true });
  }
});

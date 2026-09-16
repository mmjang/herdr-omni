import { mkdtempSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { expect, test } from "bun:test";
import { loadPaletteItems, parseKeyRemaps, unescapeToml } from "../src/config";
import { defaultItems } from "../src/catalog";
import { explain, parseLaunchContext, stepAgent, stepTab, stepWorkspace, worktreeCreateArgv, worktreeOpenArgv } from "../src/herdr";

test("reads the pane, tab, and workspace Herdr injected at launch", () => {
  const context = JSON.stringify({ workspace_id: "wG", tab_id: "wG:t1", focused_pane_id: "wG:p1", focused_pane_cwd: "/tmp" });

  expect(parseLaunchContext(context)).toEqual({ paneId: "wG:p1", tabId: "wG:t1", workspaceId: "wG" });
});

test("ignores a launch context that is missing a target", () => {
  expect(parseLaunchContext(JSON.stringify({ workspace_id: "wG", tab_id: "wG:t1" }))).toBeUndefined();
  expect(parseLaunchContext("not json")).toBeUndefined();
  expect(parseLaunchContext(undefined)).toBeUndefined();
});

test("steps between tabs in display order and wraps around", () => {
  const tabs = ["w1:t2", "w1:t5", "w1:t9"];

  expect(stepTab(tabs, "w1:t5", 1)).toEqual({ tabId: "w1:t9" });
  expect(stepTab(tabs, "w1:t9", 1)).toEqual({ tabId: "w1:t2" });
  expect(stepTab(tabs, "w1:t2", -1)).toEqual({ tabId: "w1:t9" });
});

test("says why a tab step cannot happen", () => {
  expect(stepTab(["w1:t2"], "w1:t2", 1)).toEqual({ message: "This workspace only has one tab." });
  expect(stepTab(["w1:t2", "w1:t5"], "w2:t1", 1)).toEqual({ message: "Herdr did not report the tab that opened the palette." });
});

test("steps between workspaces in list order", () => {
  expect(stepWorkspace(["w2", "w3", "wM"], "w3", 1)).toEqual({ workspaceId: "wM" });
  expect(stepWorkspace(["w2", "w3", "wM"], "w2", -1)).toEqual({ workspaceId: "wM" });
  expect(stepWorkspace(["w2"], "w2", 1)).toEqual({ message: "Only one workspace is open." });
});

test("steps between agents from the calling pane, else the focused agent", () => {
  const agents = [
    { pane_id: "w2:p1", focused: false },
    { pane_id: "w3:p1", focused: true },
    { pane_id: "wM:p1", focused: false },
  ];

  expect(stepAgent(agents, "w2:p1", 1)).toEqual({ paneId: "w3:p1" });
  expect(stepAgent(agents, "missing", 1)).toEqual({ paneId: "wM:p1" });
  expect(stepAgent(agents.slice(0, 1), "w2:p1", 1)).toEqual({ message: "Only one agent is running." });
  expect(stepAgent([], "w2:p1", 1)).toEqual({ message: "No agents are running." });
});

test("creates a named worktree or leaves naming to Herdr for blank input", () => {
  expect(worktreeCreateArgv("wM", " feature/my-task ")).toEqual(["worktree", "create", "--workspace", "wM", "--branch", "feature/my-task", "--focus"]);
  for (const input of ["", "   "]) {
    expect(worktreeCreateArgv("wM", input)).toEqual(["worktree", "create", "--workspace", "wM", "--focus"]);
  }
});

test("opens a worktree by path or branch depending on the input shape", () => {
  expect(worktreeOpenArgv("wM", "feature/x")).toEqual(["worktree", "open", "--workspace", "wM", "--branch", "feature/x", "--focus"]);
  expect(worktreeOpenArgv("wM", "~/code/app")).toEqual(["worktree", "open", "--workspace", "wM", "--path", "~/code/app", "--focus"]);
  expect(worktreeOpenArgv("wM", "./wt")).toEqual(["worktree", "open", "--workspace", "wM", "--path", "./wt", "--focus"]);
  expect(worktreeOpenArgv("wM", "main")).toEqual(["worktree", "open", "--workspace", "wM", "--branch", "main", "--focus"]);
});

test("surfaces the message from a Herdr error envelope", () => {
  expect(explain('{"error":{"code":"no_neighbor","message":"pane has no neighbor"}}', 1)).toBe("pane has no neighbor");
  expect(explain("plain failure", 1)).toBe("plain failure");
  expect(explain("", 2)).toBe("Herdr exited with status 2.");
});

test("applies config remaps to matching catalog ids", () => {
  const remaps = parseKeyRemaps(`
prefix = "ctrl+a"
rename_workspace = "prefix+shift+r"
previous_workspace = "prefix+["
open_worktree = ""
`);

  expect(remaps.get("rename_workspace")).toBe("prefix+shift+r");
  expect(remaps.get("previous_workspace")).toBe("prefix+[");
  expect(remaps.get("open_worktree")).toBe("");
});

test("decodes TOML backslash escapes so a bound backslash shows once", () => {
  expect(unescapeToml("prefix+\\\\")).toBe("prefix+\\");
  expect(parseKeyRemaps('split_vertical = "prefix+\\\\"').get("split_vertical")).toBe("prefix+\\");
});

test("keeps the word prefix in displayed shortcuts instead of expanding the leader", () => {
  const dir = mkdtempSync(join(tmpdir(), "herdr-palette-"));
  const path = join(dir, "config.toml");
  writeFileSync(path, 'prefix = "ctrl+a"\nsplit_vertical = "prefix+\\\\"\nrename_workspace = "prefix+shift+r"\n');
  const items = loadPaletteItems(path);
  expect(items.find(item => item.id === "split_vertical")?.shortcuts).toEqual(["prefix+\\"]);
  expect(items.find(item => item.id === "rename_workspace")?.shortcuts).toEqual(["prefix+shift+r"]);
  expect(items.find(item => item.id === "zoom")?.shortcuts).toEqual(["prefix+z"]);
});

/**
 * Terminal fonts ship wildly different symbol coverage, and a glyph the font lacks falls back to
 * another font with its own metrics — which misaligns the row. Keep icons inside the set verified
 * present at single-cell width in common terminal fonts (ASCII plus arrows and geometric shapes).
 */
test("draws every icon with a glyph terminal fonts actually carry", () => {
  const verified = new Set(["?", "«", "»", "×", "≡", "←", "→", "↑", "↓", "◇", "◈", "▣", "▤", "▧", "▯", "▭", "◲"]);

  for (const item of defaultItems()) {
    expect([...item.icon]).toHaveLength(1);
    expect(verified).toContain(item.icon);
  }
});

test("runs every catalog entry Herdr's CLI can perform", () => {
  const items = defaultItems();
  const runnable = items.filter(item => item.invocation.kind !== "shortcut").map(item => item.id);
  const shortcuts = items.filter(item => item.invocation.kind === "shortcut").map(item => item.id);

  expect(runnable).toContain("rename_pane");
  expect(runnable).toContain("rename_tab");
  expect(runnable).toContain("rename_workspace");
  expect(runnable).toContain("close_tab");
  expect(runnable).toContain("close_workspace");
  expect(runnable).toContain("previous_workspace");
  expect(runnable).toContain("next_agent");
  expect(runnable).toContain("resize_pane_left");
  expect(runnable).toContain("swap_pane_right");
  expect(runnable).toContain("move_pane_new_tab");
  expect(runnable).toContain("new_worktree");
  expect(runnable).toContain("open_worktree");
  expect(runnable).toContain("remove_worktree");
  expect(runnable).toContain("edit_scrollback");
  expect(shortcuts).toEqual(["cycle_pane_next", "cycle_pane_previous", "last_pane", "help", "settings", "copy_mode", "detach"]);
});

test("Edit scrollback displays its default shortcut and respects remaps", () => {
  expect(defaultItems().find(item => item.id === "edit_scrollback")?.shortcuts).toEqual(["prefix+e"]);
  const dir = mkdtempSync(join(tmpdir(), "herdr-palette-"));
  const path = join(dir, "config.toml");
  writeFileSync(path, '[keys]\nedit_scrollback = "ctrl+alt+e"\n');
  expect(loadPaletteItems(path).find(item => item.id === "edit_scrollback")?.shortcuts).toEqual(["ctrl+alt+e"]);
});

test("prompts before running commands that need text input", () => {
  const prompted = defaultItems().filter(item => item.prompt).map(item => item.id);
  expect(prompted).toEqual(["rename_workspace", "rename_tab", "rename_pane", "new_worktree", "open_worktree", "remove_worktree"]);
});

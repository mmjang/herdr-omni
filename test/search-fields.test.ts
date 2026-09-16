import { expect, test } from "bun:test";
import { itemsFromSnapshot, itemsFromWorktrees } from "../src/live";
import { defaultItems } from "../src/catalog";
import { filterPaletteItems, searchResults } from "../src/search";
import { historyKey } from "../src/history";

const snapshot = {
  workspaces: [
    { workspace_id: "w987", label: "herdr-palette", number: 8765, tab_count: 6543, pane_count: 7654 },
    { workspace_id: "w986", label: "native_shell", tab_count: 1, pane_count: 1 },
    { workspace_id: "w985", label: "feedme-workbench", tab_count: 1, pane_count: 1,
      worktree: { repo_name: "checkout", checkout_path: "/Users/binbin/project/feature-checkout", repo_root: "/repo" } },
  ],
  tabs: [{ tab_id: "w987:t543", workspace_id: "w987", label: "dev", number: 4321, pane_count: 7654, agent_status: "blocked" }],
  agents: [{ pane_id: "w987:p123", workspace_id: "w987", tab_id: "w987:t543", title: "Review checkout", agent: "codex", name: "hidden-control-name", terminal_title: "raw-spinner-metadata", agent_status: "working", cwd: "/Users/binbin/project/feature-checkout" }],
};

test("ns matches workspace names, not panes metadata, even in Recent", () => {
  const items = itemsFromSnapshot(snapshot, "w987");
  const history = { [historyKey("live:workspace:w987")]: 100, [historyKey("live:workspace:w985")]: 99 };
  expect(searchResults(items, "@ns", history).map(result => result.item.title)).toEqual(["native_shell"]);
});

test("counts, automatic numbers, IDs, statuses and hidden terminal/control titles are not search fields", () => {
  const items = itemsFromSnapshot(snapshot, "w987");
  for (const query of ["panes", "tabs", "6543", "7654", "8765", "4321", "w987", "w987:t543", "w987:p123", "blocked", "working", "raw-spinner-metadata", "hidden-control-name"]) {
    expect(filterPaletteItems(items, query)).toEqual([]);
  }
});

test("meaningful titles, kinds, branches and contiguous path fragments remain searchable", () => {
  const items = itemsFromSnapshot(snapshot, "w987");
  expect(filterPaletteItems(items, ">rvchk herdr").map(item => item.title)).toEqual(["Review checkout - herdr-palette"]);
  expect(filterPaletteItems(items, ">codex")).toHaveLength(1);
  expect(filterPaletteItems(items, "@feature-checkout").map(item => item.title)).toEqual(["feedme-workbench"]);
  const worktrees = itemsFromWorktrees([{ label: "release", branch: "feature/payments", path: "/Users/binbin/project/checkout", open_workspace_id: "w987" }], "w987");
  expect(filterPaletteItems(worktrees, "payments")).toHaveLength(1);
  expect(filterPaletteItems(worktrees, "project/checkout")).toHaveLength(1);
  expect(filterPaletteItems(worktrees, "ubpc")).toEqual([]);
  expect(filterPaletteItems(worktrees, "w987")).toEqual([]);
});

test("fallback display IDs and detached status do not become search keywords", () => {
  const items = itemsFromSnapshot({ workspaces: [{ workspace_id: "w987" }], tabs: [{ tab_id: "w987:t543", workspace_id: "w987" }] }, "w987");
  expect(filterPaletteItems(items, "w987")).toEqual([]);
  expect(filterPaletteItems(items, "")).toHaveLength(2);
  expect(filterPaletteItems(itemsFromWorktrees([{ label: "checkout", path: "/repo" }], "w987"), "detached")).toEqual([]);
});

test("metadata words remain valid when actually used in a user-facing name", () => {
  const items = itemsFromSnapshot({ workspaces: [{ workspace_id: "w1", label: "working" }] }, "w1");
  expect(filterPaletteItems(items, "@working")).toHaveLength(1);
});

test("actions use titles and curated synonyms, not prose or shortcut modifiers", () => {
  const actions = defaultItems();
  for (const query of [":prefix", ":ctrl", ":shift", ":current"]) expect(filterPaletteItems(actions, query)).toEqual([]);
  const metadataOnly = { ...actions[0]!, title: "Launch", description: "metadata-only-token", shortcuts: ["shortcut-only-token"], aliases: [] };
  expect(filterPaletteItems([metadataOnly], "metadata-only-token")).toEqual([]);
  expect(filterPaletteItems([metadataOnly], "shortcut-only-token")).toEqual([]);
  expect(filterPaletteItems(actions, ":terminal history").map(item => item.id)).toEqual(["edit_scrollback"]);
  expect(filterPaletteItems(actions, ":copy").some(item => item.id === "copy_mode")).toBe(true);
});

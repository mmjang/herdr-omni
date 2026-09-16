import { expect, test } from "bun:test";
import { filterPaletteItems, fuzzyScore, matchingPositions, searchResults } from "../src/search";
import { historyKey } from "../src/history";
import { itemsFromSnapshot } from "../src/live";
import { defaultItems } from "../src/catalog";
import type { PaletteItem } from "../src/types";

const item = (id: string, title: string): PaletteItem => ({ id, title, category: "Tabs", description: "", icon: "▣", aliases: [], shortcuts: [], invocation: { kind: "herdr", argv: [] } });

test("Edit scrollback is searchable as an action by title and terminal history", () => {
  for (const query of ["edit scrollback", ":scrollback", ":edsc", ":terminal history"]) {
    const matches = filterPaletteItems(defaultItems(), query);
    expect(matches.some(item => item.id === "edit_scrollback" && item.category === "Actions")).toBe(true);
  }
});

test("Recent holds seven matching selections without duplicates or hiding older results", () => {
  const items = Array.from({ length: 10 }, (_, i) => item(`tab${i}`, `Project ${i}`));
  const history = Object.fromEntries(items.map((entry, i) => [historyKey(entry.id), i + 1]));
  const results = searchResults(items, "", history);
  expect(results.filter(result => result.section === "Recent").map(result => result.item.id)).toEqual(["tab9", "tab8", "tab7", "tab6", "tab5", "tab4", "tab3"]);
  expect(results.slice(7).map(result => result.item.id)).toEqual(["tab0", "tab1", "tab2"]);
  expect(new Set(results.map(result => result.item.id)).size).toBe(10);
  expect(results.every(result => result.item.category === "Tabs")).toBe(true);
  expect(searchResults(items, "Project 0", history).map(result => result.section)).toEqual(["Recent"]);
  expect(searchResults(items, "", {}).some(result => result.section === "Recent")).toBe(false);
});

test("at-sign scopes fuzzy search and recency to workspaces", () => {
  const workspace = { ...item("live:workspace:w1", "ordering-service"), category: "Workspace" as const };
  const recent = { ...item("live:workspace:w2", "portal"), category: "Workspace" as const };
  const others = [item("live:tab:w1:t1", "ordering-service"), item("live:worktree:/repo", "ordering-service"), item("live:agent:w1:p1", "ordering-service"), ...defaultItems()];
  const items = [workspace, recent, ...others];
  const history = { [historyKey(recent.id)]: 10 };
  expect(filterPaletteItems(items, "@", history)).toEqual([recent, workspace]);
  for (const query of ["@ordsvc", "@ ordsvc"]) {
    expect(filterPaletteItems(items, query, history)).toEqual([workspace]);
  }
  expect(filterPaletteItems(items, "@zzzz")).toEqual([]);
});

test("colon scopes fuzzy search to the unified Actions category", () => {
  const actions = defaultItems();
  const live = item("live:tab:w1:t1", "Split pane right");
  expect(actions.every(action => action.category === "Actions")).toBe(true);
  expect(filterPaletteItems([...actions, live], ":")).toEqual(actions);
  for (const query of [":splpn", ": splpn"]) {
    const matches = filterPaletteItems([...actions, live], query);
    expect(matches.some(action => action.id === "split_vertical")).toBe(true);
    expect(matches.every(action => action.category === "Actions")).toBe(true);
  }
  expect(filterPaletteItems([...actions, live], ">")).toEqual([]);
});

test("previously recorded actions never occupy Recent slots or receive recency boosts", () => {
  const actions = defaultItems();
  const lastAction = actions.at(-1)!;
  const workspace = { ...item("live:workspace:w1", "Project"), category: "Workspace" as const };
  const history = { [historyKey(lastAction.id)]: 100, [historyKey(workspace.id)]: 10 };
  const results = searchResults([...actions, workspace], "", history);
  expect(results.filter(result => result.section === "Recent").map(result => result.item)).toEqual([workspace]);
  expect(results.filter(result => result.section === "Actions").map(result => result.item)).toEqual(actions);
  expect(searchResults(actions, ":", history).every(result => result.section === "Actions")).toBe(true);
});

test("fuzzy search matches abbreviations and favors contiguous and boundary matches", () => {
  expect(Number.isFinite(fuzzyScore("ordsvc", "ordering-service"))).toBe(true);
  expect(fuzzyScore("os", "OrderingService")).toBeGreaterThan(fuzzyScore("os", "almost"));
  expect(fuzzyScore("dev", "dev")).toBeGreaterThan(fuzzyScore("dev", "delivery view"));
  expect(fuzzyScore("dev", "dev") - fuzzyScore("dev", "development")).toBe(100);
  expect(fuzzyScore("xyz", "ordering-service")).toBe(-Infinity);
});

test("matchingPositions traces repeated-character and abbreviation matches", () => {
  expect(matchingPositions("aa", "banana")).toEqual(new Set([1, 3]));
  expect(matchingPositions("ordsvc", "ordering-service")).toEqual(new Set([0, 1, 2, 9, 12, 14]));
});

test("matchingPositions uses title codepoint indices and combines scoped tokens", () => {
  expect(matchingPositions("cf", "🚀 café")).toEqual(new Set([2, 4]));
  expect(matchingPositions(">rv chk", "Review checkout")).toEqual(new Set([0, 2, 7, 8, 11]));
  expect(matchingPositions("@zz", "世界")).toEqual(new Set());
});

test("recent matching selections lead across categories but cannot bypass the query", () => {
  const first = item("first", "dev");
  const recent = { ...item("recent", "development"), category: "Workspace" as const };
  const missing = item("missing", "xyz");
  const history = { [historyKey("recent")]: 10, [historyKey("missing")]: 20 };
  expect(filterPaletteItems([first, recent, missing], "dev", history)).toEqual([recent, first]);
  expect(filterPaletteItems([first, recent, missing], "", history)).toEqual([missing, recent, first]);
});

test("agent scope orders attention priority and honors recent selections", () => {
  const statuses = ["unknown", "idle", "working", "done", "blocked"];
  const items = itemsFromSnapshot({ agents: statuses.map((status, i) => ({ pane_id: `w1:p${i}`, workspace_id: "w1", tab_id: "w1:t1", title: "Review checkout", agent_status: status })) }, "w1");
  expect(filterPaletteItems(items, ">").map(item => item.priority)).toEqual([0, 1, 2, 3, 4]);
  expect(filterPaletteItems(items, ">rvchk", { [historyKey(items[0]!.id)]: 1 })[0]).toBe(items[0]);
});

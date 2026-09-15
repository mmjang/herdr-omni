import { expect, test } from "bun:test";
import { filterPaletteItems, fuzzyScore } from "../src/search";
import { historyKey } from "../src/history";
import { itemsFromSnapshot } from "../src/live";
import { defaultItems } from "../src/catalog";
import type { PaletteItem } from "../src/types";

const item = (id: string, title: string): PaletteItem => ({ id, title, category: "Tabs", description: "", icon: "▣", aliases: [], shortcuts: [], invocation: { kind: "herdr", argv: [] } });

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

test("fuzzy search matches abbreviations and favors contiguous and boundary matches", () => {
  expect(Number.isFinite(fuzzyScore("ordsvc", "ordering-service"))).toBe(true);
  expect(fuzzyScore("os", "OrderingService")).toBeGreaterThan(fuzzyScore("os", "almost"));
  expect(fuzzyScore("dev", "dev")).toBeGreaterThan(fuzzyScore("dev", "delivery view"));
  expect(fuzzyScore("xyz", "ordering-service")).toBe(-Infinity);
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

import { expect, test } from "bun:test";
import { tabResults, resultTabs, tabMatchCounts } from "../src/result-tabs";
import type { PaletteItem } from "../src/types";

const rows = (category: PaletteItem["category"], length: number) => Array.from({ length }, (_, index) => ({ section: category,
  item: { id: `${category}:${index}`, title: `Result ${index}`, category, description: "", icon: "", aliases: [], shortcuts: [], invocation: { kind: "shortcut" as const } },
}));

test("All caps each section at five and appends View all; detail tabs are uncapped", () => {
  const results = [...rows("Workspace", 9), ...rows("Agents", 12), ...rows("Actions", 2)];
  const all = tabResults(results, "All");
  expect(all.filter(row => !row.viewAll && row.section === "Workspace")).toHaveLength(5);
  expect(all.filter(row => !row.viewAll && row.section === "Agents")).toHaveLength(5);
  expect(all.filter(row => !row.viewAll && row.section === "Actions")).toHaveLength(2);
  expect(all.filter(row => row.viewAll).map(row => row.viewAll)).toEqual(["Workspace", "Agents", "Actions"]);
  expect(tabResults(results, "Agents")).toHaveLength(12);
  expect(tabResults(results, "Tabs")).toEqual([]);
});

test("core tabs remain stable with no results and extra sections get their own tab", () => {
  expect(resultTabs([], [])).toEqual(["All", "Workspace", "Tabs", "Agents", "Actions"]);
  expect(resultTabs([{ ...rows("Agents", 1)[0]!, section: "Transcript matches" }], []).at(-1)).toBe("Transcript matches");
});

test("tab counts use full matches and cap only the display above 99", () => {
  expect(tabMatchCounts([]).get("All")).toBe("0");
  const results = [...rows("Workspace", 99), ...rows("Agents", 100)];
  const counts = tabMatchCounts(results);
  expect(counts.get("Workspace")).toBe("99");
  expect(counts.get("Agents")).toBe("99+");
  expect(counts.get("All")).toBe("99+");
  expect(tabResults(results, "Agents")).toHaveLength(100);
  expect(tabMatchCounts(tabResults(rows("Workspace", 8), "All")).get("All")).toBe("5");
  expect(tabMatchCounts([{ ...rows("Agents", 1)[0]!, section: "Transcript matches" }]).get("Transcript matches")).toBe("1");
});

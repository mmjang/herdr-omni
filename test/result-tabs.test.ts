import { expect, test } from "bun:test";
import { tabResults, resultTabs, tabMatchCounts } from "../src/result-tabs";
import type { PaletteItem } from "../src/types";
import { searchResults } from "../src/search";

const rows = (category: PaletteItem["category"], length: number) => Array.from({ length }, (_, index) => ({ section: category,
  item: { id: `${category}:${index}`, title: `Result ${index}`, category, description: "", icon: "", aliases: [], shortcuts: [], invocation: { kind: "shortcut" as const } },
}));

test("All caps each section at five and appends View all; detail tabs are uncapped", () => {
  const results = [...rows("Actions", 2), ...rows("Agents", 12), ...rows("Workspace", 9)];
  const all = tabResults(results, "All");
  expect([...new Set(all.filter(row => !row.viewAll).map(row => row.section))]).toEqual(["Workspace", "Agents", "Actions"]);
  expect(all.filter(row => !row.viewAll && row.section === "Workspace")).toHaveLength(5);
  expect(all.filter(row => !row.viewAll && row.section === "Agents")).toHaveLength(5);
  expect(all.filter(row => !row.viewAll && row.section === "Actions")).toHaveLength(2);
  expect(all.filter(row => row.viewAll).map(row => row.viewAll)).toEqual(["Workspace", "Agents", "Actions"]);
  expect(tabResults(results, "Agents")).toHaveLength(12);
  expect(tabResults(results, "Tabs")).toEqual([]);
});

test("core tabs remain stable with no results and extra sections get their own tab", () => {
  expect(resultTabs([], [])).toEqual(["All", "Workspace", "Agents", "Tabs", "Actions"]);
  expect(resultTabs([{ ...rows("Agents", 1)[0]!, section: "Transcript matches" }], []).at(-1)).toBe("Transcript matches");
});

test("All preserves category relevance and within-category order while searching", () => {
  const items = [...rows("Workspace", 2), ...rows("Agents", 7), ...rows("Actions", 1)].map(row => ({ ...row.item,
    title: row.section === "Actions" ? "deploy" : row.section === "Agents" ? "deploy service" : "production deploy project",
  }));
  const ranked = searchResults(items, "deploy");
  expect(ranked[0]!.section).toBe("Actions");
  const all = tabResults(ranked, "All", "deploy");
  const sections = [...new Set(ranked.map(row => row.section))];
  expect(all.filter(row => row.viewAll).map(row => row.section)).toEqual(sections);
  for (const section of sections) {
    expect(all.filter(row => row.section === section && !row.viewAll))
      .toEqual(ranked.filter(row => row.section === section).slice(0, 5));
  }
  expect(tabResults(ranked, "Agents", "deploy")).toEqual(ranked.filter(row => row.section === "Agents"));
  for (const query of ["", "   "]) {
    expect(tabResults(ranked, "All", query).filter(row => row.viewAll).map(row => row.section))
      .toEqual(["Workspace", "Agents", "Actions"]);
  }
  expect(resultTabs(ranked, items)).toEqual(["All", "Workspace", "Agents", "Tabs", "Actions"]);
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

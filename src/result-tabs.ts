import type { PaletteItem } from "./types";
import { ALL_SECTION_LIMIT, CATEGORY_ORDER } from "./constants";

export interface ResultRow { item: PaletteItem; section: string; viewAll?: string }
export const sectionLabel = (section: string) => section === "Workspace" ? "Workspaces" : section;

export function tabMatchCounts(results: ResultRow[]): Map<string, string> {
  const counts = new Map<string, number>([["All", 0]]);
  for (const row of results) {
    if (row.viewAll) continue;
    counts.set("All", counts.get("All")! + 1);
    counts.set(row.section, (counts.get(row.section) ?? 0) + 1);
  }
  return new Map([...counts].map(([section, count]) => [section, count > 99 ? "99+" : String(count)]));
}

export function resultTabs(results: ResultRow[], items: PaletteItem[]): string[] {
  const sections = new Set(["Workspace", "Tabs", "Agents", "Actions", ...items.map(item => item.category === "Worktrees" ? "Workspace" : item.category), ...results.map(row => row.section)]);
  // Agents are the most frequently used destination after the aggregate view.
  // Keep the remaining sections in the established category order.
  return ["All", "Agents", ...CATEGORY_ORDER.filter(section => section !== "Agents" && sections.has(section)), ...[...sections].filter(section => !(CATEGORY_ORDER as readonly string[]).includes(section))];
}

/** Navigation rows never leave the UI or enter execution/history. */
export function tabResults(results: ResultRow[], tab: string): ResultRow[] {
  if (tab !== "All") return results.filter(row => row.section === tab);
  const groups = new Map<string, ResultRow[]>();
  for (const row of results) {
    const group = groups.get(row.section) ?? [];
    group.push(row);
    groups.set(row.section, group);
  }
  return [...groups].flatMap(([section, rows]) => [
    ...rows.slice(0, ALL_SECTION_LIMIT),
    { section, viewAll: section, item: {
      id: `ui:view-all:${section}`, title: `View all ${sectionLabel(section)} (${rows.length})`,
      category: "Custom" as const, description: "", icon: "→", aliases: [], shortcuts: [], invocation: { kind: "shortcut" as const },
    } },
  ]);
}

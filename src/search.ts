import type { PaletteItem } from "./types";
import { historyKey } from "./history";
import { agentActivity } from "./sessions";
import { scoreFuzzy } from "./vendor/vscode/fuzzyScorer";
import { CATEGORY_ORDER } from "./constants";

/** VS Code's Ctrl+P core returns UTF-16 offsets; OpenTUI titles use codepoints. */
function fuzzyMatch(query: string, value: string, trace = false): { score: number; positions: number[] } {
  if (!query) return { score: 0, positions: [] };
  const [score, offsets] = scoreFuzzy(value, query, query.toLowerCase(), true);
  if (score === 0) return { score: -Infinity, positions: [] };
  // Omni policy: prefer an exact field match over a longer label with the same prefix.
  const rankedScore = score + (value.toLowerCase() === query.toLowerCase() ? 100 : 0);
  if (!trace) return { score: rankedScore, positions: [] };
  const offsetToCodepoint: number[] = [];
  let offset = 0;
  let index = 0;
  for (const char of value) {
    for (let unit = 0; unit < char.length; unit++) offsetToCodepoint[offset++] = index;
    index++;
  }
  return { score: rankedScore, positions: offsets.map(position => offsetToCodepoint[position]!) };
}

export function fuzzyScore(query: string, value: string): number {
  return fuzzyMatch(query, value).score;
}

/** Return title codepoint indices selected by the optimal fuzzy-match paths. */
export function matchingPositions(query: string, title: string): Set<number> {
  const scopedQuery = query.startsWith(">") || query.startsWith(":") || query.startsWith("@") ? query.slice(1) : query;
  const tokens = scopedQuery.trim().split(/\s+/).filter(Boolean);
  const positions = new Set<number>();
  for (const token of tokens) {
    const match = fuzzyMatch(token, title, true);
    if (Number.isFinite(match.score)) {
      for (const position of match.positions) positions.add(position);
    }
  }
  return positions;
}

export function filterPaletteItems(items: PaletteItem[], query: string, history: Record<string, number> = {}): PaletteItem[] {
  return searchResults(items, query, history).map(result => result.item);
}

export function searchResults(items: PaletteItem[], query: string, history: Record<string, number> = {}): { item: PaletteItem; section: string }[] {
  const agentsOnly = query.startsWith(">");
  const actionsOnly = query.startsWith(":");
  const workspacesOnly = query.startsWith("@");
  const tokens = (agentsOnly || actionsOnly || workspacesOnly ? query.slice(1) : query).trim().split(/\s+/).filter(Boolean);
  const browsing = tokens.length === 0;
  const matches = items.flatMap((item, index) => {
    if (agentsOnly && !item.id.startsWith("live:agent:") && !item.savedSession) return [];
    if (actionsOnly && item.category !== "Actions") return [];
    if (workspacesOnly && item.category !== "Workspace" && item.category !== "Worktrees") return [];
    let score = 0;
    for (const token of tokens) {
      // Presentation metadata and shortcut syntax are not search keywords.
      const pathMatches = (item.searchPaths ?? []).filter(path => path.toLowerCase().includes(token.toLowerCase()));
      const match = Math.max(fuzzyScore(token, item.searchTitle ?? item.title),
        item.session?.id.toLowerCase().includes(token.toLowerCase()) ? 1000 : -Infinity,
        ...item.aliases.map(field => fuzzyScore(token, field) - 25),
        ...pathMatches.map(path => fuzzyScore(token, path) - 25));
      if (!Number.isFinite(match)) return [];
      score += match;
    }
    return [{ item, index, score, recent: item.category === "Actions" ? 0 : history[historyKey(item.id)] ?? 0 }];
  });
  const sortMatches = (a: typeof matches[number], b: typeof matches[number]) => {
    const scoreDifference = b.score - a.score;
    if (scoreDifference) return scoreDifference;
    if (a.item.category === "Agents" && b.item.category === "Agents") {
      const activityDifference = agentActivity(b.item) - agentActivity(a.item);
      if (activityDifference) return activityDifference;
    }
    if (browsing && (a.item.category === "Workspace" || a.item.category === "Worktrees")
      && (b.item.category === "Workspace" || b.item.category === "Worktrees")) {
      const visitedDifference = (lastVisitedAt(b.item) ?? 0) - (lastVisitedAt(a.item) ?? 0);
      if (visitedDifference) return visitedDifference;
    }
    if (!browsing && !(a.item.category === "Agents" && b.item.category === "Agents")) {
      const recentDifference = b.recent - a.recent;
      if (recentDifference) return recentDifference;
    }
    return a.index - b.index;
  };
  const groups = new Map<string, typeof matches>();
  for (const result of matches.sort(sortMatches)) {
    const section = result.item.category === "Worktrees" ? "Workspace" : result.item.category;
    const group = groups.get(section);
    if (group) group.push(result);
    else groups.set(section, [result]);
  }
  const orderedGroups = [...groups.entries()].sort(([categoryA, a], [categoryB, b]) =>
    b[0]!.score - a[0]!.score
      || CATEGORY_ORDER.indexOf(categoryA as typeof CATEGORY_ORDER[number]) - CATEGORY_ORDER.indexOf(categoryB as typeof CATEGORY_ORDER[number]));
  return orderedGroups.flatMap(([section, group]) => group.map(({ item }) => ({ item, section })));
}

/** Unknown visit times keep source order; never substitute Omni selections. */
function lastVisitedAt(item: PaletteItem): number | undefined {
  return item.lastVisitedAt;
}

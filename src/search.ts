import type { PaletteItem } from "./types";
import { historyKey } from "./history";
import { scoreFuzzy } from "./vendor/vscode/fuzzyScorer";
import { RECENT_CANDIDATE_LIMIT, RECENT_DISPLAY_LIMIT, CATEGORY_ORDER } from "./constants";

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

/** Recent is a presentation section; the item's category and action remain intact. */
export function searchResults(items: PaletteItem[], query: string, history: Record<string, number> = {}): { item: PaletteItem; section: string }[] {
  // Select a global pool of distinct, available navigation destinations first.
  // Searching or changing scope must not promote older history into Recent.
  const candidates = [...new Map(items.filter(item => item.category !== "Actions")
    .map(item => [item.id, { id: item.id, recent: history[historyKey(item.id)] ?? 0 }])).values()]
    .filter(item => item.recent > 0)
    .sort((a, b) => b.recent - a.recent)
    .slice(0, RECENT_CANDIDATE_LIMIT);
  const candidateIds = new Set(candidates.map(item => item.id));
  const agentsOnly = query.startsWith(">");
  const actionsOnly = query.startsWith(":");
  const workspacesOnly = query.startsWith("@");
  const tokens = (agentsOnly || actionsOnly || workspacesOnly ? query.slice(1) : query).trim().split(/\s+/).filter(Boolean);
  const browsing = tokens.length === 0;
  const matches = items.flatMap((item, index) => {
    if (agentsOnly && !item.id.startsWith("live:agent:") && !item.savedSession) return [];
    if (actionsOnly && item.category !== "Actions") return [];
    if (workspacesOnly && !item.id.startsWith("live:workspace:")) return [];
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
  const recent = matches.filter(result => browsing && candidateIds.has(result.item.id))
    .sort((a, b) => b.recent - a.recent || a.index - b.index).slice(0, RECENT_DISPLAY_LIMIT);
  const recentIds = new Set(recent.map(result => result.item.id));
  const rest = matches.filter(result => !recentIds.has(result.item.id))
    .sort((a, b) => b.score - a.score || (a.item.priority ?? 5) - (b.item.priority ?? 5)
      || (!browsing ? b.recent - a.recent : 0) || a.index - b.index);
  const groups = new Map<PaletteItem["category"], typeof rest>();
  for (const result of rest) {
    const group = groups.get(result.item.category);
    if (group) group.push(result);
    else groups.set(result.item.category, [result]);
  }
  const orderedGroups = [...groups.entries()].sort(([categoryA, a], [categoryB, b]) =>
    b[0]!.score - a[0]!.score || CATEGORY_ORDER.indexOf(categoryA) - CATEGORY_ORDER.indexOf(categoryB));
  return [
    ...recent.map(({ item }) => ({ item, section: "Recent" })),
    ...orderedGroups.flatMap(([section, group]) => group.map(({ item }) => ({ item, section }))),
  ];
}

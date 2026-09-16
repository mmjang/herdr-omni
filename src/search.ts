import type { PaletteItem } from "./types";
import { historyKey } from "./history";

/** Ordered subsequence matching with bonuses for contiguous letters and word/camel boundaries. */
type FuzzyMatch = { score: number; positions: number[] };

function fuzzyMatch(query: string, value: string, trace = false): FuzzyMatch {
  const needle = [...query.toLowerCase()];
  const original = [...value];
  const target = [...value.toLowerCase()];
  if (!needle.length) return { score: 0, positions: [] };
  let previous = new Array<number>(target.length).fill(-Infinity);
  const parents = trace ? Array.from({ length: needle.length }, () => new Array<number>(target.length).fill(-1)) : undefined;
  for (let i = 0; i < needle.length; i++) {
    const current = new Array<number>(target.length).fill(-Infinity);
    let best = -Infinity;
    let bestIndex = -1;
    for (let j = 0; j < target.length; j++) {
      if (j > 0) {
        const candidate = previous[j - 1]! + (j - 1) * 0.15;
        if (candidate > best) {
          best = candidate;
          bestIndex = j - 1;
        }
      }
      if (needle[i] !== target[j]) continue;
      const boundary = j === 0 || /[\s_./\\\-→]/.test(original[j - 1]!) || (/[a-z]/.test(original[j - 1]!) && /[A-Z]/.test(original[j]!));
      const bonus = 10 + (boundary ? 12 : 0);
      if (i === 0) {
        current[j] = bonus - j * 0.15;
      } else {
        const gap = best - j * 0.15;
        const contiguous = j > 0 ? previous[j - 1]! + 16 : -Infinity;
        current[j] = bonus + Math.max(gap, contiguous);
        // Keep the same candidate ordering as Math.max above when scores tie.
        if (trace) parents![i]![j] = gap >= contiguous ? bestIndex : j - 1;
      }
    }
    previous = current;
  }
  const score = Math.max(...previous) + (value.toLowerCase() === query.toLowerCase() ? 100 : 0);
  if (!trace || !Number.isFinite(score)) return { score, positions: [] };

  let end = 0;
  for (let j = 1; j < previous.length; j++) {
    if (previous[j]! > previous[end]!) end = j;
  }
  const targetPositions = new Array<number>(needle.length);
  for (let i = needle.length - 1, j = end; i >= 0; i--) {
    targetPositions[i] = j;
    j = parents![i]![j]!;
  }

  // Lowercasing can expand a codepoint (for example, İ). Map each generated
  // lowercased codepoint back to its original title codepoint.
  const targetToOriginal: number[] = [];
  for (let j = 0; j < original.length; j++) {
    for (const _ of [...original[j]!.toLowerCase()]) targetToOriginal.push(j);
  }
  return {
    score,
    positions: targetPositions.map(position => targetToOriginal[position] ?? position),
  };
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
  const agentsOnly = query.startsWith(">");
  const actionsOnly = query.startsWith(":");
  const workspacesOnly = query.startsWith("@");
  const tokens = (agentsOnly || actionsOnly || workspacesOnly ? query.slice(1) : query).trim().split(/\s+/).filter(Boolean);
  const matches = items.flatMap((item, index) => {
    if (agentsOnly && !item.id.startsWith("live:agent:")) return [];
    if (actionsOnly && item.category !== "Actions") return [];
    if (workspacesOnly && !item.id.startsWith("live:workspace:")) return [];
    let score = 0;
    for (const token of tokens) {
      const match = Math.max(fuzzyScore(token, item.title), ...[item.description, ...item.aliases, ...item.shortcuts].map(field => fuzzyScore(token, field) - 25));
      if (!Number.isFinite(match)) return [];
      score += match;
    }
    return [{ item, index, score, recent: history[historyKey(item.id)] ?? 0 }];
  });
  const recent = matches.filter(result => result.recent > 0 && result.item.category !== "Actions")
    .sort((a, b) => b.recent - a.recent || a.index - b.index).slice(0, 7);
  const recentIds = new Set(recent.map(result => result.item.id));
  const rest = matches.filter(result => !recentIds.has(result.item.id))
    .sort((a, b) => b.score - a.score || (a.item.priority ?? 5) - (b.item.priority ?? 5) || a.index - b.index);
  return [
    ...recent.map(({ item }) => ({ item, section: "Recent" })),
    ...rest.map(({ item }) => ({ item, section: item.category })),
  ];
}

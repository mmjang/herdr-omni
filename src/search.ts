import type { PaletteItem } from "./types";
import { historyKey } from "./history";

/** Ordered subsequence matching with bonuses for contiguous letters and word/camel boundaries. */
export function fuzzyScore(query: string, value: string): number {
  const needle = [...query.toLowerCase()];
  const original = [...value];
  const target = [...value.toLowerCase()];
  if (!needle.length) return 0;
  let previous = new Array<number>(target.length).fill(-Infinity);
  for (let i = 0; i < needle.length; i++) {
    const current = new Array<number>(target.length).fill(-Infinity);
    let best = -Infinity;
    for (let j = 0; j < target.length; j++) {
      if (j > 0) best = Math.max(best, previous[j - 1]! + (j - 1) * 0.15);
      if (needle[i] !== target[j]) continue;
      const boundary = j === 0 || /[\s_./\\\-→]/.test(original[j - 1]!) || (/[a-z]/.test(original[j - 1]!) && /[A-Z]/.test(original[j]!));
      const bonus = 10 + (boundary ? 12 : 0);
      current[j] = i === 0 ? bonus - j * 0.15 : bonus + Math.max(best - j * 0.15, j > 0 ? previous[j - 1]! + 16 : -Infinity);
    }
    previous = current;
  }
  const score = Math.max(...previous);
  return score + (value.toLowerCase() === query.toLowerCase() ? 100 : 0);
}

export function filterPaletteItems(items: PaletteItem[], query: string, history: Record<string, number> = {}): PaletteItem[] {
  const agentsOnly = query.startsWith(">");
  const actionsOnly = query.startsWith(":");
  const tokens = (agentsOnly || actionsOnly ? query.slice(1) : query).trim().split(/\s+/).filter(Boolean);
  return items.flatMap((item, index) => {
    if (agentsOnly && !item.id.startsWith("live:agent:")) return [];
    if (actionsOnly && item.category !== "Actions") return [];
    let score = 0;
    for (const token of tokens) {
      const match = Math.max(fuzzyScore(token, item.title), ...[item.description, ...item.aliases, ...item.shortcuts].map(field => fuzzyScore(token, field) - 25));
      if (!Number.isFinite(match)) return [];
      score += match;
    }
    return [{ item, index, score, recent: history[historyKey(item.id)] ?? 0 }];
  }).sort((a, b) => b.recent - a.recent || b.score - a.score || (a.item.priority ?? 5) - (b.item.priority ?? 5) || a.index - b.index)
    .map(result => result.item);
}

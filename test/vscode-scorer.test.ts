/* Copyright (c) Microsoft Corporation. All rights reserved.
 * Licensed under the MIT License; see src/vendor/vscode/LICENSE.txt.
 * Core score fixtures adapted from microsoft/vscode fuzzyScorer.test.ts,
 * revision 6182a6ebe7cfcf1ce05126fcd475f66a5650cebc.
 */
import { expect, test } from "bun:test";
import { scoreFuzzy } from "../src/vendor/vscode/fuzzyScorer";
import { matchingPositions } from "../src/search";

const score = (target: string, query: string, fuzzy = true) => scoreFuzzy(target, query, query.toLowerCase(), fuzzy);

test("VS Code upstream fuzzy scoring order", () => {
  const scores = ["HelLo-World", "hello-world", "HW", "hw", "H", "h", "W", "Ld", "ld", "w", "L", "l", "4"]
    .map(query => score("HelLo-World", query));
  expect(scores).toEqual([...scores].sort((a, b) => b[0] - a[0]));
});

test("VS Code upstream contiguous-only scoring", () => {
  const target = "HelLo-World";
  for (const query of [target, "hello-world", "h", "ello", "ld"]) expect(score(target, query, false)[0]).toBeGreaterThan(0);
  expect(score(target, target, false)[1]).toHaveLength(target.length);
  for (const query of ["HW", "eo"]) expect(score(target, query, false)[0]).toBe(0);
});

test("core handles empty, impossible and ordered matches", () => {
  for (const [target, query] of [["", "a"], ["a", ""], ["ab", "abc"], ["abc", "ca"]]) expect(score(target!, query!)[0]).toBe(0);
  expect(score("HelLo-World", "HW")[1]).toEqual([0, 6]);
  expect(score("NullPointerException", "NPE")[1]).toEqual([0, 4, 11]);
  expect(score("src/main.ts", "src\\main")[0]).toBeGreaterThan(0);
  expect(score("Hello", "H")[0]).toBeGreaterThan(score("Hello", "h")[0]);
});

test("Omni converts UTF-16 match positions for emoji and non-Latin titles", () => {
  expect(matchingPositions("🚀", "🚀 Review")).toEqual(new Set([0]));
  expect(matchingPositions("rv", "🚀 Review")).toEqual(new Set([2, 4]));
  expect(matchingPositions("世界", "🚀 世界")).toEqual(new Set([2, 3]));
  expect(matchingPositions("last", "x".repeat(150) + " last")).toEqual(new Set([151, 152, 153, 154]));
});

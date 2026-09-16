# VS Code fuzzy scorer

Source: https://github.com/microsoft/vscode/blob/6182a6ebe7cfcf1ce05126fcd475f66a5650cebc/src/vs/base/common/fuzzyScorer.ts

Revision: `6182a6ebe7cfcf1ce05126fcd475f66a5650cebc`.

Vendored under the adjacent MIT license. This is the `scoreFuzzy` core used by
VS Code Quick Open's `scoreItemFuzzy`, not the alternative scorer in `filters.ts`.

Extraction changes: retain only the first fuzzy-scorer region; remove the unused
`FuzzyScorerCache` type; inline its ASCII `CharCode` constants and `isUpper` helper;
adjust the license path in the header. Scoring and traceback are unchanged.

Omni's adapter retains its own tokenization, field penalties, exact-match bonus,
Recent section, and agent priority. It converts UTF-16 offsets into codepoint
indices for highlighting. This is not a copy of VS Code's full file-ranking policy.

When updating, pin a new revision, compare the extracted core and helpers, rerun
the upstream-derived fixtures in `test/vscode-scorer.test.ts` and Omni's search
tests, and benchmark the same workload before claiming a performance improvement.

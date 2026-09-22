# VS Code fuzzy scorer

Source: https://github.com/microsoft/vscode/blob/6182a6ebe7cfcf1ce05126fcd475f66a5650cebc/src/vs/base/common/fuzzyScorer.ts

Revision: `6182a6ebe7cfcf1ce05126fcd475f66a5650cebc`.

The `scoreFuzzy` core used by VS Code Quick Open is ported to Rust in
[`src/search.rs`](../../search.rs), under the adjacent Microsoft MIT license.
The port retains UTF-16 scoring and traceback semantics, then converts match
offsets to character indices for display. Omni adds its own tokenization,
field penalties, exact-match bonuses, and category ordering.

`tests/fixtures/search-parity.json` records results from the original TypeScript
implementation, including Unicode cases. Run `cargo test --test search_parity`
when changing the scorer. Pin and compare the upstream revision when updating.

# Rust migration verification

Rust rewrite introduced in v0.16.1. The production entrypoint is a native Rust/Ratatui
executable. The TypeScript application, Bun manifests, and JavaScript release
scripts have been removed. The palette data and Microsoft scorer license remain.

## Validation

- `cargo fmt --check`
- `cargo clippy --locked --all-targets -- -D warnings`
- `cargo test --locked --all-targets` — 119 tests passed
- `cargo build --release --locked --bin herdr-omni`
- `cargo run --locked --bin release-tool -- check v0.15.1`
- Release executable PTY smoke: render categories, edit a query, close with Escape,
  exit successfully, and restore the terminal.
- Stalled-provider PTY smoke: Escape exits in 0.05 seconds and all eight mock
  helper processes and descendants are cleaned up.

CI runs the Cargo checks and release build on Linux and macOS. Local validation
passed on macOS and in an isolated Linux container using Rust 1.96.0. Hosted
platform build results are recorded in the release workflow.

## Coverage

Native tests cover the action catalog, shortcut remaps, theme resolution, history
persistence, category filtering, Unicode fuzzy ranking, and exact live snapshot
mapping. Search and live-mapping fixtures were generated from the former
TypeScript implementation, including Unicode scoring and highlight offsets.

Backend tests cover command arguments, launch-pane targeting, socket requests,
workspace visit history, resource metadata, cancellation, and descendant cleanup.
Session tests cover Codex conversation extraction, authenticated and paginated
OpenCode history with chunked HTTP responses, strict export parsing, Claude
history filtering, deduplication, literal transcript matching, and preview bounds.

UI tests cover narrow/wide layouts, text editing, category/prefix transitions,
grouped results, transcript search, stale response rejection, preview cancellation,
live target preservation, pane selection, wrapping, scrolling context, resume
confirmation, and update dialog defaults and outcomes. Update tests cover install
eligibility, failed checks, locking, output limits, and subprocess cancellation.
Release tests run version changes and publication command sequencing against
isolated temporary repositories and mocked commands.

The original Bun suite had 226 passing tests during migration. Its sole failure
was the obsolete assertion that the plugin manifest launches Bun.

Real provider accounts and mutating Herdr operations were not exercised end to
end. Their contracts are tested with fixtures and local command/socket doubles.
Release publication and platform build results are recorded in GitHub Actions.

Detailed TypeScript reference comparisons and screen coverage are documented in
[TypeScript behavior parity](typescript-parity.md).

The follow-up [implementation review](implementation-review.md) records the
shutdown, provider bounds, socket, UI, and update fixes.

## Binary distribution

Normal plugin installs run `scripts/install.sh`, which downloads the manifest's
exact release version and verifies its SHA-256 checksum before atomically
installing `bin/herdr-omni`. End users do not need Rust or Bun. The release
workflow builds macOS x86-64/ARM64 and static-musl Linux x86-64/ARM64 binaries.
These assets must be published for the matching version before a normal install
can succeed; the tag release workflow publishes them.

Source builds are explicit: `make build`, or
`HERDR_OMNI_BUILD_FROM_SOURCE=1 herdr plugin link .`. They alone require Cargo.

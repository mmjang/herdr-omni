# Rust implementation review

Reviewed against the TypeScript reference at
`4193cbb798f66371312aed979c17ab37fa69de3e`, with particular attention to failure
paths beyond the existing search and screen fixtures.

## Findings addressed

- **P1 — shutdown and subprocess lifecycle:** stdout EOF could lead to an
  unbounded child wait; a child exiting before descendants closed inherited
  pipes could leave reader joins blocked. Provider command waits now retain
  cancellation/deadlines, and backend pipe collection has bounded grace periods.
  Worker queues disconnect before joining their senders.
- **P1 — provider input bounds:** line readers allocated whole lines before
  checking their size, Codex pagination missed non-adjacent cursor cycles, and
  Claude traversal followed directory symlinks. Readers now check bounds while
  buffering, pagination tracks seen cursors, and traversal skips symlinks.
- **P1 — terminal resize:** very small terminal dimensions could panic while
  rendering dialogs. These dimensions now show a resize hint and clear mouse
  targets; tests cover normal, dialog, and preview states.
- **P2 — socket protocol:** connection establishment was unbounded, response
  lines had no size limit, and unrelated response IDs caused premature failure.
  Connections use nonblocking connect/poll; responses have a 1 MiB limit and
  matching IDs are awaited within the request deadline.
- **P2 — TS behavior:** agent navigation edge cases, agent-only workspace cwd
  candidates, blank process metadata, invisible preview click targets, busy
  action keyboard handling, and picker cancellation now follow the reference.
- **P2 — provider responsiveness:** providers are discovered concurrently and
  Codex pages publish incrementally, while callbacks remain serialized.
- **P2 — updates:** failed plugin commands could be accepted via valid stdout;
  negative lookup cache entries were ignored; a slow lookup could overwrite
  newer state markers. Exit statuses are enforced, negative results are cached,
  and state is reread before cache replacement. Temporary files are unique and
  private. Numeric version comparison accepts the TS parser's large components.
- **P3 — configuration/release parity:** removed the extra COLORFGBG theme
  heuristic and accepted comments after TOML package section headers.

## Limits

The fixture and mock tests do not exercise real provider accounts, mutating
Herdr actions, or a published release. Linux CI has not been run locally.
Update state replacement is atomic, but simultaneous read-modify-write operations
remain last-writer-wins, as in the reference; this is not a transaction lock.
Actions in progress retain the TS keyboard guard until completion or timeout.

See [migration verification](rust-migration.md) for the final checks and
[TypeScript parity](typescript-parity.md) for fixture coverage.

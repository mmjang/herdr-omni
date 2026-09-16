# Herdr Omni

**Less hunting. More building. Your Herdr session, one search away.**

Jump to the right workspace, find an agent conversation, or run a command
without breaking your flow. **Herdr Omni** brings VS Code–style fuzzy search
to [Herdr](https://herdr.dev)—right inside your terminal.

![Herdr Omni in action: search agent sessions and run actions from the command palette](docs/herdr-omni-demo.gif)

### One shortcut. Your whole session.

- **Find it before you finish typing.** Fuzzy search understands abbreviations,
  word boundaries, and partial names. Type `ordsvc` to find `ordering-service`.
- **Pick up where you left off.** Recent successful selections rise to the top,
  with history saved between palette launches.
- **Find the conversation, not just the agent.** Search agent session titles and
  workspace names together. See who's working, finished, or waiting for you.
- **Put attention where it matters.** Agent priority follows Herdr's states:
  blocked → done → working → idle → unknown, alongside recency and match quality.
- **Jump with confidence.** Tab results show `workspace → tab`, so three tabs
  named `dev` no longer look identical.
- **Switch worktrees from the same search.** Focus an open worktree or open an
  existing checkout from the current repository.
- **Go straight to what you need.** Start with `@` for workspaces, `>` for agents, or `:` for actions.
  Leave the prefix off to search across everything.
- **Turn forgotten shortcuts into muscle memory.** Actions display your actual
  keybindings, including remaps, and prompt for names or confirmation when needed.
- **Feel at home in your terminal.** A lightweight popup follows your Herdr theme
  and keeps your tiled layout intact.

**Press `cmd+p`. Type a few letters. Hit Enter.**
Set up the binding below to make it yours.

Built with Bun and OpenTUI. Forked from
[Herdr Palette](https://github.com/cesarferreira/herdr-palette) by
[César Ferreira](https://github.com/cesarferreira).

## Install

Install [Bun](https://bun.sh), then link the checkout for local development:

```sh
bun install && herdr plugin link .
```

## Open the palette

Add this direct binding to Herdr's `config.toml`:

```toml
[[keys.command]]
key = "cmd+p"
type = "shell"
command = "\"$HERDR_BIN_PATH\" plugin pane open --plugin herdr-omni --entrypoint picker"
description = "Open Herdr Omni"
```

This uses Herdr's supported `shell` custom-command binding to call its
documented `plugin pane open` command. The `picker` pane is declared by this
plugin as a popup, so it opens without changing the tiled layout. Reload a
running configuration with:

```sh
herdr server reload-config
```

## Release

Validate the project, bump the minor version, commit, tag, and push:

```sh
make release
```

Use `make release LEVEL=patch` or `LEVEL=major` for a different bump.

## Configuration and scope

The palette reads Herdr's `config.toml`, including `[keys]` remaps, custom
`[[keys.command]]` bindings, and `[theme]`, so it displays your effective
shortcuts and paints itself with your effective theme. Bindings keep the word
`prefix` instead of expanding it to the concrete leader key
(e.g. `prefix+z`, not `ctrl+a+z`). It stores recent selections locally
and never writes to your Herdr config.

### Theme

The popup follows Herdr's `[theme]` settings — `auto_switch`, `[theme.custom]`,
and the `[theme.custom.light]`/`[theme.custom.dark]` blocks — each time it
opens. No extra configuration is needed.

Only actions documented by Herdr and backed by its CLI/API run directly from the
palette, including rename, close, workspace/agent navigation, resize, swap, move
pane, worktree create/open/remove, and Edit scrollback (through Herdr's session
socket API). Edit scrollback targets the pane that opened Omni; the palette
closes after Herdr confirms the editor opened. Commands that need text (rename, open
worktree, remove confirmation) prompt inside the palette before running. Herdr's
UI-only commands — cycle/last pane, the shortcut guide, settings, copy mode, and
detach — are shortcut-only entries. In Herdr 0.9.0, native Copy mode is client-local
and has no plugin-callable API: close Omni with Esc, then use its displayed binding.
Sending keys to a pane would send them to the running application, not invoke
Herdr's Copy mode.
Custom commands from your configuration are shown as documentation only; their
execution semantics remain owned by Herdr. A failing Herdr command shows its
error and leaves the palette open.

The palette opens immediately with actions while live results load in the
background. Workspace, tab, and agent results appear before worktree discovery
finishes. Live results refresh automatically every two seconds after each
refresh completes; the selected item and search cursor are preserved, and failed
refreshes retain the last available results. Closing the palette stops polling.
Tab results include
their workspace as a breadcrumb, so repeated tab names remain unambiguous.
Start a search with `>` to show only live agents; text after the prefix filters
those agent results further.
Use `@` for workspace-only search: `@` lists workspaces, while `@order`
fuzzy-searches them. Recent selections retain priority within this scope.
All Herdr commands appear under **Actions**. Start with `:` to show only actions,
or type a query such as `:split` to fuzzy-search them.

Search supports fuzzy abbreviations, with bonuses for consecutive letters and
word boundaries. Matching characters in result titles are highlighted using the
theme's accent color. Matching results selected successfully through the palette
appear in a **Recent** section with up to seven matching results, newest first.
Older selections remain in their regular sections without duplicating those seven.
Recent contains navigation destinations only: workspaces, tabs, worktrees, and
agent sessions. Actions stay in **Actions** and do not occupy recent slots.
Remaining results are ordered by match quality and agent attention
priority (blocked, done, working, idle, unknown). Selection history survives
reopening the palette and is stored under `$XDG_STATE_HOME/herdr-palette`
(default `~/.local/state/herdr-palette`). Merely highlighting a row does not
record a visit.

The history directory retains its original name to preserve existing selections.

## Credits

Herdr Omni is a fork of [Herdr Palette](https://github.com/cesarferreira/herdr-palette),
created by [César Ferreira (@cesarferreira)](https://github.com/cesarferreira).
Thank you for the original command palette, shortcut integration, and theme support
that this fork builds on.

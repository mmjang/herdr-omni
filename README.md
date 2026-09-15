# Herdr Omni

Herdr Omni is a popup command palette for [Herdr](https://herdr.dev). It
searches supported Herdr actions and the live session's workspaces, tabs,
worktrees, and agents. Actions show their effective shortcuts;
selecting a live result focuses or opens it directly.

![Herdr Omni](docs/screenshot4.png)

The palette is a shortcut-learning aid as well as a fallback when you forget a
binding. It uses Bun and OpenTUI for a fast, polished terminal interface.

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
pane, and worktree create/open/remove. Commands that need text (rename, open
worktree, remove confirmation) prompt inside the palette before running. Herdr's
UI-only commands — cycle/last pane, the shortcut guide, settings, copy mode, and
detach — have no CLI equivalent, so enter reports the shortcut to press instead.
Custom commands from your configuration are shown as documentation only; their
execution semantics remain owned by Herdr. A failing Herdr command shows its
error and leaves the palette open.

Live results are refreshed each time the palette opens. Tab results include
their workspace as a breadcrumb, so repeated tab names remain unambiguous.
Start a search with `>` to show only live agents; text after the prefix filters
those agent results further.
All Herdr commands appear under **Actions**. Start with `:` to show only actions,
or type a query such as `:split` to fuzzy-search them.

Search supports fuzzy abbreviations, with bonuses for consecutive letters and
word boundaries. Matching results selected successfully through the palette
appear first, most recent first, followed by match quality and agent attention
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

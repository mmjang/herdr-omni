# Herdr Omni

**Less hunting. More building.**

Your [Herdr](https://herdr.dev) workspaces, tabs, worktrees, agent sessions, and
actions—one search away.

<img width="2402" height="1876" alt="image" src="https://github.com/user-attachments/assets/f6681a02-32e3-4c93-954d-0cce5d178286" />

- **VS Code's actual fuzzy-matching core.** A few letters find the right result.
- **Find the conversation you remember.** Search live and saved Codex, Claude Code, and OpenCode sessions—or press `Ctrl+F` to search their transcripts.
- **Browse by category. Search everything.** Preview each category in All, or open its full list with a click.
- **Go straight there.** `@` workspaces · `>` agents · `:` actions.

Bind to **`cmd+p` on macOS** or **`ctrl+alt+p` on Linux** below.
Type, then press Enter or click a result.

Find actions with a few letters.

<img width="2632" height="1876" alt="image" src="https://github.com/user-attachments/assets/ebb0f122-4c20-4a0d-a1ac-6cc7651939e4" />

Remember the conversation, not its name? Press **Ctrl+F** to search session content.

<img width="2632" height="1876" alt="image" src="https://github.com/user-attachments/assets/550ce0e4-7512-45d0-861c-b686c599ee2b" />

Built with Bun and OpenTUI. Forked from
[Herdr Palette](https://github.com/cesarferreira/herdr-palette) by
[César Ferreira](https://github.com/cesarferreira).

## Install

**Supported platforms: macOS and Linux only. Windows is not currently supported.**

### Ask your agent to install

Copy this prompt into your coding agent:

```text
Please install and configure Herdr Omni for me:

1. Check the OS: Herdr Omni supports macOS and Linux only. If this is
   Windows or another unsupported OS, stop and explain that limitation.
   Check that Herdr and Bun are available. If Bun is missing, install it
   using the official instructions at https://bun.sh and make sure it is
   on the PATH available to Herdr.
2. Run `herdr plugin install mmjang/herdr-omni`.
3. Add the following binding to ~/.config/herdr/config.toml (the default).
   If HERDR_CONFIG_PATH is set, use that path instead; `herdr --help`
   shows the resolved config path. Preserve existing settings and avoid
   duplicates. Use key = "cmd+p" on macOS or key = "ctrl+alt+p" on Linux
   in the binding below. If that shortcut is already bound to another
   command, ask me how to resolve the conflict:

[[keys.command]]
key = "cmd+p"
type = "shell"
command = "\"$HERDR_BIN_PATH\" plugin pane open --plugin herdr-omni --entrypoint picker"
description = "Open Herdr Omni"

4. Run `herdr server reload-config`.
5. Confirm that `herdr plugin list` shows herdr-omni enabled and verify
   that its popup opens successfully.

Please execute these steps, troubleshoot any errors, and report the result.
```

### Manual install

Install [Herdr](https://herdr.dev) and [Bun](https://bun.sh), then run:

```sh
herdr plugin install mmjang/herdr-omni
```

Herdr downloads the plugin and runs its dependency installation. Follow the
prompts, then configure the shortcut under **Open the palette** below.
Use `herdr plugin list` to confirm that `herdr-omni` is installed and enabled.

#### Open the palette

Add this direct binding to `~/.config/herdr/config.toml`. If
`HERDR_CONFIG_PATH` is set, use that path instead. Run `herdr --help` to
check the resolved config path:

Use `cmd+p` on macOS, or change the `key` below to `ctrl+alt+p` on Linux.
If the shortcut is already assigned, choose an unused binding.

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

## Using Omni

Type to search, then **Enter** or click to jump. Use `@` for workspaces,
`>` for agents, or `:` for actions. Omni follows your Herdr theme and shortcuts;
The Actions list only includes commands Omni can execute; client-only commands
remain available through Herdr's own keyboard bindings. Actions target the pane
or workspace that opened Omni. Operations still require the appropriate state
(for example, a neighboring pane or a Git worktree); Herdr errors stay visible
in Omni instead of being reported as success.

Use **Tab / Shift+Tab** or click a category label to switch lists. **All** previews
up to five results per category; **View all** opens the complete list. Worktrees
live under **Workspaces**.
Workspace browsing follows real Herdr workspace switches when available. Omni's own
selection history is not used for workspace recency; Herdr's order is the fallback.
The current workspace remains available but is placed after other destinations while browsing.

Normal search includes saved session titles, projects, and IDs—no prefix needed.
OpenCode searches local history across projects (top-level sessions, not child-agent sessions).

To search local Codex, Claude Code, or OpenCode transcripts, press **Ctrl+F** or click
**Search session content**. Left and right arrows always move the text cursor.
Matches arrive with context; **Esc** returns to normal search. Transcript search
matches your complete phrase (case-insensitive), requires the corresponding CLI, and makes
no model requests. Only user messages and assistant replies are searched; tool calls
and tool output are excluded.

Saved sessions resume in a new tab. If the original directory is gone, Omni
suggests a workspace for you to confirm. Conversation history is restored—not
deleted files or Git changes.

Select a running agent to preview its current Herdr pane, including tool output,
approval prompts, and input areas. The selected pane refreshes about once per
second, even when no provider session ID is available. The preview preserves
terminal columns and shows the bottom of the visible screen; long lines are
clipped with an ellipsis. Expand it for more space, or scroll up to inspect earlier
rows in the current screen. Reads do not focus the pane or send it any input.

Workspaces preview their paths, Git branch (when available), tabs, and agent
states. Tabs preview their panes and the selected pane's foreground program and
live screen. Use **F6 / Shift+F6** or click a pane row to choose which pane to
preview; this does not change Herdr's focus. Unopened worktrees show their path
and branch. Workspace and tab metadata follows the normal live refresh.

Saved sessions preview their recent conversation before opening them.
Wide popups show a right-hand preview; narrower popups show it below the results.
Content search previews the matching context instead. Use **Ctrl+O** to expand or
collapse, **PageUp / PageDown** to scroll, and **Ctrl+Y** to hide or show the preview.
Click the **◧** beside a session to preview it without opening it. Previews read
only the selected pane or conversation and never resume a session. If a live pane
cannot be read, Omni shows an unavailable message and retries instead of showing
old conversation history as a live screen.

Official installs offer updates in-app; press **Ctrl+U** when a notice appears.
Reopen Omni after updating. Local checkouts and pinned installs are left alone.

## Local development

To work on the plugin source, clone the repository and link your checkout:

```sh
git clone https://github.com/mmjang/herdr-omni.git
cd herdr-omni
bun install
herdr plugin link .
herdr plugin list
```

If you already have a checkout, run the last three commands from its root.
Keep the checkout in place: Herdr runs the linked plugin from this directory.

Try the preview UI with sample conversations (no agent or Herdr session needed):

```sh
bun scripts/preview-demo.ts
```

## Release notes

### 0.14.0

- Preview workspace paths, Git branches, tabs, and agent states, including numeric and unnamed tabs in workspace overviews.
- Preview a tab's panes, the selected pane's foreground program, and its live screen; use F6 / Shift+F6 or click a pane row to switch preview targets without changing Herdr's focus.
- Keep the chosen preview pane across refreshes and recover when it closes; cancel stale pane and detail reads when switching or hiding previews.
- Show path and branch details for unopened worktrees, and extend the standalone demo with workspace and tab previews.

### 0.13.0

- Preview running agents' current Herdr pane, including tool output, approval prompts, and input areas, with automatic refresh and no session ID required.
- Preview recent conversations for saved Codex, Claude, and OpenCode sessions, and matching context during content search.
- Show previews beside results in wide popups or below them in narrow popups; expand with Ctrl+O, scroll with PageUp/PageDown, and toggle with Ctrl+Y.
- Click a session's preview button without opening it; cancel stale reads when switching results and retry unavailable live panes.
- Add a standalone preview demo with sample live and saved sessions.

### 0.12.2

- Use the consistent result tab order **All → Workspace → Agents → Tabs → Actions** in both the tab bar and All view.

### 0.12.1

- Put **Agents** immediately after **All** in the result tab bar for faster session navigation.

### 0.12.0

- Order workspace browsing by real Herdr workspace switches, keep the current workspace below other destinations, and ignore Omni selection history for workspace recency.

### 0.11.0

- Highlight blocked agents with bold error-color status indicators.
- Resolve the blocked-state color from Herdr's theme, including custom and light/dark themes.

### 0.10.0

- Speed up transcript search by skipping provider-invisible rollouts and tool-only matches before exact SDK reads.
- Keep searches responsive with bounded `rg` CPU usage and parallel OpenCode history exports.

### 0.9.0

- Speed up Codex and Claude transcript search with a safe `rg` prefilter while preserving exact conversation-only results.
- Order empty Workspace and Worktree browsing by real Herdr workspace visits, with graceful fallbacks.

### 0.8.1

- Exclude tool calls and output from Codex, Claude Code, and OpenCode content search.
- Session content search now matches complete phrases, with literal preview highlights instead of fuzzy title highlights for transcript-only matches.
- Preserve paragraphs and code indentation in previews, and format complete JSON output for readability.
- Show an animated loading indicator and scan progress during transcript search.

### 0.8.0

- Use Ctrl+F or clickable search hints for session content; arrows remain available for editing.
- Browse category tabs, with five-result previews and View all in All; no separate Recent section.
- Search and resume saved OpenCode sessions, including opt-in transcript search.
- Agent results use last activity for ordering and show relative activity times.
- Agents in the workspace that opened Omni are marked with a blue dot and `current`.
- Category labels show match counts (capped at 99+); the popup adapts to 80% of terminal height.

### 0.7.0

- Find saved Codex and Claude sessions by title, project, or ID in normal search.
- Press **→** to search session content, with highlighted context and progressive results.
- Resume sessions in a matching workspace, or choose a suggested destination when the original directory is gone.
- See a waiting indicator while resuming, with duplicate launches prevented.

## Releasing

Add a short entry under **Release notes** first, then commit your changes.
Validate the project, bump the minor version, commit, tag, and push:

```sh
make release
```

Use `make release LEVEL=patch` or `LEVEL=major` for a different bump.

## Credits

Fuzzy matching uses Microsoft's [VS Code Quick Open scorer](https://github.com/microsoft/vscode/blob/6182a6ebe7cfcf1ce05126fcd475f66a5650cebc/src/vs/base/common/fuzzyScorer.ts).
See the [vendored source notes](src/vendor/vscode/README.md) and
[Microsoft MIT license](src/vendor/vscode/LICENSE.txt).

Herdr Omni is a fork of [Herdr Palette](https://github.com/cesarferreira/herdr-palette),
created by [César Ferreira (@cesarferreira)](https://github.com/cesarferreira).
Thank you for the original command palette, shortcut integration, and theme support
that this fork builds on.

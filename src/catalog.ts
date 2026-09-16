import type { Invocation, PaletteItem, PromptSpec, ResolveAction } from "./types";

const entry = (id: string, title: string, category: PaletteItem["category"], description: string, icon: string, key: string, invocation: Invocation, prompt?: PromptSpec): PaletteItem =>
  ({ id, title, category: "Actions", description, icon, aliases: [], shortcuts: key ? [key] : [], invocation, ...(prompt ? { prompt } : {}) });

const action = (id: string, title: string, category: PaletteItem["category"], description: string, icon: string, shortcut: string, argv: string[]): PaletteItem =>
  entry(id, title, category, description, icon, shortcut, { kind: "herdr", argv });

const resolve = (id: string, title: string, category: PaletteItem["category"], description: string, icon: string, shortcut: string, actionName: ResolveAction, step?: -1 | 1, prompt?: PromptSpec): PaletteItem =>
  entry(id, title, category, description, icon, shortcut, { kind: "resolve", action: actionName, ...(step !== undefined ? { step } : {}) }, prompt);

const shortcut = (id: string, title: string, category: PaletteItem["category"], description: string, icon: string, key: string): PaletteItem =>
  entry(id, title, category, description, icon, key, { kind: "shortcut" });

const name = { placeholder: "New name" };
const branchOrPath = { placeholder: "Branch or path" };
const confirmRemove = { placeholder: 'Type "yes" to confirm' };

export const defaultItems = (): PaletteItem[] => [
  action("new_workspace", "New workspace", "Workspace", "Create and focus a workspace", "◇", "prefix+shift+n", ["workspace", "create", "--focus"]),
  resolve("rename_workspace", "Rename workspace", "Workspace", "Rename the current workspace", "◇", "prefix+shift+w", "rename-workspace", undefined, name),
  resolve("close_workspace", "Close workspace", "Workspace", "Close the current workspace", "×", "prefix+shift+d", "close-workspace"),
  resolve("previous_workspace", "Previous workspace", "Workspace", "Focus the previous workspace", "←", "", "focus-workspace", -1),
  resolve("next_workspace", "Next workspace", "Workspace", "Focus the next workspace", "→", "", "focus-workspace", 1),

  action("new_tab", "New tab", "Tabs", "Create and focus a tab", "▣", "prefix+c", ["tab", "create", "--focus"]),
  resolve("rename_tab", "Rename tab", "Tabs", "Rename the current tab", "▣", "prefix+shift+t", "rename-tab", undefined, name),
  resolve("previous_tab", "Previous tab", "Tabs", "Focus the previous tab", "←", "prefix+p", "focus-tab", -1),
  resolve("next_tab", "Next tab", "Tabs", "Focus the next tab", "→", "prefix+n", "focus-tab", 1),
  resolve("close_tab", "Close tab", "Tabs", "Close the current tab", "×", "prefix+shift+x", "close-tab"),

  action("focus_pane_left", "Focus pane left", "Panes", "Focus the pane to the left", "←", "prefix+h", ["pane", "focus", "--direction", "left", "--current"]),
  action("focus_pane_down", "Focus pane down", "Panes", "Focus the pane below", "↓", "prefix+j", ["pane", "focus", "--direction", "down", "--current"]),
  action("focus_pane_up", "Focus pane up", "Panes", "Focus the pane above", "↑", "prefix+k", ["pane", "focus", "--direction", "up", "--current"]),
  action("focus_pane_right", "Focus pane right", "Panes", "Focus the pane to the right", "→", "prefix+l", ["pane", "focus", "--direction", "right", "--current"]),
  shortcut("cycle_pane_next", "Cycle pane next", "Panes", "Focus the next pane in the tab", "→", "prefix+tab"),
  shortcut("cycle_pane_previous", "Cycle pane previous", "Panes", "Focus the previous pane in the tab", "←", "prefix+shift+tab"),
  shortcut("last_pane", "Last pane", "Panes", "Focus the previously focused pane", "«", ""),
  action("split_vertical", "Split pane right", "Panes", "Split the current pane side by side", "▯", "prefix+v", ["pane", "split", "--current", "--direction", "right", "--focus"]),
  action("split_horizontal", "Split pane down", "Panes", "Split the current pane stacked", "▤", "prefix+minus", ["pane", "split", "--current", "--direction", "down", "--focus"]),
  action("zoom", "Zoom pane", "Panes", "Toggle focused pane zoom", "◲", "prefix+z", ["pane", "zoom", "--current"]),
  resolve("rename_pane", "Rename pane", "Panes", "Rename the current pane", "▭", "prefix+shift+p", "rename-pane", undefined, name),
  resolve("clear_pane_name", "Clear pane name", "Panes", "Remove the current pane's custom name", "▭", "", "rename-pane-clear"),
  action("resize_pane_left", "Resize pane left", "Panes", "Grow the pane toward the left", "←", "", ["pane", "resize", "--direction", "left", "--current"]),
  action("resize_pane_down", "Resize pane down", "Panes", "Grow the pane toward the bottom", "↓", "", ["pane", "resize", "--direction", "down", "--current"]),
  action("resize_pane_up", "Resize pane up", "Panes", "Grow the pane toward the top", "↑", "", ["pane", "resize", "--direction", "up", "--current"]),
  action("resize_pane_right", "Resize pane right", "Panes", "Grow the pane toward the right", "→", "", ["pane", "resize", "--direction", "right", "--current"]),
  action("swap_pane_left", "Swap pane left", "Panes", "Swap with the pane to the left", "←", "", ["pane", "swap", "--direction", "left", "--current"]),
  action("swap_pane_down", "Swap pane down", "Panes", "Swap with the pane below", "↓", "", ["pane", "swap", "--direction", "down", "--current"]),
  action("swap_pane_up", "Swap pane up", "Panes", "Swap with the pane above", "↑", "", ["pane", "swap", "--direction", "up", "--current"]),
  action("swap_pane_right", "Swap pane right", "Panes", "Swap with the pane to the right", "→", "", ["pane", "swap", "--direction", "right", "--current"]),
  resolve("move_pane_new_tab", "Move pane to new tab", "Panes", "Move the current pane into a new tab", "▣", "", "move-pane-new-tab"),
  resolve("move_pane_new_workspace", "Move pane to new workspace", "Panes", "Move the current pane into a new workspace", "◇", "", "move-pane-new-workspace"),
  resolve("close_pane", "Close pane", "Panes", "Close the focused pane", "×", "prefix+x", "close-pane"),

  resolve("new_worktree", "New worktree", "Worktrees", "Create and open a Git worktree", "◈", "prefix+shift+g", "worktree-create"),
  resolve("open_worktree", "Open worktree", "Worktrees", "Open an existing Git worktree by branch or path", "◈", "", "worktree-open", undefined, branchOrPath),
  resolve("remove_worktree", "Remove worktree", "Worktrees", "Remove the current workspace worktree checkout", "×", "", "worktree-remove", undefined, confirmRemove),

  resolve("previous_agent", "Previous agent", "Agents", "Focus the previous agent", "←", "", "focus-agent", -1),
  resolve("next_agent", "Next agent", "Agents", "Focus the next agent", "→", "", "focus-agent", 1),

  shortcut("help", "Keyboard shortcuts", "Herdr", "Show Herdr's shortcut guide", "?", "prefix+?"),
  shortcut("settings", "Settings", "Herdr", "Open Herdr settings", "≡", "prefix+s"),
  shortcut("copy_mode", "Copy mode", "Herdr", "Enter copy mode", "▧", "prefix+["),
  { ...entry("edit_scrollback", "Edit scrollback", "Herdr", "Open pane terminal history in your editor", "▤", "prefix+e", { kind: "pane-api", method: "pane.edit_scrollback" }), aliases: ["terminal history", "editor"] },
  shortcut("detach", "Detach", "Herdr", "Leave the current Herdr session", "»", "prefix+q"),
];

export type Category = "Actions" | "Workspace" | "Tabs" | "Panes" | "Worktrees" | "Agents" | "Herdr" | "Custom";

export type ResolveAction =
  | "close-pane"
  | "close-tab"
  | "close-workspace"
  | "focus-tab"
  | "focus-workspace"
  | "focus-agent"
  | "rename-pane"
  | "rename-pane-clear"
  | "rename-tab"
  | "rename-workspace"
  | "move-pane-new-tab"
  | "move-pane-new-workspace"
  | "worktree-create"
  | "worktree-open"
  | "worktree-remove";

export type Invocation =
  | { kind: "pane-api"; method: "pane.edit_scrollback" }
  | { kind: "herdr"; argv: string[] }
  | { kind: "resolve"; action: ResolveAction; step?: -1 | 1 }
  | { kind: "shortcut" };

export interface PromptSpec { placeholder: string; /** Accept an empty submit (used only when the resolver allows it). */ allowEmpty?: boolean }

export interface SessionTarget { paneId: string; tabId: string; workspaceId: string }

export interface PaletteItem {
  id: string;
  title: string;
  category: Category;
  description: string;
  icon: string;
  aliases: string[];
  shortcuts: string[];
  invocation: Invocation;
  prompt?: PromptSpec;
  priority?: number;
  agentStatus?: "blocked" | "done" | "working" | "idle" | "unknown";
}

export interface CommandResult { ok: boolean; message: string }

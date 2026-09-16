import { basename } from "node:path";
import { runHerdr, parseLaunchContext } from "./herdr";
import type { PaletteItem } from "./types";

type JsonRecord = Record<string, unknown>;

const records = (value: unknown): JsonRecord[] => Array.isArray(value)
  ? value.filter((entry): entry is JsonRecord => typeof entry === "object" && entry !== null)
  : [];
const text = (value: unknown) => typeof value === "string" ? value : "";
const count = (value: unknown) => typeof value === "number" ? value : 0;

function liveItem(id: string, title: string, category: PaletteItem["category"], description: string, icon: string, aliases: string[], invocation: PaletteItem["invocation"]): PaletteItem {
  return { id: `live:${id}`, title, category, description, icon, aliases: aliases.filter(Boolean), shortcuts: [], invocation };
}

export function itemsFromSnapshot(snapshot: JsonRecord, currentWorkspaceId: string): PaletteItem[] {
  const workspaces = records(snapshot.workspaces);
  const tabs = records(snapshot.tabs);
  const agents = records(snapshot.agents);
  const workspaceLabels = new Map(workspaces.map(workspace => [text(workspace.workspace_id), text(workspace.label)]));
  const tabLabels = new Map(tabs.map(tab => [text(tab.tab_id), text(tab.label)]));

  const workspaceItems = workspaces.map(workspace => {
    const workspaceId = text(workspace.workspace_id);
    const label = text(workspace.label) || workspaceId;
    const worktree = typeof workspace.worktree === "object" && workspace.worktree !== null ? workspace.worktree as JsonRecord : undefined;
    const checkoutPath = text(worktree?.checkout_path);
    const details = `${count(workspace.tab_count)} tabs · ${count(workspace.pane_count)} panes`;
    return liveItem(`workspace:${workspaceId}`, label, "Workspace", checkoutPath ? `${details} · ${checkoutPath}` : details, "◇",
      [workspaceId, String(workspace.number ?? ""), checkoutPath, text(worktree?.repo_name)], { kind: "herdr", argv: ["workspace", "focus", workspaceId] });
  });

  const openWorktreeItems = workspaces.flatMap(workspace => {
    const worktree = typeof workspace.worktree === "object" && workspace.worktree !== null ? workspace.worktree as JsonRecord : undefined;
    const path = text(worktree?.checkout_path);
    if (!path) return [];
    const workspaceId = text(workspace.workspace_id);
    const label = text(workspace.label) || text(worktree?.repo_name) || basename(path);
    return [liveItem(`worktree:${path}`, label, "Worktrees", `${text(worktree?.repo_name)} · ${path}`, "◈",
      [path, workspaceId, text(worktree?.repo_root)], { kind: "herdr", argv: ["workspace", "focus", workspaceId] })];
  });

  const tabItems = tabs.map(tab => {
    const tabId = text(tab.tab_id);
    const workspaceId = text(tab.workspace_id);
    const label = text(tab.label) || tabId;
    const workspace = workspaceLabels.get(workspaceId) || workspaceId;
    const title = [workspace, label].filter(Boolean).join(" → ");
    return liveItem(`tab:${tabId}`, title, "Tabs", `${count(tab.pane_count)} panes · ${text(tab.agent_status)}`, "▣",
      [label, tabId, workspaceId, workspace, String(tab.number ?? "")], { kind: "herdr", argv: ["tab", "focus", tabId] });
  });

  const agentItems = agents.map(agent => {
    const paneId = text(agent.pane_id);
    const workspaceId = text(agent.workspace_id);
    const tabId = text(agent.tab_id);
    const kind = text(agent.display_agent) || text(agent.agent) || "Agent";
    const sessionName = text(agent.title) || text(agent.terminal_title_stripped) || text(agent.name) || kind;
    const workspace = workspaceLabels.get(workspaceId) || workspaceId;
    const title = `${sessionName} - ${workspace}`;
    const item = liveItem(`agent:${paneId}`, title, "Agents", `${text(agent.agent_status)} · ${tabLabels.get(tabId) || tabId}`, "◈",
      [sessionName, workspace, paneId, workspaceId, tabId, kind, text(agent.name), text(agent.cwd), text(agent.terminal_title)], { kind: "herdr", argv: ["agent", "focus", paneId] });
    item.priority = ["blocked", "done", "working", "idle", "unknown"].indexOf(text(agent.agent_status));
    if (item.priority < 0) item.priority = 4;
    item.agentStatus = (["blocked", "done", "working", "idle", "unknown"] as const)[item.priority];
    return item;
  });

  return [...workspaceItems, ...tabItems, ...openWorktreeItems, ...agentItems];
}

export function itemsFromWorktrees(worktrees: unknown, currentWorkspaceId: string): PaletteItem[] {
  return records(worktrees).map(worktree => {
    const path = text(worktree.path);
    const branch = text(worktree.branch);
    const label = text(worktree.label) || branch || basename(path) || path;
    const openWorkspaceId = text(worktree.open_workspace_id);
    const invocation: PaletteItem["invocation"] = openWorkspaceId
      ? { kind: "herdr", argv: ["workspace", "focus", openWorkspaceId] }
      : { kind: "herdr", argv: ["worktree", "open", "--workspace", currentWorkspaceId, "--path", path, "--focus"] };
    return liveItem(`worktree:${path}`, label, "Worktrees", `${branch || "detached"} · ${path}`, "◈",
      [path, branch, openWorkspaceId], invocation);
  });
}

/** Load live search targets without allowing a discovery failure to hide static commands. */
export async function loadLiveItems(onSnapshot?: (items: PaletteItem[]) => void): Promise<PaletteItem[]> {
  const snapshotResult = await runHerdr(["api", "snapshot"], 5000);
  if (snapshotResult.code !== 0) throw new Error("Unable to refresh Herdr session.");

  let snapshot: JsonRecord;
  try {
    snapshot = JSON.parse(snapshotResult.stdout)?.result?.snapshot;
    if (!snapshot || !Array.isArray(snapshot.workspaces)) throw new Error();
  }
  catch { throw new Error("Herdr returned an unreadable session snapshot."); }
  const currentWorkspaceId = parseLaunchContext()?.workspaceId || text(snapshot.focused_workspace_id);
  const items = itemsFromSnapshot(snapshot, currentWorkspaceId);
  onSnapshot?.(items);
  if (!currentWorkspaceId) return items;

  const worktreeResult = await runHerdr(["worktree", "list", "--workspace", currentWorkspaceId], 5000);
  if (worktreeResult.code !== 0) return items;
  try {
    const combined = [...items, ...itemsFromWorktrees(JSON.parse(worktreeResult.stdout)?.result?.worktrees, currentWorkspaceId)];
    return [...new Map(combined.map(item => [item.id, item])).values()];
  }
  catch { return items; }
}

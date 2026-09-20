import { basename } from "node:path";
import { runHerdr, parseLaunchContext } from "./herdr";
import type { PaletteItem, PreviewPane, PreviewTab, ResourcePreview } from "./types";
import { loadWorkspaceVisitTimes } from "./workspace-history";

type JsonRecord = Record<string, unknown>;

const records = (value: unknown): JsonRecord[] => Array.isArray(value)
  ? value.filter((entry): entry is JsonRecord => typeof entry === "object" && entry !== null)
  : [];
const text = (value: unknown) => typeof value === "string" ? value : "";
const count = (value: unknown) => typeof value === "number" ? value : 0;
const agentStatuses = ["blocked", "done", "working", "idle", "unknown"] as const;

function status(value: unknown): PaletteItem["agentStatus"] | undefined {
  const candidate = text(value);
  return (agentStatuses as readonly string[]).includes(candidate)
    ? candidate as PaletteItem["agentStatus"]
    : undefined;
}

function usefulText(value: unknown): string {
  const candidate = text(value).trim();
  return candidate;
}

function paneRecords(panes: JsonRecord[], agents: JsonRecord[]): JsonRecord[] {
  // A pane ID is the only stable identity available for preview rows.  The
  // agent list is a complete fallback on older Herdr snapshots that omit panes.
  const source = panes.length > 0 ? panes : agents;
  const seen = new Set<string>();
  return source.filter(pane => {
    const id = text(pane.pane_id);
    if (!id || seen.has(id)) return false;
    seen.add(id);
    return true;
  });
}

function layoutFocus(snapshot: JsonRecord): Map<string, string> {
  return new Map(records(snapshot.layouts).flatMap(layout => {
    const tabId = text(layout.tab_id);
    const paneId = text(layout.focused_pane_id);
    return tabId && paneId ? [[tabId, paneId] as const] : [];
  }));
}

function previewPane(pane: JsonRecord, agent: JsonRecord | undefined, focusedByTab: ReadonlyMap<string, string>): PreviewPane {
  const id = text(pane.pane_id) || text(agent?.pane_id);
  const knownAgent = usefulText(pane.agent) || usefulText(pane.display_agent)
    || usefulText(agent?.agent) || usefulText(agent?.display_agent);
  const label = usefulText(pane.label) || usefulText(pane.terminal_title_stripped)
    || usefulText(agent?.label) || usefulText(agent?.terminal_title_stripped)
    || knownAgent || id;
  const tabId = text(pane.tab_id) || text(agent?.tab_id);
  const rememberedFocus = focusedByTab.get(tabId);
  const paneStatus = status(pane.agent_status) || status(agent?.agent_status);
  const cwd = text(pane.foreground_cwd) || text(pane.cwd)
    || text(agent?.foreground_cwd) || text(agent?.cwd);
  return {
    id,
    label,
    cwd,
    ...(knownAgent ? { agent: knownAgent } : {}),
    ...(paneStatus ? { status: paneStatus } : {}),
    focused: rememberedFocus ? rememberedFocus === id : Boolean(pane.focused || agent?.focused),
  };
}

function previewPaneList(sourcePanes: JsonRecord[], agentsByPane: ReadonlyMap<string, JsonRecord>, workspaceId: string, tabId?: string, focusedByTab: ReadonlyMap<string, string> = new Map<string, string>()): PreviewPane[] {
  return sourcePanes
    .filter(pane => text(pane.workspace_id) === workspaceId && (tabId === undefined || text(pane.tab_id) === tabId))
    .map(pane => previewPane(pane, agentsByPane.get(text(pane.pane_id)), focusedByTab));
}

function pathsForPanes(panes: JsonRecord[], workspaceId: string): string[] {
  const paths = new Set<string>();
  for (const pane of panes) {
    if (text(pane.workspace_id) !== workspaceId) continue;
    for (const path of [text(pane.foreground_cwd), text(pane.cwd)]) if (path) paths.add(path);
  }
  return [...paths];
}

function tabPreview(tab: JsonRecord, workspaceId: string, sourcePanes: JsonRecord[], agentsByPane: ReadonlyMap<string, JsonRecord>, focusedByTab: ReadonlyMap<string, string>): PreviewTab {
  const id = text(tab.tab_id);
  return {
    id,
    label: usefulText(tab.label) || id,
    panes: previewPaneList(sourcePanes, agentsByPane, workspaceId, id, focusedByTab),
  };
}

function liveItem(id: string, title: string, category: PaletteItem["category"], description: string, icon: string, aliases: string[], invocation: PaletteItem["invocation"], searchTitle = title, searchPaths: string[] = []): PaletteItem {
  return { id: `live:${id}`, title, category, description, icon, aliases: [...new Set(aliases.filter(Boolean))], searchTitle, searchPaths: [...new Set(searchPaths.filter(Boolean))], shortcuts: [], invocation };
}

export function itemsFromSnapshot(snapshot: JsonRecord, currentWorkspaceId: string, workspaceVisits: ReadonlyMap<string, number> = new Map()): PaletteItem[] {
  const workspaces = records(snapshot.workspaces);
  const tabs = records(snapshot.tabs);
  const agents = records(snapshot.agents);
  const sourcePanes = paneRecords(records(snapshot.panes), agents);
  const agentsByPane = new Map(agents.flatMap(agent => {
    const paneId = text(agent.pane_id);
    return paneId ? [[paneId, agent] as const] : [];
  }));
  const focusedByTab = layoutFocus(snapshot);
  const workspaceLabels = new Map(workspaces.map(workspace => [text(workspace.workspace_id), text(workspace.label)]));
  const tabLabels = new Map(tabs.map(tab => [text(tab.tab_id), text(tab.label)]));

  const workspaceItems = workspaces.map(workspace => {
    const workspaceId = text(workspace.workspace_id);
    const label = text(workspace.label) || workspaceId;
    const worktree = typeof workspace.worktree === "object" && workspace.worktree !== null ? workspace.worktree as JsonRecord : undefined;
    const checkoutPath = text(worktree?.checkout_path);
    const workspacePanes = sourcePanes.filter(pane => text(pane.workspace_id) === workspaceId);
    const workspaceTabs = tabs.filter(tab => text(tab.workspace_id) === workspaceId)
      .map(tab => tabPreview(tab, workspaceId, sourcePanes, agentsByPane, focusedByTab));
    const paneCount = typeof workspace.pane_count === "number" && Number.isFinite(workspace.pane_count)
      ? Math.max(0, Math.floor(workspace.pane_count))
      : workspacePanes.length;
    const branch = usefulText(worktree?.branch);
    const resourcePreview: ResourcePreview = {
      kind: "workspace",
      workspaceId,
      paths: checkoutPath ? [checkoutPath] : pathsForPanes(sourcePanes, workspaceId),
      tabs: workspaceTabs,
      paneCount,
      ...(branch ? { branch } : {}),
    };
    const details = `${count(workspace.tab_count)} tabs · ${count(workspace.pane_count)} panes`;
    const item = liveItem(`workspace:${workspaceId}`, label, "Workspace", checkoutPath ? `${details} · ${checkoutPath}` : details, "◇",
      [text(worktree?.repo_name)], { kind: "herdr", argv: ["workspace", "focus", workspaceId] }, text(workspace.label), [checkoutPath]);
    item.resourcePreview = resourcePreview;
    item.currentWorkspace = workspaceId === currentWorkspaceId;
    const lastVisitedAt = workspaceVisits.get(workspaceId);
    if (typeof lastVisitedAt === "number" && Number.isFinite(lastVisitedAt) && lastVisitedAt > 0) item.lastVisitedAt = lastVisitedAt;
    // Herdr's successful workspace.focus events provide the recency signal.
    // Keep Herdr's snapshot order as the stable fallback for unknown workspaces.
    return item;
  });

  // Generated numeric tab labels add no useful destination information.
  const tabItems = tabs.filter(tab => {
    const label = text(tab.label).trim();
    return label.length > 0 && !/^\p{Decimal_Number}+$/u.test(label);
  }).map(tab => {
    const tabId = text(tab.tab_id);
    const workspaceId = text(tab.workspace_id);
    const label = text(tab.label) || tabId;
    const workspace = workspaceLabels.get(workspaceId) || workspaceId;
    const title = [workspace, label].filter(Boolean).join(" → ");
    const item = liveItem(`tab:${tabId}`, title, "Tabs", `${count(tab.pane_count)} panes · ${text(tab.agent_status)}`, "▣",
      [], { kind: "herdr", argv: ["tab", "focus", tabId] }, [workspaceLabels.get(workspaceId), text(tab.label)].filter(Boolean).join(" → "));
    item.resourcePreview = {
      kind: "tab",
      workspaceId,
      workspaceLabel: workspace,
      panes: previewPaneList(sourcePanes, agentsByPane, workspaceId, tabId, focusedByTab),
    };
    return item;
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
      [kind], { kind: "herdr", argv: ["agent", "focus", paneId] }, [sessionName, workspaceLabels.get(workspaceId)].filter(Boolean).join(" - "), [text(agent.cwd)]);
    item.priority = ["blocked", "done", "working", "idle", "unknown"].indexOf(text(agent.agent_status));
    if (item.priority < 0) item.priority = 4;
    item.agentStatus = (["blocked", "done", "working", "idle", "unknown"] as const)[item.priority];
    if (paneId) item.livePaneId = paneId;
    item.currentWorkspace = workspaceId === currentWorkspaceId;
    const session = agent.agent_session as JsonRecord | undefined;
    const provider = text(session?.agent) || text(agent.agent);
    if (session?.kind === "id" && text(session.value) && (provider === "codex" || provider === "claude" || provider === "opencode")) {
      item.session = { provider, id: text(session.value), title: sessionName, cwd: text(agent.cwd), updatedAt: 0 };
    }
    return item;
  });

  return [...workspaceItems, ...tabItems, ...agentItems];
}

export function itemsFromWorktrees(worktrees: unknown, currentWorkspaceId: string): PaletteItem[] {
  return records(worktrees).filter(worktree => !text(worktree.open_workspace_id)).map(worktree => {
    const path = text(worktree.path);
    const branch = usefulText(worktree.branch);
    const label = text(worktree.label) || branch || basename(path) || path;
    const invocation: PaletteItem["invocation"] = { kind: "herdr", argv: ["worktree", "open", "--workspace", currentWorkspaceId, "--path", path, "--focus"] };
    const item = liveItem(`workspace:worktree:${path}`, label, "Workspace", `${branch || "detached"} · ${path}`, "◇",
      [branch], invocation, label, [path]);
    item.resourcePreview = { kind: "worktree", path, ...(branch ? { branch } : {}) };
    return item;
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
  const items = itemsFromSnapshot(snapshot, currentWorkspaceId, loadWorkspaceVisitTimes());
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

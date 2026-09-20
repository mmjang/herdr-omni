import type { SessionTarget } from "./types";
import { homedir } from "node:os";
import { join } from "node:path";

const binary = () => process.env.HERDR_BIN_PATH ?? "herdr";

export async function runHerdr(argv: string[], timeoutMs?: number, cwd?: string) {
  const child = Bun.spawn([binary(), ...argv], { cwd, stdout: "pipe", stderr: "pipe" });
  const timeout = timeoutMs ? setTimeout(() => child.kill(), timeoutMs) : undefined;
  try {
    const [stdout, stderr, code] = await Promise.all([new Response(child.stdout).text(), new Response(child.stderr).text(), child.exited]);
    return { code, stdout, stderr };
  } finally { clearTimeout(timeout); }
}

/** Herdr reports server errors as JSON on stderr; show its message rather than the raw envelope. */
export function explain(stderr: string, code: number) {
  const trimmed = stderr.trim();
  try { const message = JSON.parse(trimmed)?.error?.message; if (message) return String(message); } catch {}
  return trimmed || `Herdr exited with status ${code}.`;
}

/** Herdr injects the pane, tab, and workspace the palette was summoned from as JSON. */
export function parseLaunchContext(json = process.env.HERDR_PLUGIN_CONTEXT_JSON): SessionTarget | undefined {
  if (!json) return undefined;
  try {
    const context = JSON.parse(json);
    const target = { paneId: context.focused_pane_id, tabId: context.tab_id, workspaceId: context.workspace_id };
    return Object.values(target).every(value => typeof value === "string" && value) ? target : undefined;
  } catch { return undefined; }
}

/**
 * Prefer the popup's original host. The read-only pane.current command can fall
 * back to server focus when launch context is absent; pane mutations cannot use
 * --current without HERDR_PANE_ID, so callers must pass this target explicitly.
 */
export async function sessionTarget(): Promise<SessionTarget | undefined> {
  const launched = parseLaunchContext();
  if (launched) return launched;
  const { code, stdout } = await runHerdr(["pane", "current", "--current"]);
  if (code !== 0) return undefined;
  try {
    const pane = JSON.parse(stdout)?.result?.pane;
    return pane?.pane_id ? { paneId: pane.pane_id, tabId: pane.tab_id, workspaceId: pane.workspace_id } : undefined;
  } catch { return undefined; }
}

export type RingStep = { id: string } | { message: string };

/** Step through an ordered ring of IDs, wrapping at the ends. */
export function stepRing(ids: string[], current: string, step: number, alone: string, missing: string): RingStep {
  if (ids.length < 2) return { message: alone };
  const index = ids.indexOf(current);
  if (index < 0) return { message: missing };
  return { id: ids[(index + step + ids.length) % ids.length]! };
}

export type TabStep = { tabId: string } | { message: string };

export function stepTab(tabIds: string[], current: string, step: number): TabStep {
  const result = stepRing(tabIds, current, step, "This workspace only has one tab.", "Herdr did not report the tab that opened the palette.");
  return "id" in result ? { tabId: result.id } : result;
}

export async function neighborTab(target: SessionTarget, step: number): Promise<TabStep> {
  const { code, stdout, stderr } = await runHerdr(["tab", "list", "--workspace", target.workspaceId]);
  if (code !== 0) return { message: explain(stderr, code) };
  try {
    const tabs: { tab_id: string }[] = JSON.parse(stdout)?.result?.tabs ?? [];
    return stepTab(tabs.map(tab => tab.tab_id), target.tabId, step);
  } catch { return { message: "Herdr returned an unreadable tab list." }; }
}

export type WorkspaceStep = { workspaceId: string } | { message: string };

export function stepWorkspace(workspaceIds: string[], current: string, step: number): WorkspaceStep {
  const result = stepRing(workspaceIds, current, step, "Only one workspace is open.", "Herdr did not report the workspace that opened the palette.");
  return "id" in result ? { workspaceId: result.id } : result;
}

export async function neighborWorkspace(target: SessionTarget, step: number): Promise<WorkspaceStep> {
  const { code, stdout, stderr } = await runHerdr(["workspace", "list"]);
  if (code !== 0) return { message: explain(stderr, code) };
  try {
    const workspaces: { workspace_id: string }[] = JSON.parse(stdout)?.result?.workspaces ?? [];
    return stepWorkspace(workspaces.map(workspace => workspace.workspace_id), target.workspaceId, step);
  } catch { return { message: "Herdr returned an unreadable workspace list." }; }
}

export type AgentStep = { paneId: string } | { message: string };

export function stepAgent(agents: { pane_id: string; focused?: boolean }[], currentPaneId: string, step: number): AgentStep {
  if (agents.length === 0) return { message: "No agents are running." };
  if (agents.length === 1) return agents[0]!.pane_id === currentPaneId
    ? { message: "Only one agent is running." }
    : { paneId: agents[0]!.pane_id };
  let index = agents.findIndex(agent => agent.pane_id === currentPaneId);
  if (index < 0) index = agents.findIndex(agent => agent.focused);
  if (index < 0) return { paneId: agents[step < 0 ? agents.length - 1 : 0]!.pane_id };
  return { paneId: agents[(index + step + agents.length) % agents.length]!.pane_id };
}

export async function neighborAgent(target: SessionTarget, step: number): Promise<AgentStep> {
  const { code, stdout, stderr } = await runHerdr(["agent", "list"]);
  if (code !== 0) return { message: explain(stderr, code) };
  try {
    const agents: { pane_id: string; focused?: boolean }[] = JSON.parse(stdout)?.result?.agents ?? [];
    return stepAgent(agents, target.paneId, step);
  } catch { return { message: "Herdr returned an unreadable agent list." }; }
}

export function worktreeCreateArgv(workspaceId: string, input: string): string[] {
  const branch = input.trim();
  return ["worktree", "create", "--workspace", workspaceId, ...(branch ? ["--branch", branch] : []), "--focus"];
}

/** Prefer --path when the input looks like a filesystem location; otherwise treat it as a branch name. */
export function worktreeOpenArgv(workspaceId: string, input: string): string[] {
  const value = input.trim();
  const flag = value.startsWith("/") || value.startsWith("~") || value.startsWith(".") ? "--path" : "--branch";
  // Bun.spawn bypasses the shell, so a user's ~/path needs explicit expansion.
  const expanded = value === "~" ? homedir() : value.startsWith("~/") ? join(homedir(), value.slice(2)) : value;
  return ["worktree", "open", "--workspace", workspaceId, flag, expanded, "--focus"];
}

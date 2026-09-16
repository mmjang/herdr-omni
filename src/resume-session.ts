import { stat } from "node:fs/promises";
import { realpathSync } from "node:fs";
import { basename, isAbsolute, normalize, relative, sep } from "node:path";
import { randomUUID } from "node:crypto";
import { runHerdr, sessionTarget, explain } from "./herdr";
import { requestHerdr } from "./socket";
import type { SavedSession, CommandResult, ResumeWorkspaceChoice } from "./types";
import { cleanText } from "./sessions";

const defaults = {
  run: runHerdr, target: sessionTarget, focus: (pane: string) => requestHerdr("pane.focus", { pane_id: pane }),
  directory: async (cwd: string) => isAbsolute(cwd) && await stat(cwd).then(info => info.isDirectory()).catch(() => false),
  installed: (provider: string) => Boolean(Bun.which(provider)),
  related: async (cwd: string, original: string) => {
    try {
      const child = Bun.spawn(["git", "-C", cwd, "worktree", "list", "--porcelain", "-z"], { stdout: "pipe", stderr: "ignore" });
      const timeout = setTimeout(() => child.kill(), 1000);
      try {
        const output = await new Response(child.stdout).text();
        return await child.exited === 0 && output.split("\0").some(field => field === `worktree ${original}`);
      } finally { clearTimeout(timeout); }
    } catch { return false; }
  },
};

/** Exact pane paths, or an explicit worktree containing the session directory.
 * Never use labels/basenames or a broad parent shell directory as identity. */
export function findSessionWorkspace(snapshot: any, cwd: string): string | undefined {
  if (!isAbsolute(cwd)) return;
  const canonical = (value: string) => {
    try { return realpathSync(value); } catch { return normalize(value).replace(/\/$/, "") || "/"; }
  };
  const path = canonical(cwd);
  const matches = new Map<string, number>();
  const workspaces = Array.isArray(snapshot.workspaces) ? snapshot.workspaces : [];
  const ids = new Set(workspaces.map((workspace: any) => workspace.workspace_id));
  for (const workspace of workspaces) {
    const checkout = workspace.worktree?.checkout_path;
    if (typeof checkout !== "string" || !isAbsolute(checkout)) continue;
    const root = canonical(checkout);
    if (root === "/") continue;
    const child = relative(root, path);
    if (child === "" || (!isAbsolute(child) && child !== ".." && !child.startsWith(`..${sep}`))) matches.set(workspace.workspace_id, root.length);
  }
  // Exact matches outrank an enclosing worktree path.
  const exact = new Set<string>();
  for (const pane of [...(snapshot.panes ?? []), ...(snapshot.agents ?? [])]) {
    if (!ids.has(pane.workspace_id)) continue;
    if ([pane.cwd, pane.foreground_cwd].some(value => typeof value === "string" && isAbsolute(value) && canonical(value) === path)) exact.add(pane.workspace_id);
  }
  const longest = Math.max(...matches.values());
  const candidates = exact.size ? exact : new Set([...matches].filter(([, length]) => length === longest).map(([id]) => id));
  // Resolve ties in Herdr's workspace order, independent of pane/agent ordering.
  return workspaces.find((workspace: any) => candidates.has(workspace.workspace_id))?.workspace_id;
}

/** Only surviving paths are offered; names suggest a destination, never authorize it. */
export async function resumeWorkspaceChoices(state: any, original: string, current: string, directory: (cwd: string) => Promise<boolean>, related: (cwd: string, original: string) => Promise<boolean> = async () => false): Promise<ResumeWorkspaceChoice[]> {
  const choices: Array<ResumeWorkspaceChoice & { score: number }> = [];
  const repositoryMatches = new Map<string, Promise<boolean>>();
  for (const workspace of state.workspaces ?? []) {
    const paths = [workspace.worktree?.checkout_path, ...(state.panes ?? []).filter((pane: any) => pane.workspace_id === workspace.workspace_id).flatMap((pane: any) => [pane.foreground_cwd, pane.cwd])];
    let best: (ResumeWorkspaceChoice & { score: number }) | undefined;
    for (const cwd of new Set(paths)) {
      if (typeof cwd !== "string" || !isAbsolute(cwd) || !await directory(cwd)) continue;
      const nameMatch = basename(cwd) === basename(original);
      if (!repositoryMatches.has(cwd)) repositoryMatches.set(cwd, related(cwd, original));
      const sameRepository = await repositoryMatches.get(cwd);
      const score = sameRepository ? 3 : nameMatch ? 2 : workspace.workspace_id === current ? 1 : 0;
      const choice = { id: workspace.workspace_id, label: cleanText(workspace.label || workspace.workspace_id), cwd, score,
        reason: sameRepository ? "Same Git repository" : nameMatch ? "Matching project folder" : score ? "Current workspace" : "Available directory" };
      if (!best || score > best.score) best = choice;
    }
    if (best) choices.push(best);
  }
  return choices.sort((a, b) => b.score - a.score).map(({ score, ...choice }) => choice);
}

export async function resumeSavedSession(session: SavedSession, deps: Omit<typeof defaults, "related"> & Partial<Pick<typeof defaults, "related">> = defaults, confirmedWorkspaceId?: string, destination?: { id: string; cwd: string }): Promise<CommandResult> {
  if (!["codex", "claude"].includes(session.provider) || !/^[a-zA-Z0-9][a-zA-Z0-9_-]{0,199}$/.test(session.id)) return { ok: false, message: "Invalid saved session identifier." };
  // Recheck live identity immediately before launching, not just when the popup opened.
  const snapshot = await deps.run(["api", "snapshot"], 5000);
  if (snapshot.code !== 0) return { ok: false, message: "Cannot verify whether this session is already open. Try again." };
  let state: any;
  try {
    state = JSON.parse(snapshot.stdout).result.snapshot;
    const agents = state.agents;
    if (!Array.isArray(agents)) throw new Error();
    const agent = agents.find(agent => agent.agent_session?.kind === "id" && agent.agent_session.value === session.id && (agent.agent_session.agent ?? agent.agent) === session.provider);
    if (agent?.pane_id) return deps.focus(agent.pane_id);
  } catch { return { ok: false, message: "Herdr returned an unreadable agent list." }; }
  if (!deps.installed(session.provider)) return { ok: false, message: `${session.provider} is not installed or not on PATH.` };
  const originalExists = await deps.directory(session.cwd);
  const target = await deps.target();
  if (!target) return { ok: false, message: "Cannot identify the workspace that opened Omni." };
  const workspaces = Array.isArray(state.workspaces) ? state.workspaces : [];
  let cwd = session.cwd;
  let workspaceId = findSessionWorkspace(state, session.cwd);
  if (!originalExists || destination) {
    const choices = await resumeWorkspaceChoices(state, session.cwd, target.workspaceId, deps.directory, deps.related);
    const chosen = destination && choices.find(choice => choice.id === destination.id && choice.cwd === destination.cwd);
    if (!chosen) return { ok: false, message: choices.length ? "Original directory unavailable. Choose a workspace directory. Deleted files and Git changes will not be restored." : "No available workspace directory found. Open a workspace with an existing directory and try again.", workspaceChoices: choices.length ? choices : undefined };
    workspaceId = chosen.id;
    cwd = chosen.cwd;
  }
  if (!workspaceId) {
    const current = workspaces.find((workspace: any) => workspace.workspace_id === target.workspaceId);
    if (!current) return { ok: false, message: "The workspace that opened Omni is no longer available." };
    if (confirmedWorkspaceId !== target.workspaceId) {
      const label = cleanText(current.label || current.workspace_id);
      return { ok: false, message: `No workspace matches ${session.cwd}. Resume in current workspace “${label}”? The session will keep its original project directory.`, confirmWorkspace: { id: target.workspaceId, label } };
    }
    workspaceId = confirmedWorkspaceId;
  }
  const created = await deps.run(["tab", "create", "--workspace", workspaceId, "--cwd", cwd, "--label", cleanText(session.title).slice(0, 100) || session.provider, "--no-focus"], 5000);
  if (created.code !== 0) return { ok: false, message: explain(created.stderr, created.code) };
  let pane: string;
  try { pane = JSON.parse(created.stdout).result.root_pane.pane_id; if (!pane) throw new Error(); }
  catch { return { ok: false, message: "A tab was created, but Herdr did not return its pane. Check the new tab before retrying." }; }
  const args = session.provider === "codex" ? ["resume", session.id] : ["--resume", session.id];
  if (session.provider === "codex" && destination) args.push("--cd", cwd);
  const started = await deps.run(["agent", "start", `omni-${randomUUID().slice(0, 8)}`, "--kind", session.provider, "--pane", pane, "--", ...args], 35_000);
  if (started.code !== 0) {
    await deps.focus(pane);
    // Retain the tab for diagnostics rather than close a possibly running agent.
    return { ok: false, message: `Resume could not be confirmed. Check the new tab before retrying: ${explain(started.stderr, started.code)}` };
  }
  return deps.focus(pane);
}

import { explain, neighborAgent, neighborTab, neighborWorkspace, runHerdr, sessionTarget, worktreeCreateArgv, worktreeOpenArgv } from "./herdr";
import type { CommandResult, Invocation, PaletteItem, ResolveAction, SessionTarget } from "./types";
import { requestHerdr } from "./socket";
import { resumeSavedSession } from "./resume-session";

type Resolution = { argv: string[] } | { message: string };

function needInput(input: string, label: string): Resolution | undefined {
  return input.trim() ? undefined : { message: `Enter a ${label}.` };
}

async function resolveAction(action: ResolveAction, target: SessionTarget, step: -1 | 1 | undefined, input: string): Promise<Resolution> {
  switch (action) {
    case "create-tab":
      return { argv: ["tab", "create", "--workspace", target.workspaceId, "--focus"] };
    case "close-pane":
      return { argv: ["pane", "close", target.paneId] };
    case "close-tab":
      return { argv: ["tab", "close", target.tabId] };
    case "close-workspace":
      return { argv: ["workspace", "close", target.workspaceId] };
    case "focus-tab": {
      const neighbor = await neighborTab(target, step ?? 1);
      return "tabId" in neighbor ? { argv: ["tab", "focus", neighbor.tabId] } : neighbor;
    }
    case "focus-workspace": {
      const neighbor = await neighborWorkspace(target, step ?? 1);
      return "workspaceId" in neighbor ? { argv: ["workspace", "focus", neighbor.workspaceId] } : neighbor;
    }
    case "focus-agent": {
      const neighbor = await neighborAgent(target, step ?? 1);
      return "paneId" in neighbor ? { argv: ["agent", "focus", neighbor.paneId] } : neighbor;
    }
    case "rename-pane": {
      const missing = needInput(input, "pane name");
      return missing ?? { argv: ["pane", "rename", target.paneId, input.trim()] };
    }
    case "rename-pane-clear":
      return { argv: ["pane", "rename", target.paneId, "--clear"] };
    case "rename-tab": {
      const missing = needInput(input, "tab name");
      return missing ?? { argv: ["tab", "rename", target.tabId, input.trim()] };
    }
    case "rename-workspace": {
      const missing = needInput(input, "workspace name");
      return missing ?? { argv: ["workspace", "rename", target.workspaceId, input.trim()] };
    }
    case "move-pane-new-tab":
      return { argv: ["pane", "move", target.paneId, "--new-tab", "--focus"] };
    case "move-pane-new-workspace":
      return { argv: ["pane", "move", target.paneId, "--new-workspace", "--focus"] };
    case "worktree-create":
      return { argv: worktreeCreateArgv(target.workspaceId, input) };
    case "worktree-open": {
      const missing = needInput(input, "branch or path");
      return missing ?? { argv: worktreeOpenArgv(target.workspaceId, input) };
    }
    case "worktree-remove":
      if (input.trim().toLowerCase() !== "yes") return { message: 'Type "yes" to remove this worktree.' };
      return { argv: ["worktree", "remove", "--workspace", target.workspaceId] };
  }
}

/** Some Herdr commands take an explicit target, so they need the pane the palette was summoned from. */
export async function resolve(invocation: Exclude<Invocation, { kind: "shortcut" } | { kind: "pane-api" } | { kind: "resume-session" }>, input = ""): Promise<Resolution> {
  if (invocation.kind === "herdr") {
    // Popup plugins do not receive HERDR_PANE_ID, which pane mutations require
    // for --current. Resolve the host from the injected launch context instead.
    if (invocation.argv[0] !== "pane" || !invocation.argv.includes("--current")) return { argv: invocation.argv };
    const target = await sessionTarget();
    if (!target) return { message: "Herdr did not report the pane that opened the palette." };
    return { argv: invocation.argv.flatMap(arg => arg === "--current" ? ["--pane", target.paneId] : [arg]) };
  }
  const target = await sessionTarget();
  if (!target) return { message: "Herdr did not report the pane that opened the palette." };
  return resolveAction(invocation.action, target, invocation.step, input);
}

export async function execute(item: PaletteItem, input = ""): Promise<CommandResult> {
  if (item.invocation.kind === "resume-session") {
    if (item.invocation.fallbackWorkspaceId && input.trim().toLowerCase() !== "yes") return { ok: false, message: 'Type "yes" to resume here, or press Esc to cancel.' };
    return resumeSavedSession(item.invocation.session, undefined, item.invocation.fallbackWorkspaceId, item.invocation.destination);
  }
  if (item.invocation.kind === "pane-api") {
    const target = await sessionTarget();
    if (!target) return { ok: false, message: "Herdr did not report the pane that opened the palette." };
    return requestHerdr(item.invocation.method, { pane_id: target.paneId });
  }
  if (item.invocation.kind === "shortcut") {
    const keys = item.shortcuts.join(" / ");
    const limitation = "Herdr does not expose this client action to plugins.";
    return { ok: false, message: keys ? `Close Omni (Esc), then press ${keys}. ${limitation}` : `${limitation} Configure its Herdr keybinding first.` };
  }
  const resolved = await resolve(item.invocation, input);
  if ("message" in resolved) return { ok: false, message: resolved.message };
  // Herdr 0.9.0's agent.focus updates server focus without moving the client's
  // visible tab. pane.focus projects both, and our agent targets are pane IDs.
  if (resolved.argv[0] === "agent" && resolved.argv[1] === "focus") {
    return requestHerdr("pane.focus", { pane_id: resolved.argv[2] });
  }
  const { code, stderr } = await runHerdr(resolved.argv);
  return code === 0 ? { ok: true, message: "" } : { ok: false, message: explain(stderr, code) };
}

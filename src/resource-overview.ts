import type { PaletteItem, PreviewPane } from "./types";
import { cleanText } from "./sessions";

export function paneLabel(pane: PreviewPane): string {
  return `${cleanText(pane.label)}${pane.agent ? ` · ${cleanText(pane.agent)} [${pane.status ?? "unknown"}]` : ""}`;
}

export function resourceOverview(resource: NonNullable<PaletteItem["resourcePreview"]>, branch?: string): string {
  if (resource.kind === "worktree") return `Path: ${cleanText(resource.path) || "unavailable"}\nBranch: ${cleanText(resource.branch ?? "unavailable")}\n\nNot open in a workspace.\nEnter opens this worktree.`;
  if (resource.kind !== "workspace") return "";
  const paths = resource.paths.length === 1 ? `Path: ${cleanText(resource.paths[0]!)}`
    : `Paths:\n${resource.paths.length ? resource.paths.map(path => `  ${cleanText(path)}`).join("\n") : "  unavailable"}`;
  const tabs = resource.tabs.map(tab => [
    `▣ ${cleanText(tab.label)} · ${tab.panes.length} panes`,
    ...tab.panes.map(pane => `  ${paneLabel(pane)}`),
    ...(tab.panes.length ? [] : ["  Pane details unavailable"]),
  ].join("\n")).join("\n\n");
  return `Branch: ${cleanText(resource.branch ?? branch ?? "unavailable")}\n${paths}\n\n${tabs || "No tabs available"}`;
}

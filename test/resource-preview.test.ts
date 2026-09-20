import { expect, test } from "bun:test";
import { createTestRenderer } from "@opentui/core/testing";
import { mountPalette } from "../src/palette";
import { resourceOverview } from "../src/resource-overview";
import type { PaletteItem, PreviewPane } from "../src/types";

const wait = (ms = 250) => new Promise(resolve => setTimeout(resolve, ms));
const panes: PreviewPane[] = [
  { id: "w1:p1", label: "Build terminal", cwd: "/repo/shop", focused: false },
  { id: "w1:p2", label: "Payment agent", cwd: "/repo/shop", focused: true, agent: "claude", status: "blocked" },
];
const tab: PaletteItem = { id: "tab-1", title: "Shop → Development", category: "Tabs", description: "", icon: "▣", aliases: [], shortcuts: [],
  invocation: { kind: "herdr", argv: ["tab", "focus", "w1:t1"] },
  resourcePreview: { kind: "tab", workspaceId: "w1", workspaceLabel: "Shop", panes } };
const workspace: PaletteItem = { ...tab, id: "workspace-1", title: "Shop", category: "Workspace",
  invocation: { kind: "herdr", argv: ["workspace", "focus", "w1"] },
  resourcePreview: { kind: "workspace", workspaceId: "w1", paths: ["/repo/shop"], tabs: [{ id: "w1:t1", label: "Development", panes }], paneCount: 2 } };

test("workspace overview shows paths, branch, tabs and agent state without reading a terminal", async () => {
  const h = await createTestRenderer({ width: 150, height: 30 });
  let reads = 0, runs = 0;
  const details: unknown[] = [];
  mountPalette(h.renderer, [workspace], {
    panePreview: async () => { reads++; return "terminal"; },
    resourceDetails: async target => { details.push(target); return "feature/payments"; },
    run: async () => { runs++; return { ok: true, message: "" }; }, close: () => {},
  });
  try {
    await wait(); await h.renderOnce();
    const frame = h.captureCharFrame();
    for (const text of ["Workspace overview", "/repo/shop", "feature/payments", "Development", "Payment agent", "[blocked]", "Build terminal"]) expect(frame).toContain(text);
    expect(details).toEqual([{ kind: "workspace", workspaceId: "w1", path: "/repo/shop" }]);
    expect(reads).toBe(0); expect(runs).toBe(0);
    h.mockInput.pressKey("y", { ctrl: true }); await h.renderOnce();
    expect(h.captureCharFrame()).not.toContain("Workspace overview");
  } finally { h.renderer.destroy(); }
});

for (const width of [80, 150]) {
  test(`tab preview at ${width} columns selects panes without focusing or executing them`, async () => {
    const h = await createTestRenderer({ width, height: 30 });
    const reads: Array<{ id: string; signal: AbortSignal }> = [];
    let runs = 0;
    const palette = mountPalette(h.renderer, [tab], {
      panePreview: async (id, signal) => { reads.push({ id, signal }); return `${id}\n❯ REAL INPUT PROMPT`; },
      resourceDetails: async target => target.kind === "pane" && target.paneId === "w1:p1" ? "bun" : "claude",
      run: async () => { runs++; return { ok: false, message: "opened" }; }, close: () => {},
    });
    try {
      await wait(); await h.renderOnce();
      let frame = h.captureCharFrame();
      expect(frame).toContain("Tab preview");
      expect(frame).toContain("Program: claude");
      expect(frame).toContain("❯ REAL INPUT PROMPT");
      expect(frame).toContain("enter/click");
      expect(reads[0]!.id).toBe("w1:p2");
      h.mockInput.pressKey("F6"); await wait(); await h.renderOnce();
      expect(reads[0]!.signal.aborted).toBe(true);
      expect(reads.at(-1)!.id).toBe("w1:p1");
      expect(h.captureCharFrame()).toContain("Program: bun");
      // A topology refresh must not override the pane explicitly chosen in this preview.
      palette.updateItems([{ ...tab, description: "updated" }]); await h.renderOnce();
      expect(h.captureCharFrame()).toContain("Program: bun");
      h.mockInput.pressKey("o", { ctrl: true }); await h.renderOnce();
      frame = h.captureCharFrame();
      const rows = frame.split("\n");
      const y = rows.findIndex(row => row.includes("2. Payment agent"));
      expect(y).toBeGreaterThan(-1);
      await h.mockMouse.click(rows[y]!.indexOf("Payment agent") + 1, y);
      await wait(); await h.renderOnce();
      expect(h.captureCharFrame()).toContain("Program: claude");
      expect(runs).toBe(0);
      h.mockInput.pressEnter(); await wait();
      expect(runs).toBe(1);
    } finally { h.renderer.destroy(); }
  });
}

test("removed panes cancel old reads and stale process details cannot overwrite another pane", async () => {
  const h = await createTestRenderer({ width: 150, height: 28 });
  const details: Array<{ signal: AbortSignal; resolve: (text: string) => void }> = [];
  const reads: Array<{ id: string; signal: AbortSignal }> = [];
  const palette = mountPalette(h.renderer, [tab], {
    panePreview: async (id, signal) => { reads.push({ id, signal }); return `screen ${id}`; },
    resourceDetails: async (_target, signal) => new Promise(resolve => details.push({ signal, resolve })),
    run: async () => ({ ok: true, message: "" }), close: () => {},
  });
  try {
    await wait();
    palette.updateItems([{ ...tab, resourcePreview: { kind: "tab", workspaceId: "w1", workspaceLabel: "Shop", panes: [panes[0]!] } }]);
    expect(reads[0]!.signal.aborted).toBe(true);
    expect(details[0]!.signal.aborted).toBe(true);
    details[0]!.resolve("STALE PROGRAM");
    await wait(); details[1]!.resolve("bun"); await wait(0); await h.renderOnce();
    expect(h.captureCharFrame()).not.toContain("STALE PROGRAM");
    expect(h.captureCharFrame()).toContain("Program: bun");
    expect(reads.at(-1)!.id).toBe("w1:p1");
    h.mockInput.pressKey("y", { ctrl: true });
    expect(reads.at(-1)!.signal.aborted).toBe(true);
    expect(details.at(-1)!.signal.aborted).toBe(true);
  } finally { h.renderer.destroy(); }
});

test("empty tabs and unopened worktrees have honest previews without a guessed pane", async () => {
  expect(resourceOverview({ kind: "worktree", path: "/repo/feature", branch: "feature/ui" })).toContain("Not open in a workspace");
  const h = await createTestRenderer({ width: 90, height: 24 });
  let reads = 0;
  mountPalette(h.renderer, [{ ...tab, resourcePreview: { kind: "tab", workspaceId: "w1", workspaceLabel: "Shop", panes: [] } }], {
    panePreview: async () => { reads++; return ""; },
    run: async () => ({ ok: true, message: "" }), close: () => {},
  });
  try {
    await wait(); await h.renderOnce();
    expect(h.captureCharFrame()).toContain("No pane details available");
    expect(reads).toBe(0);
  } finally { h.renderer.destroy(); }
});

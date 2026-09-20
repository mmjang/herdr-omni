import { expect, test } from "bun:test";
import { createTestRenderer } from "@opentui/core/testing";
import { mountPalette, PANE_PREVIEW_REFRESH_MS } from "../src/palette";
import { paneScreen } from "../src/pane-screen";
import { itemsFromSnapshot } from "../src/live";
import { savedSessionItem } from "../src/sessions";
import type { PaletteItem, SavedSession } from "../src/types";

const wait = (ms = 250) => new Promise(resolve => setTimeout(resolve, ms));
const live = (id = "w1:p1"): PaletteItem => itemsFromSnapshot({ agents: [{ pane_id: id, workspace_id: "w1", tab_id: "w1:t1", agent: "claude", title: "Needle live", agent_status: "working" }] }, "w1")[0]!;
const session: SavedSession = { provider: "claude", id: "history-one", title: "Saved example", cwd: "/project/shop", updatedAt: 1 };

test("pane screen follows the bottom, preserves columns, and clips whole graphemes without wrapping", () => {
  const screen = paneScreen("heading\n  a    b\n  你好🙂tail\n❯ Allow?\n\n", 9, 2);
  expect(screen.lines.map(line => line[0]!.text)).toEqual(["  你好🙂…", "❯ Allow?"]);
  expect(screen.clipped).toBe(true);
  expect(screen.totalRows).toBe(4);
  const earlier = paneScreen("heading\n  a    b\nlast", 20, 2, 100);
  expect(earlier.offset).toBe(1);
  expect(earlier.lines.map(line => line[0]!.text)).toEqual(["heading", "  a    b"]);
  expect(paneScreen("\n \n", 10, 3).lines).toEqual([]);
});

test("live agents without a session ID preview the real pane and refresh while selected", async () => {
  const h = await createTestRenderer({ width: 140, height: 26 });
  const targets: string[] = [], requests: string[] = [];
  let executions = 0;
  const item = live();
  expect(item.livePaneId).toBe("w1:p1");
  expect(item.session).toBeUndefined();
  mountPalette(h.renderer, [item], {
    panePreview: async paneId => { targets.push(paneId); return `Tool output ${targets.length}\n  command: bun test\n❯ Approve command?`; },
    sessionJob: async request => { requests.push(request.type); },
    run: async () => { executions++; return { ok: true, message: "" }; }, close: () => {},
  });
  try {
    await wait(); await h.renderOnce();
    expect(h.captureCharFrame()).toContain("Live pane · following");
    expect(h.captureCharFrame()).toContain("❯ Approve command?");
    expect(h.captureCharFrame()).toContain("Tool output 1");
    await wait(PANE_PREVIEW_REFRESH_MS + 50); await h.renderOnce();
    expect(h.captureCharFrame()).toContain("Tool output 2");
    expect(targets).toEqual(["w1:p1", "w1:p1"]);
    expect(requests).toEqual(["list"]);
    expect(executions).toBe(0);
    h.mockInput.pressKey("y", { ctrl: true });
    await wait(PANE_PREVIEW_REFRESH_MS + 50);
    expect(targets).toHaveLength(2);
  } finally { h.renderer.destroy(); }
});

test("live reads do not overlap, are aborted on selection change, and cannot overwrite saved previews", async () => {
  const h = await createTestRenderer({ width: 140, height: 26 });
  let resolve!: (text: string) => void;
  let signal!: AbortSignal;
  let reads = 0;
  mountPalette(h.renderer, [{ ...live(), session }, savedSessionItem({ ...session, id: "saved-two" })], {
    panePreview: async (_id, readSignal) => { reads++; signal = readSignal; return new Promise(done => { resolve = done; }); },
    sessionJob: async (request, _signal, emit) => {
      if (request.type === "preview") emit({ type: "preview", session: request.session, excerpt: "Saved conversation text" });
    }, run: async () => ({ ok: true, message: "" }), close: () => {},
  });
  try {
    await wait(PANE_PREVIEW_REFRESH_MS + 300);
    expect(reads).toBe(1);
    h.mockInput.pressArrow("down");
    expect(signal.aborted).toBe(true);
    resolve("STALE TERMINAL OUTPUT");
    await wait(); await h.renderOnce();
    expect(h.captureCharFrame()).toContain("Saved conversation text");
    expect(h.captureCharFrame()).not.toContain("STALE TERMINAL OUTPUT");
    expect(h.captureCharFrame()).not.toContain("Live pane");
  } finally { h.renderer.destroy(); }
});

test("failed live reads remove stale output, retry, and never use historical conversation as a live screen", async () => {
  const h = await createTestRenderer({ width: 90, height: 24 });
  let reads = 0, historyReads = 0;
  const palette = mountPalette(h.renderer, [{ ...live(), session }], {
    panePreview: async () => { reads++; if (reads === 2) throw new Error("closed"); return reads === 1 ? "OLD APPROVAL" : "NEW SCREEN"; },
    sessionJob: async request => { if (request.type === "preview") historyReads++; },
    run: async () => ({ ok: true, message: "" }), close: () => {},
  });
  try {
    await wait(); await h.renderOnce(); expect(h.captureCharFrame()).toContain("OLD APPROVAL");
    await wait(PANE_PREVIEW_REFRESH_MS + 50); await h.renderOnce();
    expect(h.captureCharFrame()).toContain("Live pane unavailable");
    expect(h.captureCharFrame()).not.toContain("OLD APPROVAL");
    await wait(PANE_PREVIEW_REFRESH_MS + 50); await h.renderOnce();
    expect(h.captureCharFrame()).toContain("NEW SCREEN");
    expect(historyReads).toBe(0);
    palette.updateItems([]);
    await wait(PANE_PREVIEW_REFRESH_MS + 50);
    expect(reads).toBe(3);
  } finally { h.renderer.destroy(); }
});

test("live preview scrolls upward from the prompt and content search switches to matching context", async () => {
  const h = await createTestRenderer({ width: 90, height: 24 });
  let lastSignal!: AbortSignal;
  mountPalette(h.renderer, [{ ...live(), session }], {
    panePreview: async (_id, signal) => { lastSignal = signal; return Array.from({ length: 30 }, (_, i) => `Screen row ${i}`).join("\n") + "\n❯ Waiting for approval"; },
    sessionJob: async (request, _signal, emit) => {
      if (request.type === "search") emit({ type: "hit", session, excerpt: "needle in saved history" });
    }, run: async () => ({ ok: true, message: "" }), close: () => {},
  });
  try {
    await wait(); await h.renderOnce(); expect(h.captureCharFrame()).toContain("❯ Waiting for approval");
    h.mockInput.pressKey("\x1b[5~"); await h.renderOnce();
    expect(h.captureCharFrame()).not.toContain("❯ Waiting for approval");
    expect(h.captureCharFrame()).toContain("Live pane · scrolled");
    h.mockInput.pressKey("\x1b[6~"); await h.renderOnce();
    expect(h.captureCharFrame()).toContain("❯ Waiting for approval");
    await h.mockInput.typeText("needle");
    h.mockInput.pressKey("f", { ctrl: true }); await wait(); await h.renderOnce();
    expect(lastSignal.aborted).toBe(true);
    expect(h.captureCharFrame()).toContain("Transcript context");
    expect(h.captureCharFrame()).toContain("needle in saved history");
    expect(h.captureCharFrame()).not.toContain("Live pane");
    h.mockInput.pressEscape(); await wait(300); await h.renderOnce();
    expect(h.captureCharFrame()).toContain("❯ Waiting for approval");
  } finally { h.renderer.destroy(); }
});

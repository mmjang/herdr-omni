import { expect, test } from "bun:test";
import { createTestRenderer } from "@opentui/core/testing";
import { mountPalette } from "../src/palette";
import { savedSessionItem, type SessionEvent } from "../src/sessions";
import type { SavedSession } from "../src/types";

const session: SavedSession = { provider: "claude", id: "one", title: "Preview example", cwd: "/project/shop", updatedAt: 1 };
const wait = () => new Promise(resolve => setTimeout(resolve, 240));

for (const width of [80, 140]) {
  test(`recent conversation preview at ${width} columns keeps navigation usable`, async () => {
    const h = await createTestRenderer({ width, height: 26 });
    let executions = 0;
    mountPalette(h.renderer, [savedSessionItem(session)], {
      sessionJob: async (request, _signal, emit) => {
        if (request.type === "preview") emit({ type: "preview", session: request.session,
          excerpt: "User: Fix checkout totals\n\nAssistant: Located the tax rounding issue\n\n" + Array.from({ length: 30 }, (_, i) => `Detail line ${i}`).join("\n") });
      }, run: async () => { executions++; return { ok: true, message: "" }; }, close: () => {},
    });
    try {
      await wait(); await h.renderOnce();
      let frame = h.captureCharFrame();
      expect(frame).toContain("User: Fix checkout totals");
      expect(frame).toContain("Assistant: Located");
      expect(frame).toContain("enter/click");
      const rows = frame.split("\n");
      const heading = rows.findIndex(row => row.includes("Recent conversation"));
      const result = rows.findIndex(row => row.includes("Preview example"));
      expect(width >= 120 ? heading <= result : heading > result).toBe(true);
      h.mockInput.pressKey("o", { ctrl: true }); await h.renderOnce();
      expect(h.captureCharFrame()).toContain("Collapse");
      h.mockInput.pressKey("\x1b[6~"); await h.renderOnce();
      expect(h.captureCharFrame()).not.toContain("User: Fix checkout totals");
      h.mockInput.pressKey("y", { ctrl: true }); await h.renderOnce();
      expect(h.captureCharFrame()).not.toContain("Recent conversation");
      expect(executions).toBe(0);
      h.mockInput.pressEnter(); await wait();
      expect(executions).toBe(1);
    } finally { h.renderer.destroy(); }
  });
}

test("selection cancels previews, ignores stale responses, and reuses cached content", async () => {
  const h = await createTestRenderer({ width: 140, height: 26 });
  const calls: Array<{ session: SavedSession; signal: AbortSignal; emit: (event: SessionEvent) => void }> = [];
  mountPalette(h.renderer, [savedSessionItem(session), savedSessionItem({ ...session, id: "two", title: "Second example" })], {
    sessionJob: async (request, signal, emit) => { if (request.type === "preview") calls.push({ session: request.session, signal, emit }); },
    run: async () => ({ ok: true, message: "" }), close: () => {},
  });
  try {
    await wait();
    h.mockInput.pressArrow("down"); await wait();
    expect(calls[0]!.signal.aborted).toBe(true);
    calls[0]!.emit({ type: "preview", session: calls[0]!.session, excerpt: "STALE CONTENT" });
    calls[1]!.emit({ type: "preview", session: calls[1]!.session, excerpt: "Current conversation" });
    await h.renderOnce();
    expect(h.captureCharFrame()).not.toContain("STALE CONTENT");
    expect(h.captureCharFrame()).toContain("Current conversation");
    h.mockInput.pressKey("y", { ctrl: true });
    h.mockInput.pressKey("y", { ctrl: true }); await wait();
    expect(calls).toHaveLength(2);
    await h.renderOnce(); expect(h.captureCharFrame()).toContain("Current conversation");
  } finally { h.renderer.destroy(); }
});

test("mouse preview button selects another conversation without opening it", async () => {
  const h = await createTestRenderer({ width: 140, height: 26 });
  let executions = 0;
  mountPalette(h.renderer, [savedSessionItem(session), savedSessionItem({ ...session, id: "two", title: "Second example" })], {
    sessionJob: async (request, _signal, emit) => {
      if (request.type === "preview") emit({ type: "preview", session: request.session, excerpt: `Conversation ${request.session.id}` });
    }, run: async () => { executions++; return { ok: true, message: "" }; }, close: () => {},
  });
  try {
    await wait(); await h.renderOnce();
    const rows = h.captureCharFrame().split("\n");
    const y = rows.findIndex(row => row.includes("Second example"));
    const x = rows[y]!.indexOf("◧");
    expect(x).toBeGreaterThan(-1);
    await h.mockMouse.click(x, y); await wait(); await h.renderOnce();
    expect(executions).toBe(0);
    expect(h.captureCharFrame()).toContain("Conversation two");
  } finally { h.renderer.destroy(); }
});

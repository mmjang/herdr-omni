import { expect, test } from "bun:test";
import { createTestRenderer } from "@opentui/core/testing";
import { mountPalette } from "../src/palette";
import { savedSessionItem, TRANSCRIPT_DEBOUNCE_MS, type SessionJob, type SessionEvent } from "../src/sessions";
import type { SavedSession } from "../src/types";

const session: SavedSession = { provider: "claude", id: "abc-123", title: "Old investigation", cwd: "/repo/shop", updatedAt: 1 };
const wait = (ms = 0) => new Promise(resolve => setTimeout(resolve, ms));

test("opening the palette loads saved metadata newest first without searching transcripts", async () => {
  const h = await createTestRenderer({ width: 110, height: 20 });
  const requests: string[] = [];
  mountPalette(h.renderer, [], {
    sessionJob: async (request, _signal, publish) => {
      requests.push(request.type);
      if (request.type === "list") publish({ type: "sessions", sessions: [session,
        { ...session, id: "new", title: "Newest session", updatedAt: 100 }] });
    }, run: async () => ({ ok: true, message: "" }), close: () => {},
  });
  try {
    await wait(); await h.renderOnce();
    const frame = h.captureCharFrame();
    expect(requests).toEqual(["list"]);
    expect(frame).toContain("Newest session");
    expect(frame.trimEnd().split("\n").at(-1)).toContain("Ctrl+F · Search session content");
    expect(frame.indexOf("Newest session")).toBeLessThan(frame.indexOf("Old investigation"));
  } finally { h.renderer.destroy(); }
});

for (const query of ["近期功能", "shop", "abc-123", ">近期功能", "@近期功能", ":近期功能"]) {
  test(`saved session metadata search respects scope for ${query} without scanning content`, async () => {
    const h = await createTestRenderer({ width: 110, height: 20 });
    const requests: string[] = [];
    mountPalette(h.renderer, [], {
      sessionJob: async (request, _signal, publish) => {
        requests.push(request.type);
        if (request.type === "list") publish({ type: "sessions", sessions: [{ ...session, title: "讨论近期功能规划" }] });
      }, run: async () => ({ ok: true, message: "" }), close: () => {},
    });
    try {
      await h.mockInput.pasteBracketedText(query); await wait(); await h.renderOnce();
      const excluded = query.startsWith("@") || query.startsWith(":");
      expect(requests).toEqual(["list"]);
      if (excluded) expect(h.captureCharFrame()).not.toContain("讨论近期功能规划");
      else expect(h.captureCharFrame()).toContain("讨论近期功能规划");
      expect(h.captureCharFrame()).not.toContain("Transcript matches");
    } finally { h.renderer.destroy(); }
  });
}

test("missing-directory picker preselects suggestion, supports choosing another workspace and cancelling", async () => {
  const h = await createTestRenderer({ width: 110, height: 20 });
  const calls: any[] = [];
  mountPalette(h.renderer, [savedSessionItem(session)], {
    run: async item => {
      calls.push(item.invocation);
      return { ok: false, message: "Original directory unavailable.", workspaceChoices: [
        { id: "w2", label: "Shop", cwd: "/repo/shop", reason: "Matching project folder" },
        { id: "w1", label: "Current", cwd: "/repo/current", reason: "Current workspace" },
      ] };
    }, close: () => {},
  });
  try {
    await h.mockInput.typeText(">Old"); h.mockInput.pressEnter(); await wait(); await h.renderOnce();
    expect(h.captureCharFrame()).toContain("Resume in workspace…");
    expect(h.captureCharFrame()).toContain("› Shop");
    expect(h.captureCharFrame()).toContain("/repo/shop");
    h.mockInput.pressEscape(); await wait(30); await h.renderOnce();
    expect(h.captureCharFrame()).not.toContain("Resume in workspace…");
    expect(calls).toHaveLength(1);
    h.mockInput.pressEnter(); await wait();
    h.mockInput.pressArrow("down"); h.mockInput.pressEnter(); await wait();
    expect(calls[2].destination).toEqual({ id: "w1", cwd: "/repo/current" });
  } finally { h.renderer.destroy(); }
});

test("resume shows elapsed waiting state, blocks duplicate input, and recovers after failure", async () => {
  const h = await createTestRenderer({ width: 100, height: 20 });
  let finish!: (result: { ok: boolean; message: string }) => void;
  let calls = 0, closes = 0;
  mountPalette(h.renderer, [savedSessionItem(session)], {
    run: () => { calls++; return new Promise(resolve => { finish = resolve; }); },
    close: () => { closes++; },
  });
  try {
    await h.mockInput.typeText(">Old");
    h.mockInput.pressEnter(); await wait(); await h.renderOnce();
    expect(h.captureCharFrame()).toContain("Resuming session… (0s)");
    expect(h.captureCharFrame()).toContain("Please wait");
    h.mockInput.pressEnter(); h.mockInput.pressEscape();
    await h.mockInput.typeText("ignored");
    await wait(1100); await h.renderOnce();
    expect(h.captureCharFrame()).toContain("Resuming session… (1s)");
    expect(calls).toBe(1);
    expect(closes).toBe(0);
    finish({ ok: false, message: "Could not resume" });
    await wait(); await h.renderOnce();
    expect(h.captureCharFrame()).not.toContain("Resuming session");
    expect(h.captureCharFrame()).toContain("Could not resume");
    expect(h.captureCharFrame()).toContain(">Old");
    h.mockInput.pressEscape(); await wait(30);
    expect(closes).toBe(1);
  } finally { h.renderer.destroy(); }
});

test("transcripts are opt-in, progressive, deduplicated, and selection remains on metadata matches", async () => {
  const h = await createTestRenderer({ width: 110, height: 20 });
  const normal = savedSessionItem({ ...session, id: "live-id", title: "Payment work" });
  normal.id = "live:agent:w1:p1"; normal.savedSession = false; normal.agentStatus = "idle";
  const calls: string[] = [];
  let publish!: (event: SessionEvent) => void;
  const job: SessionJob = async (request, _signal, emit) => {
    calls.push(request.type);
    if (request.type === "list") { emit({ type: "sessions", sessions: [session] }); emit({ type: "done" }); }
    else publish = emit;
  };
  mountPalette(h.renderer, [normal], { sessionJob: job, run: async item => { calls.push(item.id); return { ok: true, message: "" }; }, close: () => {} });
  try {
    await h.mockInput.typeText(">payment"); await wait(); await h.renderOnce();
    expect(calls).toEqual(["list"]);
    expect(h.captureCharFrame()).toContain("Payment work");
    expect(h.captureCharFrame()).toContain("Ctrl+F · Search session content");
    expect(h.captureCharFrame()).not.toContain("Old investigation");
    h.mockInput.pressKey("f", { ctrl: true });
    await wait(TRANSCRIPT_DEBOUNCE_MS + 20);
    expect(calls).toEqual(["list", "search"]);
    publish({ type: "hit", session, excerpt: "payment callback timed out" });
    publish({ type: "hit", session: normal.session!, excerpt: "payment details" });
    await h.renderOnce();
    const frame = h.captureCharFrame();
    expect(frame).toContain("Transcript matches");
    expect(frame).toContain("Old investigation");
    expect(frame.match(/Payment work/g)).toHaveLength(1);
    h.mockInput.pressEnter(); await wait();
    expect(calls.at(-1)).toBe(normal.id);
  } finally { h.renderer.destroy(); }
});

test("query changes, escaping, and closing cancel scans and ignore stale hits", async () => {
  const h = await createTestRenderer({ width: 100, height: 20 });
  const scans: Array<{ signal: AbortSignal; publish: (event: SessionEvent) => void }> = [];
  mountPalette(h.renderer, [], { sessionJob: async (request, signal, publish) => {
    if (request.type === "list") publish({ type: "sessions", sessions: [session] });
    else scans.push({ signal, publish });
  }, run: async () => ({ ok: true, message: "" }), close: () => {} });
  try {
    await h.mockInput.typeText(">payment"); await wait();
    h.mockInput.pressKey("f", { ctrl: true }); await wait(TRANSCRIPT_DEBOUNCE_MS + 20);
    await h.mockInput.typeText(" new");
    expect(scans[0]!.signal.aborted).toBe(true);
    scans[0]!.publish({ type: "hit", session, excerpt: "stale hit" });
    await h.renderOnce(); expect(h.captureCharFrame()).not.toContain("Old investigation");
    await wait(TRANSCRIPT_DEBOUNCE_MS + 20);
    scans[1]!.publish({ type: "hit", session, excerpt: "payment new match" });
    await h.renderOnce(); expect(h.captureCharFrame()).toContain("payment new match");
    h.mockInput.pressEscape(); await wait(30);
    expect(scans[1]!.signal.aborted).toBe(true);
    await h.renderOnce(); expect(h.captureCharFrame()).not.toContain("Old investigation");
    h.mockInput.pressKey("f", { ctrl: true }); await wait(TRANSCRIPT_DEBOUNCE_MS + 20);
    h.renderer.destroy();
    expect(scans[2]!.signal.aborted).toBe(true);
    scans[2]!.publish({ type: "hit", session, excerpt: "too late" });
  } finally { h.renderer.destroy(); }
});

for (const query of ["payment", ">payment", "@payment", ":payment", "支付回调🙂"]) {
  test(`Ctrl+F explores transcripts for ${query} without changing the query`, async () => {
    const h = await createTestRenderer({ width: 100, height: 20 });
    const requests: string[] = [];
    let closes = 0;
    mountPalette(h.renderer, [], { sessionJob: async (request, _signal, publish) => {
      if (request.type === "list") publish({ type: "sessions", sessions: [session] });
      else { requests.push(request.query); publish({ type: "hit", session, excerpt: "matching excerpt" }); }
    }, run: async () => ({ ok: true, message: "" }), close: () => { closes++; } });
    try {
      await h.mockInput.pasteBracketedText(query); await wait();
      expect(requests).toEqual([]);
      h.mockInput.pressKey("f", { ctrl: true }); await wait(TRANSCRIPT_DEBOUNCE_MS + 20);
      expect(requests).toEqual([query.replace(/^[>@:]/, "")]);
      await h.renderOnce();
      expect(h.captureCharFrame()).toContain(query);
      expect(h.captureCharFrame()).toContain("Transcript matches");
      expect(h.captureCharFrame()).toContain("matching excerpt");
      h.mockInput.pressKey("f", { ctrl: true }); await wait(TRANSCRIPT_DEBOUNCE_MS + 20);
      expect(requests).toHaveLength(1);
      h.mockInput.pressEscape(); await wait(30); await h.renderOnce();
      expect(closes).toBe(0);
      expect(h.captureCharFrame()).not.toContain("Transcript matches");
      expect(h.captureCharFrame()).toContain("Ctrl+F · Search session content");
      h.mockInput.pressEscape(); await wait(30);
      expect(closes).toBe(1);
    } finally { h.renderer.destroy(); }
  });
}

test("arrows only move the cursor and empty Ctrl+F never triggers scanning", async () => {
  const h = await createTestRenderer({ width: 100, height: 20 });
  const requests: string[] = [];
  mountPalette(h.renderer, [], { sessionJob: async request => { requests.push(request.type); }, run: async () => ({ ok: true, message: "" }), close: () => {} });
  try {
    h.mockInput.pressKey("f", { ctrl: true }); await wait();
    expect(requests).toEqual(["list"]);
    await h.mockInput.typeText("payment");
    h.mockInput.pressArrow("right");
    h.mockInput.pressArrow("left"); h.mockInput.pressArrow("left");
    h.mockInput.pressArrow("right");
    await wait(TRANSCRIPT_DEBOUNCE_MS + 20);
    expect(requests).toEqual(["list"]);
    await h.mockInput.typeText("X"); await h.renderOnce();
    expect(h.captureCharFrame()).toContain("paymenXt");
  } finally { h.renderer.destroy(); }
});

for (const location of ["empty", "footer"] as const) {
  test(`clicking the ${location} content-search link starts one scan`, async () => {
    const h = await createTestRenderer({ width: 110, height: 20 });
    const requests: string[] = [];
    mountPalette(h.renderer, [], { sessionJob: async request => {
      if (request.type === "search") requests.push(request.query);
    }, run: async () => ({ ok: true, message: "" }), close: () => {} });
    try {
      await h.mockInput.typeText("missing phrase"); await wait(); await h.renderOnce();
      const rows = h.captureCharFrame().split("\n");
      const y = location === "empty"
        ? rows.findIndex(row => row.includes("Ctrl+F or click"))
        : rows.findIndex(row => row.includes("Ctrl+F · Search"));
      expect(y).toBeGreaterThan(-1);
      const x = rows[y]!.indexOf("Search session content") + 2;
      await h.mockMouse.click(x, y, 2);
      expect(requests).toEqual([]);
      await h.mockMouse.click(x, y);
      await wait(TRANSCRIPT_DEBOUNCE_MS + 20);
      expect(requests).toEqual(["missing phrase"]);
    } finally { h.renderer.destroy(); }
  });
}

test("selected transcript shows multi-line context while keeping results and footer visible", async () => {
  const h = await createTestRenderer({ width: 70, height: 20 });
  mountPalette(h.renderer, [], { sessionJob: async (request, _signal, publish) => {
    if (request.type === "list") publish({ type: "sessions", sessions: [session] });
    else publish({ type: "hit", session, excerpt: "Before context explains the investigation.\n\nThe needle is here in the matching sentence.\n\nAfter context explains the resolution." });
  }, run: async () => ({ ok: true, message: "" }), close: () => {} });
  try {
    await h.mockInput.typeText("needle"); h.mockInput.pressKey("f", { ctrl: true });
    await wait(TRANSCRIPT_DEBOUNCE_MS + 30); await h.renderOnce();
    const frame = h.captureCharFrame();
    expect(frame).toContain("Old investigation");
    expect(frame).toContain("Transcript context");
    expect(frame).toContain("Before context");
    expect(frame).toContain("The needle is here");
    expect(frame).toContain("After context");
    expect(frame.trimEnd().split("\n").at(-1)).toContain("enter/click");
  } finally { h.renderer.destroy(); }
});

test("fallback resume opens a confirmation prompt; Esc cancels and yes targets the confirmed workspace", async () => {
  const h = await createTestRenderer({ width: 100, height: 20 });
  const requests: Array<{ fallback?: string; input?: string }> = [];
  let closes = 0;
  mountPalette(h.renderer, [savedSessionItem(session)], {
    run: async (item, input) => {
      if (item.invocation.kind !== "resume-session") throw new Error("wrong action");
      requests.push({ fallback: item.invocation.fallbackWorkspaceId, input });
      if (item.invocation.fallbackWorkspaceId && input === "yes") return { ok: true, message: "" };
      return { ok: false, message: "No matching workspace. Resume in Current?", confirmWorkspace: { id: "w1", label: "Current" } };
    }, close: () => { closes++; },
  });
  try {
    await h.mockInput.typeText(">Old"); h.mockInput.pressEnter(); await wait(); await h.renderOnce();
    expect(h.captureCharFrame()).toContain("Resume in current workspace?");
    expect(h.captureCharFrame()).toContain('Type "yes"');
    expect(closes).toBe(0);
    expect(requests).toEqual([{ fallback: undefined, input: undefined }]);
    h.mockInput.pressEscape(); await wait(30); await h.renderOnce();
    expect(h.captureCharFrame()).not.toContain("Resume in current workspace?");
    expect(requests).toHaveLength(1);
    h.mockInput.pressEnter(); await wait();
    await h.mockInput.typeText("yes"); h.mockInput.pressEnter(); await wait();
    expect(requests.at(-1)).toEqual({ fallback: "w1", input: "yes" });
    expect(closes).toBe(1);
  } finally { h.renderer.destroy(); }
});

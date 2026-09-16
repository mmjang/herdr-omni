import { createTestRenderer } from "@opentui/core/testing";
import { expect, test } from "bun:test";
import { filterPaletteItems, mountPalette } from "../src/palette";
import { fallbackTheme } from "../src/theme";
import type { CommandResult, PaletteItem } from "../src/types";

/**
 * A palette deliberately unlike both the built-in fallback and anything a real Herdr config
 * would resolve to, so the assertions below can only pass when the mounted theme is honored.
 */
const theme = { background: "#29284f", panel: "#3c3b68", text: "#e9e8ff", muted: "#a7a4df", accent: "#ffe11a", shortcut: "#8be9fd", footer: "#1d1c3a", footerText: "#8f8cd0" };

const item = (id: string, title: string, invocation: PaletteItem["invocation"], prompt?: PaletteItem["prompt"]): PaletteItem =>
  ({ id, title, category: "Panes", description: "Does the thing", icon: "▯", aliases: [], shortcuts: ["ctrl+a+z"], invocation, ...(prompt ? { prompt } : {}) });

const items = [
  item("zoom", "Zoom pane", { kind: "herdr", argv: ["pane", "zoom", "--current"] }),
  item("rename_pane", "Rename pane", { kind: "resolve", action: "rename-pane" }, { placeholder: "New name" }),
  item("settings", "Settings", { kind: "shortcut" }),
];

test("scopes a leading greater-than search to live agents", () => {
  const agent = { ...item("live:agent:w1:p1", "Review changes", { kind: "herdr", argv: ["agent", "focus", "w1:p1"] }), category: "Agents" as const };
  const nextAgent = { ...item("next_agent", "Next agent", { kind: "resolve", action: "focus-agent", step: 1 }), category: "Agents" as const };
  const tab = { ...item("live:tab:w1:t1", "review → dev", { kind: "herdr", argv: ["tab", "focus", "w1:t1"] }), category: "Tabs" as const };

  expect(filterPaletteItems([agent, nextAgent, tab], ">")).toEqual([agent]);
  expect(filterPaletteItems([agent, nextAgent, tab], ">review")).toEqual([agent]);
  expect(filterPaletteItems([agent, nextAgent, tab], ">missing")).toEqual([]);
});

test("searches the complete agent-session and workspace label", () => {
  const agent = { ...item("live:agent:w1:p1", "Review checkout flow - ordering-service", { kind: "herdr", argv: ["agent", "focus", "w1:p1"] }), category: "Agents" as const };

  expect(filterPaletteItems([agent], ">review checkout ordering")).toEqual([agent]);
  expect(filterPaletteItems([agent], ">ordering-service")).toEqual([agent]);
});

async function palette(result: CommandResult) {
  const harness = await createTestRenderer({ width: 60, height: 14 });
  const ran: Array<{ id: string; input?: string } | "closed"> = [];
  mountPalette(harness.renderer, items, {
    theme,
    run: async (selected, input) => {
      ran.push(input === undefined ? { id: selected.id } : { id: selected.id, input });
      return result;
    },
    close: () => ran.push("closed"),
  });
  return { ...harness, ran };
}

const hex = (color: { r: number; g: number; b: number }) =>
  "#" + [color.r, color.g, color.b].map(value => Math.round(value <= 1 ? value * 255 : value).toString(16).padStart(2, "0")).join("");

/** Enter runs the command asynchronously, so let its result land before rendering. */
const settle = () => new Promise(resolve => setTimeout(resolve, 0));
/** Escape is ambiguous until OpenTUI's stdin parser timeout (~20ms) flushes it. */
const settleEscape = () => new Promise(resolve => setTimeout(resolve, 30));
const rowsOf = (frame: string) => frame.split("\n").filter((row, index, all) => index < all.length - 1 || row !== "");

test("clicking a result label or shortcut runs that row rather than the keyboard selection", async () => {
  for (const x of [8, 50]) {
    const harness = await palette({ ok: true, message: "" });
    try {
      await harness.renderOnce();
      const y = rowsOf(harness.captureCharFrame()).findIndex(row => row.includes("Settings"));
      await harness.mockMouse.click(x, y);
      await settle();
      expect(harness.ran).toEqual([{ id: "settings" }, "closed"]);
    } finally { harness.renderer.destroy(); }
  }
});

test("clicking a prompted action opens its prompt without executing it", async () => {
  const harness = await palette({ ok: true, message: "" });
  try {
    await harness.renderOnce();
    const y = rowsOf(harness.captureCharFrame()).findIndex(row => row.includes("Rename pane"));
    await harness.mockMouse.click(8, y);
    await harness.renderOnce();
    expect(harness.ran).toEqual([]);
    expect(harness.captureCharFrame()).toContain("New name");
    await harness.mockInput.typeText("renamed");
    harness.mockInput.pressEnter();
    await settle();
    expect(harness.ran).toEqual([{ id: "rename_pane", input: "renamed" }, "closed"]);
  } finally { harness.renderer.destroy(); }
});

test("repeated clicks while an action is running execute only once", async () => {
  const harness = await createTestRenderer({ width: 60, height: 14 });
  let calls = 0;
  let finish!: (result: CommandResult) => void;
  const pending = new Promise<CommandResult>(resolve => { finish = resolve; });
  mountPalette(harness.renderer, items, { run: () => { calls++; return pending; }, close: () => {} });
  try {
    await harness.renderOnce();
    const y = rowsOf(harness.captureCharFrame()).findIndex(row => row.includes("Zoom pane"));
    await harness.mockMouse.click(8, y);
    await harness.mockMouse.click(8, y);
    expect(calls).toBe(1);
  } finally {
    finish({ ok: true, message: "" });
    await settle();
    harness.renderer.destroy();
  }
});

test("headers, right-clicks and drag gestures do not activate results", async () => {
  const harness = await palette({ ok: true, message: "" });
  try {
    await harness.renderOnce();
    const rows = rowsOf(harness.captureCharFrame());
    const y = rows.findIndex(row => row.includes("Zoom pane"));
    await harness.mockMouse.click(8, y - 1);
    await harness.mockMouse.click(8, y, 2);
    await harness.mockMouse.drag(8, y, 15, y);
    expect(harness.ran).toEqual([]);
  } finally { harness.renderer.destroy(); }
});

test("live refresh preserves the selected identity as agents reorder", async () => {
  const harness = await createTestRenderer({ width: 80, height: 18 });
  const ran: string[] = [];
  const a = { ...item("live:agent:a", "Review alpha", { kind: "herdr", argv: [] }), priority: 0, agentStatus: "blocked" as const };
  const b = { ...item("live:agent:b", "Review beta", { kind: "herdr", argv: [] }), priority: 2, agentStatus: "working" as const };
  const controller = mountPalette(harness.renderer, [a, b], { run: async entry => { ran.push(entry.id); return { ok: false, message: "test" }; }, close: () => {} });
  await harness.mockInput.typeText(">review");
  controller.updateItems([{ ...b, priority: 0, agentStatus: "blocked" }, { ...a, priority: 3, agentStatus: "idle" }]);
  await harness.renderOnce();
  expect(harness.captureCharFrame()).toContain("[idle]");
  harness.mockInput.pressEnter();
  await settle();
  expect(ran).toEqual([a.id]);
  harness.renderer.destroy();
});

test("highlights fuzzy-matched title characters with the theme accent", async () => {
  const harness = await createTestRenderer({ width: 80, height: 14 });
  mountPalette(harness.renderer, [item("ordering", "ordering-service", { kind: "shortcut" })], { theme, run: async () => ({ ok: true, message: "" }), close: () => {} });
  await harness.mockInput.typeText("ordsvc");
  await harness.renderOnce();
  const line = harness.captureSpans().lines.find(line => line.spans.map(span => span.text).join("").includes("ordering-service"))!;
  const highlighted = line.spans.filter(span => hex(span.fg) === theme.accent).map(span => span.text).join("");
  expect(highlighted).toContain("ordsvc");
  harness.renderer.destroy();
});

test("refresh leaves an in-progress rename prompt intact", async () => {
  const harness = await createTestRenderer({ width: 80, height: 14 });
  const ran: string[] = [];
  const controller = mountPalette(harness.renderer, items, { run: async (_entry, input) => { ran.push(input ?? ""); return { ok: true, message: "" }; }, close: () => {} });
  await harness.mockInput.typeText("rename");
  harness.mockInput.pressEnter();
  await settle();
  await harness.mockInput.typeText("my logs");
  controller.updateItems([...items, item("new", "New arrival", { kind: "shortcut" })]);
  harness.mockInput.pressEnter();
  await settle();
  expect(ran).toEqual(["my logs"]);
  harness.renderer.destroy();
});

test("shows each live agent status beside its session title", async () => {
  const harness = await createTestRenderer({ width: 80, height: 18 });
  const statuses = ["blocked", "done", "working", "idle", "unknown"] as const;
  const agents = statuses.map(status => ({
    ...item(`live:agent:${status}`, `Review - project`, { kind: "herdr", argv: [] }),
    category: "Agents" as const, agentStatus: status, shortcuts: [],
  }));
  mountPalette(harness.renderer, agents, { run: async () => ({ ok: true, message: "" }), close: () => {} });
  await harness.renderOnce();
  const frame = harness.captureCharFrame();
  for (const status of statuses) expect(frame).toContain(`[${status}]`);
  expect(frame).toContain("Review - project");
});

test("renders repeated categories when recent results cross category boundaries", async () => {
  const harness = await createTestRenderer({ width: 70, height: 18 });
  const mixed = [
    { ...item("first", "First", { kind: "shortcut" }), category: "Tabs" as const },
    { ...item("second", "Second", { kind: "shortcut" }), category: "Workspace" as const },
    { ...item("third", "Third", { kind: "shortcut" }), category: "Tabs" as const },
  ];
  mountPalette(harness.renderer, mixed, { history: { first: 3, second: 2, third: 1 }, run: async () => ({ ok: false, message: "" }), close: () => {} });
  await harness.renderOnce();
  const frame = harness.captureCharFrame();
  expect(frame.indexOf("First")).toBeLessThan(frame.indexOf("Second"));
  expect(frame.indexOf("Second")).toBeLessThan(frame.indexOf("Third"));
});

test("paints the palette background across the whole popup", async () => {
  const { renderer, mockInput, renderOnce, captureSpans } = await palette({ ok: true, message: "" });

  await mockInput.typeText("settings");
  await renderOnce();

  const frame = captureSpans();
  expect(frame.lines).toHaveLength(renderer.height);
  for (const line of frame.lines.slice(0, -1)) expect(line.spans.map(span => hex(span.bg))).toContain(theme.background);
});

test("pins the footer to the bottom of the popup", async () => {
  const { mockInput, renderOnce, captureCharFrame } = await palette({ ok: true, message: "" });

  await mockInput.typeText("settings");
  await renderOnce();

  const rows = rowsOf(captureCharFrame());
  expect(rows).toHaveLength(14);
  expect(rows.at(-1)).toContain("1 results");
});

test("sets the footer apart as a full-width bar", async () => {
  const { renderer, mockInput, renderOnce, captureSpans } = await palette({ ok: true, message: "" });

  await mockInput.typeText("settings");
  await renderOnce();

  const footer = captureSpans().lines.at(-1)!;
  expect(footer.spans.map(span => hex(span.bg))).toEqual(footer.spans.map(() => theme.footer));
  expect(footer.spans.reduce((width, span) => width + span.text.length, 0)).toBe(renderer.width);
  expect(footer.spans.filter(span => hex(span.fg) === theme.accent).map(span => span.text)).toEqual(["enter/click", "↑/↓"]);
});

test("closes the palette once a Herdr command succeeds", async () => {
  const { mockInput, renderOnce, ran } = await palette({ ok: true, message: "" });

  await mockInput.typeText("zoom");
  mockInput.pressEnter();
  await settle();
  await renderOnce();

  expect(ran).toEqual([{ id: "zoom" }, "closed"]);
});

test("asks for input before running a prompted command", async () => {
  const { mockInput, renderOnce, captureCharFrame, ran } = await palette({ ok: true, message: "" });

  await mockInput.typeText("rename");
  mockInput.pressEnter();
  await settle();
  await renderOnce();

  expect(captureCharFrame()).toContain("Rename pane");
  expect(captureCharFrame()).toContain("New name");
  expect(ran).toEqual([]);

  await mockInput.typeText("logs");
  mockInput.pressEnter();
  await settle();
  await renderOnce();

  expect(ran).toEqual([{ id: "rename_pane", input: "logs" }, "closed"]);
});

test("returns to search when escaping a prompt", async () => {
  const { mockInput, renderOnce, captureCharFrame, ran } = await palette({ ok: true, message: "" });

  await mockInput.typeText("rename");
  mockInput.pressEnter();
  await settle();
  mockInput.pressEscape();
  await settleEscape();
  await renderOnce();

  expect(captureCharFrame()).toContain("Herdr");
  expect(captureCharFrame()).toContain("1 results");
  expect(ran).toEqual([]);
});

test("reports why a command did not run instead of ignoring enter", async () => {
  const { mockInput, renderOnce, captureCharFrame } = await palette({ ok: false, message: "Press ctrl+a+z — Herdr only runs this one from the keyboard." });

  await mockInput.typeText("settings");
  mockInput.pressEnter();
  await settle();
  await renderOnce();

  const rows = rowsOf(captureCharFrame());
  expect(rows).toHaveLength(14);
  expect(rows.at(-2)).toContain("Press ctrl+a+z — Herdr only runs this one");
  expect(rows.at(-1)).toContain("1 results");
});

test("stays usable when running a command throws", async () => {
  const { renderer, mockInput, renderOnce, captureCharFrame, captureSpans } = await createTestRenderer({ width: 60, height: 14 });
  mountPalette(renderer, items, { run: async () => { throw new Error("herdr is not on PATH"); }, close: () => {} });

  await mockInput.typeText("zoom");
  mockInput.pressEnter();
  await settle();
  mockInput.pressEnter();
  await settle();
  await renderOnce();

  // No theme injected here on purpose, so this also covers the built-in fallback palette.
  const footer = captureSpans().lines.at(-1)!;
  expect(footer.spans.map(span => hex(span.bg))).toEqual(footer.spans.map(() => fallbackTheme.footer));
  expect(captureCharFrame()).toContain("herdr is not on PATH");
});

test("explains an empty result set", async () => {
  const { mockInput, renderOnce, captureCharFrame } = await palette({ ok: true, message: "" });

  await mockInput.typeText("nowhere");
  await renderOnce();

  expect(captureCharFrame()).toContain("No results match your search.");
});

import { expect, test } from "bun:test";
import { createTestRenderer } from "@opentui/core/testing";
import { mountPalette } from "../src/palette";
import type { CommandResult, PaletteItem } from "../src/types";

const offer = { version: "9.0.0", ref: "v9.0.0" };
const item: PaletteItem = { id: "agent", title: "Review code", category: "Agents", description: "", icon: "◇", aliases: [], shortcuts: [], invocation: { kind: "herdr", argv: ["agent", "focus", "w1:p1"] } };
const settle = () => new Promise(resolve => setTimeout(resolve, 0));

test("an update offer defaults to Later and Enter does not install or navigate", async () => {
  const h = await createTestRenderer({ width: 80, height: 16 });
  const calls: string[] = [];
  const palette = mountPalette(h.renderer, [item], {
    run: async () => { calls.push("navigate"); return { ok: true, message: "" }; },
    close: () => calls.push("close"),
    update: async () => { calls.push("update"); return { ok: true, message: "" }; },
    dismissUpdate: () => calls.push("later"),
  });
  try {
    palette.offerUpdate(offer);
    await h.renderOnce();
    expect(h.captureCharFrame()).toContain("[Later]");
    h.mockInput.pressEnter();
    await settle();
    await h.renderOnce();
    expect(calls).toEqual(["later"]);
    expect(h.captureCharFrame()).toContain("Review code");
  } finally { h.renderer.destroy(); }
});

test("a late update check preserves search input and opens on Ctrl+U", async () => {
  const h = await createTestRenderer({ width: 80, height: 16 });
  const palette = mountPalette(h.renderer, [item], { run: async () => ({ ok: true, message: "" }), close: () => {}, update: async () => ({ ok: true, message: "" }) });
  try {
    await h.mockInput.typeText("review");
    palette.offerUpdate(offer);
    await h.renderOnce();
    expect(h.captureCharFrame()).toContain("ctrl+u to review update");
    expect(h.captureCharFrame()).toContain("review");
    expect(h.captureCharFrame()).not.toContain("[Later]");
    h.mockInput.pressKey("u", { ctrl: true });
    await h.renderOnce();
    expect(h.captureCharFrame()).toContain("[Later]");
    h.mockInput.pressEscape();
    await new Promise(resolve => setTimeout(resolve, 30));
    await h.renderOnce();
    expect(h.captureCharFrame()).toContain("review");
    expect(h.captureCharFrame()).toContain("Review code");
  } finally { h.renderer.destroy(); }
});

test("confirmed updates run once, block closing during installation, and explain reopening", async () => {
  const h = await createTestRenderer({ width: 80, height: 16 });
  let finish!: (result: CommandResult) => void;
  const pending = new Promise<CommandResult>(resolve => { finish = resolve; });
  let updates = 0, closes = 0;
  const palette = mountPalette(h.renderer, [item], { run: async () => ({ ok: true, message: "" }), close: () => { closes++; }, update: async value => { expect(value).toEqual(offer); updates++; return pending; } });
  try {
    palette.offerUpdate(offer);
    h.mockInput.pressTab();
    h.mockInput.pressEnter();
    h.mockInput.pressEnter();
    h.mockInput.pressEscape();
    await new Promise(resolve => setTimeout(resolve, 30));
    await h.renderOnce();
    expect(updates).toBe(1);
    expect(closes).toBe(0);
    expect(h.captureCharFrame()).toContain("Installing update");
    finish({ ok: true, message: "" });
    await settle();
    await h.renderOnce();
    expect(h.captureCharFrame()).toContain("Reopen Omni");
    h.mockInput.pressEnter();
    expect(closes).toBe(1);
  } finally { h.renderer.destroy(); }
});

test("failed updates offer retry and leave normal navigation available after Later", async () => {
  const h = await createTestRenderer({ width: 80, height: 16 });
  const palette = mountPalette(h.renderer, [item], { run: async () => ({ ok: true, message: "" }), close: () => {}, update: async () => ({ ok: false, message: "Network unavailable" }) });
  try {
    palette.offerUpdate(offer);
    h.mockInput.pressTab();
    h.mockInput.pressEnter();
    await settle();
    await h.renderOnce();
    expect(h.captureCharFrame()).toContain("Network unavailable");
    expect(h.captureCharFrame()).toContain("Retry");
    h.mockInput.pressTab();
    h.mockInput.pressEnter();
    await h.renderOnce();
    expect(h.captureCharFrame()).toContain("Review code");
  } finally { h.renderer.destroy(); }
});

test("clicking Update confirms installation and late offers after close are ignored", async () => {
  const h = await createTestRenderer({ width: 80, height: 16 });
  let updates = 0;
  const palette = mountPalette(h.renderer, [item], { run: async () => ({ ok: true, message: "" }), close: () => {}, update: async () => { updates++; return { ok: true, message: "" }; } });
  try {
    palette.offerUpdate(offer);
    await h.renderOnce();
    const rows = h.captureCharFrame().split("\n");
    const y = rows.findIndex(row => row.includes("[Later]"));
    await h.mockMouse.click(rows[y]!.indexOf("Update") + 1, y);
    await settle();
    await h.renderOnce();
    expect(updates).toBe(1);
    expect(h.captureCharFrame()).toContain("Updated to v9.0.0");
  } finally { h.renderer.destroy(); }
  expect(() => palette.offerUpdate(offer)).not.toThrow();
});

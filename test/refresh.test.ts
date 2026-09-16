import { expect, test } from "bun:test";
import { startLiveRefresh } from "../src/refresh";
import type { PaletteItem } from "../src/types";

test("loading is asynchronous, publishes partial data, and ignores results after closing", async () => {
  let finish!: (items: PaletteItem[]) => void;
  const updates: PaletteItem[][] = [];
  const stop = startLiveRefresh(async publish => {
    publish([]);
    return new Promise(resolve => { finish = resolve; });
  }, items => updates.push(items), () => {}, 1);
  expect(updates).toHaveLength(1);
  stop();
  finish([]);
  await new Promise(resolve => setTimeout(resolve, 10));
  expect(updates).toHaveLength(1);
});

test("refresh retries failures without overlapping requests", async () => {
  let calls = 0;
  let errors = 0;
  let stop = () => {};
  let completed!: () => void;
  const done = new Promise<void>(resolve => { completed = resolve; });
  stop = startLiveRefresh(async () => {
    calls++;
    if (calls === 1) throw new Error("offline");
    return [];
  }, () => { stop(); completed(); }, () => { errors++; }, 1);
  await done;
  expect(errors).toBe(1);
  expect(calls).toBe(2);
});

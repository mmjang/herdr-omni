import { expect, test } from "bun:test";
import { activityAge } from "../src/activity";

test("activity ages use compact seconds, minutes, hours and days", () => {
  const now = 2_000_000_000_000;
  for (const [seconds, label] of [[0, "0s ago"], [1, "1s ago"], [59, "59s ago"],
    [60, "1min ago"], [180, "3min ago"], [3599, "59min ago"], [3600, "1h ago"],
    [86399, "23h ago"], [86400, "1 day ago"], [864000, "10 days ago"]] as const) {
    expect(activityAge(now - seconds * 1000, now)).toBe(label);
  }
  expect(activityAge(now + 1000, now)).toBe("0s ago");
  for (const timestamp of [0, -1, NaN, Infinity]) expect(activityAge(timestamp, now)).toBe("unknown activity");
});

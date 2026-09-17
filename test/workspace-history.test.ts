import { mkdtempSync, rmSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { afterEach, expect, test } from "bun:test";
import { clearWorkspaceVisitCache, loadWorkspaceVisitTimes, parseWorkspaceFocusLog } from "../src/workspace-history";

const temporaryDirectories: string[] = [];

afterEach(() => {
  clearWorkspaceVisitCache();
  for (const directory of temporaryDirectories.splice(0)) rmSync(directory, { recursive: true, force: true });
});

test("parses successful workspace focus events and ignores tabs or failures", () => {
  const source = [
    '2026-09-17T10:00:00.000000Z  INFO herdr::logging: workspace focused event="workspace.focus" subsystem="workspace" outcome="ok" workspace_id="w1"',
    '2026-09-17T10:01:00.000000Z  INFO herdr::logging: tab focused event="tab.focus" subsystem="tab" outcome="ok" workspace_id="w1" tab_id="w1:t1"',
    '2026-09-17T10:02:00.000000Z  INFO herdr::logging: workspace focused event="workspace.focus" subsystem="workspace" outcome="error" workspace_id="w2"',
    'not-a-timestamp INFO workspace focused event="workspace.focus" outcome="ok" workspace_id="bad"',
    '2026-09-17T10:03:00.000000Z  INFO herdr::logging: workspace focused event="workspace.focus" subsystem="workspace" outcome="ok" workspace_id="w2"',
    '2026-09-17T10:04:00.000000Z  INFO herdr::logging: workspace focused event="workspace.focus" subsystem="workspace" outcome="ok" workspace_id="w1"',
    // Concurrent log writers can occasionally finish out of timestamp order.
    '2026-09-17T10:02:30.000000Z  INFO herdr::logging: workspace focused event="workspace.focus" subsystem="workspace" outcome="ok" workspace_id="w1"',
  ].join("\n");
  const visits = parseWorkspaceFocusLog(source);

  expect(visits.get("w1")).toBe(Date.parse("2026-09-17T10:04:00.000Z"));
  expect(visits.get("w2")).toBe(Date.parse("2026-09-17T10:03:00.000Z"));
  expect(visits.has("bad")).toBe(false);
  expect(visits.size).toBe(2);
});

test("reloads workspace visits when the Herdr log changes and fails safely", () => {
  const directory = mkdtempSync(join(tmpdir(), "herdr-workspace-history-"));
  temporaryDirectories.push(directory);
  const path = join(directory, "herdr-server.log");
  writeFileSync(path, '2026-09-17T10:00:00.000000Z INFO workspace focused event="workspace.focus" outcome="ok" workspace_id="w1"\n');

  expect(loadWorkspaceVisitTimes(path).get("w1")).toBe(Date.parse("2026-09-17T10:00:00.000Z"));
  writeFileSync(path, '2026-09-17T10:01:00.000000Z INFO workspace focused event="workspace.focus" outcome="ok" workspace_id="w22"\n');
  expect(loadWorkspaceVisitTimes(path).get("w22")).toBe(Date.parse("2026-09-17T10:01:00.000Z"));
  expect(loadWorkspaceVisitTimes(join(directory, "missing.log"))).toEqual(new Map());
});

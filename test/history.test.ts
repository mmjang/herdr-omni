import { mkdtempSync, readFileSync, rmSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { afterEach, expect, test } from "bun:test";
import { historyKey, loadHistory, recordSelection } from "../src/history";

const originalSocket = process.env.HERDR_SOCKET_PATH;
const originalState = process.env.XDG_STATE_HOME;
const originalHome = process.env.HOME;

afterEach(() => {
  if (originalSocket === undefined) delete process.env.HERDR_SOCKET_PATH;
  else process.env.HERDR_SOCKET_PATH = originalSocket;
  if (originalState === undefined) delete process.env.XDG_STATE_HOME;
  else process.env.XDG_STATE_HOME = originalState;
  if (originalHome === undefined) delete process.env.HOME;
  else process.env.HOME = originalHome;
});

function historyFile(): { directory: string; path: string } {
  const directory = mkdtempSync(join(tmpdir(), "herdr-palette-history-"));
  return { directory, path: join(directory, "history.json") };
}

function clean(directory: string): void {
  rmSync(directory, { recursive: true, force: true });
}

test("persists static and session-scoped live selections", () => {
  const { directory, path } = historyFile();
  try {
    process.env.HERDR_SOCKET_PATH = "/tmp/herdr-a.sock";
    recordSelection("settings", path);
    recordSelection("live:agent:w1:p1", path);

    const history = loadHistory(path);
    expect(history.settings).toBeTypeOf("number");
    expect(history[historyKey("live:agent:w1:p1")]).toBeTypeOf("number");

    process.env.HERDR_SOCKET_PATH = "/tmp/herdr-b.sock";
    const otherSession = loadHistory(path);
    expect(otherSession.settings).toBeTypeOf("number");
    expect(otherSession[historyKey("live:agent:w1:p1")]).toBeUndefined();
  }
  finally { clean(directory); }
});

test("ignores corrupt and invalid history data", () => {
  const { directory, path } = historyFile();
  try {
    writeFileSync(path, "not json");
    expect(loadHistory(path)).toEqual({});

    writeFileSync(path, JSON.stringify({ good: 12, text: "12", nan: null, nested: {}, list: [], infinity: 1e400 }));
    expect(loadHistory(path)).toEqual({ good: 12 });
  }
  finally { clean(directory); }
});

test("bounds history to the 200 most recent entries", () => {
  const { directory, path } = historyFile();
  try {
    const entries = Object.fromEntries(Array.from({ length: 205 }, (_, index) => [`command_${index}`, index + 1]));
    writeFileSync(path, JSON.stringify(entries));
    const history = loadHistory(path);
    expect(Object.keys(history)).toHaveLength(200);
    expect(history.command_204).toBe(205);
    expect(history.command_4).toBeUndefined();

    process.env.HERDR_SOCKET_PATH = "/tmp/herdr.sock";
    recordSelection("new_command", path);
    expect(Object.keys(JSON.parse(readFileSync(path, "utf8")))).toHaveLength(200);
  }
  finally { clean(directory); }
});

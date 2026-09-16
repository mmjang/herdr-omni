import { expect, test } from "bun:test";
import { transcriptExcerpt, transcriptTerms, savedSessionItem, mergeSessions, runSessionJob } from "../src/sessions";
import { CodexHistory, codexText, claudeText } from "../src/session-worker";
import { filterPaletteItems } from "../src/search";
import { itemsFromSnapshot } from "../src/live";
import { resumeSavedSession, findSessionWorkspace, resumeWorkspaceChoices } from "../src/resume-session";
import { execute } from "../src/execute";
import { transcriptPreview } from "../src/transcript-preview";
import type { SavedSession } from "../src/types";

const session: SavedSession = { provider: "codex", id: "019abc-def", title: "Investigate checkout", cwd: "/repo/shop", updatedAt: 1 };

test("agent metadata search includes saved sessions and contiguous IDs, not transcript contents", () => {
  const item = savedSessionItem(session);
  for (const query of [">checkout", ">shop", ">019abc", ">abc-def", ">codex"]) expect(filterPaletteItems([item], query)).toEqual([item]);
  expect(filterPaletteItems([item], ">ogxyz")).toEqual([]);
  expect(filterPaletteItems([item], ":checkout")).toEqual([]);
  expect(filterPaletteItems([item], ">some remembered sentence")).toEqual([]);
});

test("deduplicates live and saved sessions by provider and ID, not title", () => {
  const live = itemsFromSnapshot({ agents: [{ agent: "codex", pane_id: "w1:p1", title: session.title,
    agent_session: { kind: "id", agent: "codex", value: session.id } }] }, "w1");
  expect(live[0]!.session?.id).toBe(session.id);
  const merged = mergeSessions(live, [session, { ...session, id: "different" }, { ...session, provider: "claude" }]);
  expect(merged).toHaveLength(3);
  expect(merged[0]!.invocation).toEqual({ kind: "herdr", argv: ["agent", "focus", "w1:p1"] });
  expect(merged[0]!.lastActiveAt).toBe(session.updatedAt);
  expect(live[0]!.lastActiveAt).toBeUndefined();
});

test("live and saved agents share activity ordering with relevance first and unknown timestamps last", () => {
  const older = savedSessionItem({ ...session, id: "old", updatedAt: 100 });
  const newer = savedSessionItem({ ...session, id: "new", updatedAt: 300 });
  const live = { ...older, id: "live:agent:p1", savedSession: false, lastActiveAt: 200 };
  const unknown = savedSessionItem({ ...session, id: "unknown", updatedAt: NaN });
  for (const query of ["", ">", ">checkout", "checkout"]) {
    expect(filterPaletteItems([unknown, older, live, newer], query).map(item => item.id))
      .toEqual([newer.id, live.id, older.id, unknown.id]);
  }
  const exact = savedSessionItem({ ...session, id: "exact", title: "checkout", updatedAt: 1 });
  expect(filterPaletteItems([newer, exact], ">checkout")[0]).toBe(exact);
  expect(mergeSessions([{ ...live, lastActiveAt: 500 }], [session])[0]!.lastActiveAt).toBe(500);
});

test("transcripts use contiguous case-insensitive phrases, Chinese, punctuation, and safe excerpts", () => {
  expect(transcriptTerms('payment "callback timeout"')).toEqual(['payment "callback timeout"']);
  expect(transcriptExcerpt(["Payment CALLBACK timeout in /api/v2"], 'payment callback timeout')).toContain("CALLBACK timeout");
  expect(transcriptExcerpt(["callback eventually hit timeout"], 'callback timeout')).toBeUndefined();
  expect(transcriptExcerpt(["prefix output: it should work now"], "fix it now")).toBeUndefined();
  expect(transcriptExcerpt(["Please FIX IT NOW, thanks"], "fix it now")).toContain("FIX IT NOW");
  expect(transcriptExcerpt(['say "fix it now"'], '"fix it now"')).toBeDefined();
  expect(transcriptExcerpt(['say fix it now'], '"fix it now"')).toBeUndefined();
  expect(transcriptExcerpt(["payment", "timeout"], "payment timeout")).toBeUndefined();
  expect(transcriptExcerpt(["我们处理支付回调失败"], "支付回调")).toContain("支付回调");
  expect(transcriptExcerpt(["error E_CONN_RESET: foo.bar()"], "foo.bar()")).toBeDefined();
  expect(transcriptExcerpt(["unrelated words"], "uwd")).toBeUndefined();
  expect(transcriptExcerpt(["anything"], "")).toBeUndefined();
  expect(transcriptExcerpt(["hello\x1b world"], "world")).not.toContain("\x1b");
});

test("transcript previews highlight only full contiguous phrases", () => {
  const preview = transcriptPreview("fix something now. Please FIX IT NOW. it now", "fix it now", 100, 6);
  expect(preview.flat().filter(part => part.match).map(part => part.text).join("")).toBe("FIX IT NOW");
});

test("transcript excerpts preserve paragraphs, code indentation and readable JSON", () => {
  const excerpt = transcriptExcerpt(["Heading\r\n\r\n```ts\r\n\tconst needle = 1;\r\n```"], "needle")!;
  expect(excerpt).toContain("Heading\n\n```ts\n    const needle = 1;\n```");
  const lines = transcriptPreview(excerpt, "needle", 100, 10).map(line => line.map(part => part.text).join(""));
  expect(lines).toContain("    const needle = 1;");
  const json = transcriptExcerpt(['{"first":"hello","second":"needle","third":"world"}'], "needle")!;
  expect(json).toContain('\n  "second": "needle",\n');
  expect(transcriptExcerpt(['literal \\n needle'], "needle")).toContain('literal \\n needle');
});

test("provider extraction keeps conversation text but excludes all tool content", () => {
  expect(codexText({ turns: [{ items: [
    { type: "userMessage", content: [{ type: "text", text: "question" }] },
    { type: "agentMessage", text: "answer" }, { type: "reasoning", text: "private" },
    { type: "commandExecution", command: "test", aggregatedOutput: "error" },
    { type: "mcpToolCall", result: { text: "tool output" } },
  ] }] })).toEqual(["question", "answer"]);
  const messages = claudeText([
    { type: "user", message: { content: "question" } },
    { type: "assistant", message: { content: [{ type: "text", text: "answer" }, { type: "thinking", thinking: "private" },
      { type: "tool_use", input: { text: "tool input" } }] } },
    { type: "user", message: { content: [{ type: "tool_result", content: [{ type: "text", text: "tool output" }] },
      { type: "tool_result", content: "plain tool output" }] } },
    { type: "system", message: { content: "system text" } },
  ]);
  expect(messages).toEqual(["question", "answer"]);
  expect(transcriptExcerpt(messages, "tool output")).toBeUndefined();
});

test("an already-aborted session job starts no worker and emits nothing", async () => {
  const events: unknown[] = [];
  await runSessionJob({ type: "list" }, AbortSignal.abort(), event => events.push(event));
  expect(events).toEqual([]);
});

test("Codex history transport frames responses, rejects errors, times out and closes", async () => {
  const history = new CodexHistory([process.execPath, `${import.meta.dir}/fixtures/codex-history.ts`], 500);
  try {
    await history.start();
    expect(await history.call("thread/list", {})).toEqual({ method: "thread/list" });
    await expect(history.call("error", {})).rejects.toThrow("failed");
    await expect(history.call("malformed", {})).rejects.toThrow("unavailable");
    await history.start();
    await expect(history.call("hang", {})).rejects.toThrow("timed out");
    await history.start();
    const pending = history.call("hang", {});
    history.close();
    await expect(pending).rejects.toThrow("unavailable");
  } finally { history.close(); }
});

test("Codex history reports an unavailable binary without hanging", async () => {
  const history = new CodexHistory(["/nonexistent/omni-codex-fixture"], 500);
  try { await expect(history.start()).rejects.toThrow("unavailable"); }
  finally { history.close(); }
});

function resumeDeps(agents: unknown[] = [], workspaceState = { workspaces: [{ workspace_id: "w1", label: "Current" }, { workspace_id: "w2", label: "Shop" }], panes: [{ workspace_id: "w2", cwd: "/repo/shop" }] }) {
  const calls: string[][] = [];
  return { calls, deps: {
    run: async (argv: string[]) => {
      calls.push(argv);
      const value = argv[0] === "api" ? { snapshot: { agents, ...workspaceState } } : { root_pane: { pane_id: "w1:p9" } };
      return { code: 0, stdout: JSON.stringify({ result: value }), stderr: "" };
    },
    target: async () => ({ workspaceId: "w1", tabId: "w1:t1", paneId: "w1:p1" }),
    directory: async (_cwd: string) => true,
    installed: () => true,
    focus: async (pane: string) => { calls.push(["focus", pane]); return { ok: true, message: "" }; },
  } };
}

test("resume rechecks live identity and focuses instead of opening a duplicate", async () => {
  const { deps, calls } = resumeDeps([{ pane_id: "w2:p3", agent_session: { kind: "id", agent: "codex", value: session.id } }]);
  expect((await resumeSavedSession(session, deps)).ok).toBe(true);
  expect(calls).toEqual([["api", "snapshot"], ["focus", "w2:p3"]]);
});

test("OpenCode metadata joins live agents by provider and session ID and focuses the existing pane", async () => {
  const saved: SavedSession = { ...session, provider: "opencode", id: "ses_example", updatedAt: Date.now() };
  const agent = { agent: "opencode", pane_id: "w2:p3", title: session.title,
    agent_session: { kind: "id", agent: "opencode", value: saved.id } };
  const merged = mergeSessions(itemsFromSnapshot({ agents: [agent] }, "w2"), [saved]);
  expect(merged).toHaveLength(1);
  expect(merged[0]!.lastActiveAt).toBe(saved.updatedAt);
  expect(filterPaletteItems(merged, ">opencode")).toHaveLength(1);
  expect(filterPaletteItems([savedSessionItem(saved)], "ses_example")).toHaveLength(1);
  const { deps, calls } = resumeDeps([agent]);
  expect((await resumeSavedSession(saved, deps)).ok).toBe(true);
  expect(calls).toEqual([["api", "snapshot"], ["focus", "w2:p3"]]);
});

test("failed resume keeps Omni focused and displays the launch error", async () => {
  const { deps, calls } = resumeDeps();
  const originalRun = deps.run;
  deps.run = async argv => argv[0] === "agent"
    ? { code: 1, stdout: "", stderr: JSON.stringify({ error: { message: "Shell is not ready" } }) }
    : originalRun(argv);
  const result = await resumeSavedSession({ ...session, provider: "opencode" }, deps);
  expect(result.ok).toBe(false);
  expect(result.message).toContain("Shell is not ready");
  expect(result.message).toContain("w1:p9");
  expect(calls.some(argv => argv[0] === "focus")).toBe(false);
});

test("resume opens a tab in the original cwd and passes native arguments without a shell", async () => {
  for (const provider of ["codex", "claude", "opencode"] as const) {
    const { deps, calls } = resumeDeps();
    expect((await resumeSavedSession({ ...session, provider }, deps)).ok).toBe(true);
    expect(calls[1]).toEqual(["tab", "create", "--workspace", "w2", "--cwd", "/repo/shop", "--label", session.title, "--no-focus"]);
    expect(calls[2]!.slice(3)).toEqual(["--kind", provider, "--pane", "w1:p9", "--", provider === "codex" ? "resume" : provider === "opencode" ? "--session" : "--resume", session.id]);
    expect(calls[3]).toEqual(["focus", "w1:p9"]);
  }
});

test("workspace matching uses exact pane paths or the closest explicit worktree, never names", () => {
  const state = { workspaces: [
    { workspace_id: "w1", label: "shop" },
    { workspace_id: "w2", worktree: { checkout_path: "/repo/shop" } },
    { workspace_id: "w3", worktree: { checkout_path: "/repo/shop/feature" } },
  ], panes: [{ workspace_id: "w1", cwd: "/repo" }] };
  expect(findSessionWorkspace(state, "/repo/shop/feature/src")).toBe("w3");
  expect(findSessionWorkspace(state, "/repo/shop-other")).toBeUndefined();
  expect(findSessionWorkspace(state, "/different/shop")).toBeUndefined();
  expect(findSessionWorkspace({ ...state, panes: [{ workspace_id: "w1", cwd: "/repo/shop/feature/src/" }] }, "/repo/shop/feature/src")).toBe("w1");
  const ambiguous = { workspaces: state.workspaces, panes: [{ workspace_id: "w1", cwd: session.cwd }, { workspace_id: "w2", cwd: session.cwd }] };
  expect(findSessionWorkspace(ambiguous, session.cwd)).toBe("w1");
  expect(findSessionWorkspace({ ...ambiguous, panes: [...ambiguous.panes].reverse() }, session.cwd)).toBe("w1");
  expect(findSessionWorkspace({ ...ambiguous, workspaces: [...state.workspaces].reverse() }, session.cwd)).toBe("w2");
  expect(findSessionWorkspace({ workspaces: [
    { workspace_id: "w2", worktree: { checkout_path: session.cwd } },
    { workspace_id: "w1", worktree: { checkout_path: session.cwd } },
  ] }, session.cwd)).toBe("w2");
});

test("multiple matching workspaces resume in the first without confirmation or current-workspace preference", async () => {
  const { deps, calls } = resumeDeps([], {
    workspaces: [{ workspace_id: "w2", label: "shop" }, { workspace_id: "w1", label: "shop" }],
    panes: [{ workspace_id: "w1", cwd: session.cwd }, { workspace_id: "w2", cwd: session.cwd }],
  });
  const result = await resumeSavedSession(session, deps);
  expect(result.ok).toBe(true);
  expect(result.confirmWorkspace).toBeUndefined();
  expect(calls[1]!.slice(0, 4)).toEqual(["tab", "create", "--workspace", "w2"]);
});

test("unmatched sessions require confirmation before any tab creation", async () => {
  const { deps, calls } = resumeDeps([], { workspaces: [{ workspace_id: "w1", label: "Current" }], panes: [] });
  const result = await resumeSavedSession(session, deps);
  expect(result.confirmWorkspace).toEqual({ id: "w1", label: "Current" });
  expect(calls).toEqual([["api", "snapshot"]]);
  expect((await resumeSavedSession(session, deps, "w1")).ok).toBe(true);
  expect(calls[2]!.slice(0, 6)).toEqual(["tab", "create", "--workspace", "w1", "--cwd", session.cwd]);
});

test("stale confirmation cannot authorize a different workspace; non-yes inputs execute nothing", async () => {
  const { deps, calls } = resumeDeps([], { workspaces: [{ workspace_id: "w1", label: "Current" }], panes: [] });
  expect((await resumeSavedSession(session, deps, "w9")).confirmWorkspace?.id).toBe("w1");
  expect(calls).toHaveLength(1);
  const item = { ...savedSessionItem(session), invocation: { kind: "resume-session" as const, session, fallbackWorkspaceId: "w1" } };
  for (const input of ["", "no", "y"]) expect((await execute(item, input)).message).toContain('Type "yes"');
});

test("excerpts retain surrounding messages and preview centers/highlights the actual match", () => {
  const excerpt = transcriptExcerpt(["Previous discussion.", "The needle happened.", "Following explanation."], "needle")!;
  expect(excerpt).toContain("Previous discussion.");
  expect(excerpt).toContain("Following explanation.");
  const long = transcriptExcerpt(["before ".repeat(80) + "needle " + "after ".repeat(160)], "needle")!;
  expect(long.length).toBeGreaterThan(1000);
  const preview = transcriptPreview(long, "needle", 45, 6);
  expect(preview).toHaveLength(6);
  expect(preview.flat().filter(part => part.match).map(part => part.text).join("")).toBe("needle");
  expect(preview[0]!.map(part => part.text).join("")).toContain("before");
  expect(preview.at(-1)!.map(part => part.text).join("")).toContain("after");
  for (const row of transcriptPreview("上下文 ".repeat(60) + "支付回调🙂 " + "后续说明 ".repeat(60), "支付回调", 25, 6)) {
    expect(Bun.stringWidth(row.map(part => part.text).join(""))).toBeLessThanOrEqual(25);
  }
});

test("missing directories offer ranked destinations and require an explicit valid choice for all providers", async () => {
  const state = { workspaces: [{ workspace_id: "w1", label: "Current" }, { workspace_id: "w2", label: "Shop" }], panes: [
    { workspace_id: "w1", cwd: "/repo/current" }, { workspace_id: "w2", cwd: "/repo/shop" },
  ] };
  const missing = { ...session, cwd: "/deleted/worktrees/shop" };
  const directory = async (cwd: string) => !cwd.startsWith("/deleted");
  const choices = await resumeWorkspaceChoices(state, missing.cwd, "w1", directory);
  expect(choices.map(choice => choice.id)).toEqual(["w2", "w1"]);
  expect(choices[0]!.reason).toBe("Matching project folder");
  const gitChoices = await resumeWorkspaceChoices(state, missing.cwd, "w1", directory, async cwd => cwd === "/repo/current");
  expect(gitChoices[0]!.reason).toBe("Same Git repository");
  for (const provider of ["codex", "claude", "opencode"] as const) {
    const { deps, calls } = resumeDeps([], state);
    deps.directory = directory;
    const saved = { ...missing, provider };
    expect((await resumeSavedSession(saved, deps)).workspaceChoices).toEqual(choices);
    expect(calls).toHaveLength(1);
    expect((await resumeSavedSession(saved, deps, undefined, { id: "w2", cwd: "/stale" })).workspaceChoices).toEqual(choices);
    expect(calls).toHaveLength(2);
    expect((await resumeSavedSession(saved, deps, undefined, choices[0])).ok).toBe(true);
    expect(calls[3]!.slice(0, 6)).toEqual(["tab", "create", "--workspace", "w2", "--cwd", "/repo/shop"]);
    expect(calls[4]!.slice(8)).toEqual(provider === "codex" ? ["resume", session.id, "--cd", "/repo/shop"] : [provider === "opencode" ? "--session" : "--resume", session.id]);
  }
});

test("missing directories and invalid IDs cannot create tabs", async () => {
  const { deps, calls } = resumeDeps();
  deps.directory = async () => false;
  expect((await resumeSavedSession(session, deps)).ok).toBe(false);
  expect(calls).toEqual([["api", "snapshot"]]);
  expect((await resumeSavedSession({ ...session, id: "--evil" }, deps)).ok).toBe(false);
  expect(calls).toHaveLength(1);
});

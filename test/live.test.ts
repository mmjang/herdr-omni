import { expect, test } from "bun:test";
import { itemsFromSnapshot, itemsFromWorktrees } from "../src/live";

const snapshot = {
  workspaces: [
    { workspace_id: "w1", label: "alpha", number: 1, focused: true, active_tab_id: "w1:t1", pane_count: 3, tab_count: 2, agent_status: "working" },
    { workspace_id: "w2", label: "beta", number: 2, focused: false, active_tab_id: "w2:t1", pane_count: 1, tab_count: 1, agent_status: "idle",
      worktree: { checkout_path: "/repo-beta", repo_name: "project", repo_root: "/repo" } },
  ],
  tabs: [
    { tab_id: "w1:t1", label: "editor", number: 1, workspace_id: "w1", focused: true, pane_count: 2, agent_status: "working" },
    { tab_id: "w2:t1", label: "shell", number: 1, workspace_id: "w2", focused: false, pane_count: 1, agent_status: "idle" },
  ],
  agents: [
    // Deliberately duplicate the display name: pane_id is the only unambiguous target.
    { agent: "codex", agent_status: "working", cwd: "/repo", focused: true, pane_id: "w1:p1", tab_id: "w1:t1", workspace_id: "w1", terminal_title_stripped: "Implement search" },
    { agent: "codex", agent_status: "idle", cwd: "/repo", focused: false, pane_id: "w2:p1", tab_id: "w2:t1", workspace_id: "w2", terminal_title_stripped: "Review changes" },
  ],
  panes: [
    { pane_id: "w1:p1", label: "logs", tab_id: "w1:t1", workspace_id: "w1", focused: false, cwd: "/repo", terminal_title_stripped: "logs" },
    { pane_id: "w1:p2", label: "1", tab_id: "w1:t1", workspace_id: "w1", focused: false, cwd: "/repo", terminal_title_stripped: "shell" },
    { pane_id: "w2:p2", label: "   ", tab_id: "w2:t1", workspace_id: "w2", focused: false, cwd: "/repo", terminal_title_stripped: "shell" },
    { pane_id: "w2:p3", tab_id: "w2:t1", workspace_id: "w2", focused: false, cwd: "/repo", terminal_title_stripped: "shell" },
  ],
};

const herdrArgv = (item: ReturnType<typeof itemsFromSnapshot>[number]) =>
  item.invocation.kind === "herdr" ? item.invocation.argv : undefined;

test("maps workspaces and tabs to exact focus commands", () => {
  const items = itemsFromSnapshot(snapshot, "w1");

  const workspace = items.find(item => herdrArgv(item)?.join(" ") === "workspace focus w2");
  expect(workspace).toMatchObject({ title: "beta", category: "Workspace" });
  expect(items.find(item => item.title === "alpha")?.currentWorkspace).toBe(true);
  expect(workspace?.currentWorkspace).toBe(false);

  const tab = items.find(item => herdrArgv(item)?.join(" ") === "tab focus w2:t1");
  expect(tab).toMatchObject({ title: "beta → shell", category: "Tabs" });
});

test("excludes unnamed and numeric tabs while retaining named tabs and workspaces", () => {
  const labels = ["1", "02", " 123 ", "１２", "", "   ", undefined, "dev", "v2", "123-api"];
  const items = itemsFromSnapshot({
    workspaces: [{ workspace_id: "w1", label: "Coffee" }],
    tabs: labels.map((label, i) => ({ tab_id: `w1:t${i}`, workspace_id: "w1", label })),
  }, "w1");
  expect(items.filter(item => item.category === "Tabs").map(item => item.title)).toEqual([
    "Coffee → dev", "Coffee → v2", "Coffee → 123-api",
  ]);
  expect(items.filter(item => item.category === "Workspace").map(item => item.title)).toEqual(["Coffee"]);
});

test("focuses agents by pane ID, even when agent names are duplicated", () => {
  const items = itemsFromSnapshot(snapshot, "w1");
  const agents = items.filter(item => item.category === "Agents");

  expect(agents).toHaveLength(2);
  expect(agents.map(agent => agent.title)).toEqual(["Implement search - alpha", "Review changes - beta"]);
  expect(agents.map(herdrArgv)).toEqual([
    ["agent", "focus", "w1:p1"],
    ["agent", "focus", "w2:p1"],
  ]);
  expect(agents.map(agent => agent.currentWorkspace)).toEqual([true, false]);
});

test("prefers session title metadata over terminal and control names", () => {
  const namedSnapshot = { ...snapshot, agents: [{ ...snapshot.agents[0], name: "reviewer", title: "Audit checkout flow" }] };
  const agent = itemsFromSnapshot(namedSnapshot, "w1").find(item => item.category === "Agents");

  expect(agent?.title).toBe("Audit checkout flow - alpha");
});

test("represents only unopened worktrees as workspace destinations", () => {
  const worktrees = [
    { branch: "main", label: "main", path: "/repo", open_workspace_id: "w1", is_bare: false, is_detached: false, is_linked_worktree: false, is_prunable: false },
    { branch: "feature/logs", label: "logs", path: "/repo-logs", open_workspace_id: "w2", is_bare: false, is_detached: false, is_linked_worktree: true, is_prunable: false },
    { branch: "feature/new", label: "new", path: "/repo-new", is_bare: false, is_detached: false, is_linked_worktree: true, is_prunable: false },
  ];
  const items = itemsFromWorktrees(worktrees, "w1");

  expect(items.map(item => item.title)).toEqual(["new"]);
  expect(items[0]).toMatchObject({ id: "live:workspace:worktree:/repo-new", category: "Workspace" });

  const closed = items.find(item => item.title === "new");
  expect(closed?.invocation).toEqual({
    kind: "herdr",
    argv: ["worktree", "open", "--workspace", "w1", "--path", "/repo-new", "--focus"],
  });
});

test("does not duplicate worktrees already represented by open workspaces", () => {
  const items = itemsFromSnapshot(snapshot, "w1");
  const worktree = items.find(item => item.id === "live:worktree:/repo-beta");

  expect(worktree).toBeUndefined();
  expect(items.filter(item => item.category === "Workspace").map(item => item.title)).toEqual(["alpha", "beta"]);
});

test("does not fabricate visit timestamps from focus or workspace order", () => {
  const items = itemsFromSnapshot({
    workspaces: [
      { workspace_id: "w1", label: "alpha", number: 2, focused: true },
      { workspace_id: "w2", label: "beta", number: 1, focused: false },
    ],
  }, "w1");

  expect(items.find(item => item.title === "alpha")?.lastVisitedAt).toBeUndefined();
  expect(items.find(item => item.title === "beta")?.lastVisitedAt).toBeUndefined();
});

test("applies only Herdr workspace visit timestamps supplied by the log reader", () => {
  const items = itemsFromSnapshot({
    workspaces: [
      { workspace_id: "w1", label: "alpha", focused: true },
      { workspace_id: "w2", label: "beta", focused: false },
    ],
  }, "w1", new Map([["w2", 42]]));

  expect(items.find(item => item.title === "alpha")?.lastVisitedAt).toBeUndefined();
  expect(items.find(item => item.title === "beta")?.lastVisitedAt).toBe(42);
});

test("joins exact workspace and tab panes, preserving remembered focus and overview-only tabs", () => {
  const items = itemsFromSnapshot({
    workspaces: [
      { workspace_id: "w1", label: "Shop", pane_count: 2, worktree: { checkout_path: "/repo/shop" } },
      { workspace_id: "w2", label: "Other", pane_count: 1 },
    ],
    tabs: [
      { tab_id: "w1:t1", workspace_id: "w1", label: "dev" },
      { tab_id: "w1:t2", workspace_id: "w1", label: "1" },
      { tab_id: "w2:t1", workspace_id: "w2", label: "shell" },
    ],
    panes: [
      { pane_id: "w1:p1", workspace_id: "w1", tab_id: "w1:t1", terminal_title_stripped: "Terminal", cwd: "/repo/shop", foreground_cwd: "/repo/shop/src" },
      { pane_id: "w1:p2", workspace_id: "w1", tab_id: "w1:t1", label: "Agent pane", cwd: "/repo/shop" },
      // Same tab ID text in another workspace must not leak into w1.
      { pane_id: "w2:p1", workspace_id: "w2", tab_id: "w2:t1", label: "Other pane", cwd: "/repo/other" },
    ],
    agents: [{ pane_id: "w1:p2", workspace_id: "w1", tab_id: "w1:t1", agent: "codex", agent_status: "working" }],
    layouts: [{ tab_id: "w1:t1", focused_pane_id: "w1:p2" }],
  }, "w1");

  const workspace = items.find(item => item.title === "Shop")?.resourcePreview;
  expect(workspace).toEqual({
    kind: "workspace", workspaceId: "w1", paths: ["/repo/shop"], paneCount: 2,
    tabs: [
      { id: "w1:t1", label: "dev", panes: [
        { id: "w1:p1", label: "Terminal", cwd: "/repo/shop/src", focused: false },
        { id: "w1:p2", label: "Agent pane", cwd: "/repo/shop", agent: "codex", status: "working", focused: true },
      ] },
      { id: "w1:t2", label: "1", panes: [] },
    ],
  });
  const tab = items.find(item => herdrArgv(item)?.join(" ") === "tab focus w1:t1");
  expect(tab?.resourcePreview).toMatchObject({ kind: "tab", workspaceId: "w1", panes: [{ id: "w1:p1" }, { id: "w1:p2", focused: true }] });
  expect(items.find(item => herdrArgv(item)?.join(" ") === "tab focus w1:t2")).toBeUndefined();
});

test("falls back to distinct agent panes without fabricating unavailable panes", () => {
  const items = itemsFromSnapshot({
    workspaces: [{ workspace_id: "w1", label: "Shop" }, { workspace_id: "w2", label: "Empty" }],
    tabs: [{ tab_id: "w1:t1", workspace_id: "w1", label: "dev" }, { tab_id: "w2:t1", workspace_id: "w2", label: "shell" }],
    agents: [
      { pane_id: "w1:p1", workspace_id: "w1", tab_id: "w1:t1", agent: "claude", cwd: "/repo", terminal_title_stripped: "Review" },
      { pane_id: "w1:p1", workspace_id: "w1", tab_id: "w1:t1", agent: "claude", cwd: "/repo" },
    ],
  }, "w1");

  const shop = items.find(item => item.title === "Shop")?.resourcePreview;
  expect(shop?.kind).toBe("workspace");
  if (shop?.kind === "workspace") {
    expect(shop.paths).toEqual(["/repo"]);
    expect(shop.tabs[0]?.panes).toEqual([{ id: "w1:p1", label: "Review", cwd: "/repo", agent: "claude", focused: false }]);
    expect(shop.paneCount).toBe(1);
  }
  const empty = items.find(item => item.title === "Empty")?.resourcePreview;
  expect(empty).toMatchObject({ kind: "workspace", paths: [], tabs: [{ id: "w2:t1", panes: [] }], paneCount: 0 });
});

test("attaches worktree previews without inventing a branch", () => {
  const items = itemsFromWorktrees([
    { path: "/repo/feature", label: "feature", branch: "feature/ui" },
    { path: "/repo/detached", label: "detached" },
  ], "w1");
  expect(items.map(item => item.resourcePreview)).toEqual([
    { kind: "worktree", path: "/repo/feature", branch: "feature/ui" },
    { kind: "worktree", path: "/repo/detached" },
  ]);
});

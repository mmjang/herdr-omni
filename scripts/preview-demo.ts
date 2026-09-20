import { createCliRenderer } from "@opentui/core";
import { mountPalette } from "../src/palette";
import { savedSessionItem, transcriptExcerpt } from "../src/sessions";
import type { SavedSession } from "../src/types";
import { itemsFromSnapshot } from "../src/live";

// Self-contained sample data: no provider processes, history reads, or resume actions.
const examples = [
  { title: "修复支付回调重复处理", provider: "claude" as const, text: "user: 支付成功后，偶尔会生成两条订单。帮我查一下回调的幂等处理。\n\nassistant: 已定位到并发回调先查询、后写入造成的竞争条件。\n\n修改方案：\n  1. 使用支付流水号作为唯一键。\n  2. 在同一个事务中创建订单和记录回调。\n  3. 重复回调直接返回已有订单。\n\n验证结果：重复请求与并发请求均只生成一条订单。" },
  { title: "设计会话搜索预览", provider: "codex" as const, text: "user: 选中搜索结果时，能不能先看看最近聊了什么？\n\nassistant: 可以。宽窗口使用右侧详情，窄窗口使用底部预览。\n\n普通搜索展示最近对话，内容搜索展示命中位置。选中停留后才加载内容，快速移动时取消旧请求。\n\nCtrl+O 展开，PageUp / PageDown 滚动，Ctrl+Y 隐藏。" },
  { title: "排查构建失败", provider: "opencode" as const, text: "message: CI 在类型检查阶段失败，本地构建正常。\n\nmessage: 原因是锁文件与依赖声明不同步。更新依赖后，类型检查与测试已通过，等待检查变更。" },
];
const sessions: SavedSession[] = examples.map((example, index) => ({ provider: example.provider, id: `demo-${index}`, title: example.title,
  cwd: `/projects/${index === 0 ? "shop" : "herdr-omni"}`, updatedAt: Date.now() - (index + 1) * 60_000 }));
const renderer = await createCliRenderer({ exitOnCtrlC: true, useMouse: true });
const items = sessions.map(savedSessionItem);
items[0] = { ...items[0]!, id: "demo:live", savedSession: false, livePaneId: "demo:p1", agentStatus: "blocked", priority: 0 };
items.unshift(...itemsFromSnapshot({
  workspaces: [{ workspace_id: "demo", label: "Shop", tab_count: 1, pane_count: 2, worktree: { checkout_path: "/projects/shop" } }],
  tabs: [{ workspace_id: "demo", tab_id: "demo:t1", label: "Development", pane_count: 2 }],
  panes: [
    { workspace_id: "demo", tab_id: "demo:t1", pane_id: "demo:p1", label: "Payment agent", agent: "claude", agent_status: "blocked", cwd: "/projects/shop", focused: true },
    { workspace_id: "demo", tab_id: "demo:t1", pane_id: "demo:p2", label: "Build terminal", cwd: "/projects/shop" },
  ],
}, "demo"));
let frame = 0;
mountPalette(renderer, items, {
  close: () => renderer.destroy(),
  run: async () => ({ ok: false, message: "Demo only — no session was opened." }),
  resourceDetails: async target => target.kind === "workspace" ? "feature/payments" : target.paneId === "demo:p2" ? "bun" : "claude",
  panePreview: async paneId => paneId === "demo:p2" ? `Build terminal\n\n$ bun run dev\nServer running at localhost:3000\n\nGET /checkout 200\nRefresh ${++frame}` : [
    "Claude Code · shop", "", "● Checking payment callback idempotency", "",
    "  bun test payment.test.ts", "  12 pass · 0 fail", "",
    `  Preview refresh ${++frame} (sample pane)`, "",
    "╭─ Permission required ─────────────────╮", "│ Run the integration tests?           │",
    "│ ❯ 1. Allow once                      │", "│   2. Reject                          │", "╰──────────────────────────────────────╯",
    "", "Enter to confirm · Esc to cancel",
  ].join("\n"),
  sessionJob: async (request, signal, publish) => {
    if (signal.aborted) return;
    if (request.type === "preview") {
      const index = sessions.findIndex(session => session.id === request.session.id);
      publish({ type: "preview", session: request.session, excerpt: examples[index]?.text ?? "" });
    } else if (request.type === "search") {
      for (const [index, session] of sessions.entries()) {
        const excerpt = transcriptExcerpt([examples[index]!.text], request.query);
        if (excerpt) publish({ type: "hit", session, excerpt });
      }
    }
    publish({ type: "done" });
  },
});

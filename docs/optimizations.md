# 优化方案记录

本文记录经过验证的优化方案。每条包含：问题、实测数据、方案、改动点、风险。
未验证的想法不进本文。

实测环境：macOS 26 (Darwin 27.0.0)，本机数据量见各条目。

---

## 1. Transcript 搜索：改为 rg 粗筛 + SDK 精读

**状态**：已确认方案，待实施
**记录时间**：2026-09-17
**涉及**：`src/session-worker.ts`、`src/sessions.ts`

### 问题

当前 transcript 搜索（`src/session-worker.ts:119-139`）对**每一个**会话串行发起 IPC 并完整解析：

- Codex → `codex app-server` 的 `thread/read --includeTurns`
- Claude → SDK `getSessionMessages()`
- OpenCode → `opencode serve` HTTP API

而 `src/palette.ts:127` 的 `scheduleScan()` 在每次 query 变化时清空 hits、kill worker、**从零重扫**，没有任何缓存或索引（debounce 仅 200ms）。

结果：它不是「边打边搜」，而是「想清楚整句、打完、然后等」。README 截图里的 `132/697` 就是这个过程。转圈动画只是把等待可视化了，没有消除等待。

### 实测数据

本机语料规模：

| Provider | 落盘位置 | 规模 |
|---|---|---|
| Codex | `~/.codex/sessions/YYYY/MM/DD/rollout-*.jsonl` | 4.2 GB / 813 文件（+ `archived_sessions/` 39） |
| Claude | `~/.claude/projects/<encoded-cwd>/<sessionId>.jsonl` | 270 MB / 164 文件 |
| OpenCode | `~/.local/share/opencode/opencode.db`（SQLite） | 10 MB / 13 会话 · 240 messages · 847 parts |

全文检索耗时对比（查询 `herdr omni`）：

| 方式 | 耗时 |
|---|---|
| `rg -l -i -F` 扫 Codex 全量 4.2 GB | **0.97s** |
| `rg -c -i` 扫 Claude 全量 270 MB | **0.10s** |
| `sqlite3 ... where data like` 扫 OpenCode 全表 | **0.046s** |
| 现有逐会话 IPC 方案 | 数分钟量级（697 会话未扫完） |

差距在一到两个数量级。

### 关键事实验证

**① Codex 的 jsonl 是 source of truth，不是留档日志。**

`~/.codex/` 下另有 `thread_history_1.sqlite`（1.3 GB），容易误认为它才是真数据。但其 schema 表明它是 rollout 文件的**投影**：

```sql
CREATE TABLE thread_history_projection_state (
    thread_id TEXT PRIMARY KEY,
    next_rollout_byte_offset INTEGER NOT NULL,  -- 读到 rollout 文件第几字节
    next_rollout_ordinal INTEGER NOT NULL
);
```

**② jsonl 语料是 app-server 可见范围的超集，rg 不会漏。**

- sqlite 中 distinct `thread_id`：683
- 磁盘 jsonl：813（`sessions/`）+ 39（`archived_sessions/`）= 852
- `session_index.jsonl`：707 行
- 抽样 30 个 sqlite thread_id 回查磁盘：**30/30 命中**

**③ 文件名直接携带会话 ID，粗筛结果无需再开文件即可映射回会话。**

- Codex：`rollout-2026-08-06T18-53-59-019fd6b5-62ef-7781-9e64-cccdfc34100f.jsonl` → 末尾 UUID 即 `thread_id`
- Claude：`<sessionId>.jsonl`，文件名即 session id

**④ OpenCode 是 SQLite，rg 不适用**，需单独分支（但其规模本就不是瓶颈）。

### 核心限制：rg 会严重过匹配，只能当粗筛

磁盘上绝大部分字节是工具调用与 reasoning，而 0.8.1 的既定行为是**要把这些排除**。

某 Codex 会话文件的 record 分布：

```
custom_tool_call + custom_tool_call_output   3922
reasoning                                    2099
message（真正的对话）                          666
```

某 Claude 会话文件：

```
assistant / thinking       26
user      / tool_result    25
assistant / tool_use       25
user      / string          5
assistant / text            1
```

裸 rg 会把「我曾经 grep 过这个词」误判成「我聊过这个词」。**因此 rg 是 prefilter，不是 replacement——精确语义仍由现有 SDK 路径保证。**

### 降噪技巧：JSONL 一行 = 一条 record

行级过滤直接等价于 record 级过滤，粗筛阶段即可降噪，无需解析 JSON：

```sh
# Codex：同一行里既含关键词、又是 message 类型
rg -l -i -F "$phrase" ~/.codex/sessions | while read -r f; do
  rg -i -F "$phrase" "$f" | rg -qF '"type":"message"' && echo "$f"
done
```

Claude 同理，排除含 `"tool_result"` / `"tool_use"` 的行。

实测效果（查询 `herdr omni`）：**813 文件 → 5 候选 → 2 候选，0.23s**（warm cache）。
进入昂贵阶段的输入量降低约两个数量级。

### 方案

两段式，**粗筛放在 worker 内、现有循环之前**：

1. **粗筛**（rg / sqlite，~1s 覆盖全量）→ 候选文件 → 会话 ID
2. **精读**（现有 `thread/read` / `getSessionMessages` / OpenCode API）只跑候选，照旧排除 tool call、生成摘录

放在 worker 内的理由：`SessionRequest` 类型不变，UI 侧 `progress` / `hit` / `done` 事件语义不变，进程隔离与 abort 行为不变。

### 改动点

- `src/session-worker.ts:121` 的 `for (const session of request.sessions)` **循环本身不用改**，只需在其之前插入一层，把 `request.sessions` 按候选集合过滤。
- 过滤时保留原有的 `updatedAt` 倒序（`src/palette.ts:143-145` 已排好），使 `TRANSCRIPT_RESULT_LIMIT = 100` 的截断语义不变。
- 副作用收益：`progress` 事件的 `total` 从全量（697）变成候选数（个位数），进度条从「看不到头」变成「立刻结束」。
- rg 可用性检测沿用现有风格：`Bun.which("rg")`（参照 `session-worker.ts:88` 的 `Bun.which("opencode")`）。

### 风险与兼容性

**路径假设会随 CLI 版本漂移**——OpenCode 早期版本用 `storage/session/**/*.json` 而非 SQLite，Codex 的 rollout 目录结构也可能变。

因此粗筛层必须可降级，且**降级要保守**：

| 情形 | 行为 |
|---|---|
| `rg` 不存在 | 该 provider 走全量扫描（现有行为） |
| 语料目录不存在 / 其中 0 个 jsonl | 走全量扫描（判定为路径假设失效） |
| 语料目录存在且有文件，但 0 命中 | 判定为**真实无结果**，直接返回空 |

关键是区分「路径错了」和「真的没搜到」——前者必须 fallback，后者必须相信粗筛，否则要么静默丢结果，要么失去全部性能收益。

### 验收标准

- 全量语料下，首个 hit 出现时间从分钟级降到 ~1s
- 搜索结果集与改造前逐会话扫描一致（同 query 对比）
- tool call / tool output 仍不参与匹配（0.8.1 既定行为不回退）
- 删除 `rg` 或改名语料目录后，功能降级但不报错、不丢结果

### 后续可选项（未验证，暂不实施）

- 粗筛结果按 query 前缀缓存，使逐字符输入不必重复粗筛
- 粗筛耗时降到 ~1s 后，重新评估 `TRANSCRIPT_DEBOUNCE_MS = 200` 是否需要调整

---

## 2. 浏览时把最近选择过的 workspace / tab 排在前面

**状态**：已确认方案，待实施
**记录时间**：2026-09-17
**涉及**：`src/search.ts`

### 问题

Palette 的核心手感来自「打开即最近，回车即上一个」（VS Code Ctrl+P 的杀手锏）。当前实现拿不到这个手感：

`src/search.ts:82` 的排序条件把 history 加权限制在**输入了内容之后**：

```ts
if (!browsing && !(a.item.category === "Agents" && b.item.category === "Agents")) {
  const recentDifference = b.recent - a.recent;
  if (recentDifference) return recentDifference;
}
```

而 browsing 分支（`src/search.ts:78-81`）只对 Workspace / Worktrees 按 `lastVisitedAt` 排序，该字段**永远是 undefined**——`src/live.ts:32` 的注释说明 Herdr 不暴露访问历史，因此主动选择不伪造：

```
// Herdr currently exposes no visit history. Preserve its order instead of
// fabricating recency from focus, workspace position, or Omni selections.
```

两者叠加的结果：**打开 palette 不输入直接回车，拿到的是 Herdr 的原始顺序。**

分类现状：

| 分类 | 浏览时排序 | 评价 |
|---|---|---|
| Agents | 按 `agentActivity()` 倒序 | 已经对了，不动 |
| Workspace / Worktrees | 源顺序（`lastVisitedAt` 恒为 undefined） | 待修 |
| Tabs | 源顺序（browsing 分支根本没覆盖 Tabs） | 待修 |
| Actions | 源顺序（`recent` 被显式置 0） | 有意为之，保持 |

### 关键事实：数据已经在了，不用等 Herdr 补 API

不伪造 Herdr 的访问历史是对的，但 Omni **自己的选择记录**不是推测，是真实使用数据，而且早就在落盘：

- `src/main.ts:20` 在每次成功执行后记录：`if (result.ok && item.category !== "Actions") recordSelection(item.id)`
- 落盘位置：`$XDG_STATE_HOME/herdr-palette/history.json`（默认 `~/.local/state/herdr-palette/history.json`）
- live id 按 Herdr socket 路径做命名空间隔离（`src/history.ts:27`），`loadHistory()` 只返回当前 scope 的条目
- 上限 `MAX_ENTRIES = 200`，按时间戳保留最新

本机实测该文件已有 36 条，其中包含 workspace 与 tab：

```
1789571712352  live@%2F...%2Fherdr.sock:live:workspace:w1V
1789560836282  live@%2F...%2Fherdr.sock:live:workspace:wK
1789560291090  live@%2F...%2Fherdr.sock:live:workspace:w1F
1789559866812  live@%2F...%2Fherdr.sock:live:tab:w1F:t2
1789558518939  live@%2F...%2Fherdr.sock:live:workspace:wC
```

两个稳定性前提也成立：

- socket 路径固定（`~/.config/herdr/herdr.sock`），scope 不会因重启漂移
- workspace / tab id（`w1V`、`w1F:t2`）由 Herdr 持久化在 `session.json`，跨重启稳定

即 `history[historyKey(item.id)]` 现在就能取到值，**只是 browsing 分支没去读它**。

### 方案

在 browsing 分支中，对 Workspace / Worktrees / Tabs 增加 recency 排序，优先级为：

1. `lastVisitedAt`（Herdr 的真实访问历史）——目前恒为 undefined，**保留此分支以便 Herdr 将来补上 API 时自动优先**，因为它覆盖所有访问，不止经由 Omni 的那些
2. `recent`（Omni 自己的选择记录）——当前实际生效的信号
3. `index`（Herdr 源顺序）——没有任何记录的项，保持原序排在后面

效果等价于 VS Code Ctrl+P：有记录的按最近使用浮到顶部，没记录的按原顺序跟在后面。

明确不改的部分：

- Agents 的 `agentActivity()` 排序不动
- Actions 继续不参与 recency（动作列表的位置需要可预测，否则肌肉记忆失效）
- 有 query 时的行为不变（score 主导，recency 已经是 tiebreaker）

### 改动点

- `src/search.ts:78-85` 的两个分支合并调整：把 browsing 分支从「仅 Workspace/Worktrees + 仅 lastVisitedAt」扩展为「Workspace/Worktrees/Tabs + lastVisitedAt → recent」
- `src/live.ts:32` 的注释需要同步更新：不伪造 Herdr 访问历史的原则不变，但要说明浏览排序现在使用 Omni 自身的选择记录
- 无需改动 `history.ts`、`main.ts`——记录侧已经完整

### 待定问题（实施前需决定）

**当前所在的 workspace 要不要置顶？** 它是记录里最新的一条，按上述规则会排第一，但它恰恰是最没用的跳转目标——你已经在那儿了。VS Code 的 Ctrl+P 会把当前文件排除在首位之外。

两个选项：

- 置底或降权：跳转列表只放「别处」，符合 Ctrl+P 直觉
- 保持置顶但标记：与 0.8.x 已有的 `●` + `current` 标记（Agents 用）保持一致

倾向前者，但需要实际用一下才能定。

### 验收标准

- 打开 palette 不输入，Workspaces / Tabs 两个分类下最近选择过的排在最前
- 从未选择过的项保持 Herdr 原顺序，跟在有记录的项之后
- 有 query 时排序与改造前一致（score 仍然主导）
- Actions 分类顺序不受影响
- history.json 不存在 / 不可读时，退回源顺序且不报错（`loadHistory()` 已有此保证）

---

## 待记录

本次讨论中确认但尚未展开的其他问题，按优先级：

1. 前缀语义与 VS Code 肌肉记忆相反（`>` 在此为 agents，VS Code 中为命令）
2. 长列表缺少 PageUp/PageDown
3. transcript 查询为整句 literal 匹配，不支持分词 AND（`src/sessions.ts:64`）
4. 每次弹窗重新枚举会话，无缓存
5. 更新弹窗抢占首次按键（`src/palette.ts:556`）
6. `redraw()` 全量重建，progress 事件未节流

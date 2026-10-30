# E2E（Playwright 真实交互测试）

6 条关键流程，**全部打真实后端**（不 mock、不打桩），浏览器里真点、真拖、真连线。

| 文件 | 流程 | 关键交互 |
| --- | --- | --- |
| `01-login-task-list.spec.ts` | 登录 → 任务列表（+ 工作任务目录三视图） | 填表登录、点侧边栏目录「工作任务」下的子菜单「任务列表」、目录标题只展开不导航、三个子菜单各进对应视图（`/task/list` `/task/board` `/task/gantt`）、刷新后仍在线 |
| `02-task-crud.spec.ts` | 建任务 → 编辑 → 删除 | 新建弹窗（标题 / 优先级下拉 / 日期面板）、行内编辑、删除确认 |
| `03-board-drag.spec.ts` | 看板拖拽 | 五列断言、HTML5 原生拖拽 TODO→DOING（计数 + 刷新后仍在）、TODO→DONE 跳级**回弹 + 错误文案** |
| `04-flow-editor.spec.ts` | SVG 流程编辑器 | 面板 pointer 拖 3 节点进画布、拖锚点连线、保存、重开整图还原、校验、发布 |
| `05-approval.spec.ts` | 审批待办 → 同意 | 待办卡片进详情、填审批意见、同意、实例状态 APPROVED |
| `06-task-detail-tabs.spec.ts` | 任务详情 4 主 Tab（E9） | 真人任务：头部单行 / 3 Tab + 徽标数字（子任务数、协作=评论+附件+工时）/ 描述折叠-展开几何断言 / 协作三段分段 / 子任务新建 / 评论 / 附件上传+下载 / 工时登记 / 状态流转 PATCH 200 / 关抽屉重开 Tab 复位；AI 任务：多出「AI 产出 N」且数字 = 列表条数 |

## 跑法

```bash
pnpm add -D @playwright/test        # 已入库，无需重复
pnpm exec playwright install chromium

pnpm test:e2e                                   # 默认打 http://172.18.10.144
E2E_BASE_URL=http://127.0.0.1:5199 pnpm test:e2e   # 打本地 vite（BACKEND_ORIGIN 指向真后端）
E2E_USERNAME=xxx E2E_PASSWORD=yyy pnpm test:e2e    # 换账号

pnpm test:unit                                     # 纯函数单测（node --test，零依赖，见 tests/unit/）
bash scripts/run-full-rounds.sh 3 65               # 连跑 3 轮全量，每轮间隔 65s（生产限流要求，见下）
node scripts/stress-board-drag.mjs 12 2000         # 单条 03 压测（偶发红复现用，结果落 e2e-stress/out/）
```

- `workers: 1`：用例读的是同一份线上数据（看板列计数、待办列表），串行才稳定；
- 失败留证据：`screenshot: only-on-failure` + `trace: retain-on-failure`，报告在 `e2e-report/`；
- CI：`CI=1` 时自动重试 1 次并额外产 JUnit（`e2e-report/junit.xml`）。
- 03 那条偶发红的现场会自动落到 `test-results/diag/*.json`（见「流程3 偶发红」一节）。

## 限流与连跑间隔（生产 300 req / 60s）

生产 `backend/config/config-prod.toml` 开着全局限流：

```toml
[middleware.rate_limit]
enabled = true
requests_per_window = 300
window_secs = 60
```

- 实现是框架 `alun-web 0.4.2` 的 `RateLimitLayer`：**按客户端 IP**（nginx 覆写的 `X-Real-IP`）
  做 60 秒固定窗口计数，超限直接 `429` + `{"code":429,"msg":"请求过于频繁，请稍后再试"}`。
- **一轮全量 e2e ≈ 264 条**后端请求（2026-10-30 12:40:12–12:41:05 实测：264 条、
  0 条 429、1 条设计内 409）。所以**同一 IP 60 秒内只许跑一轮**；两轮挤进 60s ≈ 520 条，
  必然打满。
- 症状不是「登录失败」，而是**用例拿到 429 后数据为空 → 断言干等到超时**：
  2026-10-30 12:25:34 实测 `GET /api/business/flow/task/todo → 429`，
  流程5 待办卡片 20s 找不到 → 红；冷却 ≥65s 单跑即绿。
- **跑法约束（唯一允许的做法）**：轮与轮之间至少 **60s**，用
  `bash run-full-rounds.sh <轮数> <间隔秒>`（默认 `3 65`）——脚本每轮结束复核 `[E2E]` 残留条数
  并打印起止时间，卡着间隔跑，别再手工连点。
- **是否给 e2e 账号 / IP 开豁免：不开。** 依据：
  ① 限流键是 **IP**，账号白名单解决不了（同一台机器上任何脚本共用一个额度）；
  ② 框架 0.4.2 的 `[middleware.rate_limit]` 只有 `enabled / requests_per_window / window_secs`
     三个开关，**没有白名单 / 豁免配置项**，要开得改框架中间件（跨仓库、跨版本）；
  ③ 调大 `requests_per_window` 是在为测试方便削弱线上防护（真实用户也吃这 300 的额度），不划算。
  → 结论：用「间隔 65s + 一轮一跑」这条跑法约束收口；真要开豁免，另立后端卡在框架层做
  「按 IP 白名单」并把配置项补上，不要动生产数值。
- 另有一层**独立的**登录限流（10 次 / 60s / 账号+IP），已由 `global-setup.ts`「只登录一次 +
  复用 storageState」规避，用例里不要自己再登（详见该文件的注释）。

## 数据与清理

- 名字统一带 `RUN`（每次运行唯一的 `e2e<base36>` 戳）与 `[E2E]` 前缀，便于检索；
- 预置数据走真接口（`support.ts` 的 `newApi()`，就是测试账号的登录态），不是插库；
- 用例结束 `afterAll` 自己清：任务 `DELETE /business/task/{id}`；流程先撤回在跑实例
  （`/instance/{id}/withdraw`，否则后端拒绝删定义）再 `DELETE /business/flow/def/{id}`；
- 审批实例**没有删除接口**，只能走到终态（APPROVED/REJECTED/WITHDRAWN）+ 流程软删；
  需要彻底清库时按 `title like '[E2E]%'` / `flow_code like 'e2e_%'` 归零。

## 已知前提

- **首次登录的「新功能引导」（E10）会挡点击**：`cp-onboarding-done` 不在 localStorage 时，
  `BasicLayout` 会浮一层 `role=dialog` 的引导卡，`intercept pointer events` 把所有点击挡掉
  （症状是 `locator.click` 15s 超时，日志里 `subtree intercepts pointer events`，而元素本身
  `visible, enabled and stable`）。`global-setup.ts` 已把该键写进 storageState，所有用例免弹，
  用例里**不用**再写 `addInitScript`（01 那条测登录的走干净上下文，引导会正常出现，
  只是它不挡这条链路要点的东西）。
- **原生 HTML5 拖拽别用 `locator.dragTo()`**：一次性位移下 Chromium 会让 `drop` 落回**源元素**
  （看板列 `handleDrop` 见 `from === to` 直接 return → 卡片不动、也不报错，断言只看到 `toHaveCount 0`）。
  用 `support.mouseDrag(page, 源元素, 落点)`：分 25 步移动 + 落点前补两小步再松手。
- **任务清理要连子任务一起删**：`DELETE /business/task/{id}` 只软删父任务，子任务仍留在列表里。
  用例标题带 `RUN` 唯一戳，清理按**标题前缀**把父 + 子逐条删并复核残留 0（生产库有别人的数据，别模糊删）。
- **列表搜索不匹配任务号**：只能按标题关键词搜，再用 `.filter({ hasText: taskNo })` 精确定位行。
- `business:flow:*` 的菜单在生产上是 `is_visible=false`（D5 交付时按卡面用 URL 直达），
  所以流程编辑器 / 审批中心的用例**按 URL 进入**，其余交互仍是真实点击；
- `flow/def` 新建的默认图是「开始 → 结束」，用例在其上扩展；落点可能与默认节点重叠，
  故拖完节点先点一次「自动布局」再连线（否则锚点被别的节点盖住点不到）。
- AI 任务（`exec_mode=AI`）的发现走接口：列表不下发 `exec_mode`，逐个拉详情判定，且要求
  `ai-workspace-files.total > 0`；生产库里没有这样的任务时该条 `test.skip`（不空转、不假绿）。

## 流程3 偶发红：现象 / 根因 / 复跑矩阵

### 现象（2026-10-30 12:20 那轮，至今唯一一次）

| 侧 | 观测 |
| --- | --- |
| e2e 断言 | `expect(col('DOING').locator('.board-card').filter({hasText:'[E2E]拖拽-正常-…'})).toHaveCount(1)` → 44 次解析全是 `0`，20s 超时 |
| 后端 journal（同刻） | `{"message":"任务状态已流转","from":"TODO","to":"DOING"}` + `PATCH /api/business/task/{id}/status -> 200 (27ms)` |
| 后端 journal（之后 20s） | **没有任何** `GET /business/task/page`（说明不是「重拉列表把卡片刷没了」） |
| 结论 | 服务端已落地，**页面本地把这步丢了**（卡片既不在 DOING，也没触达任何接口） |

### 根因结论

1. **最初怀疑的「回执缺 `parent_progress`」被证伪**：生产实测 `PATCH .../status` 的
   `data.parent_progress` **恒为数组**（顶层任务 `[]`；后端是 Rust `Vec<ParentProgressRes>`，
   不可能是 `null`）。所以那次不是缺字段。
2. 唯一能把卡片从目标列抹掉、又完全不产生后续请求的代码路径是 `TaskBoard.vue#moveTask` 的
   `catch`：当时 `try` **同时罩住「PATCH 请求」和「回执处理」**，任何一步抛错都走**整体回弹**
   （两列快照还原 + 错误 toast）。能触发它的只剩两类：请求 promise 被 reject
   （响应在链路上丢 / 响应体解析失败），或回执处理抛错。
3. **修法不赌是哪一类，把边界收紧**（`moveTask` 现在长这样）：
   - `try/catch/finally` **只罩 `changeTaskStatus`**；catch 里才回弹（`return` 前跑 `finally` 清 pending）；
   - 拿到 200 后走 `components/board-receipt.ts#applyStatusReceipt()`：`null` 回执 / 缺 `status` /
     缺 `parent_progress` / `parent_progress` 不是数组 / 进度不是数 → 一律安全跳过或兜底，
     **永不回滚**；真出岔子只 `console.error('[task-board] …')`；
   - 子任务勾选（`toggleChildDone`）也走同一套边界。
4. **单测**：`tests/unit/board-receipt.test.ts`（`node --test`，零新依赖，10 例）。
   改前（把 `moveTask` 里的原逻辑原样搬进纯函数）**6 例红**，典型报错
   `TypeError: list is not iterable`；改后 **10/10 绿**。

### 加固：下次偶发红能自证

`support.ts` 新增 `installBoardProbe(page)` + `withDiagnostics(probe, testInfo, label, body)`，
03 的两段关键断言（拖到进行中 / 跳级回弹）都包在里面。断言失败时自动落盘
`test-results/diag/<label>-<ts>.json`（+ attach 到报告 + 控制台打印 `[diag:...]`），
内容是**四件套现场**：两列卡片标题、列计数、当时页面上的 toast 文案、console 的
error/warning（Vue 运行时错误也在里面）、`business/task` 接口流水（含 `requestfailed`）。
→ 下次再红，先看这个文件：`toasts` 里是「已移动到「进行中」」还是「状态流转失败…」，
就能一刀切开「服务端成功但前端回滚」和「请求压根没成功」。

反向对照已验证（故意断言 DOING 列有 99 张卡 → 测试红 + `test-results/diag/negative-control-*.json` 落盘）。

### 复跑矩阵

| 时间 | 跑法 | 结果 |
| --- | --- | --- |
| 12:20:35 | 全量（第 1 次，刚换 dist） | **03 红**（本次要收口的那次） |
| 12:24–12:31 | 03 单跑 ×1 + 全量 ×3 | 全绿，未复现 |
| 12:29 / 12:31 | 全量 ×2（发布验收） | 8/8 |
| 12:40–12:44 | **全量 ×3，间隔 65s**（本卡验收） | 8/8 × 3（45.6s / 43.7s / 48.6s），`[E2E]` 残留 0 |
| 12:37–12:40 | **单条 03 压测 ×12**（随机 0–2s 抖动，记录 DOM/console/网络时间线） | 12/12 绿，未复现 |
| 12:46 | 修复后的 dist（本地 5292 反代真后端）全量 | 8/8 |

→ 12 + 3 + 2 + 4 ≈ 21 次跑到，**未再复现**。所以这条不当「回归基线必红的坑」看：白天不会红，
但一旦红，`test-results/diag/*.json` 会告诉我们它到底是「请求没落地」还是「回执处理抖了一下」——
两种都已经被 `moveTask` 的新边界挡在回滚之外了。


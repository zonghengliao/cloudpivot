# T7b 看板视图 · 验证证据

- 任务：`t_ff10fbc4`（`【任务管理·T7b】看板视图（五列+原生拖拽改状态+乐观回弹+打回必填原因）`）
- 功能提交：`a57ed71` — `feat(task): T7b 看板视图 —— 五列 + HTML5 原生拖拽改状态 + 乐观回弹 + 打回必填原因`
  （5 files changed, 1371 insertions(+), 49 deletions(-)，**只动 `frontend/`**）
- 文档提交：`6d90e6b`（本文件 + 复现脚本；`git diff a57ed71..6d90e6b -- frontend/` 为空 → 生产 dist 与 HEAD 的前端源码一致）
- 三方一致：`HEAD` == `origin/main` == `bare main`（收尾 `git rev-parse` 三处同值；本文件自身的提交会推高 HEAD，故这里不写死最终哈希，**功能代码哈希见上一行 `a57ed71`**）；`/opt/cloudpivot` 工作区 0 改动
- 生产：`http://172.18.10.144/`（nginx root = `/opt/cloudpivot/frontend/dist`，dist 构建于 **17:34:54**，晚于合并）
- 环境：`cloudpivot` 库 + 真实 admin 表单登录（`Input.insertText` 敲键 → 点「登录」）；拖拽用 **CDP 原生拖拽管线**（`Input.setInterceptDrags` + `dragIntercepted` + `dispatchDragEvent(dragEnter/dragOver/drop)`），不是合成 DOM 事件
- 截图：`docs/evidence/t7b-board.png`（**清理后**的生产看板：五列齐全、已取消列默认折叠、5 条真实任务；验收期 68 条时的形态见下文第 2、11 节文字证据）
- 复现脚本：`scripts/verify/t7b_board_fixture.sql`（临时账号/角色）、`t7b_board_setup.py`（造 7 条覆盖 5 状态）、`t7b_board_page.py`（造 55 条验加载更多）、`t7b_board_cleanup.sql`（软删清理）

## 一、交付文件

```
frontend/src/views/business/task/components/TaskBoard.vue   看板主体（五列 / 拖拽 / 乐观回弹 / 打回弹窗 / 加载更多）
frontend/src/views/business/task/components/BoardCard.vue   看板卡片（优先级 + 任务号 + 标题 2 行 + 进度条 + 负责人 + 截止日）
frontend/src/views/business/task/index.vue                  三视图切换 + URL query 持久化 + 看板挂载
frontend/src/views/business/task/gantt.vue                  甘特工具条补同款三视图切换（原来只有「返回列表」）
frontend/src/api/business/task-api.ts                       + changeTaskStatus（PATCH {to, reason?}）+ TASK_ERROR_CODE
```

> 未新增任何依赖（`package.json` 0 改动）：拖拽是 HTML5 原生 `draggable` + `dragstart/dragover/drop`。

## 二、功能验收（13 条逐条真机）

### 1. 三视图切换 + URL 持久化 ✅

| 操作 | URL | 选中态 | 页面 |
|---|---|---|---|
| 进 `/task` | `/task` | `列表*` | 表格 10 行（分页） |
| 点「看板」 | `/task?view=board` | `看板*` | 5 列看板 |
| **刷新（F5）** | `/task?view=board` | `看板*` | 5 列仍在，58 张卡片 |
| 点「甘特」 | `/task/gantt` | — | 甘特页（`.gantt-page`） |
| 甘特页点「看板」 | `/task?view=board` | `看板*` | 回到看板 |

### 2. 五列分组 + 计数与库一致 ✅

造 7 条 `T7b验收-*`（覆盖 5 状态 + 一条「admin 创建 / qa.boarder 负责」的待验收）后：

```
看板渲染：TODO 4 / DOING 2 / REVIEW 3 / DONE 1 / CANCELLED 1 = 11，工具条「全部可见任务 · 共 11 条」
库：      TODO 4 / DOING 2 / REVIEW 3 / DONE 1 / CANCELLED 1（select status,count(*) from biz_task where deleted_at is null）
```

### 3. 卡片字段完整 ✅

`TODO` 列实测（P0 11-01 → P0 11-15 → P1 10-27 → P2 10-30，即 `priority ASC → due_date ASC`）：

```
P0 | TSK-202610-000001 | 对接 ERP 库存接口   | 0%   | 超级管理员 | 11-01 | title 属性=标题
P0 | TSK-202610-000018 | T7b验收-待开始-正常 | 0%   | 超级管理员 | 11-15
P1 | TSK-202610-000017 | T7b验收-待开始-逾期 | 0%   | 超级管理员 | 10-27（逾期红）
P2 | TSK-202610-000024 | 【老六验证】T5b…     | 0%   | 老六测试号 | 10-30
标题：`-webkit-line-clamp: 2` + `overflow: hidden` + `title` 属性（悬停看全称）
卡片全部 `draggable="true"`
```

### 4. 逾期标红（主题变量，非写死 hex）✅

```
截止 2026-10-27（今天 10-29）→ class `board-card__due--overdue`，计算色 rgb(193, 123, 126)
  = 当前 mist-blue 调色板的 --td-error-color（#c17b7e）
非逾期 → rgb(66, 97, 103) = --td-text-color-secondary
DONE / CANCELLED 不算逾期（代码里显式排除）
```

### 5. 拖 TODO→DOING：成功（卡片移动 + 计数 + 落库）✅

```
拖前：TODO 4 / DOING 2     拖后：TODO 3 / DOING 3
toast：已移动到「进行中」
库：TSK-202610-000018  status=DOING  progress=10（后端状态联动把 0% 抬到 10%，前端用回执校正显示）
```

### 6. 拖 TODO→DONE（跳级）：乐观 → 回弹 + 具体文案 ✅

用 `MutationObserver` 记录每一帧列计数（10ms 定时器会漏帧）：

```
t=103049  before  TOD:3 DOI:3 REV:3 DON:1 CAN:1
t=103363  dom     TOD:2 DOI:3 REV:3 DON:2 CAN:1   ← 乐观更新（卡片已挪到「已完成」）
t=103375  dom     TOD:3 DOI:3 REV:3 DON:1 CAN:1   ← 12ms 后回弹（后端 409+63010）
toast：该状态流转不被允许（如 TODO 不能直接到 DONE）
```

### 7. 拖 REVIEW→DONE（无验收权账号）：回弹 + 权限文案 ✅

临时账号 `qa.boarder`（角色 `T7B_BOARDER`：只授 `business:task:read` + `business:task:status`，`data_scope=4` 仅本人），
任务 `TSK-202610-000023` 由 **admin 创建、qa.boarder 负责**（所以他是「负责人」而不是创建人/分派人/管理员）：

```
登录 qa.boarder（真实表单登录）→ /task?view=board 只看到 1 条（数据范围生效）
拖前：REV:1 DON:0
TOD:0 DOI:0 REV:0 DON:1   ← 乐观
TOD:0 DOI:0 REV:1 DON:0   ← 回弹（后端 403+63011）
toast：你没有权限执行该流转（待验收需验收人操作）
```

### 8. 拖 REVIEW→DOING：先弹窗填原因（不填不发请求；取消卡片不动）✅

```
① 松手 → 弹窗「打回原因」（列计数不变、无 toast、无请求）：任务「T7b验收-待验收」从待验收打回进行中…
② 空原因点「提交打回」→ 行内红字「请填写打回原因，至少 5 个字（当前 0 个）」，弹窗不关，请求未发
③ 填 2 字「太短」→ 「…（当前 2 个）」，请求未发
④ 点「取消」→ 弹窗关闭，卡片仍在 REVIEW，无任何请求
⑤ 重拖 → 填「验收标准未对齐，单测未覆盖」→ 提交
   乐观：DOING 5 / REVIEW 2 → 确认，toast「已移动到「进行中」」
   库：TSK-202610-000020 status=DOING progress=10 reject_reason=验收标准未对齐，单测未覆盖
```

### 9. CANCELLED 列默认折叠，可展开 ✅

```
初始：col--collapsed=true，只有「已折叠 1 条」竖排窄条（132px），卡片 0 张，列头计数 1
点展开：出现 1 张卡片（TSK-202610-000022），按钮 title 变「折叠已取消列」
（把卡片拖进折叠的已取消列时会自动展开，否则折叠态没法当落点）
```

### 10. 点卡片打开详情抽屉 ✅

```
点 TODO 首张 → `.t-drawer.task-detail` 打开
header「任务详情 · TSK-202610-000001」，标题「对接 ERP 库存接口」，徽标 待开始 / P0 / 需验收
流转按钮 4 个仍 disabled（T6a 口径，本卡不动详情里的按钮），编辑 / 删除可用
```

### 11. 每列 50 条 + 加载更多 ✅

```
造 55 条 `T7b分页-*`（全部 TODO）后：
  TODO 列：计数 58，实际渲染 50 张，底部「加载更多（还有 8 条）」
  点一下：58 张、去重 58（无重复）、按钮消失
  跨页排序仍成立：前 5 张 P0（11-01/02/05/06/09）→ 尾部 P3（11-21/24/25）
```

### 12. `pnpm typecheck && pnpm build` ✅

```
/root/t7b_w1/frontend（干净 clone + 本次提交源码）：vue-tsc 0 error，vite build ✓，pnpm check（色板断言 330 条 + typecheck）通过
/opt/cloudpivot/frontend（合并后生产工作区）：vue-tsc 0 error，vite build ✓ ← §6.2 用的就是这一次产物
```

### 13. 其它顺带验到的点 ✅

| 场景 | 结果 |
|---|---|
| 关键词筛选（看板） | 输入 `T7b验收` → 查询 → 各列 1/3/1/1/1，共 7 条，标题全部命中 |
| 列内空态 | 关键词 `zzz-不存在的任务` → 4 列「暂无任务」+ `border-style: dashed` 淡色虚线框，计数全 0 |
| 重置 | 回到 58/5/2/1/1，共 67 条（当时含 55 条分页数据） |
| 只看我的任务（看板） | 开 → 61 条，owner 全为「超级管理员」；关 → 回 67 条 |
| 看板内新建 | 点「新建任务」建 `T7b验收-看板新建` → 弹窗关闭、看板自动重拉（TODO 59、总 68）、新卡片在列内按排序落位 |
| 列头 | 「待开始 59 / 进行中 5 / 待验收 2 / 已完成 1 / 已取消 1」 |
| 布局 | 1280×633 下 4 列 ×258px + 折叠列 132px，超出横向滚动；卡片不溢出所属列 |

## 三、§6.2 部署验收（生产）

| 步骤 | 结果 |
|---|---|
| `/opt/cloudpivot` 合并 | `git fetch && git merge --ff-only origin/main` → `7364def → a57ed71`，工作区 0 改动 |
| `pnpm typecheck && pnpm build` | 在 `/opt/cloudpivot/frontend` 通过；dist 重新生成（17:34:54） |
| 产物落到 nginx 目录 | nginx root = `/opt/cloudpivot/frontend/dist`；`index.html` sha256 `03e82cf31de76efcc70d0fd4e2d6f691cf44ca31aec21fe6b2868e88bc66fc57`，引用 `assets/index-DLCGlMTy.js` |
| 产物含本卡逻辑 | `dist/assets/index-CeRAj9N_.js` 内命中「该状态流转不被允许」与「打回原因」 |
| 真实登录（人类路径） | `http://172.18.10.144/login` → 输入 admin 口令 → 跳 `/dashboard`，`localStorage.x_access_token` 有值，页面显示「超级管理员」 |
| 登录后看到看板 | `/task?view=board` → 标题「工作任务 - 云枢管理后台」，5 列渲染，工具条「全部可见任务 · 共 68 条」（当时含验收数据） |
| 生产上真拖一次 | CDP 原生拖拽 `TSK-202610-000017` TODO→DOING → toast「已移动到「进行中」」，库 `status=DOING progress=10` |
| 清理后复核 | 软删全部 `T7b*` 验收数据 + 停用临时账号/角色后：`/task?view=board` 显示 5 条真实任务（TODO 2 / DOING 2 / REVIEW 1 / DONE 0 / CANCELLED 0），「已完成」列显示空态 |

## 四、偏差与已知边界（都写进代码注释）

1. **列头不做「总预估工时」**（卡面是可选项）：`TaskCardRes` 没有 `estimate_hours`，只有详情 `TaskDetailRes` 有；不为一个可选字段去改后端 DTO。
2. **一次多状态查询不可行**：后端 `TaskPageQuery.status` 走 `dto::normalize_status`，**只收单值**（`status=TODO,DOING` 会 400），所以是 5 次并发请求（每列 `page_size=50`）。
3. **「我的任务」模式**走 `GET /business/task/my` 一次拉全量后本地按状态分组；该接口不吃 `keyword/priority/due_*`，这三个筛选在本地过一遍（保证与列表页口径一致）。
4. **列内排序是规则驱动**（`priority → due_date → created_at DESC`），**不支持列内手动排序**：卡面没要求，且看板上「看起来落在哪儿」就是「刷新后落在哪儿」。
5. **拖进折叠的已取消列会自动展开**：否则折叠态没有落点；这是刻意的交互补偿。
6. **错误文案判据用业务码而非 HTTP 码**：`api/request.ts` 已把 HTTP 层错误规整成 `ApiError`（只保留后端 `code`），所以按 `63010 / 63011 / 63013 / 63006 / 63001` 翻译；未命中的码兜底用后端 `msg`（后端 msg 本身就具体）。
7. **headless 隐藏标签页里 Vue transition 不结束**：弹窗/路由切换会「看着卡住」（`document.visibilityState=hidden` 时 leave 动画不触发 transitionend）。`Page.bringToFront` 之后一切正常 —— 与 T6a 记录的现象同源，**不是本功能 bug**；证据里所有弹窗相关断言都是在 `visible` 状态下取的。

## 五、数据清理（生产无残留）

| 项 | 处理 | 复核 |
|---|---|---|
| `T7b验收-*` / `T7b分页-*` 共 63 条任务 | 软删（`deleted_at`） | `t7b_live = 0` |
| 临时账号 `qa.boarder` | 软删 + 停用 + 解绑角色 | `qa.boarder_live = 0` |
| 临时角色 `T7B_BOARDER` | 软删 + 停用 + 撤销 `sys_role_menu` 授权 | `t7b_role_live = 0` |
| 生产未删任务 | 5 条（原有 3 条 + T5b 造的 2 条 `【老六验证】`） | 看板 = 库计数 |

> 本卡未改 `backend/`、未动 `package.json`、未新增迁移。

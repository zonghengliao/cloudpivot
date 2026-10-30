# 批次 E · E6a 子任务基础 —— 验证证据

> 卡片：`t_39bed2c9`　执行：manong　日期：2026-10-30（CST）
> 目标：列表/详情返回子任务统计（`child_count` / `child_done_count`）+ 前端 API 层补子任务函数 + 详情页「+ 新建子任务」入口

## 0. 产物指纹

| 项 | 值 |
|---|---|
| 功能提交 | `94075f9`（后端 3 + 前端 3） |
| 修复提交 | `5764f42`（`detail.vue` 单 watcher 修正，真机验收后补） |
| HEAD / origin/main / bare main | `5764f423791979dedf66c8edd60d986d81f31985`（三者一致） |
| release 二进制 | `6e15bc9d39ec5daf03aa59c84cbc2076e67aaa8e7dd7725b953e4d31ae925ceb` |
| dist 新文案 chunk | `frontend/dist/assets/detail-B5XG1Eu5.js`（含「新建子任务」） |
| 迁移 | **0**（无 DDL 变更，仅 SELECT 派生列） |
| 新接口 | **0**（复用 `POST /business/task/{id}/subtask`、`GET /business/task/{id}/subtasks`） |

提交文件（6 个，比卡面 4 个多 2 个，原因见 §6）：

```
backend/src/business/task/model.rs        +25   CARD_COLUMNS / TaskCardRow / from_row / find_raw / 单测
backend/src/business/task/dto.rs          +2    TaskCardRes.child_done_count
backend/src/business/task/handler.rs      +1    card_res 映射（不改就编译不过）
frontend/src/api/business/task-api.ts     +63    createSubtask / listSubtasks / 两个类型 / TaskCard 补字段
frontend/src/constants/permissions.ts     +12   PERM.taskSubtask
frontend/src/views/business/task/detail.vue +249 子任务区块常显 + 新建弹窗 + 行可点 + 返回父任务
```

## 1. 后端：派生列与索引

`CARD_COLUMNS`（`model.rs:465` 起，被 `page_visible` / `find_detail_with_visibility` / `list_children` /
`my_tasks` / `scan_overdue` 共用）：

```sql
(SELECT count(*) FROM biz_task c WHERE c.parent_id = t.id AND c.deleted_at IS NULL) AS child_count,
(SELECT count(*) FROM biz_task c WHERE c.parent_id = t.id AND c.deleted_at IS NULL
    AND c.status = 'DONE') AS child_done_count,
```

`find_raw`（写路径详情，手写列清单）同源补齐 —— 漏这处会让「新建/编辑任务」直接 500。

### EXPLAIN（隔离库 cloudpivot_e6a，`SET enable_seqscan=off` 强制暴露索引可选择性）

```
Seq Scan on biz_task t
  Filter: (deleted_at IS NULL)
  SubPlan 1
    ->  Aggregate
          ->  Index Scan using idx_biz_task_parent on biz_task c
                Index Cond: (parent_id = t.id)
                Filter: (deleted_at IS NULL)
  SubPlan 2
    ->  Aggregate
          ->  Index Scan using idx_biz_task_parent on biz_task c_1
                Index Cond: (parent_id = t.id)
                Filter: ((deleted_at IS NULL) AND ((status)::text = 'DONE'::text))
```

两条子查询都走 `idx_biz_task_parent`（等值 `parent_id`），**无 `Seq Scan on biz_task c`**；
验收库里数据量只有个位数行，planner 对主表本身选 Seq Scan 属正常（主表无可用等值条件）。

### 单测

新增 `business::task::model::tests::card_columns_expose_child_stats`：断言两条 `AS child_count/child_done_count`、
`c.deleted_at IS NULL`、`c.status = 'DONE'`、`c.parent_id = t.id`，并**反向断言不含 `CANCELLED`**（取消不算完成）。
防的是「少一列 → `TaskCardRow::from_row` 运行期 `get_typed` 失败 → 列表/详情 500」，现有连库用例覆盖不到。

```
cargo test   → 824 passed; 0 failed   （基线 823 → +1）
cargo clippy --all-targets -- -D warnings → 无告警
cargo check --all-targets → Finished
```

## 2. 接口实测（隔离实例 127.0.0.1:8027 / 新库 cloudpivot_e6a，不碰 prod 8023 · dev 8024 · 别人的 8030）

脚本 `/root/e6a_verify.py`（28 条断言全过）：

| # | 断言 | 结果 |
|---|---|---|
| 1 | `GET /health` code=0（profile=test） | ✅ |
| 2 | admin 登录（新库首登强制改密，脚本自动走 `PUT /api/system/auth/password` 后重登） | ✅ |
| 3 | 建 1 父 + 3 子（第 1 个置 DONE） | ✅ 4/4 |
| 4 | `GET /api/business/task/page` 列表项含 `child_count` / `child_done_count` | ✅ |
| 5 | 父任务 `child_count == 3` | ✅ |
| 6 | 父任务 `child_done_count == 1` | ✅ |
| 7 | 无子任务任务 `child_count == 0` 且 `child_done_count == 0` | ✅ |
| 8 | `GET /api/business/task/{id}` 详情 `child_count=3 / child_done_count=1` | ✅ |
| 9 | 详情 `children.length == 3` | ✅ |
| 10 | `GET /api/business/task/{id}/subtasks` `total=3`、`list.length=3` | ✅ |
| 11 | 索引 `idx_biz_task_parent` 存在 + 两条子查询命中 | ✅ |

响应摘要（真实返回）：

```
列表样例：{"task_no":"TSK-202610-000001","child_count":3,"child_done_count":1}
详情样例：{"task_no":"TSK-202610-000001","child_count":3,"child_done_count":1,"progress":0}
```

## 3. 前端：详情页「子任务」区块 + 新建入口（真机 prod）

浏览器（headless Chromium，`Emulation.setFocusEmulationEnabled`，全部断言读 `textContent`）：

1. **0 子任务时区块常显**（改造前是 `v-if="children.length > 0"` 整块隐藏）：
   `子任务 0 + 新建子任务 暂无子任务`，按钮 `disabled=false`；网络面板确认 `GET /api/business/task/{id}` 已发出。
2. **弹窗字段齐全**：`标题（必填）/ 预估工时（小时，可空）/ 负责人（可空，留空 = 我自己）/ 优先级（默认继承父任务）/ 截止日期（可空，留空 = 继承父任务）`；
   优先级回显 `P2 中`（= 父任务优先级，验证「默认继承」）。
3. **新建成功**：填标题 → 点「创建」→ toast `子任务已创建`、弹窗关闭、区块变
   `子任务 1 已完成 0 / 1 + 新建子任务 待开始E6A-UI验收-子任务A超级管理员0%`（列表随即刷新，父任务进度由后端重算）。
4. **子任务行可点**：点击行 → 同一抽屉就地切到子任务详情（`任务详情 · TSK-202610-000149`），
   header 出现 `← 返回父任务 TSK-202610-000148`，无错误态、无控制台报错；
   子任务自身的「+ 新建子任务」按钮 `disabled=true`（子任务不能再往下拆，与后端 `validate_parent` 一致）。
5. **进度重算 + child_done_count 呈现**：子任务 `PATCH /progress 50` → 父任务 `progress` 0 → 50；
   子任务置 DONE 后再触发一次重算 → 父任务 `progress=100 / child_count=1 / child_done_count=1`，
   抽屉里显示 `进度100%`、`子任务 1 已完成 1 / 1`、行徽标 `已完成`。
6. 截图：`e6a_detail_subtask.jpg`（随任务附件提交）。

## 4. 静态检查

```
pnpm typecheck            → 0 error（vue-tsc --noEmit）
pnpm build                → 成功，dist/assets/detail-B5XG1Eu5.js 命中「新建子任务」
grep -rl 新建子任务 dist  → dist/assets/detail-B5XG1Eu5.js
```

## 5. 生产部署验收（§6.2 + §6.2.1）

```
== release 重编（按 HEAD 工作树）
6e15bc9d39ec5daf03aa59c84cbc2076e67aaa8e7dd7725b953e4d31ae925ceb  target/release/cloudpivot
== 同盘 mv 替换（cp 会 Text file busy）→ 一致
== systemctl restart cloudpivot.service
HEALTH OK after 2s
{"code":0,"msg":"ok","data":{"status":"ok","app":"cloudpivot","profile":"prod",...}}
systemctl is-active → active
启动日志：检测到 265 条 #[permission] 规则 … cloudpivot 启动完成, 耗时 33ms
```

prod 侧脚本 `/root/e6a_prod.py`（9 条断言全过）：

```
✅ prod /health code=0（profile=prod）
✅ prod admin 登录
✅ GET /api/business/task/page code=0
✅ 列表首行含 child_count / child_done_count
   样例：{"task_no":"TSK-202610-000002","child_count":0,"child_done_count":0}
✅ dist 里存在「新建子任务」文案（detail-CJuKHdM4.js → 修复后 detail-B5XG1Eu5.js）
✅ index.html 引用构建产物 assets/
✅ 新建 UI 验收父任务 / 其详情 child_count=0、child_done_count=0
```

**收尾**：prod 验收数据（父 1 + 子 1）已 `DELETE` 软删，`E6A-UI验收` 残留 0 条；
一次性令牌文件 `frontend/dist/_e6a_once.txt` 已删除；隔离实例 8027 已 kill、库 `cloudpivot_e6a` 已 drop；
dev 8024 / prod 8023 / 别人的 8030 未受影响（dev 由 `cargo-watch` 重编后自行回来）。

## 6. 与卡面的偏差（逐条说明）

1. **后端改了 3 个文件而非 2 个**：`dto.rs` 加字段后，`handler.rs::card_res` 不映射就编译不过（Rust 结构体字面量必须写全字段）。
2. **`child_done_count` 加在共享 `CARD_COLUMNS`，而不是「只在 page_visible / find_detail 两处」**：
   卡面假设 `TASK_COLUMNS` 有三份手写副本，实际代码里 `CARD_COLUMNS` 是**唯一常量、5 处共用**，
   且 `child_count` 早已在里面；若只加两处，另外 3 个查询映射到同一个 `TaskCardRow::from_row` 会因缺列 500。
   代价是 `list_children` / `my_tasks` / `scan_overdue` 也多一条 `idx_biz_task_parent` 等值子查询（行数都很小，
   `page_visible` 本来就有同形状的 `child_count` 子查询，无新增量级）。**代码里没有 LATERAL 看板查询**（`grep -rn LATERAL` 为空）。
3. **按钮权限用 `business:task:subtask`（后端注解真实码），不是卡面写的 `business:task:create`**：
   两者是不同权限点（`V0108` 种 …0767），用 `task:create` 会让「只有建任务权限」的角色看到按钮但点下去 403。
   为此在 `permissions.ts` 新增 `PERM.taskSubtask`（第 6 个 `business:task:*` 码），新增 1 个文件。
4. **额外做的两件事**（都是验收必需）：① 子任务行可点后在 header 加「← 返回父任务」（子任务 header 原本无回退路径）；
   ② 终态父任务 / 子任务本身把「+ 新建子任务」置灰（后端本来就 409/400，置灰避免白点）。
5. **修复提交 `5764f42`**：首版把 `currentId` 同步和 `loadDetail` 拆成两个 watcher，真机出现「抽屉空白且不发请求」
   （watcher 执行顺序先 loadDetail 后同步 → `currentId` 仍为 null → early return）。合并为单一 watcher 后修复，
   已重跑 typecheck + build 并重新真机验收。

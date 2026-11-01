# D7 批量操作框架 · 验收证据

> 卡面：`t_72bbbbb7` —「批量操作框架（勾选 + 批量过账/审批/导出，8 个核心页，12 单测）」
> 日期：2026-11-02 · 执行：backend-developer

## 一、交付内容

### 1.1 后端：批量接口（统一形态，逐条独立事务）

| 资源 | 接口 | 权限码 | 说明 |
|------|------|--------|------|
| 收货入库单 | `POST /business/erp/stock-in/batch-status` | `business:erp:stock-in:status` | 批量过账（`POSTED`）/ 批量作废（`CANCELLED`，作废已过账单必须给原因） |
| 收货入库单 | `POST /business/erp/stock-in/batch-delete` | `business:erp:stock-in:delete` | 批量删除（逐条判：只删草稿 / 过账审批中） |
| 收货入库单 | `POST /business/erp/stock-in/batch-export` | `business:impexp:export:create` + 函数内 `stock-in:list` | 批量导出（异步任务，落到导入导出页） |
| 采购订单 | `POST /business/erp/purchase-order/batch-status` | `purchase-order:status` / `purchase-order:submit-approval` | `SUBMITTED` 走审批入口，其余走状态流转 |
| 调拨单 | `POST /business/erp/transfer/batch-status` | `business:erp:transfer:status` | 批量提交审批（`PENDING`）/ 批量作废 |
| 序列号 | `POST /business/erp/serial/batch-status` | `business:erp:serial:status` | 批量作废（`SCRAPPED`） |
| 应付单 | `POST /business/erp/payable/batch-export` | `business:impexp:export:create` + `payable:list` | 批量导出 |
| 凭证 | `POST /business/erp/voucher/batch-export` | `business:impexp:export:create` + `voucher:list` | 批量导出 |
| 审批中心 | `POST /business/flow/task/batch-approve` | `business:flow:approve` | 批量同意 / 驳回（逐条走单条审批的**全部**业务回调与内控闸） |
| 任务 | `POST /business/task/batch-status` | `business:task:status` | 批量改状态 |
| 任务 | `POST /business/task/batch-assign` | `business:task:assign` | 批量指派（必须给 `owner_id`） |

**统一响应**（`shared/batch.rs::BatchResult`）：

```json
{ "succeeded": [{"id":"…","no":"IN-2026…"}],
  "failed":    [{"id":"…","no":"IN-2026…","code":10003,"error":"…已过账（POSTED，时间 …），不能重复过账…"}],
  "total": 5, "success_count": 3, "fail_count": 2 }
```

三条硬红线在实现里的落点：

1. **逐条独立事务**：批量层**不开事务**，逐条调用已有的单条业务函数
   （`change_stock_in_status` / `submit_approval` / `change_transfer_status` / `apply_status` /
   `flow::handler::approve_task` / `task::handler::change_status`），它们各自 `begin/commit/rollback`。
2. **失败逐条报原因**：`failed[].error` 是单条接口原文（含单号、当前状态、下一步怎么改），
   `failed[].no` 由批量层一次性 `id = ANY(?::text::uuid[])` 查出单号后回填。
3. **幂等**：重复点同一批 = `0 成功 / N 失败`（单条状态机判 409），库存不会加两遍（e2e ③ 查库验证）。

### 1.2 批量导出（复用 impexp 异步机制）

- `impexp::batch_export::create_by_ids` 直接写 `sys_export_task`（与「导入导出」页手动导出**同一条 worker 链路**）；
- `ExportFilter` 新增 `ids`（`Vec<Uuid>`）字段：非空则收窄为 `id = ANY(...)`，空则按 `keyword/status` 走筛选；
- 新增 3 个 ERP 导出定义（`impexp/builtin/erp_doc.rs`）：`erp.stock_in` / `erp.payable` / `erp.voucher`，
  注册进 `builtin::register`（导入导出页下拉自动出现）；
- `ids` 为空且无筛选 → **400**（防止「全选」悄悄变成「全表导出」）。

### 1.3 前端

| 文件 | 作用 |
|------|------|
| `src/components/BatchActionBar.vue` | 通用批量工具栏：勾选条数 / 本页 vs 全部筛选结果 / 按钮按 `allowedStatus` 禁用 + tooltip / 高危二次确认（带影响条数）+ 原因必填 / 指派选人 / 进度 N/M / 结果弹窗（失败逐条 + 复制清单） |
| `src/utils/batch-action.ts` | 全部纯判定（可 node:test 直测）：`actionDisabledReason` / `selectionText` / `confirmMessage` / `reasonError` / `ownerError` / `progressText` / `mergeBatchResults` / `failureLines` / `chunk` / `toggleSelection` |
| `src/composables/useBatchSelection.ts` | 勾选状态（`selected-row-keys` 绑定 + 跨页缓存，用于判状态） |
| `src/api/business/batch.ts` | 11 个批量接口的前端契约（路径写全 `/api/...`） |

接入的 8 个页面：

| 页面 | 批量动作 | 全选筛选结果 |
|------|----------|--------------|
| `erp/stockin` | 批量过账 / 批量作废 / 批量删除 / 批量导出 | ✅ |
| `erp/purchase` | 批量提交审批 / 批量取消 | ✅ |
| `erp/transfer` | 批量提交审批 / 批量作废 | ✅ |
| `erp/payable` | 批量导出 | ✅ |
| `erp/voucher` | 批量导出 | ✅ |
| `erp/serial` | 批量作废 | ✅ |
| `flow/task` | 批量同意 / 批量驳回（卡片勾选 + 全选本页） | 本页（待办列表一次 50 条） |
| `task` | 批量完成 / 批量开始 / 批量取消 / 批量指派 | 本页（树形表格父子行分别可勾） |

交互口径（卡面 §三）：表头勾选列（无可用动作的行复选框禁用）→ 「已选 N 条（本页）/ 选中全部 M 条筛选结果」明确提示 →
执行时进度条 `N/M`（前端 20 条一批，逐批刷新）→ 结果弹窗「共 N 条：成功 X 条，失败 Y 条」+ 失败清单「单号：原因」可整段复制 →
**跑完清空勾选**（刷新不保持，避免误点第二次批量）。

## 二、单测

### 后端（`cargo test --lib`，真库 `ERP_DB_URL`）

| 模块 | 条数 | 覆盖 |
|------|------|------|
| `business::shared::batch` | 6 | 空 ids→400 / 去重保序 / 非法 UUID 点名第几条 / 超上限 / 汇总计数 / uuid 数组字面量 |
| `business::erp::batch` | 8 | ① 5 张全成功 ② 3 成功 2 失败逐条报告（成功已提交、失败原样未动）③ 空 ids 400 ④ 不存在 id 只该条失败 ⑤ 无权限 403（超管放行）⑥ 幂等第二次 0 成功 ⑦ 批量删除只删草稿 ⑧ 导出参数闸 |
| `business::flow::batch` | 2 | 决策归一化（默认同意 / 大小写 / 非法值 400）+ 权限与空 ids |
| `business::task::batch` | 2 | 改状态参数闸（权限 / 空 ids / 缺目标态）+ 指派必须给负责人 |
| `business::impexp::builtin::erp_doc` | 5 | 定义唯一性 / 列与别名一致 / 过滤器绑定 / 主键表达式 + **真库：`count`+`fetch` 真能取到行**（钉住 `SELECT SELECT` 这类拼错） |

### 前端（`pnpm test:unit`）

`tests/unit/batch-action.test.ts` 共 8 条：① 工具栏条数与「本页/全部」文案 ② 状态不符禁用 + 点名叫原因 ③ 行可勾性与全选跳过禁用行
④ 进度 N/M 与 20 条分批 ⑤ 结果汇总 / 失败清单 / 导出任务文案 ⑥ 全选·反选·清空（不误清其它页）⑦ 高危确认带条数 + 原因必填 ⑧ 指派必须选负责人。

## 三、验收记录（可复跑）

```bash
# 1) 干净库（134 个迁移）全量后端单测
cd backend && ERP_DB_URL=postgres://d7role:***@127.0.0.1:5432/cloudpivot_d7v \
  cargo test --lib -j 2 -- --test-threads=1
  → test result: ok. 1824 passed; 0 failed（既有 1801 + 本卡 23；495.04s）

# 2) 前端
cd frontend && npx vue-tsc --noEmit && npx tsx --test "tests/unit/**/*.test.ts"
  → typecheck 0 error；# tests 403 / # pass 403 / # fail 0（既有 395 + 本卡 8）
  → npx vite build 通过（模板编译 + 8 个 *-batch testid 进 dist）

# 3) 不变量（干净库）
NIK_DATABASE_USER=d7role NIK_DATABASE_NAME=cloudpivot_d7v \
  python3 scripts/verify/erp_invariants.py            → 32/32 · BLOCKER 0 · WARN 0

# 4) 真机 e2e（隔离实例 + 真 HTTP + 真库）
D7_BASE_URL=http://127.0.0.1:18091 D7_DB_NAME=cloudpivot_d7 \
  python3 scripts/verify/d7_batch_e2e.py              → 30 PASS / 0 FAIL

# 5) lint
cargo fmt -p cloudpivot -- --check && cargo clippy --lib -- -D warnings   → 均绿
```

e2e 覆盖的真机链路（卡面 §五）：

```
① 勾 3 张 DRAFT → 批量过账 → 3 成功（库里 3 张 POSTED）
② 勾 1 DRAFT + 1 POSTED → 批量过账 → 1 成功 1 失败（失败码 10003、"已过账"、带单号；成功那条已提交 = 逐条事务）
③ 同一批再点一次 → 0 成功 3 失败；库存合计 5.0000（1+1+1+2，没有被重复过账翻倍）
④ 空 ids → 400；不存在的 id → 该条 10002 失败、其它继续
⑤ 勾 5 张 → 批量导出 → 导入导出页任务 SUCCESS → 下载产物 zip 头 PK（真 xlsx）
⑥ 批量删除：删草稿成功、已过账单被拒（1 成功 1 失败）
⑦ 调拨单 / 序列号 / 采购单 / 审批待办 / 任务 五条批量接口已注册并逐条报告
⑧ 未登录 → 401（路由在鉴权后面，不是 404）
```

## 四、生产部署（144）

```bash
ssh root@172.18.10.144 'bash /tmp/d7_deploy_144.sh'    # = scripts/verify/d7_deploy_144.sh
ssh root@172.18.10.144 'bash /tmp/d7_prod_smoke.sh'    # = scripts/verify/d7_prod_smoke.sh（只读）
```

结果（2026-11-02 01:06）：

| 项 | 值 |
|----|----|
| 部署 HEAD | `1c8415f`（`git merge --ff-only origin/main`，**零迁移**） |
| 二进制 md5 | `006b7c2d2e1ba6dae3a32c4818fb4ab4`，`PROC_EQ_DISK=OK` |
| health | 200 |
| 前端入口 chunk | `BasicLayout-ClD1jr1k.js` → `BasicLayout-Dnrvy29o.js`；8 个 `*-batch` testid 逐个命中 |
| 11 条批量路由 | 未登录全部 **401**（路由已注册，不是 404） |
| 生产不变量 | **32/32 · BLOCKER 0 · WARN 0** |

生产只读冒烟（**不写库**的入参，结果与设计一致）：

```
1) 收货单批量过账 ids=[]            → 400「ids 不能为空：批量操作至少要勾选一条」
2) 收货单批量过账 不存在的 id        → 200 + 1 条失败 {code:10002, error:"收货入库单不存在或已删除"}
3) 批量导出 ids=[] 且无筛选          → 400（不许「全选」变「全表」，且不建任务）
4) 审批中心批量同意 不存在的待办     → 200 + 1 条失败 {code:65003, error:"待办不存在或已被删除"}
5) 任务批量改状态 不存在的任务       → 200 + 1 条失败 {code:63001, error:"任务不存在或已删除"}
```

## 五、已知边界 / 后续

- 卡面第二批「其余 19 个列表页」未接（本卡按卡面只做 8 个核心页）。组件与 API 已通用，接入一页约 30 行。
- 批量导出目前为 3 类单据（收货 / 应付 / 凭证）注册了导出定义；其余资源若要导出，需按 `erp_doc.rs` 的 `DocSpec` 再加一段 SELECT。
- 「选中全部筛选结果」一次最多拉 1 万条（50 页 × 200），超过请缩小筛选；后端单次批量上限 200 条（`MAX_BATCH_IDS`），前端按 20 条分批。
- 前端组件单测按仓库惯例测纯逻辑（`utils/batch-action.ts`），`.vue` 只做渲染；`.vue` 的渲染回归走 Playwright / 真机页面检查。
- 共享工作区 `/opt/cloudpivot` 与其它卡并行：本卡改了 `business/{erp,flow,task,shared}/mod.rs`、`impexp/{mod,definition,handler,worker,builtin/mod}.rs` 与 8 个页面文件（hotspot 已在完成说明里点名）。

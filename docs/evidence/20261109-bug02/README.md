# BUG-02：重新过账接口 + 前端按钮（证据与说明）

- 卡：`t_1bf6c58d`
- 日期：2026-11-09
- 关联：BUG-01 `t_f427e5d6`（免审过账失败一律打回草稿）—— 两者配套：
  BUG-01 不再产生新死单，BUG-02 给存量死单一条出路。
- 生产现场：`IN-20261031-000002` 单据 `PENDING_POST`、审批实例 `APPROVED`、六本账没动，
  系统内无任何接口能推进它（当时只能软删，单号与历史一起丢掉）。

---

## 1. 交付了什么

### ① 新接口 `POST /business/erp/stock-in/{id}/repost`

| 项 | 值 |
|---|---|
| 路由文件 | `backend/src/business/erp/stockin_handler_status.rs`（`stock_in_repost`） |
| 业务实现 | `backend/src/business/erp/stockin_handler_post.rs`（`repost_stock_in`） |
| 权限码 | `business:erp:stock-in:submit-post-approval`（**与 submit-post 相同**，见下「权限评估」） |
| 返回 | `StockInDetailRes`（与 submit-post / status 一致，前端可直接刷新详情） |

**前置（任一不满足 → 400 + 明确原因，状态一行不改）**

1. 单据必须是 `PENDING_POST`：
   - `DRAFT` → 「草稿请直接点「过账」（配了审批流会先送审）」
   - `POSTED` → 「已过账的单据不能重复过账：库存只加一次，要重收请先作废后重开」
2. 关联审批实例必须是 `APPROVED`：
   - 实例 `PENDING` → 「审批还没通过（通过后系统会自动过账），现在不能重新过账」
   - 没有实例 → 「没有关联的过账审批实例…请先点「过账」提交审批」

**行为**

- 直接调用 `post_after_approval_with_reason(...)` —— 与「审批通过回调」**同一份实现**：
  - `guard_posting_supplier`（单据头供应商 = 来源采购单供应商）
  - `separation::guard(Duty::Post)`（制单 ≠ 过账，按开关）
  - `guard_posting_units`（明细单位复检）
  - `guard_purchase_before_post`（采购单状态 + 待收额度）
  - 六本账 + 凭证 + 应付 + 采购单状态推进，全在**一个事务**里
- 成功 → `POSTED`；`biz_status_log` 记 `PENDING_POST → POSTED`，`reason = 审批通过后重新过账`（可审计出这是一次重试）
- 失败 → 事务整笔回滚，**单据仍 `PENDING_POST`**（不打回草稿）：
  - 审批已通过、按钮还在，用户修完数据可以原地再点；
  - 打回草稿反而要重新走一遍审批，且与 BUG-01 的「免审无人推进」场景不同 —— 这里**不是死单**。

### ② 详情页能分清「审批中」与「审批已过、过账失败」

`StockInDetailRes` 新增两个字段（`backend/src/business/erp/stockin_dto.rs`，
由 `stockin_handler_common.rs::detail_of` 组装）：

| 字段 | 语义 |
|---|---|
| `approval_instance_status` | 关联审批实例状态：`PENDING` / `APPROVED` / `REJECTED` / `WITHDRAWN` / `CANCELLED` / `null` |
| `repost_available` | 服务端算好的「可重新过账」：`status == PENDING_POST && 实例 == APPROVED` |

口径只有一份（服务端算），前端不再自己拼条件。

### ③ 前端（`frontend/src/views/business/erp/stockin/index.vue`）

- 详情操作栏新增按钮 **「重新过账」**（`data-testid="btn-stockin-detail-repost"`），
  `v-if="detail.repost_available"` —— **只在 `PENDING_POST + APPROVED` 时出现**。
- 详情头部新增红色标签 **「审批已通过 · 过账失败」**（`stockin-post-failed-tag`）。
- 详情新增错误色告警条（`stockin-repost-alert`）：
  > 审批已通过，过账失败 —— 修正数据后可重试
  > 上次过账时的校验（采购单状态 / 待收额度 / 供应商 / 单位 / 库位 / 内控）没通过，
  > **六本账一笔都没动**，单据仍停在「过账审批中」。点「重新过账」会重跑全部校验：
  > 失败会给出具体原因、状态不变，改完再试即可。
- 审批进度区块标题按状态切换：「过账审批进度」/「作废审批进度」（原来一律写作废）。
- API 封装：`repostStockIn(id)`（`frontend/src/api/business/erp/index.ts`）。
- 确认弹窗文案明确「六本账没动 / 失败不改状态 / 改完可再试」；成功后刷新详情与列表。

---

## 2. 权限评估（为什么不新开权限码）

- `business:erp:stock-in:submit-post-approval` 的语义是「这个岗位能对这张收货单发起/参与过账」；
  重新过账是**同一动作的重试**，动作对象与后果完全一样。
- 真正的职责分离不靠权限码表达，而由 `separation::guard(Duty::Post)` 按开关判定
  （制单人不能自己过账）—— 换 URL 也绕不过（与 `PATCH /status` / `submit-post` 同一收口）。
- 不新增权限码 = 不动 `sys_menu` 与菜单迁移（避免菜单不变式与授权矩阵的额外风险）。
- 现有角色矩阵里 `WAREHOUSE_MANAGER` 同时持有 `stock-in:status` 与 `submit-post-approval`，
  重新过账不会出现「能提交不能重试」的断层。

---

## 3. 姊妹单据评估（卡面 ③）

逐个查了「审批通过后**自动**过账」的路径；结论：**不需要 repost**，因为它们都没有
「审批通过后由回调自动动账、失败即无人推进」的结构。

| 单据 | 过账触发 | 失败后能否原地重试 | 结论 |
|---|---|---|---|
| 收货入库 `stock-in` | 审批通过**回调自动**过账（`stockin_bridge::on_post_settled`） | ❌ 回调只来一次；`PATCH /status` 明确拒绝 `PENDING_POST → POSTED`（防绕过审批） | **本卡新增 repost** |
| 出库单 `stock-out` | `PENDING_POST → POSTED` 是**人工**动作（`stockout_handler::change_status → post_stock_out`） | ✅ 失败不改状态，再点「过账」即可 | 无需 |
| 盘点单 `stock-take` | 审批通过**不落状态**（`stocktake_bridge` 只在驳回/撤回时打回草稿）；过账是独立人工动作 `post_stock_take` | ✅ 失败整笔回滚、仍 `PENDING_POST`，再点「过账」即可（`stocktake_handler::post_tx` 只认 `PENDING_POST`） | 无需 |
| 调拨单 `transfer` | 状态机 `DRAFT → PENDING → APPROVED → IN_TRANSIT → DONE`，**没有** `PENDING_POST`，审批通过不自动动账 | ✅ 人工流转 | 无需 |
| 收款退款 `receipt_refund` | 审批通过回调直接过账 | ✅ 失败留 `CONFIRMED`，可原地重新提交审批（BUG-01 审计已列） | 无需 |

依据（代码位置）：

- `backend/src/business/shared/state_machine.rs:179-194`：出库边表含 `PENDING_POST → POSTED`（人工过账）+ `PENDING_POST → DRAFT`（撤回）。
- `backend/src/business/erp/stockout_handler.rs:1164`：`STATUS_POSTED => post_stock_out(...)`。
- `backend/src/business/erp/stocktake_bridge.rs:96-138`：`APPROVED` 不改单据状态，「过账要另一次点击」。
- `backend/src/business/erp/stocktake_handler.rs:753-790`：`post_stock_take` 只认 `PENDING_POST`，失败 `rollback`。
- `backend/src/business/erp/transfer_base.rs:66-93`：调拨状态集无 `PENDING_POST`。

> **反过来说明**：本卡的问题不在「有没有报错」，而在「审批通过后**没有第二个动作**能推进」。
> 出库/盘点的过账按钮是常驻的，所以它们的失败天然可重试。

---

## 4. 验收对照

| 验收 | 怎么验的 | 结果 |
|---|---|---|
| ① `POST /stock-in/{id}/repost` 存在 | `#[nik::post("/business/erp/stock-in/{id}/repost")]` 注册；实机 `curl` 无令牌 → 401（路由命中，不是 404） | ✅ |
| ② PENDING_POST + APPROVED：数据正常 → POSTED + 六本账 | `bug02_repost_pending_approved_posts_and_moves_books`：库存 600 / 流水 1 / 凭证 1 / 应付 1 / 采购单 RECEIVED / 状态日志 `PENDING_POST→POSTED`（reason 含「重新过账」） | ✅ |
| ② 数据有问题 → 明确错误 + 仍 PENDING_POST | `bug02_repost_over_receive_keeps_pending_then_recovers_after_fix`：采购单已收货 → 409 + 文案点名采购单现状；状态仍 `PENDING_POST`、库存不变；**改完数据再点 → POSTED（600+600=1200）** | ✅ |
| ③ DRAFT 调 repost → 400 | `bug02_repost_rejects_draft` | ✅ |
| ④ POSTED 调 repost → 400 | `bug02_repost_rejects_posted`（库存仍只加一次） | ✅ |
| ⑤ SoD 开关打开时制单人被拦 | `bug02_repost_blocked_by_separation_of_duties`：409 + `66013`，状态不变；**换人重试成功** | ✅ |
| ⑥ 前端按钮只在 PENDING_POST + APPROVED 显示 | `v-if="detail.repost_available"`，字段由服务端 `status==PENDING_POST && 实例==APPROVED` 计算；DRAFT/POSTED/PENDING 三例断言 `repost_available == false`，APPROVED 一例断言 `true`。**说明**：仓库前端只有 Playwright e2e、无单测框架，本卡未跑 e2e；按钮条件本身是纯 `v-if` + 服务端 flag，flag 的四种状态由真库用例钉死 | ✅ |
| ⑦ cargo test + 前端 typecheck/build + commit | 见 `runs.md` | ✅ |

补充：`bug02_repost_rejects_instance_still_pending` 钉住「审批还在跑 → 400，不能借新接口绕过复核」。

---

## 5. 风险与边界

- **只放行、不放松**：本接口不新增任何判定，只是把「审批通过后的过账」再执行一次；
  SoD / 采购单复核 / 单位 / 供应商 / 库位校验全部重跑。
- **失败不打回草稿**是刻意的：审批已过、按钮常驻；打回草稿会让用户重走审批。
  （BUG-01 的「打回」只针对**免审分支** —— 那条路失败后没有任何待办会来推进。）
- **并发**：过账事务里 `lock_stock_in` + 状态 CAS（`WHERE status = PENDING_POST`），
  两个并发 repost 只有一个能成功，另一个 409。
- **BUG-01 之后**：免审 / 审批回调失败都会打回 DRAFT，所以 `PENDING_POST + APPROVED`
  的新单不会再产生；repost 主要面向**存量数据**与「回调异常（如进程重启）」场景。
- 前端 `repost_available` 只用于显隐；真正的准入判定在服务端（按钮藏不住的越权由后端 400/409 兜住）。

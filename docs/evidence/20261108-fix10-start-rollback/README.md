# t_0127714a — 提交审批被引擎拒绝时必须整笔回滚（单据卡死）

## 1. 卡面现象（100% 可复现）

环境：145 / 全新演示库（261 迁移 + 演示种子）/ 隔离实例。

1. 报销人 = **部门负责人本人**（`demo_lead`，`DEPT_DEMO.leader_id`）时提交报销单
   `POST /business/erp/expense/{id}/submit` → **422 / 65018**（职责分离：该节点解析到的
   审批人就是制单人本人）—— 这个 422 是**设计**（内控不允许自审），不是缺陷。
2. 但单据**已经被置成 `SUBMITTED`**，`biz_flow_instance(biz_type=EXPENSE)` 落了 1 条
   `PENDING` 实例、**`biz_flow_task` 0 行**。
3. 后果：单据永久卡死 —— 批不了（无待办）、付不了（`409 66041 当前是「审批中」`）、
   也重提不了（不是 DRAFT）。

## 2. 根因（代码级）

`flow::engine::start()` 把引擎的**业务拒绝**（`65018` 派不出审批人 / `65019` 自审被禁 /
`65039` 标题超长 …）放进 [`ActionOut::rejected`] 后**正常返回 `Ok(..)`**，而此刻它已经
写过 `biz_flow_instance`（PENDING）与实例节点了。业务入口若写成：

```rust
// 旧代码（报销 / 资产处置）
let out = engine::start(&tx, &start).await?;
tx.commit().await?;              // ← 先提交
if let Some(rejected) = out.rejected.take() { return Err(rejected); }  // ← 再判拒绝
```

库里就留下「单据 SUBMITTED + 实例在跑 + 0 待办」的半截审批单：实例占着
`uk_biz_flow_instance_biz_active`，单据既批不动也提不了。

调拨更彻底：`apply_transition` **根本不看 `rejected`**，拒绝被静默吞掉，接口还回 200。

## 3. 修法

### 3.1 引擎侧：把「业务拒绝」变成硬错误

`flow::engine` 新增：

```rust
pub enum StartFailure { Db(DbError), Rejected(ApiError) }

pub async fn start_in_tx(tx: &Transaction, req: &StartReq)
    -> Result<ActionOut, StartFailure>;   // 拒绝 ⇒ Err(Rejected(原始 ApiError))
```

调用方拿到 `Err` 只能回滚，**不存在「先 commit 再判 rejected」的写法了**。
原始 `ApiError` 原样带出（用 `appended()` 补提示，不用 `ApiError::new()` 重建 —— 那会丢
`65018` 的结构化载荷 / `internal_detail`）。

哨兵 `START_REJECTED_MARKER` + `is_start_rejected()` 从 `flow::handler` 提到 `engine`，
供「跑在 `DbResult` 闭包里、返回不了 `ApiError`」的入口复用（验收即发起）。

### 3.2 各业务入口

| 入口 | 修复前 | 现在 |
|---|---|---|
| `erp::expense_handler::report_submit`（报销提交） | commit 后才判 `rejected` ⇒ 单据卡死 | `start_in_tx`，拒绝即回滚 |
| `erp::asset_handler::asset_dispose`（资产处置） | commit 后才判 `rejected` ⇒ 处置卡死 | `start_in_tx`，拒绝即回滚 |
| `erp::transfer_handler_status::apply_transition`（调拨提交） | **完全不判 `rejected`**（静默吞掉、回 200） | `start_in_tx` + 新增 `SettleError::Approval(ApiError)` ⇒ 回滚 + 原码 |
| `task::task_bridge::start_in_tx`（验收即发起） | `engine::start` 后不看 `rejected` ⇒ 任务落 REVIEW 挂半截审批单 | 拒绝 ⇒ 原始 `ApiError` 进槽位 + 抛哨兵回滚；`task::change_status` 用 `engine::is_start_rejected` 取回原码 |
| `task::task_handler_common::change_status_tx_error` | 只认 T11 / 流程冲突哨兵 | 增加「发起被拒」分支 |

### 3.3 全量排查：`engine::start` 的所有非测试调用点

口径：**是否在同一个事务里把 `rejected` 变成回滚**。

| 调用点 | 拒绝处理 | 结论 |
|---|---|---|
| `erp::expense_handler::report_submit` | 事务内 `start_in_tx` | ✅ 本卡修 |
| `erp::asset_handler::asset_dispose` | 事务内 `start_in_tx` | ✅ 本卡修 |
| `erp::transfer_handler_status::apply_transition` | 事务内 `start_in_tx` | ✅ 本卡修（原来不判） |
| `flow::task_bridge::start_in_tx`（验收即发起） | 哨兵 + 槽位回滚 | ✅ 本卡修（原来不判） |
| `erp::purchase_handler::submit_po_approval` | 事务闭包内 `return Err(rejected)` | ✅ 原本正确 |
| `erp::sales_order_handler::start_approval_tx` | 事务闭包内 `return Err` | ✅ 原本正确 |
| `erp::stocktake_handler`（盘点提交） | 事务闭包内 `return Err` | ✅ 原本正确 |
| `erp::budget_handler`（预算提交） | 事务闭包内 `return Err` | ✅ 原本正确 |
| `erp::stockin_handler_post`（收货过账提交） | 事务闭包内 `return Err` | ✅ 原本正确 |
| `erp::stockin_handler_cancel`（收货作废） | 事务闭包内 `return Err` | ✅ 原本正确 |
| `erp::contract_handler`（合同提交） | 事务闭包内 `return Err` | ✅ 原本正确 |
| `erp::payable_payment`（付款提交 / 应付作废） | 事务闭包内 `return Err` | ✅ 原本正确 |
| `erp::voucher_write::submit_reverse_tx`（凭证红冲） | 事务闭包内 `return Err` | ✅ 原本正确 |
| `erp::receipt_refund`（收款退款提交） | 显式 `tx.rollback()` 后返回 | ✅ 原本正确 |
| `flow::handler`（审批中心手工发起） | 事务内判 + 哨兵 | ✅ 原本正确 |
| `flow::edge_admin`（流程配置变更审批） | `rollback()` 后返回 | ✅ 原本正确 |
| `flow::task_bridge::{on_complete, ...}` | 事务内判 → 转 `DbError` | ✅ 原本正确 |

> PO / SO / 盘点 / 预算四个卡面点名的同类单据**原本就没有这个缺陷**（都在事务闭包里
> `return Err(rejected)`），排查后未做无意义改动。真正有缺陷的是报销、资产处置、
> 调拨、验收即发起四条路径。

## 4. 验证

### 4.1 单元 / 真库用例

新增 `business::flow::internal_control::db_tests::start_in_tx_turns_business_reject_into_rollback`
（`#[cfg(test)] mod db_tests`，真库，`P0FIX_DB_URL` 开关）：

* 拒绝路径：会签 2 人全是当事人 → `65018`，`start_in_tx` 返回 `Err(Rejected)`；
  回滚后 `biz_flow_instance` / `biz_flow_instance_node` / `biz_flow_task` **均 0 行**；
  `error.data.is_some()`（结构化载荷不丢）。
* 正常路径：有非制单人可签 → `Ok`，实例 `PENDING` + **1 条** PENDING 待办。

```bash
# 种子库（scripts/verify/make_full_suite_db.sh，演示组织 + 角色成员 + 16 条已发布流程）
export ERP_DB_URL=… SYS_DB_URL=$ERP_DB_URL TASK_DB_URL=$ERP_DB_URL P0FIX_DB_URL=$ERP_DB_URL
cd backend && cargo test --lib business::flow::internal_control::db_tests -- --test-threads=1
```

### 4.2 145 真机（全新演示库 + 隔离实例 8054）

脚本：`scripts/fix10_reset_145.sh`（重建库 + 带种子首启）、
`scripts/fix10_expense_e2e_145.sh`（curl + psql 断言）。
原始输出：`raw/fix10_e2e_145.txt`（**PASS=20 FAIL=0**）。

| # | 断言 | 实测 |
|---|---|---|
| ① | demo_lead（= DEPT_DEMO 负责人）提交 → http | 422 |
| ① | 错误码 | 65018 |
| ① | 单据状态 | DRAFT（修复前 SUBMITTED） |
| ① | 该单 EXPENSE 实例条数 | 0（修复前 1） |
| ① | 该单关联待办条数 | 0 |
| ① | 该单 `approval_instance_id` | 空 |
| ① | 全库 EXPENSE 实例总数 | 0 |
| ② | demo_finance 提交 → http | 200 |
| ② | 单据状态 / 实例状态 / 实例条数 | SUBMITTED / PENDING / 1 |
| ② | PENDING 待办条数 | 1（审批人 demo_lead ≠ 发起人） |
| ② | `approval_instance_id` 已回写 | true |
| ③ | ① 那张单仍可作废（草稿生命周期正常，未卡死） | 200 / CANCELLED |
| ③ | 作废后仍无实例；全库只剩正常路径 1 条 | 0 / 1 |

关键片段（摘自 `raw/fix10_e2e_145.txt`）：

```
  [curl] POST /business/erp/expense/{id}/submit（as demo_lead）→ http=422
  body: {"code":65018,"msg":"节点「直属上级审批」的审批人全部是制单人 / 发起人本人，内控（职责分离）不允许他们审批自己的单据…"}
  ✅ ①单据状态（必须仍 DRAFT） = DRAFT
  ✅ ①EXPENSE 实例条数（必须 0） = 0
  ✅ ②PENDING 待办条数 = 1
  ✅ ②待办审批人不是发起人 = true
  ✅ ③草稿可作废 http = 200
```

### 4.3 回归

* `cargo check --all-targets`：0 warning / 0 error。
* `cargo clippy --all-targets -- -D warnings`：见卡面汇报。
* `cargo test --lib -- --test-threads=1`（种子库 `cloudpivot_test_fix10`，由
  `scripts/verify/make_full_suite_db.sh` 建：演示组织 / 部门负责人 / 角色成员 / 16 条已发布流程）：

  ```
  test result: ok. 3356 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 1777.01s
  ```

## 5. 复跑

```bash
# 145（不碰 8023 服役实例、不碰 144）
BIN=/opt/cloudpivot/backend/target/release/cloudpivot DB=cloudpivot_fix10_demo PORT=8054 \
  bash scripts/fix10_reset_145.sh
BASE=http://127.0.0.1:8054 DB=cloudpivot_fix10_demo bash scripts/fix10_expense_e2e_145.sh
```

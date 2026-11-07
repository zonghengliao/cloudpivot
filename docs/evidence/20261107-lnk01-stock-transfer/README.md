# LNK-01 库存调拨单（STOCK_TRANSFER）接入联动引擎 —— 证据表

日期：2026-11-07 · 卡：`t_885a181b` · 分支：`main`

## 一、交付内容

| # | 文件 | 改动 |
|---|------|------|
| 1 | `backend/src/business/erp/transfer_linkage.rs`（新） | 调拨状态变更的**唯一**联动接线：`status_ctx` / `run_in_tx`（`engine::run_excluding`）/ `run_deferred_after_commit`（`engine::run_deferred`） |
| 2 | `backend/src/business/erp/transfer_handler_status.rs` | `apply_transition` 第 ⑥ 步接联动（CAS 成功才跑），返回值改为 `TransitionOutcome{affected, linkage}`；`change_transfer_status` 在 `commit()` 之后跑 `run_deferred_after_commit` |
| 3 | `backend/src/business/flow/task_bridge.rs` | 审批回调 `settle_transfer`（PENDING → APPROVED / DRAFT）事务内 `run_in_tx` + 提交后 `run_deferred_after_commit` |
| 4 | `backend/src/business/erp/transfer_settle.rs` / `transfer_handler_common.rs` | `SettleError::Linkage(ApiError)` + `settle_error` 原样透出（关键动作失败 → 整笔回滚并给出引擎文案） |
| 5 | `backend/src/business/flow/linkage/spec.rs` | `wired = true` + `wiring` 写实际调用点 + `proofs` 两条；新增 `TRANSFER_EXCLUDED_ACTIONS`；接线表头 3/15 → 4/15 |
| 6 | `backend/src/business/flow/linkage/tests_wiring.rs` | 已接线清单加 `STOCK_TRANSFER`、真实调用点 4 → 5 个文件；新增两条漂移守卫（接线细节 + 排除清单语义） |
| 7 | `backend/src/business/flow/linkage/admin.rs` | 配置页字典单测的已接线清单同步 |
| 8 | `backend/src/business/erp/transfer_handler_db_linkage_tests.rs`（新） | 真库 4 条用例：正常触发 / 重复结算不重复触发 / 排除动作不跑 / 审批回调也触发 |
| 9 | `scripts/verify/lnk01_start.sh` / `lnk01_curl_evidence.sh` | 隔离实例启动 + 真机 curl 取证脚本 |

## 二、为什么这么接线

### 1. 两条状态变更路都要接

```text
人工流转（提交 / 审核 / 发运 / 收货 / 作废）
  transfer_handler_status::change_transfer_status
    → apply_transition（state_machine::transition + CAS + settle）
    → ⑥ transfer_linkage::run_in_tx(tx, …)         ← 事务内 Phase::InTx
    → commit()
    → transfer_linkage::run_deferred_after_commit  ← 提交后 Phase::AfterCommit

审批回调（通过 → APPROVED / 驳回 → DRAFT）
  flow::task_bridge::settle_transfer（**绕开 change_transfer_status**，直接 erp_transfer::settle_approval）
    → 事务内 transfer_linkage::run_in_tx
    → commit 后 run_deferred_after_commit
```

只接第一条会让「配了已发布流程」的生产环境里 `STOCK_TRANSFER/APPROVED` 的规则
**永远不触发**（能保存、能启用、永远不响）—— 正是 P1-FIX-5C 那类静默失效。
`settle_guard` 与 `flow_gate_blocked` 保证两条路互斥（挂着在跑实例时人工不许直推 APPROVED），
所以不会同一次状态变更跑两遍联动。

### 2. 排除清单：`spec::TRANSFER_EXCLUDED_ACTIONS`

```text
WRITE_INVENTORY / WRITE_INVENTORY_TXN / GENERATE_VOUCHER
```

- 调拨的库存落账在 `transfer_model::settle_transfer`（同事务：调出仓 −、调入仓 +、两条
  `TRANSFER_OUT` / `TRANSFER_IN` 流水、成本原价转移）。通用引擎再跑一次 = 同行明细扣两次、
  一张单四条流水。
- `GENERATE_VOUCHER`：调拨不抛凭证（只搬成本），且该动作目前只接管 `STOCK_IN` 取数，
  挂在调拨上只会产生噪音；一并排除。
- `SEND_NOTICE` / `CREATE_TASK` **不排除**——通知与建任务就靠通用引擎的 AfterCommit 动作。
- `UPDATE_COST` 刻意不排除（调拨成本转移只在 `settle_transfer`；通用路线上该动作是占位动作，执行时 Skipped）。

## 三、验证（全部真实执行）

| 项 | 命令 | 结果 |
|----|------|------|
| 全量单测 | `cargo test --lib` | **3154 passed / 0 failed**（本卡 +6：4 条真库 + 2 条接线守卫） |
| 接线漂移守卫 | `cargo test --lib business::flow::linkage` | 67 passed，`wired_triggers_match_call_sites` ✅ |
| 真库联动用例 | `ERP_DB_URL=…cloudpivot_so44 cargo test --lib business::erp::transfer_handler::db_linkage -- --test-threads=1` | **4 passed**（13.6s） |
| 调拨全模块回归（真库） | `ERP_DB_URL=… cargo test --lib business::erp::transfer_handler -- --test-threads=1` | **60 passed / 0 failed**（158s，六态 + 审批 + 结算全绿） |
| rustfmt | `rustfmt +1.94.1 --edition 2024 --check <本卡文件>` | 无 diff |
| clippy | `cargo clippy --lib --tests` | 本卡 0 警告（剩 2 条均为他人文件既有：`sales_order_model::update_order` too_many_arguments、`fx_revaluation.rs` useless format!） |

### 真库 4 条用例覆盖

| 用例 | 断言 |
|------|------|
| `done_status_triggers_linkage_notice_once` | IN_TRANSIT→DONE 后 `sys_notice` 有 1 条（幂等键 `LNK:STOCK_TRANSFER:DONE:<id>`），标题「调拨单已完成」、`biz_type=STOCK_TRANSFER`、`link_url` 指回单据 |
| `repeated_settle_does_not_trigger_linkage_twice` | 首次 DONE 建 1 个任务；再推 DONE → 409；绕过接口重放 `settle_transfer` → `settled=false`；任务数仍 1、流水仍 2 条 |
| `excluded_actions_are_not_executed_by_the_generic_engine` | 带排除清单 `run_excluding` → `runs`/`deferred` 全空；不带清单 → 读到该规则 1 条（反证不是假绿）；无凭证、流水 2 条 |
| `approval_callback_triggers_linkage_on_approved` | 走生产审批路径（`approve_flow_instance`）→ APPROVED 后收到 `LNK:STOCK_TRANSFER:APPROVED:<id>` 通知 |

## 四、真机 curl 取证（`curl_evidence.txt` 逐条落库）

隔离实例：`target/debug/cloudpivot` @ `127.0.0.1:18046`，DB `cloudpivot_so44`（250 迁移已应用）。

```text
1. PUT /api/business/flow/linkage   {STOCK_TRANSFER/PENDING, SEND_NOTICE, TAKEOVER, recipient=CREATOR}
   → {"implemented":true,"phase":"AFTER_COMMIT","problems":[]}
2. GET /api/business/flow/linkage?trigger_entity=STOCK_TRANSFER
   → trigger.wired = true，wiring 文案为 LNK-01 实际调用点
3. POST /api/business/erp/transfer  → TK-20261107-000070（DRAFT，原料A × 2，WH01 → WH02）
4. PATCH /api/business/erp/transfer/{id}/status {"status":"PENDING"} → code 0，submitted_at 落值
5. 落库真值（psql）：
   sys_notice      NTW202611070359221ceba30b | 调拨单待审批 | USER | STOCK_TRANSFER |
                   LNK:STOCK_TRANSFER:PENDING:01a1b429-3655-753b-bdcf-b35bd6265802
   biz_status_log  STOCK_TRANSFER DRAFT→PENDING
6. 收尾：DELETE 规则 → 库里恢复 3 条预置；单据 PENDING 不许删（业务规则），
   已按 CANCELLED → DELETE 两步清掉（软删行保留，`deleted_at` 置值）
```

## 五、残留与说明

- 验收库 `cloudpivot_so44` 留下 **1 条**取证通知（`LNK:STOCK_TRANSFER:PENDING:…`，指向已软删的调拨单），
  是本次真机触发的物证；规则表已恢复 3 条预置，无本卡残留规则。
- 实例与端口已停（`pkill target/debug/cloudpivot`，18046 已释放）。
- 本卡**没有新增迁移**：只接线，不改表。

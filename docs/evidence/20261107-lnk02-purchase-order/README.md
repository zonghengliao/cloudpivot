# LNK-02 采购订单（PURCHASE_ORDER）接入联动引擎 —— 交付与取证

卡：`t_027b39e1`｜日期：2026-11-07｜提交：见卡内汇报（真实 hash）

## 一句话

采购单的两条状态写入路（`purchase_model::apply_status[_with_instance]` 与
`erp/stockin_purchase.rs::sync_purchase_status`）现在都真的调联动引擎：
新增 `erp/purchase_linkage.rs` 作为**唯一 hook**，事务内跑 `run_excluding`（排除 4 类由收货单落账的动作），
提交后由调用方的 `LinkageOutbox::flush` 跑 `run_deferred`（通知 / 建任务）。

`spec::TRIGGER_SPECS` 的 `PURCHASE_ORDER` 从 `wired: false` 改成 `true`（4/15 → **5/15**）。

## 改了什么（按文件）

| 文件 | 改动 |
|------|------|
| `backend/src/business/erp/purchase_linkage.rs`（新） | hook：`status_ctx` / `run_in_tx` / `PendingLinkage` / `LinkageOutbox`（事务内动作 + 提交后动作的唯一落点） |
| `backend/src/business/erp/purchase_model.rs` | `apply_status[_with_instance]` 加 `&mut LinkageOutbox`；**CAS 成功（affected > 0）才** `run_in_tx`；用 `#[path]` 挂 hook 模块（不动 `erp/mod.rs` 这个并行热点） |
| `backend/src/business/erp/stockin_purchase.rs` | `sync_purchase_status`（部分收货 / 全部收货 / 收货作废回退，走 `transition_if_changed`）CAS 成功后 `run_in_tx` |
| `backend/src/business/erp/purchase_handler.rs` | ① `change_purchase_order_status` ② `submit_po_tx`（提交审批）→ outbox + commit 后 flush |
| `backend/src/business/erp/purchase_bridge.rs` | 审批回调（SUBMITTED → APPROVED / DRAFT）→ outbox + commit 后 flush |
| `backend/src/business/erp/cancel_cascade.rs` | 联动作废：一个 outbox 收集「逐张收货单作废 + 采购单作废」多次状态推进，commit 后一次 flush |
| `backend/src/business/erp/stockin_handler_post.rs` | `post_in_tx` 第 ⑥ 步（推采购单状态）带 outbox；两个过账入口 commit 后 flush |
| `backend/src/business/erp/stockin_handler_cancel.rs` / `stockin_handler_status.rs` | 收货单作废 → 采购单状态回退那条路带 outbox |
| `backend/src/business/flow/linkage/spec.rs` | 新增 `PURCHASE_EXCLUDED_ACTIONS`；`PURCHASE_ORDER` wired=true + wiring + 2 条 proofs；表头 5/15 |
| `backend/src/business/flow/linkage/tests_wiring.rs` | 已接线清单 + 调用点 5 → 6 文件；新增 2 条守卫 |
| `backend/src/business/flow/linkage/admin.rs` | 配置页字典的已接线清单 |
| `backend/src/business/erp/purchase_linkage_db_tests.rs`（新） | 5 条真库用例（挂在 `stockin_handler.rs` 的测试部件里，用收货夹具跑完整链路） |
| `scripts/verify/lnk02_start.sh` / `lnk02_curl_evidence.py` | 真机取证的启动 + 取证脚本 |

## 排除清单（为什么是这 4 个）

采购单自身**不落账**，库存 / 流水 / 凭证 / 已收数量全部由收货单过账在同一个事务里写完：

| 动作码 | 专用落账点 | 引擎再跑一次的后果 |
|--------|-----------|------------------|
| `WRITE_INVENTORY` | `post_in_tx` ③ 成本账 + 库位账 | 同一物料加两次库存 |
| `WRITE_INVENTORY_TXN` | `post_in_tx` ② 流水 | 一张收货单两条流水 |
| `GENERATE_VOUCHER` | `post_in_tx` ⑦ 进项凭证 | 一张单两张凭证 |
| `UPDATE_RECEIVED_QTY` | **不需要写**：已收数量是派生值（`POSTED_RECEIVED_SQL`） | 累计量被二次推进，与页面 / 过账闸口径分叉 |

`SEND_NOTICE` / `CREATE_TASK` 永远不排除（它们就是联动的用户可见价值）。

## 验证（真实执行结果）

### 1. 编译 / 静态检查

```
cargo check --lib --tests         ✅ 0 error / 0 warning
cargo clippy --lib --tests        ✅ 本卡 0 警告（仅他人文件既有 2 条：
                                     sales_order_model::update_order too_many_arguments、
                                     fx_revaluation useless format!）
rustfmt +1.94.1 --edition 2024    ✅ 本卡 15 个文件无 diff
```

### 2. 单测（无库）

```
cargo test --lib                  ✅ 3161 passed / 0 failed（基线 3154，本卡 +7：
                                     5 条真库 + 2 条接线守卫）
cargo test --lib business::flow::linkage::tests_wiring
                                  ✅ 9 passed（含新增
                                     purchase_order_wires_generic_engine_and_excludes_stock_actions
                                     purchase_excluded_actions_are_routed_elsewhere）
                                     wired_triggers_match_call_sites：真实调用点 5 → 6 文件、已接线 5/15
```

### 3. 真库用例（`ERP_DB_URL=cloudpivot_so44`，`--test-threads=1`）

```
cargo test --lib business::erp::stockin_handler::purchase_linkage_db
  ✅ submit_status_triggers_linkage_notice_once                     提交触发（DRAFT→SUBMITTED → 通知 1 条）
  ✅ partial_receipt_triggers_linkage                               部分收货触发（4/10 → PARTIAL_RECEIVED）
  ✅ full_receipt_triggers_linkage                                  全部收货触发（补 6 → RECEIVED）+ 派生已收 = 10
  ✅ repeated_partial_receipt_does_not_trigger_twice                状态没变不重复触发（任务数 1，不是 2）
  ✅ excluded_received_qty_action_is_not_executed_by_the_generic_engine
                                                                    排除动作不跑：qty=999 的超收规则没把过账搞挂；
                                                                    反证：不传排除清单 → 引擎 Err（500 超收）
```

### 4. 回归（真库）

```
cargo test --lib business::erp::stockin_handler    ✅ 140 passed / 0 failed（439s，六态 + 审批 + 结算）
cargo test --lib cancel_cascade                    ✅ 14 passed / 0 failed（联动作废整笔回滚用例仍绿）
```

### 5. 真机 e2e（隔离实例 :18047 + `cloudpivot_so44`）

`scripts/verify/lnk02_start.sh` → `scripts/verify/lnk02_curl_evidence.py`，全程走 HTTP（不直接调函数）：

```
① PUT  /api/business/flow/linkage           配置 PURCHASE_ORDER/PARTIAL_RECEIVED → SEND_NOTICE（写入口校验通过，problems=[]）
② GET  /api/business/flow/linkage           trigger.wired = true（配置页不再骗人）
③ POST /api/business/erp/purchase-order     PO-20261107-000005 DRAFT，22.60
④ PATCH …/status {"SUBMITTED"}              code 0 → SUBMITTED
⑤ POST /api/business/erp/stock-in           带来源采购单收 4/10（IN-20261107-000218，DRAFT）
⑥ PATCH …/status {"POSTED"}                 code 0 → POSTED（金额小额走种子免审分支）
⑦ GET  …/purchase-order/{id}                status = PARTIAL_RECEIVED
⑧ psql sys_notice                           「采购订单部分收货」| USER | PURCHASE_ORDER |
                                             LNK:PURCHASE_ORDER:PARTIAL_RECEIVED:<id>
⑨ psql biz_status_log                       DRAFT→SUBMITTED（提交审批）→ SUBMITTED→PARTIAL_RECEIVED（收货过账 IN-… 自动推进）
```

原始记录：`curl_evidence.txt`（同目录）。

## 残留（如实记录）

- `cloudpivot_so44` 留 3 张取证采购单（PO-20261107-000003/000004/000005，均 `PARTIAL_RECEIVED`）
  与 3 张已过账收货单，是本卡的真机物证；已过账单据按业务规则不可硬删。
- 取证规则已按写入口 DELETE 软删（`deleted_at` 非空，不再生效），库里无活跃的 `PURCHASE_ORDER` 规则。
- 取证账号 `lnk02_buyer`（采购经理角色）留在库里，供复跑脚本使用（幂等：已存在就跳过创建）。
- 本卡**未新增迁移**（只接线不改表）。

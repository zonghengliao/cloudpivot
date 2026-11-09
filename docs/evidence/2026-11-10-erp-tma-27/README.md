# REFACTOR【P6】erp 剩余 27 处 `too_many_arguments` allow 收口 — 证据

- 卡号：`t_75b88406`（父卡 `t_c5afaedf` / P5-补）
- 基线：`e356ad9e` 的 `docs/evidence/2026-11-10-apply-status-struct/04_grep_allow.out`（27 个真属性 + 2 行 doc 提及）
- 动手前本地 HEAD：`0355476c`；提交时远端已推进到 `b252db2e`（都是 ARCH-9 文档 commit，与 `backend/src` 无关）
- 图形：3 批 commit（状态流转族 / 流水写入族 / 其余），每批都过了 `cargo check --all-targets`

## 0. 结论

```
grep -rn "allow(clippy::too_many_arguments)" backend/src/business/erp/     → 0 命中（见 01_grep_allow.out）
cargo clippy --all-targets -- -D warnings                                  → exit 0（见 02_cargo_clippy_all_targets.out）
cargo test --lib                                                           → 4021 passed / 0 failed（见 03_cargo_test_lib.out）
cargo fmt -- --check                                                       → exit 0（见 04_cargo_fmt_check.out）
```

## 1. 27 处的处理方式（逐条）

| # | 位置 | 函数 | 原参数 | 现在 | 形态 |
|---|---|---|---|---|---|
| 1 | purchase_model.rs | `apply_status` | 8 | 3 | `ApplyStatusInput<'a>`（`ex` / `outbox` 留参数位） |
| 2 | period.rs | `apply_status`（私有） | 8 | 2 | `PeriodApplyStatusInput<'a>`（私有 struct，与函数同可见性） |
| 3 | transfer_write.rs | `apply_status` | 10 | 2 | `TransferApplyStatusInput<'a>` |
| 4 | budget_model.rs | `apply_status` | 7 | 7 | **删无用 allow**（阈值 7，7 个参数本就不触发） |
| 5 | commission_model.rs | `apply_status` | 9 | 2 | `CommissionApplyStatusInput<'a>` |
| 6 | stockin_write.rs | `write_txn` | 10 | 2 | `StockInTxnInput<'a>` |
| 7 | stockin_cancel.rs | `write_cancel_txn` | 6 | 6 | **删无用 allow** |
| 8 | stockout_model.rs | `write_txn` | 16 | 2 | `StockOutTxnInput<'a>`（14 字段，逐字段中文文档） |
| 9 | stockout_model.rs | `write_cancel_txn` | 13 | 2 | `StockOutCancelTxnInput<'a>` |
| 10 | stockin_handler_post.rs | `post_in_tx` | 11 | 3 | `PostInTxInput<'a>`（`pub(super)`，`ex` / `linkage_outbox` 留参数位） |
| 11 | purchase_handler.rs | `submit_po_tx` | 8 | 3 | `SubmitPoTxInput<'a>`（`tx` / `linkage_outbox` 留参数位） |
| 12 | cancel_cascade.rs | `cascade_purchase_order_tx` | 7 | 7 | **删无用 allow** |
| 13 | payable_payment.rs | `insert_payment_row` | 10 | 2 | `InsertPaymentRowInput<'a>` |
| 14 | payable_payment.rs | `submit_for_approval` | 5 | 5 | **删无用 allow** |
| 15 | payable_payment.rs | `submit_for_approval_tx` | 9 | 2 | `SubmitForApprovalTxInput<'a>` |
| 16 | budget_guard.rs | `occupy` | 10 | 2 | `OccupyInput<'a>` |
| 17 | budget_model.rs | `map_insert` | 10 | 2 | `MapInsertInput<'a>` |
| 18 | budget_model.rs | `map_update` | 10 | 2 | `MapUpdateInput<'a>` |
| 19 | commission_model.rs | `insert_settlement` | 9 | 2 | `InsertSettlementInput<'a>` |
| 20 | voucher_multi.rs | `post_explicit_lines` | 10 | 2 | `ExplicitVoucherInput<'a>` |
| 21 | asset_model.rs | `batch_insert` | 8 | 2 | `DepBatchInsertInput<'a>` |
| 22 | asset_model.rs | `batch_set_status` | 8 | 2 | `DepBatchStatusInput<'a>` |
| 23 | expense_model.rs | `report_mark_submitted` | 9 | 2 | `ReportMarkSubmittedInput<'a>` |
| 24 | receipt_refund.rs | `start_approval` | 3 | 3 | **删无用 allow** |
| 25 | depreciation_linkage.rs | `run_in_tx` | 8 | 2 | `DepLinkageInput<'a>` |
| 26 | receivable.rs | `create_manual_in_tx` | 10 | 2 | `CreateManualInTxInput`（**无 `'a`**：字段全是值 / 自有 `String`，`req` 无 `Copy` 故只 `derive(Debug, Clone)`） |
| 27 | price_inquiry_model.rs | `award` | 7 | 7 | **删无用 allow** |

卡面第 4 条（一致性）：`stockin_write::apply_status`（7 参数、本来没 allow）跟着 `purchase_model::apply_status`
一起收成**同名同形**的 `ApplyStatusInput<'a>` + `(ex, input)`，两个包装函数形态一致；5 处调用点同步改成 struct 字面量。

## 2. 关键判断：6 处 allow 是**无用属性**（不是收口对象）

clippy 的 `too_many_arguments` 默认阈值是 7，**参数个数 > 7 才触发**（`= 7` 不触发）。
清点时按 AST 口径数了每个函数的参数（`ex` / `tx` 也算），有 6 个函数的参数个数 ≤ 7：

- `budget_model::apply_status`(7)、`cancel_cascade::cascade_purchase_order_tx`(7)、`price_inquiry_model::award`(7)
- `stockin_cancel::write_cancel_txn`(6)、`payable_payment::submit_for_approval`(5)、`receipt_refund::start_approval`(3)

这 6 处的 allow 从来就没生效过（删掉 clippy 也不会报），所以处置是**直接删除属性**，没有为了
「形式统一」把 3～5 个参数的业务函数也包成结构体 —— 那只会让 `submit_for_approval(id, req, amount, operator, flow)`
这种本来一眼读懂的签名多一层 `SubmitForApprovalInput { .. }`，没有收益还扩大改动面。
其余 **21 处**全部按 P5 / P5-补 的定型写法收成 `XxxInput`。

## 3. 只改签名与调用点

- 每个函数体只在开头插入 `let XxxInput { .. } = input;`，**SQL / 状态机分支 / 事务边界 / outbox 调用顺序一行未动**。
- 解构出来的局部名与原参数名逐字相同，所以函数体其余部分零改动（`git diff` 里函数体中段没有非空行变化）。
- 顺手删掉/改写了几处「反结构体」的旧注释（`// 参数就是这一行的列……`、`// 这十个参数……拆成两个结构体只是换个地方堆字段`、
  `commission_model::apply_status` 的「参数多于 7 个……显式豁免」段、`sales_order_model` 两行提到 `#[allow]` 标记的 doc）。
- `state_machine::Operator.id` 是 `Option<Uuid>` 的坑本次没有新增踩点：新增 struct 里凡是 operator 都写成
  `operator: op.id` / `operator: operator.id`（与原位置传参同义），没有出现 `Some(operator.id)`。

## 4. 死代码核查

27 处全部在**活路径**上，没有死代码：

- `budget_guard::occupy` 被 `occupy_for_purchase` / `occupy_for_expense` 在生产路径调用（`purchase_handler` / `expense_handler`），
  外加 `expense_model_tests` 的「撤回 → 再提交」回归用例；
- `map_insert` / `map_update` 由 `budget_handler` 的映射 CRUD 调用（含 5 处真库单测）；
- 付款链（`insert_payment_row` / `submit_for_approval` / `submit_for_approval_tx`）由应付直付与「发起付款审批」两条活路径调用；
- `post_in_tx` / `write_txn` / `write_cancel_txn` / `batch_insert` / `batch_set_status` / `run_in_tx` 都在过账、作废、折旧、联动的主链上；
- `price_inquiry_model::award` 由 `price_inquiry_handler::price_inquiry_award` 调用。

## 5. 复现命令

```bash
cd backend
grep -rn "allow(clippy::too_many_arguments)" src/business/erp/       # 0 行
cargo clippy --all-targets --message-format short -- -D warnings     # EXIT=0（3m01s）
cargo test --lib                                                     # 4021 passed / 0 failed
cargo fmt -- --check                                                 # EXIT=0
```

## 6. 证据文件

| 文件 | 内容 |
|---|---|
| `01_grep_allow.out` | `grep -rn "allow(clippy::too_many_arguments)" src/business/erp/` = 0 行；以及残留的 12 行 **doc 散文提及**（`避开 too_many_arguments` 一类说明，均非 `#[allow]` 标记） |
| `02_cargo_clippy_all_targets.out` | `cargo clippy --all-targets -- -D warnings` 实跑输出（EXIT=0） |
| `03_cargo_test_lib.out` | `cargo test --lib` 实跑输出（4021 passed；`test result:` 行 + EXIT=0） |
| `04_cargo_fmt_check.out` | `cargo fmt -- --check` 实跑输出（空 = EXIT 0） |

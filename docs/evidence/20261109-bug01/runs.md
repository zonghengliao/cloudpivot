# BUG-01 执行记录（真库实测，2026-11-09）

环境：本机（172.18.10.99）隔离库 `cp_bug01_e2e`（owner `alun`，空库 + 全量迁移 +
`NIK_SEED_DEMO_USERS=true` 演示组织种子），`ERP_DB_URL=postgres://alun:***@127.0.0.1:5432/cp_bug01_e2e`。

## ① 新增用例（修复后）

```
$ cargo test --lib bug01
running 3 tests
test ...::bug01_auto_post_supplier_mismatch_reverts_to_draft ... ok
test ...::bug01_auto_post_over_receive_reverts_to_draft ... ok
test ...::bug01_auto_post_success_still_posts ... ok

test result: ok. 3 passed; 0 failed; 0 ignored; 0 measured; 3835 filtered out; finished in 13.03s
```

## ② 负向验证（临时还原修复前行为 → 用例必须红）

```
$ # 把免审分支改回 `Err(error) => return Err(error),`
$ cargo test --lib bug01
---- bug01_auto_post_over_receive_reverts_to_draft stdout ----
assertion `left == right` failed: 免审过账失败必须打回 DRAFT —— 停在 PENDING_POST 就是死单（BUG-01）
  left: "PENDING_POST"
 right: "DRAFT"

---- bug01_auto_post_supplier_mismatch_reverts_to_draft stdout ----
assertion `left == right` failed: 免审过账失败必须打回 DRAFT（不只是 SoD 才兜底）
  left: "PENDING_POST"
 right: "DRAFT"

test result: FAILED. 1 passed; 2 failed
```

`left: "PENDING_POST"` 即生产事故（IN-20261031-000002）的死单状态。

## ③ 收货入库全模块回归

```
$ cargo test --lib stockin
test result: ok. 158 passed; 0 failed; 0 ignored; 0 measured; 3687 filtered out; finished in 459.64s
```

## ④ 全量

```
$ cargo test --lib
test result: FAILED. 3814 passed; 24 failed; ... finished in 1430.22s
```

24 条失败逐条复跑（均与本次改动无关，详见 README §4）：

```
$ cargo test --lib asset_linkage_tests          → ok. 11 passed
$ cargo test --lib expense_linkage_tests -- --test-threads=1 → ok. 6 passed
$ cargo test --lib voucher_linkage_tests -- --test-threads=1 → ok. 6 passed
$ cargo test --lib payable_bridge::tests        → ok. 23 passed
$ cargo test --lib receivable_aging             → ok. 9 passed
$ cargo test --lib report_pdf                   → FAILED. 9 passed; 1 failed（report_pdf.rs 无 stockin 引用）
$ cargo test --lib return_pdf                   → FAILED. 8 passed; 2 failed（断言 `<th>价税合计</th>` vs 模板 `<th class="c-with-tax">`，PDF-04 起已过期）
```

## ⑤ 静态检查

```
$ rustfmt +1.94.1 --edition 2024 --check src/business/erp/stockin_handler_post.rs \
    src/business/erp/stockin_bridge.rs src/business/erp/stockin_handler_tests_pflow3a.rs
（无输出 = 通过）

$ cargo clippy --all-targets -- -D warnings
error: the following explicit lifetimes could be elided: 'a   --> src/business/erp/invoice.rs:1967
error: doc list item without indentation                      --> src/business/erp/receivable.rs:800
error: this function has too many arguments (8/7)             --> src/business/erp/sales_order_model.rs:699
EXIT=101
```

三条 clippy 错误全部落在**别人未提交的改动**（invoice / receivable / sales_order_model），
本次改动的 3 个文件零告警。

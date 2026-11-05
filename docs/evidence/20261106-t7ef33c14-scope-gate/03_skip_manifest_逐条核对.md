# SKIP_MANIFEST 新增 18 条 · 逐条核对记录

`scripts/lint/scope_gate.py` 的跳过清单是**硬断言**（实测跳过集合 ≠ 登记清单即 FAIL），
补登记之前必须逐条确认「它确实是测试面」，否则就是把生产文件藏进测试面。

核对三判据（脚本 `scripts/tmp-verify-skip-manifest.py` 自动跑，原始输出见
`03b_未登记清单核对_原始输出.txt`）：

1. 父模块声明处必须是 `#[cfg(test)]`（脚本已逐条打印声明位置上下文）；
2. 文件本身有 `#[test]` / `#[tokio::test]`；
3. 文件里 `pub fn` 的数量为 0（没有对外生产 API；纯夹具文件由第 4 条兜住）；
4. 引用它的文件只能是**测试面**文件或它自己的父模块（`transfer_handler_test_support.rs`
   的函数全部 `#[cfg(test)] pub(super)`，只被 `transfer_handler.rs` 的测试模块使用）。

实测（2026-11-06，判定面 = HEAD 392ca5c + 本卡改动）：

| # | 文件 | 声明位置 | test 数 | pub fn |
|---|------|----------|--------|--------|
| 1 | `business/erp/expense_model_tests.rs` | `erp/expense_model.rs:1323` `#[cfg(test)] #[path]` | 16 | 0 |
| 2 | `business/erp/stockout_fifo_tests.rs` | `erp/stockout_model.rs:1170` `#[cfg(test)] #[path]` | 14 | 0 |
| 3 | `business/erp/transfer_handler_db_tests_books.rs` | `erp/transfer_handler.rs:110` | 4 | 0 |
| 4 | `business/erp/transfer_handler_db_tests_create.rs` | `erp/transfer_handler.rs:97` | 10 | 0 |
| 5 | `business/erp/transfer_handler_db_tests_fixtures.rs` | `erp/transfer_handler.rs:92` | 1 | 0 |
| 6 | `business/erp/transfer_handler_db_tests_query.rs` | `erp/transfer_handler.rs:114` | 2 | 0 |
| 7 | `business/erp/transfer_handler_db_tests_settle.rs` | `erp/transfer_handler.rs:106` | 21 | 0 |
| 8 | `business/erp/transfer_handler_db_tests_status.rs` | `erp/transfer_handler.rs:102` | 17 | 0 |
| 9 | `business/erp/transfer_handler_test_support.rs` | `erp/mod.rs:235` `#[cfg(test)] mod` | 0（夹具，函数全 `#[cfg(test)] pub(super)`） | 0 |
| 10 | `business/erp/transfer_handler_tests.rs` | `erp/transfer_handler.rs:87` | 2 | 0 |
| 11 | `business/flow/flow_model_delete_tests.rs` | `flow/model.rs:55` `#[cfg(test)] #[path]` | 8 | 0 |
| 12 | `business/flow/flow_model_tests.rs` | `flow/model.rs:51` `#[cfg(test)] #[path]` | 6 | 0 |
| 13 | `business/flow/linkage/tests_presets.rs` | `flow/linkage.rs:63` `#[cfg(test)] mod` | 13 | 0 |
| 14 | `business/flow/linkage/tests_wiring.rs` | `flow/linkage.rs:66` `#[cfg(test)] mod` | 5 | 0 |
| 15 | `business/task/task_handler_copy_db_tests.rs` | `task/handler.rs:127` | 3 | 0 |
| 16 | `business/task/task_handler_stats_tests.rs` | `task/handler.rs:119` | 3 | 0 |
| 17 | `business/task/task_handler_tests.rs` | `task/handler.rs:123` | 18 | 0 |
| 18 | `system/auth/cache_tests.rs` | `system/auth/mod.rs:37` `#[cfg(test)] mod` | 7 | 0 |

结论：18 条全部是**测试面**（P2-1B flow/model 拆分 2 条 + 各模块 handler 拆分的测试部件 16 条），
补登记不扩大生产面的豁免半径 —— 生产面仍然是 378 个文件，跳过面从 61 → 79 条且逐条有声明位置可查。

复核命令：

```bash
python3 scripts/lint/scope_gate.py --root . --list-skipped | grep -E 'expense_model_tests|transfer_handler_db_tests_books|flow_model_tests|task_handler_tests|cache_tests'
```

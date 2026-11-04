# 2026-11-04 P2-5 cargo test 结论

## 一次全量（干净库 cp_p25_test + P0FIX_DB_URL=cp_p25_p0fix，`--test-threads=1`）

```
test result: FAILED. 2313 passed; 7 failed; 0 ignored; 0 measured; 0 filtered out; finished in 1196.98s
```

7 条失败**全部**是 `business::flow::internal_control::db_tests::*`，
原因见 `skill/cloudpivot-backend-workflow/references/fresh-db-and-instance-hygiene.md` §2：

> `set_db()` 是进程级 `OnceLock` ⇒ 一个进程只能一个库，所以**分模块分库分进程**；
> 跑 `business::flow` 要 `--skip business::flow::internal_control::db_tests`，
> 那 7 条单独用 `P0FIX_DB_URL` 指向干净 p0fix 库跑（否则 ensure_db 会 panic）。

本轮 panic 原文（`cargo-test-full.log` 尾部）：

```
[internal_control::db_tests] 全局 db() 已被同进程更早的用例初始化，且连的不是 P0FIX 库 ——
本组用例拒绝在别人的库上跑
  · 当前全局连接实际连的库：cp_p25_test
  · P0FIX_DB_URL 要求的库    ：cp_p25_p0fix
```

→ **是「必须独立进程」的组被跑在了共享进程里，不是本卡改动导致的失败。**

## 那 7 条单独跑（独立进程，只有 P0FIX_DB_URL）

```
$ P0FIX_DB_URL=postgres://alun:alun@127.0.0.1:5432/cp_p25_p0fix cargo test --lib \
    business::flow::internal_control::db_tests -- --test-threads=1
running 7 tests
test ...::countersign_all_excluded_fails_closed ... ok
test ...::countersign_one_excluded_then_two_approve_passes ... ok
test ...::countersign_two_excluded_then_one_approves ... ok
test ...::excluded_user_cannot_bypass_by_calling_approve ... ok
test ...::remaining_approver_votes_normally ... ok
test ...::role_members_are_not_excluded ... ok
test ...::skipped_task_is_not_in_my_todo ... ok
test result: ok. 7 passed; 0 failed
```

## 合计

**2313（全量一次跑）+ 7（本组单独进程）= 2320 通过 / 0 真失败。**

本卡改动直接相关的 5 个模块（`business::shared::doc_kind`、`business::shared::state_machine`、
`business::erp::transfer_handler`、`business::erp::cancel_cascade`、`business::erp::fixture_hygiene`）
都在全量里跑过且全绿（日志见 `cargo-test-full.log`）：

```
$ grep -c "state_machine" cargo-test-full.log
$ grep "transfer_handler::db_tests" cargo-test-full.log | grep -c ok
```

（具体条数见同目录 `grep-summary.txt`）

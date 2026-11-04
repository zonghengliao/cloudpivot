# t_9a6e3d35（第二次执行）· 干净隔离库全量 `cargo test -j 1` 连跑 2 次 = **0 failed**

> 结论：在**新建干净库** `cloudpivot_9a6e3d35b` 上，用**被测树自己编译的 release 二进制**自举（`应用 163 个迁移` → V0219 / 102 表 / admin 已种 / 超管 1），
> 连跑两轮全量 `cargo test -j 1`：**第 1 轮 2331 passed / 0 failed（1108.42 s），第 2 轮在同一库的残留上 2331 passed / 0 failed（1138.19 s）**，
> 两轮 `SKIP=0`、22 个 test target 合计 **2556 passed / 0 failed**、`exit=0`。
> 全程**没有**用 `--test-threads=1` 当修复手段，**没有**放宽/删除任何断言。

## 1. 被测树与二进制（先钉死"跑的是哪份代码"）

| 项 | 值 |
|---|---|
| 主仓 | `/opt/cloudpivot`，本地 `main` = `0528788`（t_18dcb777 的夹具身份改造，已含 t_71b01775 的盘点单路由 `73f2097`） |
| 被测树 | `/root/25926f63-tree` = `git archive HEAD(0528788)` + **t_f404258c 的 5 个未提交测试侧文件**（`contract_approval_tests.rs` / `fixture_hygiene.rs` / `period_items_tests.rs` / `period_test_support.rs` / `purchase_handler.rs`） |
| 有意识地排除 | 其他卡未提交的产品代码 WIP：`opening.rs` / `payable_payment.rs` / `serial_handler.rs` / `voucher.rs`（树内与 HEAD 逐字节一致）、`frontend/src/views/business/flow/**` |
| 本卡新增 | `tests/batchT_t1_task.rs` 的 `DB_LOCK`（见 §4，28 行插入，断言零改动） |
| 自举二进制 | `/root/9a6e3d35-target/release/cloudpivot`，50 809 288 B，sha256 `fcbf28444079d9640ac7e0ccb23f7d9d740577ee764b7fcbdf0770941bebebd7`（与上一轮第 1 次执行同一指纹 —— 因为 88fe548→0528788 的后端改动**全部在 `#[cfg(test)]` 里**，release 产物不变，可作交叉印证） |
| 测试二进制 | `/root/9a6e3d35-target/debug/deps/*`（私有 `CARGO_TARGET_DIR`，不与别的卡共用） |

## 2. 库指纹（脚本 `scripts/verify/erp_clean_db.sh`，REPO/BIN 都指向被测树）

```
==> created: cloudpivot_9a6e3d35b (owner=alun)
2026-11-04T15:27:09Z  INFO nik_web::app: 数据库迁移完成: 应用 163 个迁移
schema_migrations(max|count) = 219|163      public 表数 = 102
admin 存在且持 SUPER_ADMIN = 1              超管人数 = 1
```

- 卡面写的「`应用 155 个迁移` / V211 / 101 表」是 **DEPLOY-C 那支 release（`24655b0`）** 的数字；
  被测树含 `V0212…V0219` 8 条 ERP 迁移，所以是 163 / V0219 / 102。口径不变：同一支二进制起一次实例、引擎全量应用迁移 + bootstrap。
- 正式验收的两轮之间**没有重建库**（`fingerprints2.txt` 里 `before-run3` → `after-run4` 的迁移数/表数一致）。

## 3. 两次全量的完整 `test result:` 行

```
# 库：cloudpivot_9a6e3d35b（00:12:41 重建干净库，run3→run4 之间未重建）
# 环境：ERP_DB_URL = SYS_DB_URL = TASK_DB_URL = postgres://alun:***@127.0.0.1:5432/cloudpivot_9a6e3d35b
#       NIK_PROFILE=test  CARGO_TARGET_DIR=/root/9a6e3d35-target  CARGO_BUILD_JOBS=2
# 命令：cargo test -j 1        （-j 只限编译并行度；测试线程是 libtest 默认 4）

【run3 · 干净库】2026-11-05 00:12:41 → 00:34:08 CST，exit=0，1287 s，22 个 test target，SKIP=0
test result: ok. 2331 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 1108.42s
test result: ok. 0 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.00s
test result: ok. 1 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.00s
test result: ok. 1 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.00s
test result: ok. 1 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.00s
test result: ok. 11 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.00s
test result: ok. 8 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.02s
test result: ok. 4 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.00s
test result: ok. 0 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.00s
test result: ok. 1 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.00s
test result: ok. 5 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.28s
test result: ok. 6 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.00s
test result: ok. 9 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.00s
test result: ok. 7 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.00s
test result: ok. 62 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.01s
test result: ok. 12 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.00s
test result: ok. 14 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.00s
test result: ok. 12 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.00s
test result: ok. 1 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.00s
test result: ok. 2 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.00s
test result: ok. 67 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 160.68s
test result: ok. 1 passed; 0 failed; 11 ignored; 0 measured; 0 filtered out; finished in 0.00s

【run4 · 同一库（带 run3 残留）】2026-11-05 00:34:08 → 00:56:14 CST，exit=0，1326 s，22 个 test target，SKIP=0
test result: ok. 2331 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 1138.19s
（其余 21 行同 run3：全部 `ok. … 0 failed`，最大差异是 testd 67 passed 用了 167.84 s；完整原文见 `run4.log.txt`）
```

- **lib 主套件 2331 条两轮全绿**，第 2 轮跑在第 1 轮的残留上 ⇒ 用例清场是干净的，不靠清库。
- `SKIP=0`：真库用例没有一条走 SKIP 假绿（`ERP_DB_URL` / `SYS_DB_URL` / `TASK_DB_URL` 三个都指同一个隔离库）。
- 两轮 `biz_task=48 / biz_flow_instance=10 / biz_purchase_order=6 / biz_status_log=56` **完全一致**（`fingerprints2.txt`）⇒ 夹具不随轮次累积。

## 4. 本轮新暴露并已收口的缺陷：`tests/batchT_t1_task.rs` 真库用例外撞

lib 变绿之后 `cargo` 才会继续跑集成 target（此前 lib 一红就停，这个 target 从未被真正执行过，历史上一直打印 `SKIP` 后 0.00 s 返回）。
在被测树上**先跑两轮未修复版本**：

```
【run1 · 干净库】lib：test result: ok.   2331 passed; 0 failed; … 1139.43s     ← lib 已绿
                batchT_t1_task：test result: FAILED. 1 passed; 4 failed; … 0.13s
test my_buckets_and_overdue_scan ... FAILED
test crud_roundtrip ... FAILED
test visibility_four_scenarios ... FAILED
test task_no_is_unique_and_increasing ... FAILED
【run2 · 同库残留】lib：test result: ok. 2331 passed; 0 failed; … 1118.25s
                batchT_t1_task：test result: FAILED. 4 passed; 1 failed; … 0.35s
test crud_roundtrip ... FAILED
```

红集合两轮不同（4 红 vs 1 红）⇒ 并发互踩，不是产品回归。报错原文：

```
建研发部: UniqueViolation … duplicate key value violates unique constraint "sys_dept_pkey"
          Key (id)=(7e000000-0000-0000-0000-000000000001) already exists
绑定角色: ForeignKeyViolation … sys_user_role_user_id_fkey
          Key (user_id)=(7c000000-0000-0000-0000-000000000001) is not present in table "sys_user"
```

根因：该 target 的 4 条真库用例共用**同一批固定 UUID 夹具**（`7c/7e/7f` 段的 5 用户 / 3 部门 / 2 角色 + `t1t_*` 用户名），
`cleanup()` 也按这批固定 id 整批删；而且 `visibility_four_scenarios` 里有一条**整库型断言**
（「超管可见条数 == 库内未软删任务总数」）。默认 4 线程下它们并行地「清同一批行 → 插同一批行」。
文件头原本就写着「用法：`-- --test-threads=1`」—— 也就是说这个 target 一直是靠人工串行跑的。

修复（`backend/tests/batchT_t1_task.rs`，+28/-1，**只有注释与一把锁**）：

- 新增 `static DB_LOCK: tokio::sync::Mutex<()>`（与 `erp::fixture_hygiene::FINANCE_SEED_LOCK` 同思路：**同一批全局行 = 同一把锁**）；
- 4 条真库用例在 `ensure_db()` 之后取 `_db_lock`，用例结束时随 guard 释放；
- **断言一条没改、断言强度没动、命令行线程数没动**；被 `cargo test -j 1` 调用时仍是 libtest 默认 4 线程。

为什么不用"改成每条用例一套唯一身份"：这条路径上还有**整库型断言**（超管可见数 == 全库未软删总数），
身份唯一化也排除不掉并发下别的用例正在建/删任务造成的抖动，只能靠同一把锁把「共用这批行 + 整库断言」的用例互斥。
这一条与 `erp` 侧的修法不冲突：那边是"夹具身份唯一化"，这边是"共用种子行 + 整库断言 ⇒ 共锁"。

A/B 结果（同一库、同一二进制、同一条命令）：

| 版本 | batchT 结果 |
|---|---|
| 未修复（run1/run2） | 1 passed / 4 failed；4 passed / 1 failed |
| 已修复（run3/run4） | **5 passed / 0 failed（0.28 s）**；**5 passed / 0 failed** |

## 5. 对照：改动前的失败行（原文）

```
docs/evidence/20261103-deployc/cargo-test-clean.log（DEPLOY-C 干净库 cloudpivot_deployc，新 release 自举 155 迁移/V211/101 表）
405:test business::erp::payable_bridge::tests::zz_teardown_purges_payment_status_logs ... FAILED
450:test business::erp::period_items::tests::can_close_is_unaffected_by_items ... FAILED
2222:test result: FAILED. 2187 passed; 2 failed; 0 ignored; 0 measured; 0 filtered out; finished in 631.80s

docs/evidence/20261103-deployc/cargo-test-parallel.log（同口径首轮）
451:test business::erp::period_items::tests::can_close_is_unaffected_by_items ... FAILED
2217:test result: FAILED. 2188 passed; 1 failed; 0 ignored; 0 measured; 0 filtered out; finished in 634.93s

/root/fixcon/logs/before-targeted.log（顺序跑 t=1 时的 contract_approval 两条）
6:test business::erp::contract_approval_tests::large_amount_requires_two_levels_in_sequence ... FAILED
10:test business::erp::contract_approval_tests::small_amount_single_level_approval_signs_contract ... FAILED
80:test business::erp::period_items::tests::can_close_is_unaffected_by_items ... FAILED
228:test result: FAILED. 190 passed; 3 failed; 0 ignored; 0 measured; 2011 filtered out; finished in 161.78s
```

> 卡面提到的 `/root/dc-regression/cargo-test-clean.log` 第 144 行在本机**不存在**（`/root/dc-regression/` 目录已不在）；
> 同一份 DEPLOY-C 干净库日志在仓内 `docs/evidence/20261103-deployc/cargo-test-clean.log`，上面引的就是它的原文行号。

这些历史红在本轮两轮里的状态（`grep` 原文，run3/run4 一致）：

```
test business::erp::contract_approval_tests::large_amount_requires_two_levels_in_sequence ... ok
test business::erp::contract_approval_tests::small_amount_single_level_approval_signs_contract ... ok
test business::erp::period_items::tests::can_close_is_unaffected_by_items ... ok
test business::erp::payable_bridge::tests::p17_skip_approval_payment_logs_are_purged_by_id ... ok
test business::erp::payable_bridge::tests::p17_seeded_flow_graph_is_consistent ... ok
test system::menu::tests::n08_menu_paths_all_resolve_in_frontend_router ... ok
test business::masking::tests::m05_list_and_detail_share_same_ruleset ... ok
test business::masking::tests::m07_config_change_takes_effect_immediately ... ok
test system::user::tests::u04_locked_user_cannot_login ... ok
test system::user::tests::u06_disabled_user_cannot_login ... ok
test system::user::tests::u07_delete_user_keeps_business_rows ... ok
test system::role::tests::r03_role_in_use_cannot_be_deleted ... ok
test system::role::tests::r05_mutex_roles_rejected ... ok
test system::role::tests::r07_assign_menus_bumps_holders_token_version ... ok
test system::tests_support::self_check::x00_suite_leaves_zero_fixture_residue ... ok
```

注：`zz_teardown_purges_payment_status_logs` 这条已由 `t_bfa81333` 重写为
`p17_skip_approval_payment_logs_are_purged_by_id`（不再依赖「`zz_` 名字排序最后跑」），原名在树里只剩注释。

## 6. 观察项（不阻塞本卡，供后续卡参考）

1. 一轮全量跑完，库里有 **2 个 p17 夹具用户仍是活的 `SUPER_ADMIN`**（`p17_ap1` / `p17_ap2`），
   所以「超管人数」从 1 变 3 并**稳定停在 3**（run3 后 3、run4 后仍是 3，不增长）。
   `t_18dcb777` 把这两个用户改成"解绑 + 软删、setup 复活"，但收尾时没有把它们置成软删/解绑，于是留在库里。
   影响面：只影响测试库；本轮 run4 就是**在超管 3 人的库上跑出 2331/0**，说明用例已不受会签人数影响。
2. `tests/testd.rs` 缺省连 **共享库 `cloudpivot_test`**（它自己文件头写明），本轮两轮各 67 条都在那把库上真实执行、0 failed；
   如果后续要把"全库隔离"做到极致，给 testd 也传 `TESTD_DB_URL=cloudpivot_test_<tag>`。

## 7. 复现命令

```bash
# 树
mkdir -p /root/25926f63-tree && cd /opt/cloudpivot && git archive HEAD | tar -x -C /root/25926f63-tree
for f in contract_approval_tests.rs fixture_hygiene.rs period_items_tests.rs period_test_support.rs; do
  cp backend/src/business/erp/$f /root/25926f63-tree/backend/src/business/erp/$f; done
cp backend/src/business/erp/purchase_handler.rs /root/25926f63-tree/backend/src/business/erp/purchase_handler.rs
cp backend/config/config-dev.toml backend/config/config-test.toml /root/25926f63-tree/backend/config/
# 建干净库 + 两轮（脚本已随本目录入库）
bash docs/evidence/20261104-testfix-parallel/9a6e3d35b/run_twice_fixed_tree.sh
```

原始日志：本目录 `run1.log.txt`（未修复版干净库）、`run2.log.txt`（未修复版残留库）、`run3.log.txt`（验收第 1 轮）、`run4.log.txt`（验收第 2 轮）、
`summary.tsv` / `summary2.tsv` / `fingerprints2.txt` / `clean2-db.out` / `release-bin.sha256`。

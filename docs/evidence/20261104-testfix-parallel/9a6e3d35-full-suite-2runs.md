# 干净隔离库全量 `cargo test -j 1` 连跑 2 次 —— 证据（卡 t_9a6e3d35）

> **结论：未达卡面验收（两次全量均 0 failed）。**
> 第一次全量（全新自举的干净库）就 **20 failed**，第二次（同一库、带 run1 残留）**19 failed**，
> 两次**共同红 15 条、各自独有 5 / 4 条**（同一二进制、同一库，红的集合会变 ⇒ 并发互踩，不是确定逻辑）。
> 失败**不落在本卡修复面的 5 条历史假红**上（那 5 条在两次全量里全部 `ok`），
> 集中在 `business::masking`(7) / `system::user`(6) / `system::role`(4) / `system::` 残留自检(1)
> / `system::menu` 真实 404(1) / `payable_bridge` 夹具清理 helper(1)。
> **全程未使用 `--test-threads=1` 当修复手段，未放宽/删除任何断言**（`--test-threads=1` 只用于 §7 的对照诊断）。

- 执行时间：2026-11-04 21:06–22:20 CST
- 执行人：test-enginner
- 关联：`docs/evidence/20261103-deployc/README.md` **第 6 节**（干净隔离库 `cloudpivot_deployc` 上 `cargo test -j 1` = 2187 passed / 2 failed）
- 前置卡：`t_25926f63`（4 模块 `--test-threads=4` ×3 全绿）、`t_f404258c`、`t_bfa81333`

---

## 1. 被测版本与二进制（可复现）

| 项 | 值 |
|---|---|
| 基线提交 | `88fe5489c2dd43c21afaabec4c9f1a2d509e314a`（t_bfa81333：zz_teardown 按 id 清场） |
| 叠加的未提交修复 | t_f404258c 的 5 个文件（`contract_approval_tests.rs` / `fixture_hygiene.rs` / `period_items_tests.rs` / `period_test_support.rs` / `purchase_handler.rs`） |
| 独立验证树 | `/root/25926f63-tree`（与前置卡同一棵，未改一行） |
| 自举用**新 release 二进制** | `/root/9a6e3d35-target/release/cloudpivot`，2026-11-04 21:06，50 809 288 B，sha256 `fcbf28444079d9640ac7e0ccb23f7d9d740577ee764b7fcbdf0770941bebebd7`（`cargo build --release`，源=上面的树） |
| 测试构建目录 | `/root/9a6e3d35-target`（本卡私有；从共享 target 复制后独占，避免与他卡并发覆盖二进制） |
| 全量命令 | `ERP_DB_URL=postgres://alun:***@127.0.0.1:5432/cloudpivot_9a6e3d35 SYS_DB_URL=同 TASK_DB_URL=同 NIK_PROFILE=test CARGO_TARGET_DIR=/root/9a6e3d35-target cargo test -j 1` |

## 2. 库

| 项 | 值 |
|---|---|
| 库名 | `cloudpivot_9a6e3d35`（owner `alun`） |
| 建库方式 | `scripts/verify/erp_clean_db.sh`（`RECREATE=1`，`BIN=` 上面那支新 release 二进制） |
| 自举日志 | `/root/9a6e3d35/bootstrap-server.log`：`数据库迁移完成: 应用 163 个迁移` |
| 指纹（run1 前） | `schema_migrations` max **V0219** / **163 条**；public 表 **102**；`admin` 已种 1；超管 **1** 人 |
| 是否中途重建 | **否**（run1 → run2 之间无 drop/create/truncate；run2 指纹仍是 V0219/163 条） |
| 与卡面「155 迁移 / V211 / 101 表」的差异 | 卡面数字来自 DEPLOY-C 那支 release（`24655b0`，最后一条迁移 V0211）。本次用**被测树自己的 release**自举，树里已含 `V0212…V0219` 共 8 条 ERP 迁移 ⇒ 163 条 / V0219 / 102 表。口径未变：同一支二进制起一次实例，引擎把 migrations 全部应用 + bootstrap，没有手工补迁移 |

## 3. 第一次全量（干净库，run1）

```
cmd    : ERP_DB_URL=…/cloudpivot_9a6e3d35 cargo test -j 1
window : 2026-11-04 21:18:47 → 21:37:38 CST（1131s 含编译，测试本体 1130.31s）
结果   : test result: FAILED. 2310 passed; 20 failed; 0 ignored; 0 measured; 0 filtered out; finished in 1130.31s
SKIP   : 0 行（真库用例没有一条走 SKIP 假绿）
```

20 条失败（按模块）：`business::masking` 7、`system::user` 6、`system::role` 4、
`system::tests_support::self_check` 1、`system::menu` 1、`business::erp::payable_bridge` 1。

<details><summary>run1 失败清单（原文 `test ... FAILED`）</summary>

```
business::erp::payable_bridge::tests::p17_seeded_flow_graph_is_consistent
business::masking::tests::m01_create_rule_and_reject_duplicate
business::masking::tests::m02_unauthorized_role_gets_masked_value
business::masking::tests::m03_exempt_principal_sees_plaintext
business::masking::tests::m04_export_applies_mask_from_database_rule
business::masking::tests::m05_list_and_detail_share_same_ruleset
business::masking::tests::m07_config_change_takes_effect_immediately
business::masking::tests::m10_sweep_drains_fk_pinned_fixture_and_stays_prefix_scoped
system::menu::tests::n08_menu_paths_all_resolve_in_frontend_router
system::role::tests::r02_role_permission_change_takes_effect_immediately
system::role::tests::r05_mutex_roles_rejected
system::role::tests::r06_duplicate_role_code_conflict
system::role::tests::r07_assign_menus_bumps_holders_token_version
system::tests_support::self_check::x00_suite_leaves_zero_fixture_residue
system::user::tests::u02_duplicate_username_conflict
system::user::tests::u03_password_reset_bumps_token_version
system::user::tests::u04_locked_user_cannot_login
system::user::tests::u05_repeated_login_failures_lock_account
system::user::tests::u08_dept_change_updates_data_scope_immediately
system::user::tests::u09_user_list_search_and_pagination
```
</details>

## 4. 第二次全量（同一库，run1 残留之上）

run2 前残留指纹（`fp-before-run2.txt`）：超管 **3** 人（run1 留了 2 个带 SUPER_ADMIN 的夹具用户）、
`sys_user` 25 / `sys_role` 10、`biz_voucher` 13、`biz_accounting_period` 43、`biz_purchase_order` 6、
`biz_status_log` 91、`sys_field_mask` 4 条规则、`tc_` 残留 user 2 / role 1 / mask 4。

```
cmd    : ERP_DB_URL=…/cloudpivot_9a6e3d35 cargo test -j 1     （库未清空、未重建）
window : 2026-11-04 21:46:50 → 22:13:00 CST（1570s 含一次重编译，测试本体 1309.76s）
结果   : test result: FAILED. 2311 passed; 19 failed; 0 ignored; 0 measured; 0 filtered out; finished in 1309.76s
SKIP   : 0 行
```

**两轮的差集（同一二进制、同一库，红的集合不稳定）**：

- 两次都红 15 条；
- 只在 run1 红 5 条：`payable_bridge::p17_seeded_flow_graph_is_consistent`、`masking::m05`、`masking::m07`、`role::r06`、`user::u04`；
- 只在 run2 红 4 条：`payable_payment::d5_cancel_payable_revokes_pending_payment`、`role::r03`、`user::u06`、`user::u07`。

## 5. 改动前对照（同一命令的失败行，原文引用）

`/opt/cloudpivot/docs/evidence/20261103-deployc/cargo-test-clean.log`（DEPLOY-C 干净库 `cloudpivot_deployc`）：

```
405:test business::erp::payable_bridge::tests::zz_teardown_purges_payment_status_logs ... FAILED
450:test business::erp::period_items::tests::can_close_is_unaffected_by_items ... FAILED
2222:test result: FAILED. 2187 passed; 2 failed; 0 ignored; 0 measured; 0 filtered out; finished in 631.80s
```

同口径首轮 `20261103-deployc/cargo-test-parallel.log`：

```
451:test business::erp::period_items::tests::can_close_is_unaffected_by_items ... FAILED
2217:test result: FAILED. 2188 passed; 1 failed; 0 ignored; 0 measured; 0 filtered out; finished in 634.93s
```

顺序跑（`--test-threads=1`）时的 `contract_approval_tests` 两条，`/root/fixcon/logs/before-targeted.log`：

```
6:test business::erp::contract_approval_tests::large_amount_requires_two_levels_in_sequence ... FAILED
10:test business::erp::contract_approval_tests::small_amount_single_level_approval_signs_contract ... FAILED
80:test business::erp::period_items::tests::can_close_is_unaffected_by_items ... FAILED
228:test result: FAILED. 190 passed; 3 failed; 0 ignored; 0 measured; 2011 filtered out; finished in 161.78s
```

同族「同一库第二轮 2 failed」（`docs/evidence/20261104-test-hygiene/raw_prefix_2failed/erp_run2_同一库第二轮_2failed_修复前.txt`）：

```
47:test business::erp::contract_approval_tests::large_amount_requires_two_levels_in_sequence ... FAILED
50:test business::erp::contract_approval_tests::small_amount_single_level_approval_signs_contract ... FAILED
876:test result: FAILED. 838 passed; 2 failed; 0 ignored; 0 measured; 1364 filtered out; finished in 919.87s
```

⇒ 上面这 5 条历史假红（`period_items::can_close` / `payable_bridge::zz_teardown` / `contract_approval` ×2 及其同族）
在本次 run1、run2 **全部 `ok`**，一条都没进失败清单 ⇒ 本卡修复面对那 4 个 ERP 模块的隔离改造有效。

## 6. 20 条失败是谁（分类与典型报错）

| 类 | 条数 | 典型报错 | 判据 |
|---|---|---|---|
| A. 模块内前缀清扫互删（`masking`/`system::user`/`system::role`） | 17 | `执行 SQL 失败：foreign key violated (constraint sys_user_role_user_id_fkey) … INSERT INTO sys_user_role`、`30001 操作者账号不存在，拒绝操作`、`用户应存在`、`改后必须仍命中`、`系统内部错误 规则写入成功但回读失败` | 同库同二进制、`--test-threads=1` 全绿、`--test-threads=4` 红（§7）⇒ 并发窗口里 A 用例的 sweep 把 B 用例的「操作者用户/规则/角色」删了 |
| B. 残留自检把 A 的垃圾记账 | 1 | `x00_suite_leaves_zero_fixture_residue`：`角色夹具残留必须为 0 … 样例 tc_m_m07_…` | A 的清理被外键挡住 ⇒ 残留被 x00 正确抓到（断言没问题，是执行顺序问题） |
| C. 菜单种子 vs 前端路由缺失（**真实缺陷，非并发**） | 1 | `n08_menu_paths_all_resolve_in_frontend_router`：`business:erp:stock-take:list (/business/erp/stock-take, component=business/erp/stock-take/index)` 前端未注册路由 | 单独 `--exact --test-threads=1` 仍红（§7）⇒ 确定性缺陷：`V0211__erp_stock_take.up.sql` 种了菜单，但 `frontend/src/views/business/erp/` 下没有 `stock-take` 页面、`router/index.ts` 也无该路由 ⇒ 点进去必然 404 |
| D. 共享种子用户被跨模块流程任务引用 | 1（run1）/1（run2 换成 `d5_cancel_payable_revokes_pending_payment`） | `SQL 失败：foreign key violated (constraint biz_flow_task_assignee_id_fkey) … DELETE FROM sys_user WHERE id IN ('e7e70001-…0170'…)`；run2：`发起付款审批单: foreign key violated (biz_flow_task_assignee_id_fkey)` | 单条 `--exact --test-threads=1` 绿（§7）⇒ `payable_bridge`(p17 种子用户) 与 `payable_payment` 的流程任务互相踩，属跨模块共享夹具清理顺序问题 |

## 7. 诊断：同库、同二进制，只改测试线程数（干净库 `cloudpivot_9a6e3d35_diag`）

该库同样是新 release 二进制自举（`应用 163 个迁移` / V0219 / 超管 1），**没有跑过任何测试**，排除残留影响。
`--test-threads=1` 仅作**对照诊断**，不是本卡任何"修复"。

| 目标 | `--test-threads=4` | `--test-threads=1` | 结论 |
|---|---|---|---|
| `business::masking::tests`（10 条） | **8 passed / 2 failed**（m05、m07） | **10 passed / 0 failed** | 模块内并发互删 |
| `system::user::tests`（12 条） | **7 passed / 5 failed**（u03、u05、u07、u08、u09） | **12 passed / 0 failed** | 模块内并发互删 |
| `system::role::tests`（8 条） | **4 passed / 4 failed**（r02、r03、r05、r07） | **8 passed / 0 failed** | 模块内并发互删 |
| `payable_bridge::p17_seeded_flow_graph_is_consistent`（`--exact`） | — | **1 passed / 0 failed** | 全量里的红是跨模块共享夹具所致 |
| `system::menu::n08_menu_paths_all_resolve_in_frontend_router`（`--exact`） | — | **0 passed / 1 failed** | 确定性缺陷（菜单 404） |

原始日志：`/root/9a6e3d35/diag-*.log`，汇总 `/root/9a6e3d35/diag-summary.txt`。

## 8. 为什么改动前的全量是绿的（差异归因，供后续卡参考）

- 树比 DEPLOY-C 时多了 **57 个提交 / 145 条新用例**（`business::erp` 68、`impexp` 37、`shared` 16、`attachment` 15、`system::auth` 5 …），
  其中多个模块继续用「整批 `DELETE … LIKE 'tc_x%'`」当夹具清扫，同前缀族在 4 线程下互相删。
- `business::masking/tests.rs` 文件头自己写着「**必须 `--test-threads=1`**：`sweep()` 会清掉全部 `tc_m` 夹具，并行会与同前缀兄弟用例互删」——
  也就是说全量套件在默认线程数下**本来就是不安全的**，只是 DEPLOY-C 那次跑的 2189 条用例恰好没踩到。
- `system::user/role` 的清扫同样是前缀族（`tc_u%` / `tc_r%`），且会删到别的用例正在用的「操作者用户」⇒ `30001 操作者账号不存在`。

## 9. 后续

已建两张卡并让本卡以它们为父（本卡在它们完成后自动回到可执行，再验收「两次全量 0 failed」）：

1. `t_??`（backend-developer）：`business::masking` / `system::user` / `system::role` 夹具清扫改为并发安全（按本用例 id 集合删，禁止整批前缀扫），
   目标：干净库 `--test-threads=4` 连跑 3 次全绿；含 `payable_bridge` × `payable_payment` 共享 p17 用户与 `biz_flow_task.assignee_id` 外键的清理顺序。
2. `t_??`（frontend-developer）：`business:erp:stock-take:list` 种子菜单无前端页面/路由（n08 单独跑即红，点进去 404）。

**原始日志**：`/root/9a6e3d35/run1.log`、`run2.log`、`run1.status`、`run2.status`、`bootstrap-server.log`、
`fp-before-run2.txt`、`fp-after-run2.txt`、`run1.failures.txt`、`run2.failures.txt`、`diag-summary.txt`、`diag-*.log`、
`release-bin.sha256`。汇总表：`summary.tsv`。

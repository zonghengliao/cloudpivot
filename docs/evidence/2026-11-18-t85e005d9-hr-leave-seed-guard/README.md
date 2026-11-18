# t_85e005d9 · hr leave 两套真库夹具顶掉 V0393 正式种子且不恢复

**结论：已修。** 两套 leave 真库夹具改成「**借出 → 归还**」：装夹前把 V0393 的 `hr_leave`
整链抓成 JSON 再从库里删掉，收尾原样写回并按迁移字面量逐项对账；再用真实接口
（`create_leave` → `submit_leave`）证明还回来的种子**真能跑**。

跑完两轮全量套件后（同一个库 `cloudpivot_test_hrseed`）：

```sql
SELECT count(*) FROM biz_flow_def WHERE flow_code='hr_leave';   -- 1（修前是 0）
```

---

## 1. 现象（卡面）

```text
全新种子库：hr_employee_resign / hr_leave / hr_salary / hr_salary_adjust 四条 PUBLISHED
跑过一轮全量用例后：SELECT count(*) FROM biz_flow_def WHERE flow_code='hr_leave';  -- 0
```

原因：`uk_biz_flow_def_code` 是 `flow_code` 全局唯一（`deleted_at IS NULL` 的部分唯一索引），
夹具要写死自己的审批人 ⇒ 必须先删掉库里那条 `hr_leave`；旧写法删的正好是 V0393 的正式种子，
**删完不恢复** ⇒ 种子的快照 / hash / 5 个节点行 / 5 条边 / 审批角色绑定永久丢失。

## 2. 修法（卡面给的三选一 → 选「备份 / 恢复」）

新增 `backend/app/src/business/hr/leave_flow_seed_guard.rs`（`#[cfg(test)]`、`pub(crate)`），
两套夹具共用：

| 时机 | 做什么 | 落点 |
|:--|:--|:--|
| 装夹前 | `borrow()`：首次把种子五张表抓成 `jsonb`（`to_jsonb(行)`，进程内只抓一次），随后删掉整链腾出编码 | `seed_leave_flow` |
| 装夹前 | `purge_fixture_defs(note)`：只删**版本 `change_note` 是本夹具**或**0 版本**的定义；剩下不是种子的 `hr_leave` 定义 → 一句能读懂的 panic（而不是撞唯一索引的 duplicate key） | `seed_leave_flow` |
| 收尾 | `restore()`：`jsonb_populate_recordset(NULL::<table>, …)` 逐表原样写回（id / 时间戳 / 快照 / hash / 节点行 / 绑定一个不差），再按迁移字面量对账 | `zz_teardown_purges_fixtures` |
| 收尾 | 真实接口可用性：`create_leave` → `submit_leave`，断言实例钉在 `…e0393` / `…e0394`、`flow_code=hr_leave`、请假 2 天（<3 天）只过「直属上级审批」一个节点、待办落在部门负责人头上 | HR-25 收尾 |

**为什么不用卡面第 2 条（保留定义 + 只换版本）**：`ck_biz_flow_def_version_current_published`
只许 `PUBLISHED` 版本挂 `is_current`；手写夹具版本（`graph_hash = repeat('0',64)`、无 L1 行）
一旦 `PUBLISHED`，`flow::maintain` 的全库三方对账就报 `RowsEdited`（t_9c3551b0 第二轮实测）。
绕开它得同时写真 hash + L1 行，等于把发布事务在测试里重写一遍，比抓 JSON 更容易漂。

**为什么不用第 3 条（种子可重建）**：那要把 V0393 §2~§5 的 SQL 抄进 Rust，迁移一改就漂；
抓 JSON / 写回是「按列名对位」（加列、调列序都不用手改）。

## 3. 改了哪些文件

| 文件 | 改动 |
|:--|:--|
| `backend/app/src/business/hr/leave_flow_seed_guard.rs` | **新增**（`#[cfg(test)]`）：`borrow` / `purge_fixture_defs` / `restore` / `shape` / `assert_intact`，含 V0393 的字面量常量 |
| `backend/app/src/business/hr/mod.rs` | `#[cfg(test)] pub(crate) mod leave_flow_seed_guard;`（+5 行） |
| `backend/app/src/business/hr/leave_bridge_db_tests.rs` | `seed_leave_flow` 换成 `borrow()` + `purge_fixture_defs(FIXTURE_NOTE)`；收尾加 `restore()` + 断言 |
| `backend/app/src/business/hr/leave_handler_db_tests.rs` | 同上；收尾再加「用正式种子真提交一次」的四条断言 |

零迁移、零权限码、零菜单行、零生产代码（所有新代码都在 `#[cfg(test)]` 下）。

## 4. 验收（全部真跑，原始输出见同目录）

| 项 | 命令 | 结果 | 证据 |
|:--|:--|:--|:--|
| 卡面最后一条 SQL | `SELECT count(*) … flow_code='hr_leave'` | **1**，`graph_hash = 4fc6722a…1f97`（非零）、`change_note` 与迁移逐字一致、5 节点 / 5 边 / 1 绑定 | `07_sql_after_two_rounds.txt` |
| `leave_` 全绿 | `cargo test -p cloudpivot --lib -- --test-threads=1 leave_` | `75 passed; 0 failed` | `05_leave_after_two_rounds.out` |
| p16 全库对账全绿 | `cargo test -p cloudpivot --lib business::erp::stockin::stockin_handler::stockin_handler_tests_p16 -- --test-threads=1` | `17 passed; 0 failed`（含 `p16_graph_snapshot_selfcheck_is_clean`） | `06_p16_after_two_rounds.out` |
| 同一库连跑两轮全量 | `cargo test -p cloudpivot --lib -- --test-threads=1` ×2 | 第一轮 `6460 passed; 2 failed`；第二轮 `6445 passed; 17 failed` —— **17 条全部与本卡无关**，见 §6 | `01_full_round1.out` / `02_full_round2.out` |
| 收尾打印（证据） | `cargo test --lib zz_teardown_purges_fixtures -- --nocapture` | 两个模块各打一行「归还 V0393 正式种子：…hash=4fc6722a…」；HR-25 还有「真提交 + 清场后种子仍在」 | `04_teardown_nocapture.out` |

### 假绿对照（证明新断言有牙，不是空转）

在控制库上**手删** V0393 的整链（模拟旧版夹具造成的污染），再跑 `leave_`：

```text
test result: FAILED. 73 passed; 2 failed
zz_teardown_purges_fixtures（HR-25 / HR-27 各一条）
  panic: hr_leave 正式种子（V0393）必须**恰好 1 条**未删定义，实际 0 条（…）：
         跑完夹具后种子没回来就是这张卡的 bug；种子从头就不在 ⇒ 用
         scripts/verify/make_full_suite_db.sh 重建演练库
```

⇒ 库被污染时**会红并点名重建库**，不会静默放过（`03_negative_control_polluted_db.out`）。

## 5. 复现（照卡面）

```bash
ERP_DB=cloudpivot_test_hrseed bash scripts/verify/make_full_suite_db.sh
export ERP_DB_URL='postgres://cloudpivot:***@172.18.10.145:5432/cloudpivot_test_hrseed'
export SYS_DB_URL="$ERP_DB_URL" TASK_DB_URL="$ERP_DB_URL" P0FIX_DB_URL="$ERP_DB_URL" HR_DB_URL="$ERP_DB_URL"
psql "$ERP_DB_URL" -c "select flow_code,status from biz_flow_def where flow_code like 'hr_%'"   # 4 条
cd backend/app && cargo test -p cloudpivot --lib -- --test-threads=1 leave_
psql "$ERP_DB_URL" -c "select count(*) from biz_flow_def where flow_code='hr_leave'"            # 1 ← 修好了
```

## 6. 与卡面口径的偏差（只报不改，逐条给证据）

卡面第三条验收写「同一库连跑两轮全量仍 0 failed」。实测**做不到 0 failed**，但 17 条红
**没有一条**来自本卡（也不来自 `hr` 域）：

### 6.1 第一轮 2 条红：HEAD 上就红的兄弟卡遗留

| 用例 | 报错 | 来源 |
|:--|:--|:--|
| `system::menu::permission_audit_tests::n18_route_level_exception_ledger_is_live` | `business:erp:framework-agreement:page：后端源码已有引用（business/erp/purchase/framework_agreement_dto.rs）` | 卡 **M4-11b**（t_f358da98，提交 3d256f92）：该文件里既在文档注释里写了页面码，又在 `#[cfg(test)]` 里写了 `!AGREEMENT_PERMISSIONS.contains("…:page")` 的反证断言 ⇒ 违反 AGENTS 约定 14 的「后端源码零引用页面码」。`git show HEAD:…framework_agreement_dto.rs` 可复现（与本次改动无关） |
| `system::menu::tests::n08_menu_paths_all_resolve_in_frontend_router` | `supplier-access (/business/erp/supplier-access) 前端未注册路由` | 卡 **M4-14**（t_583852b1，提交 209eefe1）：后端 `V0455` 建了菜单行，前端 `frontend/src/router/index.ts` 至今没有对应路由（`grep -c supplier-access frontend/src/router/index.ts` = 0） |

### 6.2 第二轮多出 15 条：别的夹具把演示账号的角色绑定整表删了

`business::erp::cost::cost_period_approval_db_tests::*`（5 条）+ `business::erp::period::tests::p1_*/p3_*`（10 条）：

```text
夹具污染：FINANCE_MANAGER 上有夹具与演示种子之外的在职成员 ["tst02b.finance_manager"]
会计期间 2031-09 不能结账：成本核算未结算（… 批次 CS-203109-0028 状态「已提交待审批」…）
```

库侧对照（同一时刻）：`cloudpivot_test_hrseed` 里五个审批岗位的成员已全部变成夹具账号
（`p3a_0002/p3a_0003/p3a_0004/p2c1_reviewer/tst02b.finance_manager`），演示账号
`demo_finance_mgr` 等**仍在职但 `sys_user_role` 绑定没了**；而只跑了 cost 域的控制库
`cloudpivot_test_t85ctrl` 上演示绑定完好、cost 域连跑两轮全绿 ⇒ 污染来自**别的域**的
「按 `role_id` 整表删 `sys_user_role`」型清场（例：`stockin_handler_tests_helpers.rs:377/633`
的 `DELETE FROM sys_user_role WHERE role_id = '{ROLE_WH_MGR}'::uuid`），随后
`test_helpers::ensure_role_member` 见岗位没人就补出 `tst02b.<role>`，把 cost 的严格断言打红。

⇒ 与 t_7938e24a 同一族（夹具卫生），已开后续卡，不在本卡范围。

## 7. 附：本卡不涉及的东西

- 没有新增/修改迁移（版本号仍是 V0456 之后未变）
- 没有新增权限码 / 菜单行（41 行不变）
- 没有改生产代码路径：`leave_flow_seed_guard` 整个模块带 `#![cfg(test)]`

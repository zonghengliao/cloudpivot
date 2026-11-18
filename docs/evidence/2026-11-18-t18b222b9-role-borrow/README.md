# t_18b222b9 · 第二轮 15 条红：演示账号 `sys_user_role` 绑定被整表清场 + 成本批次残留

**结论：已修。** 两条根因都改了判据、不是改期望值：

1. **演示账号的岗位绑定被「按 `role_id` 整表删」型清场永久摘掉** —— 5 处夹具改成
   **借出 → 归还**（`fixture_hygiene::borrow_role_bindings` / `restore_role_bindings`），
   并给每个模块的收尾加一条**断言**：跑完之后按 `system::setup::model::DEMO_USERS` 逐条核对
   「演示账号在它该在的岗位上」，漏还一条当场点名。
2. **成本批次停在 `SUBMITTED` 不收回** —— 根因①的下游：cost 用例在 `role_tasks` 断言处 panic，
   尾部的 `purge()` 没跑到，`CS-203109-00xx` 就挡住 `period` 的 10 条结账用例。
   ①修好后它自然不再残留；另加 `cost::...::zz_teardown_purges_fixtures` 兜住「用例中途 panic」
   这一形态（按本模块建单人收全部期间批次 + 逐项断言残留 0）。

验收（全新种子库 `cloudpivot_test_t18v3`，同一库连跑两轮全量）：

```text
round 1: test result: ok. 6564 passed; 0 failed; 1 ignored
round 2: test result: ok. 6564 passed; 0 failed; 1 ignored
跑完两轮后库形态 —— 与种子库基线**逐字一致**（7 个岗位、无夹具残留）：
DEPT_LEADER | demo_lead
FINANCE_DIRECTOR | (空)
FINANCE_MANAGER | demo_finance,demo_finance_mgr
GENERAL_MANAGER | demo_gm
PURCHASE_MANAGER | demo_purchase,demo_purchase_mgr
SALES_SUPERVISOR | demo_sales_mgr
WAREHOUSE_MANAGER | demo_wh
biz_cost_period=0 biz_cost_period_item=0 COST_SETTLE_instances=0
```

---

## 1. 现象（卡面）

同一库 `cloudpivot_test_hrseed` 连跑两轮全量：第一轮 15 条绿，第二轮 15 条红。

| 用例 | 报错 |
|:--|:--|
| `business::erp::cost::cost_period_approval_db_tests::db::*`（5 条） | `夹具污染：FINANCE_MANAGER 上有夹具与演示种子之外的在职成员 ["tst02b.finance_manager"]` |
| `business::erp::period::tests::p1_* / p3_*`（10 条） | `会计期间 2031-09 不能结账：成本核算未结算（1 个批次：CS-203109-0028 状态「已提交待审批」…）` |

## 2. 根因（两族，实测确认）

### ① 五个审批岗位的种子绑定被整表删、且删完不还

lib 套件里**真正**越界的是下面 5 处（`grep -rn "DELETE FROM sys_user_role" backend/app/src/`
逐条核过，其余全是「按自己的用户名/角色前缀/固定 id 认领」，不越界）：

| 文件 | 原写法 | 删掉的是谁 |
|:--|:--|:--|
| `business/erp/pflow3b_tests.rs:168`（purge_fixtures） | `role_id IN (f0005 FINANCE_MANAGER, f0006 FINANCE_DIRECTOR)` —— **整表删** | `demo_finance` / `demo_finance_mgr` |
| `business/erp/purchase/purchase_handler.rs:2800`（setup_approval） | `role_id IN (DEPT_LEADER, PURCHASE_MANAGER, GENERAL_MANAGER) AND user_id NOT IN (本卡三个)` | `demo_lead` / `demo_purchase` / `demo_purchase_mgr` / `demo_gm` |
| `business/erp/stockin/stockin_handler_tests_helpers.rs:377`（setup_cancel_reviewer） | `role_id = WAREHOUSE_MANAGER AND user_id <> 复核人` | `demo_wh` |
| `business/erp/stockin/stockin_handler_tests_helpers.rs:633`（setup_post_approval） | 同上 | `demo_wh` |
| `business/erp/voucher/mod.rs:3638`（cancel_via_approval） | 同上（复核人换 `p2c1_reviewer`） | `demo_wh` |

（另有一处 `stock_policy/water_check_db_tests.rs:642` 是「先记成员 → 整表删 → 用完 bind 回来」，
语义本身是借出/归还，但**中途 panic 就还不了**，一并改成走同一本台账。）

滚雪球链条（这就是「只红第二轮」的原因）：

```
第一轮：种子库基线（岗位上有 demo_*）→ 上面 5 处摘掉 → 用本模块夹具 → 不还
        ⇒ 第一轮所有用例都绿（它们跑的时候自己的绑定都在）
第一轮末：五个岗位的成员只剩夹具账号（实测库侧：p3a_0002 / tst02b.finance_manager / ...）
第二轮：别的模块 ensure_role_member 见 FINANCE_MANAGER **一个在职成员都没有**
        ⇒ 补出 tst02b.finance_manager ⇒ cost 的严格断言先红（5 条）
        ⇒ cost 那 5 条在断言处 panic，尾部 purge() 没跑到 ⇒ 2031-09 的批次停在 SUBMITTED
        ⇒ period 的 10 条结账用例被它挡住
```

`FINANCE_MANAGER` 被清空的那一处正是 `pflow3b_tests`（整表删，最狠），
`WAREHOUSE_MANAGER` 上的 `p2c1_reviewer` 是 `voucher` 留下的 —— 与卡面给的库侧对照逐条对上。

### ② 成本批次 `CS-203109-0028` 残留 = ①的下游

`cost_period_approval_db_tests` 每条用例尾部都有 `purge(PERIOD) + purge_fixtures()`，
但**中途 panic 就跑不到**。第二轮它先红在 `role_tasks` 的污染断言上，
于是那张 `2031-09` 批次停在 `SUBMITTED`（实例 `PENDING`），挡住会计期间结账。

⇒ 修①即不再产生残留；为「用例中途 panic」这一形态再加一条模块收尾兜底（见下）。

## 3. 修法

### 3.1 新增：`fixture_hygiene` 的「借出 / 归还」台账（`#[cfg(test)]`，零生产代码）

| API | 作用 |
|:--|:--|
| `borrow_role_bindings(roles, keep)` | 把角色上**不属于自己**的绑定逐行登记进进程内台账，然后只删这些行（`keep` = 本模块夹具，不动）。`keep` 为空 = 全部借出（此时整段省掉 `NOT IN (...)`，避免 `NOT IN (NULL)` 恒 NULL 的坑） |
| `restore_role_bindings()` | 台账原样挂回（`SELECT … WHERE EXISTS(sys_user)` + `ON CONFLICT DO NOTHING`，幂等；账号已被对方删掉的跳过） |
| `release_fixture_role_bindings(roles, own)` | 收尾用：**自己的**夹具绑定摘掉并从台账划掉（自己的临时账号不该在活儿干完之后还占着岗位） |
| `restore_and_assert_role_bindings(stage)` | 归还 + 断言台账清零 + 按 `DEMO_USERS` 逐条核对演示账号岗位绑定 —— **本卡的护栏** |
| `missing_demo_role_bindings()` | 缺哪些 `"用户名→角色码"`（人读；空库/生产快照库没有演示账号时自然跳过，不假红） |

设计要点（写进注释，免得下一个人再踩）：

* **判据只加不减**：制造「角色上只有本模块夹具」是**合法的夹具需求**（会签语义下待办条数与
  「谁批完节点才推进」都靠它），越界的只是「**永久**摘掉别人的绑定」。所以不改成「不删」，
  改成「登记者删、收尾归还」。
* **自愈**：每个借用模块的 `setup()` 起手都先 `restore_role_bindings()` ——
  上一条用例 panic 没还的，这里补还，污染不跨用例累积。
* **归还不能替代「摘自己的」**：只归还别人，自己的夹具账号（`p3a_wh_mgr` / `p2c1_reviewer` /
  `m214.*` …）要在收尾摘掉，否则下一轮基线与种子库不同（岗位成员数从 1 变 5）——
  那正是这类卡「第一轮绿、第二轮红」的另一种形态。
* **`--test-threads=1` 前提**：台账是进程级的，串行跑时按模块收口；并行跑时「谁是最后一个」
  不可依赖（与本仓其它 `zz_` 收尾同口径）。

### 3.2 改动清单

| 文件 | 改动 |
|:--|:--|
| `business/erp/fixture_hygiene.rs` | **+270 行**：借出/归还台账 + 演示种子对账断言（`#[cfg(test)]`） |
| `business/erp/pflow3b_tests.rs` | 删掉整表删那句；`setup()` 起手自愈 + 借出 FM/FD；`pflow3b_zz_teardown_purges_fixtures` 加归还+断言 |
| `business/erp/purchase/purchase_handler.rs` | `setup_approval` 改借出（DEPT_LEADER/PM/GM）；`setup()` 起手自愈；`zz_teardown_purges_po_approval_residue` 加「摘自己的 + 归还 + 断言」 |
| `business/erp/stockin/stockin_handler_tests_helpers.rs` | `setup_cancel_reviewer` / `setup_post_approval` 改借出（WAREHOUSE_MANAGER） |
| `business/erp/stockin/stockin_handler_tests_fixtures.rs` | `setup()` 起手自愈；模块收尾用例加「摘自己的 + 归还 + 断言」 |
| `business/erp/stockin/stockin_handler_tests_zz_shared_roles.rs` | **新增**：收尾模块（模块名排在所有 `stockin_handler_tests_*` 之后） |
| `business/erp/stockin/stockin_handler.rs` | 挂上面这个新模块 |
| `business/erp/voucher/mod.rs` | `cancel_via_approval` 改借出；`setup()` 起手自愈；`zz_teardown_purges_finance_status_logs` 加「摘自己的 + 归还 + 断言」 |
| `business/erp/stock_policy/water_check_db_tests.rs` | 「岗位上没人」用例的整表删改走台账；`setup()` 自愈；`zz_teardown_purges_fixtures` 加归还+断言 |
| `business/erp/cost/cost_period_approval_db_tests.rs` | **新增** `zz_teardown_purges_fixtures`：按本模块建单人收全部期间批次，逐项断言「批次 / 计算单 / 实例 / 待办」残留 0 + 归还+断言 |

零迁移、零权限码、零菜单行、零生产代码（全部落在 `#[cfg(test)]` 下）。

### 3.3 为什么 stockin 的收尾要单独开一个「模块」

libtest 按**完整测试路径**排序，模块路径也参与排序：
`...::stockin_handler::tests::stockin_handler_tests_fixtures::zz_module_namespace_has_no_residue`
虽然以 `zz_` 开头，模块名 `..._fixtures` 却排在 `..._p16` / `..._posting` **之前**（`f` < `p`）——
它清理完，后面的收货用例又把 `p3a_wh_mgr` / `p16_reviewer` 绑回去，全量跑完库里永远留着这两个成员
（实测未修时就是 `WAREHOUSE_MANAGER | demo_wh,p16_reviewer,p3a_wh_mgr`）。
所以新增 `stockin_handler_tests_zz_shared_roles`（模块名 > 所有 `stockin_handler_tests_*`），
在模块最后一个跑，收口并断言。**仓库里 `t_9476d9f1` 记过「别依赖 `zz_` 排序」——这里记的是
它的补充：`zz_` 只在同模块内有效，跨模块要用模块名。**

## 4. 验收（全部真跑，原始输出见同目录）

| 项 | 命令 | 结果 | 证据 |
|:--|:--|:--|:--|
| 全新种子库 | `ERP_DB=cloudpivot_test_t18v3 bash scripts/verify/make_full_suite_db.sh` | rc=0，六个审批岗位都有在职成员 | `01_two_rounds_fresh_db.out` |
| **同一库连跑两轮全量** | `cargo test -p cloudpivot --lib -- --test-threads=1` ×2 | 两轮均 `6564 passed; 0 failed` | `01/02/03` |
| 跑完后库形态 | 七岗位成员 SQL | 与种子基线**逐字一致**（`WAREHOUSE_MANAGER | demo_wh`，无夹具残留） | `01_…` 每轮尾部 |
| 成本残留 | `select count(*) from biz_cost_period / biz_cost_period_item / biz_flow_instance(COST_SETTLE)` | 两轮后都是 **0 / 0 / 0** | `01_…` |
| 护栏自证（6 个收尾用例打印） | `cargo test … <zz 用例> -- --nocapture --exact` | `[fixture_hygiene] …：归还共享角色绑定 N 行，演示种子岗位绑定完好 ✓` | `06_teardown_nocapture.out` |
| **反证①（库侧污染，跑的是真二进制）** | 手工把 `FINANCE_MANAGER` 的绑定整表删掉（= 老写法的效果），再跑 `cost::…::zz_teardown_purges_fixtures` | **RED**，点名 `["demo_finance→FINANCE_MANAGER", "demo_finance_mgr→FINANCE_MANAGER"]`；跑完已把绑定补回并复核 | `07_teeth_polluted_db_red.out`（脚本同目录 `.sh`） |
| **反证②（代码层变异）** | 给 `borrow_role_bindings` 加一次性开关让「只删不登记」（= 历史整表删），跑 `pflow3b_zz_teardown_purges_fixtures` | **RED**，同样点名两条 `demo_finance*→FINANCE_MANAGER`；同刻库侧 `FINANCE_MANAGER | (空)`。**变异已用 `cp` 备份还原并 `sha256sum` 逐字节校验**，两轮全量是在还原后的终版代码上跑的 | 见 §5 说明 |
| fmt | `cargo fmt --check` | rc=0 | `05_fmt_clippy.out` |
| clippy | `cargo clippy -p cloudpivot --lib --tests -- -D warnings` | rc=0（零告警） | `05_fmt_clippy.out` |

反证②做的代码变异已用 `cp` 备份还原 `fixture_hygiene.rs` 并 `sha256sum` 逐字节校验
（`b7afc0f8ed5c1463e02de45429189d07a9bbf71c709f7ec9b04a8d51f3222eef`）；
两轮全量是在**还原后的终版代码**上跑的（`01_two_rounds_fresh_db.out`）。

## 5. 边界与取舍（只报不藏）

* **没改任何期望值**：`cost_period_approval_db_tests` 的「岗位上只能有夹具 + 演示种子成员」断言
  一个字没动，红的那 15 条是靠修夹具的 teardown 变绿的（卡面明确要求）。
* **`FINANCE_DIRECTOR` 仍是 0 人**：`DEMO_USERS` 里没有财务总监的演示账号，种子库本来就是这样；
  断言按 `DEMO_USERS` 逐条对账，不会把它判红。
* **`--test-threads=1` 前提**：借出台账是进程级、按模块收口；并行跑（`-j4` 多线程）时收口时机不可依赖，
  与本仓其它 `zz_` 收尾同口径风险。`scripts/check.sh` / 全量套件都是串行档。
* **本次只修 lib 套件里的越界点**：`backend/app/tests/`（集成测试二进制 `pflow1a.rs` 等）
  里还有同形的 `DELETE … WHERE role_id = ?`，它们不在 `--lib` 套件里、也不碰这五个岗位，
  未纳入本卡（留给对应卡按同一台账改）。

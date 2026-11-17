# 卡 t_0fd37353 · 修复 o13 存量红：`POST /report/cash-flow/worksheet/check` 用读权限码

> 日期：2026-11-17 ｜ 仓库：`/opt/cloudpivot`（99 工作机，PG 在 145）｜ 提交：见卡面 metadata
> 结论：**选方案 2（在 O13 的「POST 但语义是查询」豁免里显式登记）**，并把豁免判据从
> 「路径含某个词」收紧为「路径段名相等 + 精确白名单」，同时新增 4 条失效条目判据 + 1 条纯逻辑用例。

## 1. 复现（修前）

```text
$ cd backend && cargo test -p cloudpivot --lib system::authz_tests
test system::authz_tests::o13_every_write_route_requires_permission ... FAILED
test result: FAILED. 14 passed; 1 failed; 0 ignored; 6100 filtered out

---- stdout ----
写接口只要求了读权限（有权查看即可改数据）：
POST /business/erp/report/cash-flow/worksheet/check → business:erp:cashflow-worksheet:list
```

与卡面一致；`git status` 对该文件为空（不是本卡引入）。

## 2. 为什么选方案 2（而不是补一个 `:check` 权限码）

| 判据 | 方案 1（新码 `business:erp:cashflow-worksheet:check`） | 方案 2（显式登记豁免） |
|:--|:--|:--|
| 语义正确性 | 接口**一行都不写库**（有真库用例证明，见 §3），「谁能跑勾稽」与「谁能看底稿」在本接口上没有可区分的行为差异 —— 能看底稿的人本来就能在前端自己算同样的数 | 同 |
| 授权副作用 | 新码要挂菜单行才授得出去（否则 `perm_menu_gate` 判 missing_in_menu、`permission_audit_tests` 判 N20 零授予）。而**菜单总数实测 540，正顶 `menu_invariants` ⑤ 的上界 540** ⇒ 一行新增立刻判红，得再去动闸门常量（闸门自己写着「不是功能增长闸门」）。且不给 FINANCE_MANAGER 补授就该角色点校验 403（前端已在用该接口） | 零迁移、零菜单行、零授权变化，风险面最小 |
| 闸门强度 | 不变 | **变强**：顺带把 `contains("/check")`→`段名相等`+`精确白名单`，`…/checkout`、`…/export-job` 这类真写接口不再被顺手放行；并新增失效条目判据（过期/码已改/依据为空/段名规则已覆盖 → 判红） |
| 代价 | 1 条迁移 + 1 个码 + N 条授权 + 闸门常量调整 | 一个常量条目 + 4 条失效判据 + 1 条纯逻辑用例 |

菜单基数的实测依据（145 的 `cloudpivot_dev`，2026-11-17）：

```sql
select count(*) from sys_menu where deleted_at is null;  -- 540
```

`scripts/verify/menu_invariants.sh` 的第 ⑤ 条断言区间是 `[400, 540]`（变量 `MENU_TOTAL_MAX`）。

## 3. 「不落库」的依据（不是「看着不像写」）

`backend/app/src/business/erp/cashflow_worksheet.rs::check_sheet` 只有两条 SQL：
`load_config_items(db())`（读项目配置）、`load_rows(db(), &period)`（读已存底稿），
再加纯计算 `prepare_flow_writes` / `reconcile`，返回 `CheckRes`；没有事务、没有 INSERT/UPDATE/DELETE。

真库用例（同一文件 `tests::draft_check_writes_nothing`）断言：

```rust
let check = check_sheet(&req).await.expect("草稿闭合");
assert!(check.balanced, "{}", check.message);
let stored = load_rows(db(), OPEN_PERIOD).await.expect("回读底稿行");
assert!(stored.is_empty(), "校验接口一个字节都不许写库，实际落了 {} 行", stored.len());
```

授权侧的口径沿用 `V0415__cashflow_worksheet_perm.up.sql` 的「为什么校验接口复用读码」一节。

## 4. 改了什么（3 个文件）

1. `backend/app/src/system/authz_tests.rs`
   - 新增常量 `QUERY_BY_POST_EXACT: &[(&str, &str, &str)]`：精确路径白名单
     （`POST /business/erp/report/cash-flow/worksheet/check`，第三条元素写清不落库依据 + 证明用例名）；
   - 新增 `is_read_query_path()`：`preview/export/search/statistics` 判**段名相等**（原来是 `path.contains("/preview")`）；
   - O13 主体：豁免 = 段名规则 ∪ 精确白名单；新增「失效条目」断言（扫不到该写路由 / 权限码已不是读码 /
     段名规则已覆盖属多余 / 依据为空 → 红）；
   - 新增纯逻辑用例 `o13_read_query_path_matches_whole_segment_only()`：
     6 条真读路径必须命中、6 条「长得像」的（`…/checkout`、`…/export-job`、`…/previews`…）必须不命中。
2. `backend/app/src/business/erp/cashflow_worksheet.rs`：只在接口文档注释里补「为什么复用读码 + 在 O13 里登记过」，
   **零逻辑改动**。
3. `docs/test-report-system.md`：BUG-SYS-07 补记现状（全库 3 条读码 POST 接口 + 判据收紧）。

## 5. 验证（真实执行输出）

```text
$ cd backend && cargo test -p cloudpivot --lib system::authz_tests
test result: ok. 16 passed; 0 failed; 0 ignored; 0 measured; 6100 filtered out

$ cargo test -p cloudpivot --lib system::authz_tests::o13 -- --nocapture
[O13] 例外（POST 但语义是查询，要求读权限）：[
  "POST /business/erp/report-item/preview → business:erp:report-item:list",
  "POST /business/flow/linkage/preview → business:flow:linkage:list",
  "POST /business/erp/report/cash-flow/worksheet/check → business:erp:cashflow-worksheet:list",
  "POST /business/notice/template/{id}/preview → business:notice:template:list"]
[O13] 431 个写接口全部有独立写权限码，无公开写接口 ✓
```

全量 lib 套件、fmt、clippy 的输出见 §7 与 `docs/evidence/2026-11-17-o13-cashflow-check/*.txt`。

**反向证明（闸门没被放水）**：把白名单条目的路径改成不存在的 `…/checkMUTATION` 再跑，
o13 立刻回到原来的红，且报错文案直接给出两条出路（`12_mutation_proof_no_exemption_fails.txt`）：

```text
thread 'system::authz_tests::o13_every_write_route_requires_permission' panicked at authz_tests.rs:859:
写接口只要求了读权限（有权查看即可改数据）：
POST /business/erp/report/cash-flow/worksheet/check → business:erp:cashflow-worksheet:list
（若确实「只回显不落库」，要么把路径段名改成 preview/export/search/statistics，要么在 QUERY_BY_POST_EXACT 里登记并写清不落库的依据）
```

（改回原路径后 `cargo test -p cloudpivot --lib system::authz_tests` 又回到 `16 passed; 0 failed`。）

## 6. 【上游缺陷，只报不改】V0442 让**空库首启直接失败**

造全量库时（`scripts/verify/make_full_suite_db.sh`）实测：空库上服务起不来。

```text
$ ERP_DB=cloudpivot_test_o13 bash scripts/verify/make_full_suite_db.sh
==> 已建空库 cloudpivot_test_o13（owner=cloudpivot）
==> 实例已停（日志尾部）
    ERROR nik_web::app: 数据库迁移失败: query failed: : db error
    FATAL: 服务启动失败: 配置错误: 数据库迁移失败: query failed: : db error
!! bootstrap 没跑完（库里没有 admin+SUPER_ADMIN）

# 手工 psql 应用同一文件，能看到被内核吞掉的真正原因：
ERROR:  V0442: 祖先目录「系统管理」(1000…0001) 不存在，先应用系统菜单迁移
CONTEXT:  PL/pgSQL function inline_code_block line 62 at RAISE
```

根因（实测数据）：`1000…0001`「系统管理」这一行来自**种子迁移** `migrations/seed/S__menu_data.sql`，
而内核的执行顺序是**版本化迁移先跑、种子后跑**。空库上那一刻 `schema_migrations` 里
`kind='versioned'` 已 361 条、`kind='seed'` 0 条 ⇒ V0442 的前置护栏必然命中。

同一个 V0442 在**已有种子的库**上完全正常（本次实测）：

```text
# cloudpivot_test_o13b：先用「摘掉 V0442」的迁移目录副本起实例（bootstrap + 种子 + 演示组织），
# 再手工 psql 应用 V0442：
DO
UPDATE 1
INSERT 0 1
INSERT 0 0
INSERT 0 14
INSERT 0 1
NOTICE:  V0442 自检通过：notice:mine 按钮归位「消息中心」+ 账龄/互斥两对孤儿补授父页码 + N20 六码已授（目标角色 5 个）
```

影响与约束（供后续卡决策）：

* 任何「空库首启 / 新建环境 / 演练库」都会在 V0442 处失败 ⇒ `make_full_suite_db.sh`、L3 干净检出闸、
  新客户部署全部被挡。
* **不能靠新迁移补救**（V0443 永远轮不到执行），只能改 V0442 本身（把「系统管理」相关补偿段改成
  「该行不存在就跳过」或把自带依赖换成版本化迁移种下的节点）。
* 改 V0442 的代价：`cloudpivot_dev` **已经应用过**它（`checksum=2e6fbee6…`，2026-11-17 16:30 由别的
  worker 起服务时应用）⇒ 原地改文件后该库下次启动会撞指纹校验，需要同步
  `UPDATE schema_migrations SET checksum=… WHERE version='442'`（`cloudpivot_test` / `cloudpivot_test_full`
  / 144 生产均停在 441，未受影响：144 实测 `git log -1 = 56368b1f`、最新迁移 `441`）。

本卡**不改**它：属于上游卡 t_fa41e0e1 的 V0442，且改法涉及「已应用迁移能否原地改 + 指纹怎么修」的决策，
已另立卡（见卡面 comment）。全量 lib 套件用「库已手工到 442」的方式跑，判据不受影响。

## 7. 全量 lib 套件（本树 + 已到 442 的种子库）

库：`cloudpivot_test_o13b`（`make_full_suite_db.sh` 口径的种子库 + 手工应用 V0442，见 §6；
零历史单据、540 菜单、12 条 seed）。
命令：`cd backend/app && cargo test -p cloudpivot --lib -- --test-threads=1 --nocapture`
（四条 URL 指向同一库：`ERP_DB_URL` / `SYS_DB_URL` / `TASK_DB_URL` / `P0FIX_DB_URL`）

```text
test result: FAILED. 6111 passed; 4 failed; 1 ignored; 0 measured; 0 filtered out; finished in 204.70s
```

* **o13 已绿**（`system::authz_tests` 16/16，修前 14 passed + 1 failed）。
* 真 SKIP 行 **0**（日志里唯一一处 "SKIP" 是 `impexp` 用例的断言文案，不是跳过）。
* 1 条 `ignored` 是 `test_seed::tests::bench_fixture_data_preparation`（耗时基准，本身标注 `#[ignore]`）。
* 4 条红**全部与本卡无关**，见 §8。

## 8. 【存量红，只报不改】真库全量套件的 4 条「跨套件残留」红

四条都不是本卡引入（本卡只动 `system/authz_tests.rs` 与两处文档注释）：

| 用例 | 现象 | 判定 |
|:--|:--|:--|
| `business::erp::fx_ledger::tests::fx_ledger_reports_rows_accounts_and_running_balance` | `assert_eq!(report.accounts.len(), 2)` 实得 3 | **残留驱动**：同一轮里先跑的 `fx_balance` 套件留下了 `M105FX01 / M107FX01 / M107FX02` 等外币科目（实测库里 `account_code LIKE 'M1%FX%'` 6 行），而该报表列的是「全部外币科目」 |
| `…fx_ledger_running_balance_stays_continuous_across_pages` | `全区间 4 行（期初 2 + 本期 2）` 实得 6 | 同上 |
| `business::erp::inventory_aging::slow_moving::…::threshold_from_config_and_query_override` | `配置值生效 left 90 / right 30` | 残留驱动：`sys_config.erp.inventory.slow-moving-days` 被别的用例读/写后未被本用例拿到（该模块自己的 `zz_teardown` 随后把参数恢复到 V0426 种子 90，说明夹具本身认为「值就该是种子值」） |
| `business::erp::ledger::tests::p2d1_ledger_excludes_draft` | `DRAFT 只应 1 张` 实得 2 | 断言写的是**全表口径**（`voucher_page(status=DRAFT)` 的 `total`），任何别的套件留下草稿就打红；本次是 `fx_ledger` 场景的 `M108-D1` 草稿 |

反证（在**全新种子库** `cloudpivot_test_o13c` 上只跑这 4 条，见 `04_isolate_4_reds.txt`）：

```text
fx_ledger_reports_rows_accounts_and_running_balance ... ok
fx_ledger_running_balance_stays_continuous_across_pages ... ok
slow_moving_db_tests::threshold_from_config_and_query_override ... ok
ledger::tests::p2d1_ledger_excludes_draft ... FAILED   ← 全表口径断言，仍被同进程里 fx_ledger 造的草稿打红
test result: FAILED. 7 passed; 1 failed
```

⇒ 前 3 条是「隔离跑就绿」的跨套件残留；第 4 条是「断言口径本身不稳」（不该数全库草稿数），
两者都是**存量缺陷**，需要单独立卡收口（夹具清理 / 断言改成按自己的夹具过滤），不属于本卡修法范围。
本卡**不动**它们：改别模块的真库夹具既超出卡片范围，也会把共享工作区里别人的在途改动卷进来。


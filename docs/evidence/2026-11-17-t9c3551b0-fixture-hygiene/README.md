# t_9c3551b0 —— 真库全量跨套件夹具残留 / 全表口径断言（4 条存量红 + 第二轮 3 族新红）

卡：`t_9c3551b0`（测试卫生）。负责人：test-enginner。日期：2026-11-17。
库：`cloudpivot_test_t9c35`（`scripts/verify/make_full_suite_db.sh` 口径，145 上，空库首启 376 条迁移 / V444 / 菜单 540）。
跑法：`cd backend/app && cargo test -p cloudpivot --lib -- --test-threads=1`（真库、串行、**零 `--skip` / 零 `--ignored` 新增**）。

## 1. 结论（判据 = 同一库连跑两轮都 0 failed）

| 轮次 | 结果 | 证据 |
|:--|:--|:--|
| 卡面基线（他人跑） | `6111 passed; 4 failed; 1 ignored` | `01_baseline_full.txt`（另有一份含 `cash_flow::r3` 的 5 红基线，见卡面 §现象） |
| 本轮 第 1 轮（修完 4 红后） | `6130 passed; 0 failed; 1 ignored` | `03_round1_full.txt` |
| 本轮 第 2 轮（同一库，**未重造**） | `6120 passed; **10 failed**` ← 暴露 3 族新残留 | `04_round2_full.txt` |
| 收口后 第 1 轮（全新种子库） | **`6132 passed; 0 failed; 1 ignored`** | `06_final_round1.txt` |
| 收口后 第 2 轮（同一库，**未重造**） | **`6132 passed; 0 failed; 1 ignored`** | `07_final_round2.txt` |

- 唯一 `ignored` 是 `test_seed::tests::bench_fixture_data_preparation`（P2-62 耗时基准，只在取证时手动跑），与基线同一条，非本轮 `--ignored` 新增。
- 4 条卡面红在最终第二轮逐条绿：`fx_ledger_reports_rows_accounts_and_running_balance`、`fx_ledger_running_balance_stays_continuous_across_pages`、
  `slow_moving_db_tests::threshold_from_config_and_query_override`、`ledger::tests::p2d1_ledger_excludes_draft`。

## 2. 卡面 4 条红的根因与修法（判据只收紧，不放宽）

| # | 用例 | 根因 | 修法 |
|:--:|:--|:--|:--|
| 1/2 | `fx_ledger::tests::*`（科目数 3≠2、全区间 6 行≠4） | `fx_balance` / `period_fx_reval` / `fx_ledger` 三个套件只在**造数前**清场，模块最后一条用例跑完就把 `M105FX01/M107FX01/M108FX01…` 留给下一轮；而 `fx_ledger` 报表列的是「该币种**全部**外币科目」，不是自己的两个 | 每个套件新增 `zz_teardown_purges_fixtures`（`zz_` 排模块最后）按**自己的 `account_code` 前缀**收尾删 + 断言残留 0；断言改成「外币科目里没有别人的、自己的两个都在」，写死 `len()==2` 换成**点名夹具**（干净库上等价，脏库上能指出是谁漏了） |
| 3 | `slow_moving_db_tests::threshold_from_config_and_query_override`（left 90 / right 30） | `SysConfigModel::read_text` 优先命中**进程内快照**；同一进程里跑过 `fx_settings`/`opening` 之类写过参数的用例后，直改 `sys_config` 不再回源 ⇒ 「库里有 30，报表读回 90」 | `support/config/model.rs` 新增 `#[cfg(test)] clear_runtime_snapshot_for_tests()`（生产无符号）；阈值用例的三个写入口（write/disable/restore）写后显式作废快照，并**先断言写入落库（直查 `sys_config`）再断言取值生效**，失败时能分辨「没写进去」还是「读了旧快照」 |
| 4 | `ledger::tests::p2d1_ledger_excludes_draft`（DRAFT 2≠1） | `voucher_page(status=DRAFT).total` 是**全表口径**，任何别的套件留下草稿（本例 `fx_ledger` 的 `M108-D1`）就打红 | 断言收窄到**本夹具的凭证集合**（`P2D1-` 前缀里恰好 1 张 DRAFT 且 `== ["P2D1-0004"]`），并保留全表 `total` 打印 |

## 3. 第二轮才暴露的 3 族残留（同属「残留不留到下一轮」，一并收口）

| 族 | 症状 | 根因 | 修法 |
|:--|:--|:--|:--|
| A `inventory_transit::transit_query_tests`（8 条） | 第二轮 `left: 2 / right: 1`、`left: 4 / right: 3`… | 「默认筛选」按契约是**全表口径**（`page_of(filter())` / `summary_of(default)`），而 `receivable_bridge`(`SO14-`) / `shipment_receipt_tests`(`SO10R-`) 造完发货单不收回在途行 | 造数前 `purge_foreign_transit_rows()` 清掉**非本模块发货单**的在途行并**断言清干**（在途账是派生投影，可按发货单重算；判据仍是全表，不缩成「只查我的」） |
| B `customer_handler::tests::page_filters_by_code_name_and_status` | 第二轮 `left: 2 / right: 1` | 名称模糊筛用**通用词**「客户乙」做全表计数，`inventory_transit` 的夹具「在途查询客户乙」也命中 | 计数锚定唯一词「分页测试客户乙」（仍走 LIKE），再用通用词验一次「命中包含」（`any`，不计数） |
| C `stockin::stockin_handler_tests_p16::p16_graph_snapshot_selfcheck_is_clean` | 第二轮 `drifted=1 … flow_code: "hr_leave"` | `hr/leave_bridge_db_tests`、`hr/leave_handler_db_tests` 各自**手写**一条 `hr_leave` 流程定义 + 版本（快照手写、`graph_hash = repeat('0',64)`、**无 L1 节点行**），收尾只按单号/员工清场，流程定义留到下一轮；p16 是**全库**三方对账（行 ↔ 快照 ↔ hash） | 两个模块各加 `zz_teardown_purges_fixtures`：按 `flow_code='hr_leave'` + 版本 `change_note`（本文件标记）/「零版本空壳」认领，删「实例→待办→节点→版本→定义」整条链，并断言 `hr_leave` 上**零 hash 的已发布版本 = 0**（就是 p16 红的那条判据，早暴露） |

### 踩坑（写代码时踩到的，记下来防复发）

- **不能每一步都用「JOIN 版本」的子查询删定义**：第 8 步删完版本，第 9 步的子查询就再也 JOIN 不出定义，定义会**静默留下**（实测留了一条 `请假审批（HR25 夹具）` 的空壳定义、0 个版本）。正确做法：先把定义 id 抓成列表，再按 id 逐层删。
- `biz_flow_def_version.graph_hash = repeat('0',64)` 的夹具版本只要留到下一轮，p16 必红（行是空图、快照手写、hash 全 0，三方必然对不上）。**测试夹具不许伪造「已发布」版本**，除非同时写 L1 节点行；本项目已有的正确做法是 `version_status='DRAFT'`（`quotation_state_tests` 等）让全库自检天然跳过。

## 4. 收尾后库状态（可复核）

第二轮结束后 `cloudpivot_test_t9c35` 上：

```text
hr_leave_defs = 0        -- 夹具流程已清（含空壳定义）
zero_hash_pub = 0        -- PUBLISHED/ARCHIVED 里没有零 hash 版本（p16 的判据）
acct_M1*      = 0        -- M105*/M107*/M108* 外币科目 0 行
cf_item_m114  = 0        -- 底稿临时分类配置 0 行
1913-01/02 期间 = 0
transit_foreign = 2      -- ⚠ 稳定值：receivable_bridge / shipment_receipt 留下的在途行，
                          --   由 inventory_transit 的造数前清场吃掉；两轮都是 2，不再增长
```

## 5. 遗留（不在本卡范围，已开后续卡）

1. **`hr_leave` 的正式种子会被测试夹具顶掉**：`leave_bridge_db_tests::seed_leave_flow` / `leave_handler_db_tests::seed_leave_flow`
   为满足 `uk_biz_flow_def_code`（flow_code 全局唯一）先删掉「没被实例引用的 hr_leave 定义」——**V0393 的正式种子也在其中**。
   本轮收尾后该流程定义在库里为 0 行（夹具自己那条已被删干净），即「跑过全量套件的库不再有 hr_leave 正式流程」。
   本卡只保证**不留零 hash 脏版本**（p16 绿）；「种子被顶掉」需要夹具改为 **备份/恢复种子行**（`to_jsonb` → `jsonb_populate_recordset`）
   或让种子可重建，属 HR 模块的夹具设计，另开卡。
2. **`receivable_bridge` / `shipment_receipt_tests` 不收回在途行 + 发货单**（`SO14-*` / `SO10R-*`）：本卡用「造数前清外来在途行」
   兜住 `inventory_transit` 的全表口径，但源头（那两个套件漏清自己的投影）应各自补收尾清场。

## 6. 口径与门禁

- 命令：`bash scripts/verify/make_full_suite_db.sh`（`ERP_DB=cloudpivot_test_t9c35`）→ 四条 URL 指同一库 → `cargo test -p cloudpivot --lib -- --test-threads=1`。
- 门禁：`cargo fmt --check -p cloudpivot` 通过（只对本卡改的 11 个 `.rs` 跑 `rustfmt --edition 2024`，未格式化他人未提交文件）；
  `cargo clippy -p cloudpivot --lib --tests -- -D warnings` 通过（1m46s，零告警）。
- 本卡零迁移、零权限码、零菜单行，不触碰任何生产代码路径（`clear_runtime_snapshot_for_tests` 是 `#[cfg(test)]`）。

## 7. 证据文件

| 文件 | 内容 |
|:--|:--|
| `01_baseline_full.txt` | 卡面基线（5 红版：含 `cash_flow::r3_real_config_covers_all_accounts`），611650 B 原始输出 |
| `02_round1_after_fix.txt` | 上一轮 attempt 崩溃前的编译失败（`sales_target_handler` 缺列，被同僚在途改动打断）—— 说明为什么换隔离树 |
| `03_round1_full.txt` | 修完卡面 4 红后的第 1 轮：6130 passed / 0 failed |
| `04_round2_full.txt` | **同一库**第 2 轮：10 failed（3 族新残留的完整 panic，见 §3） |
| `05_round1_fresh_full.txt` | 重建种子库后的第 1 轮：6132 passed / 0 failed |
| `06_final_round1.txt` | 收口后（含 §3 三族修复）全新种子库第 1 轮：6132 passed / 0 failed |
| `07_final_round2.txt` | 收口后同一库第 2 轮：**6132 passed / 0 failed** |

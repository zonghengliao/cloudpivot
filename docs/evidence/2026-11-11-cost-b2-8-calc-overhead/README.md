# COST-B2-8【成本】calc 第 3 段：费归集 —— 证据

- 卡：`t_89836f59`（COST-B2-8），批次 B2，前置 B2-4（手工费用台账，`t_883e4819`）/ B2-7（段 2 工归集，`t_c485dcf1`）
- 设计依据：`docs/architecture/ARCH-9-成本核算升级设计.md`
  - §4.3 归集时点与来源（费三路：折旧 / 报销 / 手工，幂等键 `(source_type, source_id, 0)`）
  - §3.1 ③ `biz_cost_collect`（DDL 与 5 条 CHECK）/ ⑤ `biz_cost_overhead` / ④ `biz_depreciation` / `biz_expense_item`
  - §4.4.3 ① 池口径：`Σ collect.amount WHERE element='OVERHEAD' AND cost_object_id IS NULL`
  - §4.5.1 数据源矩阵（折旧科目配 5602 → 「不进成本」，不是错误）
  - §8.1 B2-8 行：`折旧/报销/手工 → 池行 + 直接行`
- 范围：**只有**「三路费用 → 幂等写 collect + 按 (车间, 科目) 汇总成池」。不做分摊（B2-9/B2-10）、
  不做约当与计算单（B2-11）、不做未归集清单接口（B2-12）、不改任何已应用迁移。

## 1. 改了什么

| 文件 | 类型 | 内容 |
|---|---|---|
| `backend/src/business/erp/cost_overhead_collect_dto.rs` | 新增（纯逻辑，无库） | `OverheadLine`（行校验 / 是否直接归集 / 备注）、`plan_of`（增量计划：写谁 / 跳谁 / 清谁 / 谁 0 元未归集）、`ExistingOverheadRow`（费段专用已归集行，`cost_object_id: Option`）、`pool_of`（按 (车间, 科目) 汇总）、`OverheadCollectOutcome`、`UncollectedOverhead`、`66064` 错误构造；**18 条单测** |
| `backend/src/business/erp/cost_overhead_collect_model.rs` | 新增（数据层） | `DEPRECIATION_SOURCE_SQL` / `EXPENSE_SOURCE_SQL` / `OVERHEAD_SOURCE_SQL`（取数口径逐条落 WHERE）、`INSERT_OVERHEAD_SQL`（幂等写，池行可空对象）、`POOL_SQL`（按 (车间,科目) 汇总，科目从三路来源反查）；`depreciation_source_lines` / `expense_source_lines` / `overhead_source_lines` / `all_source_lines` / `overhead_collect_rows_of_period` / `collected_overhead_total` / `pool_of_period` / `insert_overhead_line(s)`；**10 条 SQL 形状单测** |
| `backend/src/business/erp/cost_calc_overhead.rs` | 新增（编排） | `collect_overhead(period, operator)`（自开事务并提交）+ `collect_overhead_in_tx(ex, …)`（供 calc 编排卡复用）；扫→校验→清过期→写新→池汇总→回查；`pool_total`；**4 条单测** |
| `backend/src/business/erp/cost_calc_overhead_db_tests.rs` | 新增（真库用例，`ERP_DB_URL` 开关） | **8 条**：三路归集 + 成池 / 非 POSTED·非 4101 全不写 / 直接行不进池 / 重复跑幂等 / 来源作废后清旧留痕 / 0 元进未归集清单 / 池与 B2-5 试算互校 / 空期间无操作 |
| `backend/src/business/erp/mod.rs` | 改动（登记） | `pub mod cost_calc_overhead;` + `#[cfg(test)] mod cost_calc_overhead_db_tests;` + `pub mod cost_overhead_collect_{dto,model};`（4 行 + 4 行注释） |

**无迁移**：`biz_cost_collect`（V0371）/ `biz_depreciation`（V0230）/ `biz_expense_*`（V0242）/
`biz_cost_overhead`（V0371）全部已应用，本卡一字未改 ⇒ 卡面「迁移幂等 + `.down.sql`」N/A。
**无 HTTP 入口**：设计 §5.2 只有 `POST /cost-period/calc`（#27，属 calc 编排卡），段卡不单独暴露接口
（单独暴露会让期间停在半算状态）；无前端改动 ⇒ `pnpm typecheck/build` N/A。

## 2. 四条口径（本卡的全部规则）

1. **折旧取固定资产模块当月折旧**：`biz_depreciation`，`status='POSTED'` + `period_code=?` +
   `expense_account_code LIKE '4101.%'`。`REVERSED`（已红冲）不算；科目配成 5602.* 的资产
   **不进成本**（§4.5.1 的零代码开关，不是数据错误，故不报错也不进未归集清单）。
   金额取 `dp.amount`（计提写入、已生成凭证的账上值），`occur_date` 取批次 `run_at` 日期
   （折旧明细表没有日期列，这是该行唯一的真实发生时间；批次缺失时回落期间首日）。
2. **报销取已过账的费用报销（科目 4101.*）**：`biz_expense_item ⋈ biz_expense_report`，
   主单 `status IN ('APPROVED','PAID')` + `period_code=?` + `deleted_at IS NULL` + 明细
   `account_code LIKE '4101.%'`。金额取**明细不含税 `amount`** —— 与 `expense_bridge` 生成凭证时
   借记费用科目的金额同源（税金单独走「应交税费」行，不进 4101）。
   车间取**报销单**的 `dept_id`（凭证口径的部门快照），不 join 报销人当前部门。
3. **手工台账直接进**：`biz_cost_overhead`，`period_code=?` + `deleted_at IS NULL` + `4101.*`；
   **挂了成本对象 → 直接行**（`cost_object_id` 非空），**没挂 → 池行**。判据只有一个
   （`OverheadLine::is_direct()`），不接受外部布尔 —— 池行与直接行由同一列决定，不会两处漂移。
4. **按 (车间, 科目) 汇总成池**：`pool_of_period` 把**池行**（`cost_object_id IS NULL`）按
   `(dept_id, account_code)` 汇总，科目按 `source_type` 反查三张来源表（`biz_cost_collect` 没有
   `account_code` 列，这是 V0371 DDL 的结构事实）；`ALLOC` 行**不进池**（它是池的去向不是来源，
   进来会自我循环）。直接行**不进池**（否则既直接计入又参与分摊，重复计成本）。

## 3. 幂等（与料 / 工段同构，两种语义都实现）

| 语义 | 做法 |
|---|---|
| 不重复写 | 幂等键 `(source_type, source_id, 0)` 撞 `uk_biz_cost_collect_src`（`WHERE deleted_at IS NULL AND source_id IS NOT NULL`）⇒ `ON CONFLICT DO NOTHING`，返回行数才是真实新增 |
| 结果收敛 | 已归集行与来源比对 `(cost_object_id, amount)`：单据作废（折旧红冲 / 报销作废 / 台账软删）、改挂对象、金额被修正 ⇒ 旧行软删 + 重写 |

★ **顺序不能反**：部分唯一索引下必须先**软删旧行再 INSERT**（反之 `ON CONFLICT DO NOTHING`
会静默吞掉新行，随后再删旧行会让这一键一行不剩）。

★ **已归集行只取本段三种 `source_type`**：`ALLOC` 行同样是 `element='OVERHEAD'`，
若一并取回，本段的「过期判定」会把编排卡第 ⑥ 步刚写的分摊行全删掉（真库用例 + SQL 单测各钉一条）。

★ **0 元行不写 collect**：`amount = 0` 的费来源（例如明细不含税 0、只有税金的报销行）
进「未归集清单」（带 `reason` + `hint` 怎么修），不写 0 元池行 —— 否则「来源单据笔数」与
「池行笔数」永远对不上，且 0 元池行对分摊毫无意义（与工段「缺费率不写 0」同一处理，D-7 不猜）。

## 4. 实测输出

### 4.1 真库用例（`ERP_DB_URL=…/costb26_drill_1625844`，见 `02_real_db_tests.out`）

```
running 12 tests
cost_calc_overhead::tests::bad_period_is_rejected_before_any_query ... ok
cost_calc_overhead::tests::entry_point_rejects_bad_period_without_touching_db ... ok
cost_calc_overhead::tests::pool_total_sums_rows ... ok
cost_calc_overhead::tests::segment_only_writes_overhead_from_the_three_sources ... ok
... collects_the_three_sources_and_builds_the_pool ... ok     # 三路各一行，1700.0000，池三个科目各一行
... ignores_non_posted_and_non_4101_sources ... ok            # REVERSED / DRAFT / CANCELLED / 上期 / 5602 / 软删 全不写
... hand_ledger_with_cost_object_is_collected_directly ... ok # 直接行 600 不进池，池只有 150
... rerun_is_idempotent_and_leaves_rows_untouched ... ok      # inserted=0 / skipped=3，id + updated_at 不变
... purges_rows_whose_source_no_longer_qualifies ... ok       # 三路作废 ⇒ purged=3，软删留痕，第三次跑 noop
... zero_amount_rows_go_to_the_uncollected_list ... ok        # 0 元报销行不写 collect，带修复建议
... pool_matches_b2_5_preview_and_scanned_lines ... ok        # 本段池 = B2-5 试算池 = 扫描行纯函数汇总
... empty_period_is_a_clean_noop ... ok                       # 空期间一行不写；非法期间 400
test result: ok. 12 passed; 0 failed; ... finished in 10.76s
```

跑完演练库零残留（`biz_cost_collect` / `biz_depreciation` / `biz_depreciation_batch` /
`biz_expense_report` / `biz_expense_item` / `biz_cost_overhead` / `biz_fixed_asset` /
`biz_cost_object` 计数全部为 0，见 `04_residue.out`）。

### 4.2 纯逻辑单测

- `cargo test --lib cost_overhead_collect` ⇒ **26 passed**（dto 18 + model 10，`01_unit_tests.out`）
- `cargo test --lib cost_calc_overhead` ⇒ **4 passed**（无 `ERP_DB_URL` 时真库 8 条 SKIP）

### 4.3 门禁

| 命令 | 结果 |
|---|---|
| `cargo fmt -- --check` | 干净（`FMT_EXIT=0`，`03_gates.out`） |
| `cargo clippy --all-targets -- -D warnings` | 0 warning / 0 error（`03_gates.out`） |
| `cargo test`（全仓） | **33 targets / 4819 passed / 0 failed**（`01_full_cargo_test.out`，EXIT=0） |

## 5. 已知偏差 / 未做

1. **`source_no` 的取值按来源分**：报销 = 报销单号（`report_no`），折旧 = 资产编号（`asset_no`，
   折旧明细没有单号列），手工台账 = **NULL**（设计 §3.1 ⑤ 台账无单号列，不编造单号；人话上下文
   —— 期间 / 费用项目 / 科目 / 车间 —— 写 `remark`）。设计 §4.5.2 硬规则 3「每条归集行都留 source_no」
   对手工台账行是**结构性**做不到，在此具名报告。
2. **`occur_date` 的取值按来源分**：报销 = 明细费用日期；折旧 = 批次计提日（`run_at::date`）；
   手工台账 = 录入日（`created_at::date`，台账表没有业务日期列）。期间归属一律由 `period_code`
   决定（§4.3 口径说明 ②），`occur_date` 只作展示。
3. **「折旧科目配成 5602.*」不报错也不进未归集清单**：按 §4.5.1 这是业务配置（这笔费用根本
   不进成本），不是数据缺口。若产品要求「本期有多少折旧没进成本」的提示，属 B2-12 的未归集
   清单口径（需要新增一类「配置类缺口」），本卡不擅自扩范围。
4. **没有迁移**：所需表全部由既有迁移建好并已应用 ⇒ 卡面「迁移幂等 + `.down.sql`」N/A。
5. **没有前端 / 没有 HTTP 接口**：段卡按 B2-6/B2-7 的既定打法只交付 `collect_overhead` +
   `collect_overhead_in_tx`；`pnpm typecheck && pnpm build` N/A。
6. **池里的车间名**由 `LEFT JOIN sys_dept` 一次取回（实时名优先，部门已删回落来源快照），
   不做逐行查询（无 N+1）。
7. **演出用真库**：`costb26_drill_1625844`（B2-7 保留的演练库），已在 4.1 复核零残留。

## 6. 给下游卡的交接

- **B2-9 / B2-10（分摊与结算）**：池直接读 `cost_overhead_collect_model::pool_of_period(ex, period)`
  （带车间名 / 笔数）或 B2-5 的 `cost_alloc_rule_model::pool_rows(period)`（同口径，走全局 `db()`）；
  两者在真库用例里已互校一致。**不要**从 `biz_cost_collect` 自己拼 SQL 反查科目 —— 三路来源的
  科目列名各不相同（`debit_account_code` / `expense_account_code` / `account_code`）。
- **calc 编排卡（#27）**：`collect_overhead_in_tx(ex, period, operator)` 不提交；段间顺序无关，
  但**先清当期时按 element 分段清**，别一次 `DELETE WHERE period_code=?` 把别段刚写的行删掉；
  另注意本段会清「本段三路来源」里已不合口径的旧行，**不会**碰 `ALLOC` 行。
- **B2-11（约当与计算单）**：直接行（`cost_object_id` 非空）已经在 collect 里，算对象成本时
  按 `element='OVERHEAD' AND cost_object_id = ?` 取；池行必须等分摊后才有对象。
- **B2-12（未归集清单接口）**：费侧缺口判据（本卡未持久化任何新表，`Outcome.uncollected` 可直接透传）：
  ```sql
  -- 0 元来源（折旧 / 报销明细 / 手工台账），且没有活归集行
  SELECT 'DEPRECIATION' AS src, dp.id FROM biz_depreciation dp
   WHERE dp.status='POSTED' AND dp.period_code=? AND dp.expense_account_code LIKE '4101.%'
     AND dp.amount = 0
     AND NOT EXISTS (SELECT 1 FROM biz_cost_collect c WHERE c.source_type='DEPRECIATION'
                       AND c.source_id=dp.id AND c.deleted_at IS NULL)
  UNION ALL
  SELECT 'EXPENSE', ei.id FROM biz_expense_item ei JOIN biz_expense_report r ON r.id=ei.report_id
   WHERE r.deleted_at IS NULL AND r.status IN ('APPROVED','PAID') AND r.period_code=?
     AND ei.account_code LIKE '4101.%' AND ei.amount = 0
     AND NOT EXISTS (SELECT 1 FROM biz_cost_collect c WHERE c.source_type='EXPENSE'
                       AND c.source_id=ei.id AND c.deleted_at IS NULL);
  -- 手工台账 amount > 0 是库层 CHECK，0 元台账造不出来（无需列）
  ```

## 7. 复跑

```bash
# 纯逻辑
cd backend && cargo test --lib cost_overhead_collect
cargo test --lib cost_calc_overhead

# 真库（演练库 costb26_drill_1625844，B2-7 保留）
ERP_DB_URL='postgres://alun:***@127.0.0.1:5432/costb26_drill_1625844' \
  cargo test --lib cost_calc_overhead -- --test-threads=1

# 门禁
cargo fmt -- --check && cargo clippy --all-targets -- -D warnings && cargo test
```

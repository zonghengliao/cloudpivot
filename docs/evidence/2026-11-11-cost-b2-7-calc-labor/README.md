# COST-B2-7【成本】calc 第 2 段：工归集 —— 证据

- 卡：`t_c485dcf1`（COST-B2-7），批次 B2，前置 B2-1（工时台账，`t_2e11875e`）/ B2-6（段 1，`t_a9416552`）
- 设计依据：`docs/architecture/ARCH-9-成本核算升级设计.md`
  - §4.3 归集时点与来源（工行：`biz_cost_labor` `status='CONFIRMED'`、幂等键 `(LABOR_TICKET, labor_id, 0)`）
  - §3.1 ③ `biz_cost_collect` / ④ `biz_cost_labor`（DDL 与 CHECK）
  - §4.5.4 工资率取自哪（对象 > 车间 > 公司，缺费率不按 0）
  - §8.1 B2-7 行：`工时 CONFIRMED → 逐条写 collect`
- 范围：**只有**「工时 CONFIRMED → 逐条写 collect」。不做费归集（B2-8）、不做池对账（B2-9）、
  不做分摊（B2-10）、不做约当/计算单（B2-11）、不做未归集清单接口（B2-12）。

## 1. 改了什么

| 文件 | 类型 | 内容 |
|---|---|---|
| `backend/src/business/erp/cost_labor_collect_dto.rs` | 新增（纯逻辑，无库） | `LaborLine`（行校验 / 期望金额 / 能否归集）、`plan_of`（增量计划：写谁 / 跳谁 / 清谁 / 谁的费率缺）、`LaborCollectOutcome`、`UncollectedLabor`（未归集清单行）、`66064` 错误构造；**16 条单测** |
| `backend/src/business/erp/cost_collect_model.rs` | 追加（未改既有料段函数） | `LABOR_SOURCE_SQL`（取数口径）、`INSERT_LABOR_SQL`（幂等写）、`labor_source_lines` / `labor_collect_rows_of_period` / `insert_labor_line(s)` / `collected_labor_total`；**5 条 SQL 形状单测** |
| `backend/src/business/erp/cost_calc_labor.rs` | 新增（编排） | `collect_labor(period, operator)`（自开事务并提交）+ `collect_labor_in_tx(ex, …)`（供 calc 编排卡复用）；扫→校验→清过期→写新→补「怎么修」→回查；**4 条单测** |
| `backend/src/business/erp/cost_calc_labor_db_tests.rs` | 新增（真库用例，`ERP_DB_URL` 开关） | **7 条**：只取 CONFIRMED / 重复跑幂等 / 缺费率进未归集清单 / 来源作废后清旧留痕 / 改挂对象搬金额 / 空期间无操作 / **与料段互不踩** |
| `backend/src/business/erp/mod.rs` | 改动（登记） | `pub mod cost_calc_labor;` + `#[cfg(test)] mod cost_calc_labor_db_tests;` + `pub mod cost_labor_collect_dto;` |

**无迁移**：`biz_cost_collect` / `biz_cost_labor` 由 V0371（B1-2）建好且已应用，本卡一字未改
（卡面「迁移幂等 + .down.sql」⇒ N/A，见 §5 偏差）。
**无 HTTP 入口**：设计 §5.2 只有 `POST /cost-period/calc`（#27，属 calc 编排卡），段卡不单独暴露接口
（单独暴露会让期间停在半算状态）；无前端改动 ⇒ `pnpm typecheck/build` N/A。

## 2. 三条口径（本卡的全部规则）

1. **只取 CONFIRMED**：`LABOR_SOURCE_SQL` 的 `WHERE` 里 `l.status = 'CONFIRMED'` + `period_code = ?` +
   `deleted_at IS NULL`。`DRAFT` 还能改，归集它等于归集一个会变的数（真库用例 ① 造了 DRAFT / 上期 /
   已软删三类行，全部不写）。
2. **金额 = 工时 × 费率，取台账上的 `amount`**：`biz_cost_labor.amount` 由库层
   `ck_biz_cost_labor_amount CHECK (amount = round(hours * rate, 4))` 钉死；`rate` 是录入/导入时
   由 **B2-3** 按「对象 > 车间 > 公司」取数后落的**快照**（§4.5.4）。
   归集段**照抄**，不再乘一遍（多一个舍入点就多一次「谁算的」分歧）；`LaborLine::validate()` 里用
   `cost_labor_dto::expected_amount`（与 CHECK 同引擎同舍入）复核一遍，不一致 ⇒ 400 + `66064`。
3. **缺费率的行不写 0，进未归集清单**：`rate = 0`（或 `round(hours × rate, 4) = 0`）的已确认工时
   **不写 collect 行**，进 `LaborCollectOutcome::uncollected`（带工时 / 员工 / 对象 / 车间 / 原因 /
   「怎么修」）。理由：一期成本核算不接受 0 工资率行参与归集 —— `rate = 0` 在账上分不清
   「真的不值钱」与「财务还没配费率」，静默按 0 计正是设计 D-7「不猜」禁止的。

### 2.1 「怎么修」是怎么给出来的

真有未归集行时才查一次 `biz_cost_rate`（`cost_rate_handler::candidates(RATE_TYPE_LABOR)`，
一次取回同类型全部未删除行），逐行用纯函数 `cost_rate_dto::resolve` 判：

- 能取到 ⇒ `现在已能取到 X 元/小时（来源：成本对象（CB26-…）/车间（…）/公司），请在工时台账把这一行的工资率补上（或作废重录）后重新确认，再重跑计算`
- 取不到 ⇒ `费率取数三级都没命中：<B2-3 的 422 报文> 请先维护 biz_cost_rate（生效期要覆盖 <工作日期>）`

查费率失败不打断整段（缺费率本来就可以「先算着、结算前补」，不该让料段已写的行回滚）。

## 3. 幂等（两种语义都实现，与料段同构）

| 语义 | 做法 |
|---|---|
| 不重复写 | 幂等键 `(LABOR_TICKET, labor_id, 0)` 撞 `uk_biz_cost_collect_src`（`WHERE deleted_at IS NULL AND source_id IS NOT NULL`）⇒ `ON CONFLICT DO NOTHING`，返回行数才是真实新增 |
| 结果收敛 | 已归集行与来源比对 `(cost_object_id, amount)`：工时行被作废 / 改挂对象 / 金额被修正 ⇒ 旧行软删 + 重写 |

★ **顺序不能反**：`uk_biz_cost_collect_src` 是部分唯一索引，必须**先软删旧行再 INSERT**
（反之 `ON CONFLICT DO NOTHING` 会静默吞掉新行，再删旧行会让这一键一行不剩）。

★ 缺费率的行**不参与**「已归集 / 过期」判定 ⇒ 历史上若有它的旧归集行，那行走 `stale` 被清掉
（否则「费率被清零」的脏数据会永远留着旧金额）。单测 `rows_whose_source_disappeared_are_marked_stale`
与真库用例 ③ 钉住这条。

## 4. 实测输出

### 4.1 真库用例（`ERP_DB_URL=…/costb26_drill_1625844`，`02_real_db_tests.out`）

```
running 7 tests
collects_only_confirmed_labor_of_the_period ... ok          # 只取 CONFIRMED；金额 = round(hours×rate,4)
rerun_does_not_duplicate_and_leaves_rows_untouched ... ok   # 第二次 inserted=0 / skipped=2，id+updated_at 不变
missing_rate_rows_go_to_the_uncollected_list ... ok         # rate=0 不写 collect，金额合计只有 400
purges_rows_whose_source_no_longer_qualifies ... ok         # 作废后软删留痕，第三次跑收敛
relinked_cost_object_moves_the_collected_amount ... ok      # 改挂对象：清旧写新，总额不变
period_without_confirmed_labor_is_a_clean_noop ... ok       # 空期间一行不写；非法期间 400
labor_segment_ignores_material_rows_of_the_same_period ... ok # 同期间同对象的料行不混进工段回查
test result: ok. 7 passed; 0 failed; ... finished in 4.72s
```

跑完演练库零残留（`biz_cost_labor` / `biz_cost_object` / `biz_cost_collect` 里 `B27-` / `CB27-` 前缀行数均 0）。

### 4.2 纯逻辑单测

- `cargo test --lib cost_labor_collect` ⇒ **16 passed**
- `cargo test --lib cost_calc_labor` ⇒ **4 passed**（+ 真库 7 条在无 `ERP_DB_URL` 时 SKIP）
- `cargo test --lib cost_collect_model` ⇒ **10 passed**（含料段 4 条原样通过）

### 4.3 门禁

| 命令 | 结果 |
|---|---|
| `cargo fmt -- --check` | `FMT_EXIT=0` |
| `cargo clippy --all-targets -- -D warnings` | `Finished dev profile`，0 warning / 0 error |
| `cargo test`（全仓） | 见 `01_full_cargo_test.out` |

### 4.4 首跑红（与本节无卡，见 `00_full_cargo_test_first_run_flake.out`）

`testd::impexp::i01_import_chain_template_precheck_confirm` 撞
`uk_sys_import_task_no`（`IMP20261110-B8CB` 已存在）。这是 B2-6 已记录的既有 flake 的**同族**：
`sys_import_task.task_no` 只有 4 位十六进制随机（`IMP{date}-{4 hex}`），共享 `cloudpivot_test` 库
里并发跑的用例 / 别的 worker 留下的行会随机撞键。单跑该用例即过（7.36s），与本卡无关（未碰 impexp）。

## 5. 已知偏差 / 未做

1. **`source_no` 留 NULL**：工时台账没有单号列（设计 §3.1 ④），不编造「单号」。溯源靠
   `source_id`（精确到工时行）；人可读上下文（日期 / 员工 / 工时 × 费率 / 台账来源）写
   `biz_cost_collect.remark`，归集明细列表不 join 也能看懂这一行是什么。
   ⚠ 设计 §4.5.2 硬规则 3 说「每条归集行都留 `source_type + source_no`」，工行是**结构性**做不到
   （台账无单号），已在卡内报告。
2. **`dept_id` / `dept_name` 取工时行的「工作中心」**（料行取成本对象的车间）：工时台账的车间就是
   干活的地方，费池对账（B2-9）只按 `element='OVERHEAD'` 取数，不看工行的 dept，不会被这里影响。
3. **0 工资率行不进归集**（见 §2 口径 3）：若客户确有「不计成本的人工」，一期口径是**不写**归集行、
   留在未归集清单里让人看见；不静默记 0。
4. **没有迁移**：所需两表已由 V0371 建好，本卡一行 SQL 迁移都没写 ⇒ 卡面「迁移幂等 + `.down.sql`」N/A。
5. **没有前端 / 没有 HTTP 接口**：段卡按 B2-6 的既定打法只交付 `collect_labor` +
   `collect_labor_in_tx` 两个函数；`pnpm typecheck && pnpm build` N/A。
6. **期间区间**：工行只按 `biz_cost_labor.period_code` 取数（设计 §4.3 口径说明 ②：跨月补录按录入时
   选的期间），不需要 `biz_accounting_period` 的起止日（那是料段按 `out_date` 落期间才要的）。

## 6. 给下游卡的交接

- **B2-8（费归集）**：`cost_collect_model` 里请加**同族函数**（`overhead_source_lines` 之类）并守
  `element='OVERHEAD' + source_type` 过滤，别改料段 / 工段既有函数（三段互不踩）。
- **B2-9（池与 4101 对账）**：工行不参与池（`element='LABOR'`）。
- **B2-10（分摊基数）**：`LABOR_HOURS` / `MACHINE_HOURS` 基数**直接查工时台账**（`status='CONFIRMED'`），
  不要从 collect 反推 —— collect 里缺费率的行为 0 条（正是为了让基数不被 0 费率带偏）。
- **B2-12（未归集清单接口）**：工侧缺口的判据（**不需要**本卡持久化任何东西）：
  ```sql
  SELECT l.* FROM biz_cost_labor l
   WHERE l.status = 'CONFIRMED' AND l.period_code = ? AND l.deleted_at IS NULL
     AND (l.rate = 0 OR l.amount = 0)
     AND NOT EXISTS (SELECT 1 FROM biz_cost_collect c
                      WHERE c.source_type = 'LABOR_TICKET' AND c.source_id = l.id
                        AND c.deleted_at IS NULL);
  ```
  本段在 `LaborCollectOutcome::uncollected` 里给出同样的行 + `reason` + `hint`（calc 编排卡可直接透传）。
- **calc 编排卡（#27）**：`collect_labor_in_tx(ex, period, operator)` 与料段一样不提交；两段顺序无关
  （各扫自己的 element），但**先清当期再重灌**时要按 element 分段清，别一次 `DELETE WHERE period_code=?`
  把别段刚写的行删掉。

## 7. 复跑

```bash
# 纯逻辑
cd backend && cargo test --lib cost_labor_collect
cargo test --lib cost_calc_labor
cargo test --lib cost_collect_model

# 真库（演练库 costb26_drill_1625844，B2-6 保留）
ERP_DB_URL='postgres://alun:alun@127.0.0.1:5432/costb26_drill_1625844' \
  cargo test --lib cost_calc_labor_db -- --test-threads=1

# 门禁
cargo fmt -- --check && cargo clippy --all-targets -- -D warnings && cargo test
```

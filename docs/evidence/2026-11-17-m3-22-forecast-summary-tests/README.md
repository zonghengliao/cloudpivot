# M3-22【M3·销售】销售预测：单测（含汇总口径）—— 证据

- 卡：`t_e33d2100`（M3-22）
- 上游：`t_31257c83`（M3-21，`GET /business/erp/sales-forecast/variance` 偏差分析）、`t_0be1f0b6`（M3-20，表结构 + 当前版本视图）
- 本卡**只加测试**，不动生产代码、不动迁移、不动权限码/菜单：`git diff` 只有 3 个文件（2 个测试相关 + 1 个 mod 注册）+ 1 个新测试文件。
- 日期：2026-11-17

## 一、这一卡补的是什么（M3-21 已有什么，别重复）

M3-21 已经落了「单条链路能跑通」的用例（v2 覆盖 v1、本位币折算、漏预测行、粒度计数、7 种 400、清场），
本卡按上游 `next_steps` 的一句原话：「M3-22 单测只补**汇总口径**更细边界（多行同粒度求和、容差边界、
pair 漏预测、软删当前版本回落），**别重复造夹具**」。

所以本卡的 16 条用例全部钉 `summarize_variance` / `assemble_variance` 的**汇总语义**：

### 纯逻辑 10 条（`sales_target_handler::tests`，不连库，`cargo test --lib` 直接跑）

| 用例 | 钉住的口径 |
|---|---|
| `summary_multi_rows_add_up_like_the_rows` | 汇总 = 同一批行相加（含数量和档位计数 `over+under+on_target = forecast_count`）；把行自己再加一遍必须等于 summary |
| `summary_rate_comes_from_totals_not_mean_of_rates` | **合计偏差率 = Σ实际 ÷ Σ预测**（1.10%），行偏差率算术平均是 MAPE（50.50%），两者不等 |
| `summary_keeps_unforecast_actuals_in_totals` | 漏预测行的实际额**进合计**（否则「预测 10 万 / 实际 5 万」会被算成实际 0） |
| `summary_counts_zero_amount_forecast_row_but_excludes_it_from_mape` | 只预测数量（金额 0）的行算 OVER、进 `forecast_count`，但比率 `null` ⇒ 不进 MAPE（不是 0） |
| `summary_quantity_rate_is_null_when_quantity_never_forecast` | 数量侧同规则：预测数量合计 0 ⇒ 数量偏差率 `null` |
| `pair_grain_unforecast_rows_keep_both_dimensions` | 客户 × 物料粒度的漏预测行带**两维** id/名字（物料粒度的客户列是空的，别混） |
| `tolerance_boundaries_pick_the_right_level` | 容差 0 / ±5% / 100% 三档边界：正好 ±容差算命中，多一分钱就出界 |
| `rows_sort_by_absolute_variance_then_actual_then_name` | 行序 `|偏差|` 倒序 → 实际额倒序 → 显示名升序（同额偏差不会每次刷新换位置） |
| `summary_worst_prefers_the_row_with_a_forecast` | 同额偏差时 `worst` 取**有预测**那行（能说清是哪条预测偏了） |
| `all_grain_quantity_only_actual_is_still_a_missing_forecast` | 全公司没预测但只卖了数量（金额 0）也要给漏预测行 —— 判据是「金额**或**数量非 0」 |

### 真库 6 条（`sales_variance_summary_db_tests`，新文件，夹具**复用** M3-21 的）

| 用例 | 钉住的口径 |
|---|---|
| `summary_adds_rows_and_excludes_history_versions` | 预测合计只算当前版本（v1 的 10 万不进，进了就是 31 万）；合计率 -28.57% ≠ 行率平均 -35.00%；MAPE 48.33%；worst = 偏差最大的那行 |
| `soft_deleted_current_version_falls_back_in_variance` | 软删当前版本 v2 后，**对比接口**回落到 v1（M3-20 只在 SQL 层验过视图，这里穿过 handler 验到接口） |
| `pair_grain_reports_unforecast_pairs` | 客户 × 物料漏预测行两维齐全，实际额进合计（16 万 = 1 万预测 + 6 万实际差额的分子分母都算） |
| `grains_share_actual_total_but_isolate_forecast` | 同一批订单四种切法：实际合计**都是 9 万**（不是四份钱），预测合计 10/5/3/20 万各归各的，任一份都不等于相加的 38 万 |
| `tolerance_only_moves_the_level_not_the_totals` | 容差 0 / 5 / 10 只动判档（OVER / ON_TARGET / ON_TARGET），金额·偏差·率·数量一字不差 |
| `zz_teardown_purges_fixtures` | 清场：预测 0 行、订单 0 行（与 M3-21 共用同一份判据） |

「汇总 = 行相加」不是硬编码几个数字就算数：每条真库用例都调 `assert_summary_is_sum_of_rows(&response)`，
把返回的行自己再加一遍，并检查 `row_count = forecast_count + unforecast_count`、
`forecast_count = over + under + on_target` —— 取数口径漂了（汇总另跑一遍不同过滤）这条会直接红。

## 二、门禁与结果（全部单跑，原始输出同目录）

| 项 | 命令 | 结果 | 原始输出 |
|---|---|---|---|
| 纯逻辑（本卡新增 10 + 既有 26 + 目标/期间/看板等） | `cargo test -p cloudpivot --lib business::erp::sales::sales_target -- --test-threads=1` | **60 passed; 0 failed** | `01_pure_tests.txt` |
| 真库（M3-21 回归 7 + 本卡 6） | `ERP_DB_URL=…/cp_m322_summary cargo test -p cloudpivot --lib business::erp::sales::sales_variance -- --test-threads=1` | **13 passed; 0 failed** | `02_db_tests.txt` |
| M3-20 回归（表结构/视图 11 条） | `ERP_DB_URL=…/cp_m322_summary cargo test -p cloudpivot --lib business::erp::sales::sales_forecast -- --test-threads=1` | **11 passed; 0 failed** | `06_m3_20_regression.txt` |
| rustfmt | `cargo fmt -p cloudpivot --check` | 通过（exit 0） | — |
| clippy（lib + tests） | `cargo clippy -p cloudpivot --lib --tests -- -D warnings` | Finished，零告警 | `05_clippy.txt` |

跳过保护：真库日志里 `SKIP` 行数 = **0**（`grep -c SKIP` ⇒ 0），即真的连上了 `cp_m322_summary` 走完夹具，
不是「没设 ERP_DB_URL 静默绿」。

## 三、变异反证（证明这些断言真的会咬人）

把 `summarize_variance` 里两行位置挪一下 —— 让 `actual_amount` / `actual_qty` 只在 `has_forecast` 之后累加
（即「漏预测行的实际额不进合计」这个**经典错法**），其它一概不动，重跑：

```text
test …sales_variance_summary_db_tests::pair_grain_reports_unforecast_pairs ... FAILED
  left: 8000.00   right: 16000          ← 实际合计少了漏预测行的 8000
test result: FAILED. 5 passed; 1 failed
```

```text
test …sales_target_handler::tests::summary_keeps_unforecast_actuals_in_totals ... FAILED
test result: FAILED. 5 passed; 1 failed
```

原始输出：`03_mutation_db_failure.txt` / `04_mutation_pure_failure.txt`。变异已还原（还原后重跑 13/13 与 60/60 全绿，
见 §二的两行结果），`git diff` 里不存在该变异。

## 四、环境（按卡面「数据库约束」走：本机是 99，PG 走 145）

```bash
HOST=172.18.10.145
PGPASSWORD=… createdb -h $HOST -U cloudpivot -T cloudpivot_test cp_m322_summary
# 模板库停在 V0400，biz_sales_forecast 缺 forecast_qty / revised_from_id / 当前版本视图：
psql -h $HOST -U cloudpivot -d cp_m322_summary -v ON_ERROR_STOP=1 -q \
  -f backend/app/migrations/business/V0444__sales_forecast_qty_version.up.sql
#   ⇒ NOTICE: M3-20 自检通过：forecast_qty(NUMERIC(18,4) 非负) + 版本链 revised_from_id(禁自指)
#             + 物料维度索引 + 当前版本视图（7 组行为探针）
ERP_DB_URL="postgres://cloudpivot:…@$HOST:5432/cp_m322_summary" cargo test --lib … -- --test-threads=1
PGPASSWORD=… dropdb -h $HOST -U cloudpivot cp_m322_summary
```

M3-20 回归的 11 条也用同一个 `cp_m322_summary` 跑的（`cargo test --lib business::erp::sales::sales_forecast`）。
收工前查残留：`SELECT count(*) FROM biz_sales_forecast WHERE remark LIKE 'M3-2%'` ⇒ 0；
`SELECT count(*) FROM biz_sales_order WHERE so_no LIKE 'M32%'` ⇒ 0；`cp_m322_summary` 已 dropdb。

## 五、改了哪些文件

| 文件 | 改动 |
|---|---|
| `backend/app/src/business/erp/sales/sales_target_handler.rs` | 测试模块末尾追加「汇总口径（M3-22）」10 条纯逻辑用例 + 补一个 `DEVIATION_NO_FORECAST` 到测试的 import 表。**生产代码零改动**（变异验证时临时改过两行，已还原） |
| `backend/app/src/business/erp/sales/sales_variance_summary_db_tests.rs` | 新增：6 条真库汇总口径用例（含 `assert_summary_is_sum_of_rows` 自洽断言） |
| `backend/app/src/business/erp/sales/sales_variance_db_tests.rs` | 只把夹具函数/常量标成 `pub(crate)`（`sell` / `sell_line` / `forecast` / `setup` / `cleanup` / `query` / `row_named` / `dec` / `customer` / `material` / `PERIOD` / `MARK` / `SO_PREFIX`），供本卡复用，**不另造一套夹具**；用例本体一字未改 |
| `backend/app/src/business/erp/sales/mod.rs` | 注册 `sales_variance_summary_db_tests`（`#[cfg(test)]`） |
| `docs/evidence/2026-11-17-m3-22-forecast-summary-tests/` | 本 README + 5 份原始输出 |

零迁移、零权限码、零菜单、零路由、前端零改动（本卡不涉及前端）。

## 六、留给下一张卡的话

- M3-23（前端页面）要注意的三点仍成立：`variance_rate = null` **不要**渲染成 0%；切粒度要重新请求（`summary` 与 `rows` 同源同粒度，
  禁止前端把两个粒度的 summary 相加）；`dimension_counts` 用来做「按物料（3 条）」这类提示。
- 若以后要改判档口径（比如容差改成不对称，或 `ON_TARGET` 不含边界），本节 10 条纯逻辑用例是唯一需要同步改的地方，
  真库那 6 条里有 3 条会跟着红（`tolerance_only_moves_the_level_not_the_totals` 会直接指出容差已经影响到了合计）。

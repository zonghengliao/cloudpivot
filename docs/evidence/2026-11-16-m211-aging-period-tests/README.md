# M2-11 库龄：单测（含跨期与呆滞判定）—— 交付证据

卡：`t_855bc226`（M2-11【M2·仓库】库龄：单测 · 含跨期与呆滞判定）
上游：`t_397dc77c`（M2-08 计算 · V0424 库函数）+ `t_4e2cb6f6`（M2-09 报表 · V0425）+
`t_bb0fe3c6`（M2-10 呆滞料 · V0426）

## 1. 做了什么（一句话 + 边界）

给库龄补上**跨期**与**呆滞判定随时间推进**的单测：新增
`backend/app/src/business/erp/inventory_aging/aging_period_tests.rs`（**11 条**：4 条纯逻辑 + 7 条真库），
把「基准日往前走，数字怎么变」钉死 —— 月末快照逐期演进、补录单据按业务日期归期、
期初层按期间月初、业务日期等于基准日当天含、以及同一层货**第 91 天才转呆滞**。

* **零生产代码改动**：本卡不加接口、不加权限码、不加迁移（`V0424~V0426` 已由上游落地）。
  唯一的非测试改动是 `inventory_aging/mod.rs` 的模块文档（把新用例文件挂进导航）。
* **为什么单开一张卡**：M2-08/09/10 的 86 条用例各自只钉**自己那个视图**在**一个基准日**
  （2026-11-16）上的口径；跨期口径漂了（某处用业务日期、某处用过账时间）单日用例照样全绿。
* **本卡不夹带**：前端页（M2-12）、导出、呆滞处置动作、`biz_inventory_txn_dated` 视图都没做。

## 2. 用例清单（11 条，逐条对应卡面「含跨期与呆滞判定」）

| # | 用例 | 钉住的口径 | 类型 |
|:--:|:--|:--|:--|
| 1 | `bucket_and_slow_flip_on_the_same_day` | 档位与呆滞**同一把尺**：`bucket_of(age)=="OVER_90"` ⇔ `is_slow(age,90)`，0..=400 天逐日 | 纯逻辑 |
| 2 | `age_days_across_month_year_and_leap_boundaries` | 库龄是**日历天数**：跨月 29/60/90/91、跨年 1/31、闰年 2/366 天，不因跨期重置 | 纯逻辑 |
| 3 | `period_end_identity_holds_for_slow_quantity` | 6 个期末基准日上 `Σ OVER_90 档数量 == Σ 呆滞层数量`（同一批层两种判法） | 纯逻辑 |
| 4 | `inbound_bounds_hold_at_period_ends` | 月末 / 月初 / 闰日 / 跨年基准日上 `inbound_bounds` 与 `age_days` 逐天等价（6×6×(-3..=200)） | 纯逻辑 |
| 5 | `cross_period_snapshots_evolve_per_period_end` | 同一批流水在 08-31/09-30/10-31/11-30 的层剩余 / 结存 / 库龄逐期演进；**老层只变老、不消失、层日期不变**（`include_consumed=true` 交叉核对） | 真库 |
| 6 | `backdated_document_lands_in_its_business_period` | 补录单据按**业务日期**归期：业务 09-28/09-29、过账 11-25/11-26 的单据必须进 9 月期末快照 | 真库 |
| 7 | `slow_flag_flips_when_period_advances_past_threshold` | 呆滞**跨期**翻转：29 / 60 / 90 天都不呆滞，**91 天**才呆滞；档位同期迁移 0-30→31-60→61-90→90+ | 真库 |
| 8 | `three_views_agree_at_every_period_end` | 明细 / 报表 / 呆滞三个视图在**每个**期末基准日互相自洽（Σ层剩余 = Σ档 = 呆滞 + 不呆滞；N=90 时呆滞 == OVER_90） | 真库 |
| 9 | `period_boundary_day_is_inclusive` | 业务日期 == 基准日**含**当天：当天进、当天出都在当天期末可见（库龄 0） | 真库 |
| 10 | `opening_layer_dates_from_period_start` | 期初层（`doc_type=OPENING`）的层日期 = `period_code` 月初，**不取过账时间**（否则 9 月期初被算成 11 月新货） | 真库 |
| 11 | `zz_teardown_purges_fixtures` | 清场：`M211-` 残留 0（含期初流水按物料精确认领，绝不按 `OPENING-%` 全删） | 真库 |

夹具一律**直接插流水**（与 M2-08/09/10 同因）：本卡要造「业务日期 × 过账时间 × 基准日」三者错位，
走真实过账造不出「业务日期 09-28、过账 11-25」的补录单据（过账一律 `Utc::now()`）。
主数据走 `crate::test_seed::ensure_*`；串行锁复用 `aging_db_tests::DB_LOCK`（四批用例共用一个 `db()`）。

## 3. 证据清单

| 文件 | 内容 | 结果 |
|:--|:--|:--|
| `01_cargo_test.out` | `ERP_DB_URL=…cargo test --lib business::erp::inventory_aging`（克隆库 `cp_m211_period_test`） | **97 passed / 0 failed / 0 SKIP**（M2-08 36 + M2-09 26 + M2-10 24 + 本卡 11） |
| `02_mutation_drill.out` | `bash scripts/verify/m211_aging_period_mutation_drill.sh`（**反证演练**） | **PASS=15 FAIL=0 / ALL PASS** |
| `03_pure_tests.out` | 不带 `ERP_DB_URL` 跑本卡 11 条（4 条纯逻辑照跑，7 条真库 SKIP） | **11 passed**，SKIP 提示 1 条（`ensure_db` 的 OnceCell，后续复用的不重复打印） |
| `04_gates.out` | `cargo fmt --check`（本卡两文件）+ `cargo clippy --lib -- -D warnings` + `cargo clippy --lib --all-targets -- -D warnings` | fmt rc=0；clippy 两侧 **0 warning** |

本卡**零前端改动**（前端页是 M2-12），所以没跑 `pnpm typecheck/build`。

## 4. 反证演练（02）—— 为什么「全绿」不等于「有用」

`scripts/verify/m211_aging_period_mutation_drill.sh` 把 V0424 库函数的判据**故意改错两次**，
每次要求本卡用例立刻变红，恢复后要求重新全绿：

| 变异 | 错法（真实项目里见过） | 结果 |
|:--|:--|:--|
| A | 期末快照改用**过账时间** `created_at` 当判据 | **2 条变红**，其中 `backdated_document_lands_in_its_business_period` 点名抓住 ⇒ 补录单据会漏期 |
| B | 期末快照改成 `biz_date < p_as_of`（当天漏掉） | **2 条变红**：`period_boundary_day_is_inclusive` + `opening_layer_dates_from_period_start` |
| 恢复 | 重放 V0424 `CREATE OR REPLACE` | 11 条全绿（证明变异只动了库函数，没污染夹具） |

演练库 `cp_m211_mutation_drill` 由脚本自建自删（库名必须含 drill/e2e/test，否则拒绝执行）。

## 5. 已知口径（明写，不假装）

1. 纯逻辑用例里「层是否存在」按**入库日期 ≤ 基准日**判断；库函数本身对「入库日期晚于基准日」
   的层给**负库龄**（M2-08 的 `future_inbound_keeps_negative_age` 钉着这件事），本卡不改变该口径。
2. 跨期恒等式的「呆滞」固定用缺省阈值 90 天（`DEFAULT_THRESHOLD_DAYS`）；阈值三级取值
   （参数 > `sys_config` > 缺省）由 M2-10 的用例负责，本卡不重复。
3. 期初层（`OPENING`）的层日期取 `period_code` 月初，是 V0424 `DATED_CTE` 的兜底链决定的；
   本卡只在**真库**上验证该行为，不在 Rust 侧再实现一份（第二份口径 = 漂移源）。
4. 本卡的用例**只按 `物料 × 仓库` 过滤**取数，所以报表 / 呆滞结果里恰好 0 或 1 行；
   全库口径（跨物料单位混合等）由 M2-09 / M2-10 负责。

## 6. 复现步骤

```bash
export PGPASSWORD=fbb1263691ff75c31ec99eaf258138b4
# ① 克隆模板库（181 表）+ 补到 V0426（模板库不带库龄函数，必须自己补）
createdb -h 172.18.10.145 -U cloudpivot -T cloudpivot_test cp_m211_period_test
for f in V0424__inventory_aging_calc V0425__inventory_aging_report V0426__inventory_slow_moving; do
  psql -h 172.18.10.145 -U cloudpivot -d cp_m211_period_test -v ON_ERROR_STOP=1 -q \
    -f backend/app/migrations/business/$f.up.sql
done

# ② 全量库龄用例（97 条：上游 86 + 本卡 11）
cd backend && ERP_DB_URL="postgres://cloudpivot:***@172.18.10.145:5432/cp_m211_period_test" \
  cargo test --lib business::erp::inventory_aging -- --test-threads=1

# ③ 本卡 11 条（带输出）
ERP_DB_URL="…同上…" cargo test --lib \
  business::erp::inventory_aging::aging_period_tests -- --test-threads=1 --nocapture

# ④ 反证演练（自建自删库 cp_m211_mutation_drill）
bash scripts/verify/m211_aging_period_mutation_drill.sh

# ⑤ 门禁
cd backend && cargo fmt -- --check && cargo clippy --lib -- -D warnings \
  && cargo clippy --lib --all-targets -- -D warnings

# ⑥ 清场
dropdb -h 172.18.10.145 -U cloudpivot cp_m211_period_test
```

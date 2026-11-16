# M1-21【M1·财务】应付账龄：补分档（现有只有总账龄）

- 日期：2026-11-16
- 卡号：`t_4d47e4ff`（M1-21）
- 结论：**已落地**。应付账龄从 5 档（未到期 / 1-30 / 31-60 / 61-90 / **90 天以上**）
  补齐到与应收账龄（M1-19）**同一张档位表**的 7 档：
  `未到期 / 1-30 / 31-60 / 61-90 / 91-180 / 181-365 / 365 天以上`。
  付款账龄与收款账龄此后刻度一致、可直接对照。
- 迁移：**0 条**（账龄是现算报表，没有任何落库结构变更）。

## 做了什么

| # | 改动 | 文件 |
|:--:|------|------|
| 1 | 删掉应付专有的 5 档表（`BUCKETS` 原为 `[BucketSpec; 5]`，末档 `OVER_90`），把 `BUCKETS` 改成 `AGING_BUCKETS` 的**别名**（`pub const BUCKETS: [BucketSpec; 7] = AGING_BUCKETS;`）—— 全库只剩**一张**档位表 | `backend/app/src/business/erp/payable/payable_aging.rs` |
| 2 | 应付整链自动跟上 7 档（这些调用点一个字没改，因为都走 `BUCKETS` / `bucket_of` / `bucket_case_sql`）：<br>· SQL 聚合分档（`load_cells` 的 `CASE`）<br>· 明细行现算（`assemble` 的 `bucket_of`）<br>· 供应商透视初值（`pivot_suppliers`）<br>· 全公司汇总（`summarise`）<br>· 出参表头 `buckets` / `summary`（各 7 格）<br>· xlsx 导出（`export_table` 由 9 列变 **11 列**） | 同上 |
| 3 | 口径提示里加一条「分档：未到期 / 1-30 / … / 365 天以上（与应收账龄同一张档位表）」—— 由 `AGING_BUCKETS` 生成，不是手写串 | 同上 |
| 4 | 单测改造：边界测试补 91/180/181/365/366；`receivable_buckets_extend_payable_prefix` → `payable_and_receivable_share_one_bucket_table`（钉死 `BUCKETS == AGING_BUCKETS` + 逐天 −400..=3650 同档 + 两边生成的 SQL `CASE` 逐字相同）；SQL 常量测试补 180/365 两道阈值；透视 / 汇总 / 导出测试改用新档位键；**新增** `export_table_has_seven_bucket_columns`（11 列逐列 + 缺档 `0.00`） | 同上 |
| 5 | 应收侧文档口径回写（原写着「应付暂 5 档，切细是 M1-21 的事」） | `backend/app/src/business/erp/receivable/receivable_aging.rs` |
| 6 | 模块总览表里应付账龄的档位说明改成 7 档 | `backend/app/src/business/erp/mod.rs` |
| 7 | 老的 FIX-8A 真机脚本同步到 7 档（否则它下次跑会误报红）：档位键表 / 91 天 → `D91_180` / 注释说明夹具只覆盖前五档边界 | `scripts/verify/fix8a_aging_e2e.py`、`fix8a_aging_db.sh`、`fix8a_aging_page_check.mjs` |
| 8 | 前端：**零改动**（应付账龄页的表头按后端 `report.buckets` 渲染、列数按 `report.buckets.length + 4` 算 colspan，7 档自动变宽 11 列） | — |

## 关键决策（为什么这么切）

1. **一张表、两个使用者，而不是「应付再抄一份 7 档表」。** M1-19 已经把机制参数化
   （`bucket_of_in` / `bucket_case_sql_in` / `bucket_defs` / `empty_bucket_cells`），
   档位表是纯数据。本卡只把应付指向同一张表，**不存在第二份阈值**；
   `payable_and_receivable_share_one_bucket_table` 逐天比对 −400..=3650，谁再加一张表立刻红。
2. **保留 `BUCKETS` 这个名字当别名，不把 60 多个调用点全部改名。** 应付侧
   `pivot_suppliers` / `summarise` / `export_table` / `assemble` 等都用 `BUCKETS`；
   改成别名让 diff 只有「删 5 档表 + 加一行别名」，语义一处收口，回归面最小。
3. **不保留 `OVER_90`。** 若为了兼容把旧的「90 天以上」留成第 8 档，就会出现
   「91-180 + 181-365 + 365 天以上 + 90 天以上」互相重叠的格子，合计加两遍是必然事故。
   老的 5 档是**契约要改的对象**，不是要兼容的历史值（页面按表头渲染，没有写死的列）。
4. **接口契约变化写清楚**（前端/报表订阅方需要知道）：
   - `buckets` / `summary` / `suppliers[].buckets` 由 5 格 → **7 格**；
   - 分档键 `OVER_90` **消失**，新增 `D91_180` / `D181_365` / `OVER_365`；
   - 导出 xlsx 由 9 列 → **11 列**（列序 = 供应商 / 单据数 / 七档 / 未付合计 / 最长逾期天数）；
   - `overdue_unpaid`（除未到期之外各档之和）与 `overdue_ratio` 的**算法不变**，只是分档更细。
5. **迁移 0 条**：没有表结构 / 种子变化（档位表在 Rust 常量里，不在库里）。

## 验收证据（全部真跑，非描述）

### ① 真接口 78 条断言（本机 `:8023` + **空库首启** `cp_m121_api` + SQL 夹具 16 张应付）

`python3 api_check.py` → `00_api_check.out`（exit 0，**78 PASS / 0 FAIL**）。覆盖：

- **档位表**：7 个 key 顺序固定、7 个中文名逐字、`(min_days, max_days)` 逐档
  `(−2^63,0) / (1,30) / (31,60) / (61,90) / (91,180) / (181,365) / (366,null)`、
  `OVER_90` 不存在；
- **档位金额**：夹具把每一档的**上下界**都摆了一张单
  （−15 / 0 | 1 / 30 | 31 / 60 | 61 / 90 | 91 / 180 | 181 / 365 | 366 天），
  7 档合计 `1700 / 1100 / 1500 / 2800 / 2100 / 2900 / 1600`，未付合计 `13700`、
  逾期 `12000`、占比 `87.59`、单据数 13、供应商 3；
  **每档期望值由 Python 侧独立实现的分档函数算出**（不是人写死的合计），两条实现互证；
- **明细逐行**：13 张单的 `overdue_days` 与 `bucket` 全部落在应落的档（含 91 / 180 / 181 / 365 / 366 五个新边界）；
- **排除口径**：`SETTLED` / `CANCELLED` / 基准日之后的期间（2026-12）均不进表；
- **供应商透视**：丙 5600 / 甲 4400 / 乙 3700 按未付倒序，每行 7 个格子、各档之和 == 该行未付合计、
  最长逾期 365 / 366 / 181；
- **汇总 == 明细按档求和**（SQL 聚合 vs Rust 逐行分档，两条独立路径）；
- **基准日 +1**：到期当天那张推进 1-30、逾期 30 天那张推进 31-60、60 天那张推进 61-90、
  90 天那张推进 91-180，7 档合计逐档与 Python 独立实现一致；
- **极端基准日 2099-12-31**：全部金额落「365 天以上」（14400，含届时已纳入的 2026-12 那张），前六档 `0.00`；
- **导出 xlsx（真字节读回，zipfile + xml 直读 OOXML）**：表头 11 列逐字、行数 = 表头 + 3 供应商 + 合计、
  每个供应商 7 档 / 单据数 / 未付合计 / 最长逾期**逐格** == 报表 JSON、
  合计行逐档 == `report.summary`、合计最长逾期 366、`Content-Disposition` 文件名
  `应付账款账龄分析表_2026-11_截止2026-11-05.xlsx`；
- **出错路径**：非法 `as_of=2026-13-01` → `400 + 10001`；导出无 token → `401`。

夹具：`seed.sql`（自带清理可重复跑，16 行 = 13 纳入 + 3 排除）、
起服务：`run_backend.sh`（空库首启，库 `cp_m121_api`）。

### ② 后端单测 → `03_unit_aging.out`

| 模块 | 结果 |
|------|------|
| `business::erp::payable::payable_aging` | **10/10 PASS**（含新增 `export_table_has_seven_bucket_columns`、改名后的 `payable_and_receivable_share_one_bucket_table`） |
| `business::erp::receivable::receivable_aging` | **13/13 PASS**（应收侧一条没动，仍全绿 —— 证明「同一张表」没有把应收改坏） |

### ③ 门禁 → 同名 `.out`

| 门禁 | 结果 | 日志 |
|------|------|------|
| `cargo fmt --check` | exit 0 | `01_backend_fmt.out` |
| `cargo clippy --lib -- -D warnings` | exit 0，0 warning | `02_backend_clippy.out` |
| `cargo test --lib system::menu::permission_audit_tests` | 8/8 PASS | `04_perm_audit.out` |
| `pnpm typecheck` | exit 0 | `05_typecheck.out` |
| `pnpm build` | exit 0 | `06_build.out` |

> 权限码：本卡**没有新增 / 修改任何权限码**（报表接口 `business:erp:report:payable-aging`
> 与导出 `business:erp:report:export` 都沿用 V0255 / V0183 已种的那两个）。

## 诚实缺口（没做的，别当已完成）

- **迁移 0 条符合预期**，但「档位表可配置」没做：档位仍是 Rust 常量（改档位要发版）。
  要做成库里的可配置项是另一件事（会牵动应收 / 应付两张报表的契约与缓存的 `expr_snapshot` 类思路），另开卡。
- **前端没有新增 E2E 真点**：本卡前端零改动，只跑了 `pnpm typecheck` + `pnpm build`；
  应付账龄页 7 档表头的真实渲染由「按 `report.buckets` 渲染」这条既有实现保证，
  想补真点可跑老的 `scripts/verify/fix8a_aging_page_check.mjs`（需 Playwright + 隔离实例）。
- **`scripts/verify/fix8a_aging_*` 的 DB/实例脚本仍是老环境口径**（`runuser -u postgres` + `alun` 用户，
  在 99 上已不可用：本机 PG 已 purge、`alun` 已删）。本卡只把它们的**断言口径**同步到 7 档，
  没有重写建库方式（不在本卡范围）。

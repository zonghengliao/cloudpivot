# M3-21 销售预测：预测 vs 实际对比（偏差分析）— 交付证据

卡：`t_31257c83`（M3-21【M3·销售】销售预测：预测 vs 实际对比）
范围：**只做后端对比接口**（前端页面是 M3-23，单测扩展是 M3-22）。零迁移、零新权限码、零菜单行。

## 一、落地的是一件事：一个只读接口

```
GET /api/business/erp/sales-forecast/variance?period_code=2032-08[&dimension=CUSTOMER][&tolerance_pct=5]
权限：business:erp:sales-forecast:list（复用 V0318 已有码，**没有新增** sys_menu 行）
```

出参：`period_code / dimension / dimension_label / basis / tolerance_pct / summary / dimension_counts / rows`。

### 口径（决定，不是实现细节）

| 项 | 口径 | 理由 |
|---|---|---|
| 预测侧 | 只查 `v_biz_sales_forecast_current`（V0444 视图） | 同（客户, 物料, 期间）多版本时取 `version_no` 最大；查表 SUM 会把历史版本加成几倍（M3-20 已钉死这条） |
| 实际侧 | 期间内 `order_date` 命中、`status ∈ ORDER_STATUSES`（APPROVED/PARTIAL_OUT/OUTBOUND/CLOSED）的订单，**本位币**含税金额 + 明细数量 | 与达成率看板的 ORDER 口径逐字一致；外币单按 `base_total_with_tax` / 逐行比例折算，不会把 1000 USD 当 1000 CNY |
| 收款口径 | **不做** | 预测谈「能卖多少」，对标订单；收款夹着账期，会把「客户晚付钱」记成「预测不准」。收款口径在 `/sales-target/achievement` 的 `by_receipt` 里 |
| 粒度 | `CUSTOMER` / `MATERIAL` / `CUSTOMER_MATERIAL` / `ALL`，**必须选一种**（缺省 CUSTOMER） | 客户级与物料级预测的「实际」是同一批订单的两种切法，混在一页相加 = 双计，偏差率凭空变小。`dimension_counts` 回四种粒度条数供前端提示 |
| 漏预测 | 有实际、无当前版本预测的对象补一行：`has_forecast=false`、`forecast_amount=0`、`deviation_level=NO_FORECAST`、`variance_amount`=实际 | 只看「预测过的」会漏掉最贵的偏差。四种粒度都补（含 ALL：全公司没预测却有实际时给一行「全公司 · 漏预测」） |
| 偏差档位 | `OVER` / `UNDER` / `ON_TARGET`（`|差异率| ≤ tolerance_pct`，缺省 ±5%）/ `NO_FORECAST` | 容差比较走**未四舍五入**的差异率（5.004% 不会被判成命中） |
| 比率 | `variance_rate` 是百分数、2 位小数；预测为 0 ⇒ `null`（不是 0%、也不是 100%） | 分母为 0 的比率没有意义，差额已经由 `variance_amount` 给全 |
| 汇总 | `summary` 与 `rows` **同源**（只汇总本次粒度那批行）；合计偏差率 =（Σ实际−Σ预测）÷Σ预测；`mape_pct` 是各行 \|差异率\| 的算术平均（只算有预测金额的行）；`worst` 给 \|偏差金额\| 最大的那一行 | 汇总另跑一遍全库 SQL 就会出现「表里有 3 行、合计对不上」；合计看钱、MAPE 看稳 |
| 排序 | 按 \|偏差金额\| 倒序 → 实际金额倒序 → 显示名升序 | 偏差最大的在最上面，漏预测行凭全差额自然浮起 |

### 默认拒绝的写法（都是踩过/想过的坑）

- 粒度条件必须 `IS NOT NULL` / `IS NULL` 成对写：`material_id = NULL` 恒为 UNKNOWN，写成 `= NULL` 会**静默零行**。
- 客户粒度金额用订单级 `base_total_with_tax`、数量**先按单汇总明细再按客户加**：直接 `JOIN item` 会把订单金额按行数翻倍。
- query 里的 `tolerance_pct` 收 `Option<String>` 手工 parse（`rust_decimal` 的 serde 反序列化在 query string 上依赖 feature 组合，行为不唯一）。

## 二、改动文件

| 文件 | 内容 |
|---|---|
| `backend/app/src/business/erp/sales/sales_target_dto.rs` | 偏差分析契约：4 个粒度常量 + 4 个档位常量 + `normalize_dimension` / `normalize_tolerance_pct` / `variance_amount` / `variance_rate` / `deviation_level` / `mean_absolute_pct` + 6 个出参结构 |
| `backend/app/src/business/erp/sales/sales_target_model.rs` | 取数：`current_forecasts_for_period`（视图 + 维度名）、`forecast_dimension_counts`、`order_actual_by_customer` / `by_material` / `by_pair` / `total_with_qty`（金额+数量，各带名字） |
| `backend/app/src/business/erp/sales/sales_target_handler.rs` | `assemble_variance` / `summarize_variance`（纯函数）+ `forecast_variance`（编排）+ `GET /business/erp/sales-forecast/variance` 路由 + 8 条纯逻辑单测 |
| `backend/app/src/business/erp/sales/sales_variance_db_tests.rs`（新） | 7 条真库用例（当前版本、漏预测、物料明细、本位币、全公司、粒度计数、400、清场） |
| `backend/app/src/business/erp/sales/mod.rs` | 挂上 `sales_variance_db_tests` |

**零迁移**：本卡没有改表结构（`biz_sales_forecast` 的列/视图由 M3-20 的 V0444 备齐，V0444 注释里写的「M3-21 用它」兑现了）。

## 三、验证（全部真实执行，原始输出在同目录）

| 证据 | 命令 | 结果 |
|---|---|---|
| `01_http_smoke.txt` | 提交版二进制连 `cp_m321_variance` 起 :18199 → 登录 → 建预测 → 直插订单 → 查对比 → 错误码 | ALL 粒度：预测 120000 / 实际 96000 / 偏差 −20% / `UNDER`；CUSTOMER 粒度：`冒烟客户` 漏预测行 `NO_FORECAST`；`dimension=REGION`→400、`tolerance_pct=abc`→400、缺 `period_code`→400、无 token→401 |
| `02_db_tests.txt` | `ERP_DB_URL=…/cp_m321_variance cargo test --lib business::erp::sales::sales_variance -- --test-threads=1` | `7 passed; 0 failed` |
| `03_db_tests_m320_regression.txt` | 同一库跑 M3-20 的 `sales_forecast` 用例 | `11 passed; 0 failed`（本卡未破坏上游口径） |
| `04_unit_tests.txt` | `cargo test --lib business::erp::sales::sales_target_handler::tests` | `26 passed; 0 failed`（含本卡新增 8 条偏差分析纯逻辑用例） |
| `05_clippy_lib_tests.txt` | `cargo clippy -p cloudpivot --lib --tests -- -D warnings` | `Finished`，零告警 |
| `06_fmt_check.txt` | `cargo fmt -p cloudpivot --check` | 通过 |

### 真库用例怎么跑起来的（模板库滞后，记录在此免得下一个人重踩）

`cloudpivot_test` 模板库停在 V0400（`biz_sales_forecast` 还没有 `forecast_qty`/`revised_from_id`/当前版本视图），
直接 `createdb -T cloudpivot_test` 跑本卡用例会缺列。做法：

```bash
createdb -h 172.18.10.145 -U cloudpivot -T cloudpivot_test cp_m321_variance
psql -h 172.18.10.145 -U cloudpivot -d cp_m321_variance \
  -f backend/app/migrations/business/V0444__sales_forecast_qty_version.up.sql
```

HTTP 冒烟那轮额外用提交版二进制首启过一遍（`auto_migrate=true` 会把 V0401~V0445 补齐），
所以接口是对「迁移到最新」的库跑的。表结构与 V0444 自检块都通过（迁移输出见 `02`/`01` 同批日志）。

### 清场

- 用例库 `cp_m321_variance` 已 `dropdb`；测试进程已退出（`pgrep -f '^\./target/debug/cloudpivot'` 为空）。
- 前端零改动 ⇒ 未跑 `pnpm typecheck/build`（本卡范围是后端接口，页面在 M3-23）。
- 未新增迁移 / 权限码 / 菜单行 ⇒ `menu_invariants.sh` 与 `perm_matrix` 类门禁的输入没有变化。

## 四、留给后续卡的口径（M3-22 单测、M3-23 前端、M3-26 收口）

- 前端只做展示，**不要自己算偏差率/判档**：`deviation_level`、`tolerance_pct`、`dimension_counts`、`summary.worst` 都已经在出参里。
- 换粒度要重新请求（`dimension` 是服务端取的数，不是前端过滤）；`dimension_counts` 给出各粒度条数用于提示。
- `variance_rate = null` 的语义是「预测为 0，比率无意义」，不要在前端渲染成 0% 或 100%。
- 版本历史（M3-23 抽屉）查表按 `version_no` 排、链走 `revised_from_id`；当前版本一律用 `v_biz_sales_forecast_current`。

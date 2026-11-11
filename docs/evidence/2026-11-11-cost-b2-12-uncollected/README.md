# COST-B2-12 「未归集清单」接口（ARCH-9 §5.2 #33 · §6.2.2 ②）

> 卡：`t_0d4fc482`｜依赖 B2-11（已完）｜分支 `main`｜本卡**无迁移**（只读已有表）

## 1. 交付物

| 路径 | 内容 |
|---|---|
| `backend/src/business/erp/cost_uncollected_dto.rs` | 纯逻辑 + 出参：四块的 `kind`/字段、`truncated` 计算、池缺口三态判定 `pool_reason`、「怎么修」文案（设计 §6.2.2 逐条原文）、跳转拼装（含最小百分号编码）、`account_label`；14 条单测 |
| `backend/src/business/erp/cost_uncollected_model.rs` | 取数层：未挂对象领料（明细 + 汇总）、当期工时行数（`CONFIRMED`/`DRAFT` 分列）、费用池 − 已摊余额、生效规则区间、本期有活动在产对象车间、按 id 取批次；5 条 SQL 口径单测 |
| `backend/src/business/erp/cost_uncollected_handler.rs` | 装配 + 两条 HTTP 入口 + 入参归一化 + 404/400 文案；5 条单测 |
| `backend/src/business/erp/cost_uncollected_db_tests.rs` | 真库用例 7 条（`ERP_DB_URL` 开关，`CB212-*` 前缀夹具、跑完清零） |
| `backend/src/business/erp/mod.rs` | 注册 4 个模块（**本卡 hunk**：`cost_uncollected_*` 5 行 + 3 行 doc） |
| `frontend/src/api/business/erp/cost-uncollected-kinds.ts` | 前端契约类型 + 机器码常量 + 文案/配色纯函数 |
| `frontend/src/api/business/erp/cost-uncollected.ts` | `fetchUncollectedByPeriod` / `fetchUncollectedByBatch` |
| `frontend/src/views/business/erp/cost-period/steps/UncollectedPanel.vue` | 向导第 ② 步面板：四块表格 + 条数/金额/工时 + 「怎么修」+ 可点跳转 + 截断提示 + `recalc` 事件 |
| `frontend/src/views/business/erp/stock-out/index.vue` | **跨页最小改动**：`?out_no=` 深链（跳转要真能定位到那张单） |
| `frontend/src/views/business/erp/cost-labor/index.vue` | **跨页最小改动**：`?period_code=&cost_object_id=` 深链 + 对象筛选标签（可清除） |

## 2. 四块缺口（口径与来源）

| 块 | 机器码 | 判定 |
|---|---|---|
| ① 未挂成本对象的领料 | `STOCK_OUT_WITHOUT_OBJECT` | `biz_stock_out`：`status='POSTED'` + `out_type='ISSUE'` + `cost_object_id IS NULL` + `out_date` 落期间。**恰好是** `cost_collect_model::MATERIAL_SOURCE_SQL`（`IS NOT NULL`）的镜像 |
| ② 缺工时的在产对象 | `LABOR_MISSING` | 对象集 = `cost_period_item_model::objects_of_period`（与第 6 段**同一份**）∩ `status='OPEN'` − 本期有 `CONFIRMED` 工时行的对象；`DRAFT` 行数一并回（文案区分「没录」与「没确认」） |
| ③ 缺费率的工时行 | `LABOR_RATE_MISSING` | 复用 B2-7 的 `LaborLine::uncollected_reason`（`rate=0` / 金额舍入成 0），**不重写口径**；hint 走 B2-3 费率三级 `rate_dto::resolve` 给出「现在能不能取到、该配哪一级」 |
| ④ 未摊完的费用池 | `POOL_UNALLOCATED` | 池（三类来源、`cost_object_id IS NULL`）按 (车间,科目) 汇总 − 同期 `biz_cost_alloc_result` 已摊 > 0；原因三态：`NO_RULE` / `NO_OBJECT` / `NOT_ALLOCATED` |

每块回：`count`（真实条数）+ `amount_total` + `hours_total`（仅③）+ `hint`（怎么修）+ `jump`（整块跳转）+ `truncated` + `rows[]`（≤200，行内带 `jump` 与来源 id）。

## 3. 验收对照

| 卡面验收 | 证据 |
|---|---|
| 三类：缺工时 / 缺费率 / 缺成本对象的领料 | `01_unit_tests.out`、`02_real_db_tests.out::four_blocks_report_the_real_gaps`（2 张未挂对象领料 + 1 个缺工时对象 + 1 行缺费率 + 1 个池 = `gap_count 5`），`03_http.out` ①（HTTP 实际返回 3 块各 1 条：该夹具对象有 1 行「已确认但费率为 0」的工时 ⇒ 计缺费率不计缺工时） |
| 每类返回可点击跳转的来源单据 id | ① `stock_out_id` + `jump=/business/erp/stock-out?out_no=OUT-…`（含 `stock_out_gap_carries_doc_id_and_no`）；② `cost_object_id` + `jump=/business/erp/cost-labor?period_code&cost_object_id`；③ `labor_id` + 同上跳转；④ `dept_id`+`account_code` + `jump=/business/erp/cost-alloc-rule`（`NOT_ALLOCATED` 时 `null`=回本页点计算，不造死链） |
| 前端要能直接用 | `04_frontend_gates.out`（`pnpm typecheck` / `pnpm build` 均 0）+ `UncollectedPanel.vue`（`period-code` 一个入参自动加载、`@recalc` 交给向导、跳转用后端给的路由串）+ 两个页面深链已实测字段（`stock-out` 的 `keyword` 支持单号模糊、`cost-labor` 的 `cost_object_id` 是后端已有筛选参数） |

## 4. 已知偏差 / 说明

1. **接口比设计多一个入口**：设计 #33 只有 `GET /cost-period/{id}/uncollected`（批次 id），但向导 ①选期间 → ②自查缺口 → ③计算，批次是 ③ 才建的（`#27 calc`「不存在则建」）⇒ 第一次核算的期间**没有 id 可传**。故补 `GET /cost-period/uncollected?period_code=yyyy-MM`（同权限码、同装配函数、同出参），设计原文接口照实现。
2. **卡面「三类」与设计「三类」取并集**：卡面 = 缺工时 / 缺费率 / 缺成本对象领料；设计 §6.2.2 = 未归集领料 / 缺工时对象 / 未分摊池。并集四块**全返回**（缺费率在设计里属于 B2-7 的「未归集工时」子集，未单独成块）。没有改设计口径，只是不漏块。
3. **跨页改动 2 处（报备）**：`stock-out/index.vue` 加 `?out_no=` 深链、`cost-labor/index.vue` 加 `?period_code=&cost_object_id=` 深链与对象筛选标签。不加则「可点击跳转」落不到具体单据（两页原先都不读这两个 query）。均为各自页面 10-20 行的独立小改，与既有 `?ar_no=` / `?voucher_no=` / `?cost_object_id=` 深链同款写法。
4. **池缺口另写了一份池 SQL**：`cost_overhead_collect_model::POOL_SQL` 是 B2-8 的私有常量且不含「已摊合计」，本卡要的是「池 − 已摊」。改它等于改归集段出参语义（跨卡改口径），故在本卡 model 内另写一份并注释「两处口径必须同步」。
5. **无迁移**：只读 V0203 / V0371 / V0372 已有表与 V0378 已种权限码 `business:erp:cost-period:list` ⇒ 卡面「迁移幂等 + `.down.sql`」N/A；`git status backend/migrations` = 0 行（`06_residue.out`）。
6. **不阻断**：接口永远 200（只有期间非法 400 / 批次不存在 404 / 查库失败 500）；`settled=true` 时缺口降级为「历史解释」，仍原样列出。
7. **耗时**：本卡实际约 2h（超 1.5h 软限）。多出的时间在两处：① 同工作区另一卡（B3-1）的 `cost_period_dto.rs` 当时编译不过，`cargo check` 被挡约 15 分钟；② 补了真实 HTTP 冒烟（起隔离实例 8038 + 登录 + 5 条请求 + 清场），这部分是卡面「前端要能直接用」的实证，未另开卡。

## 5. 复跑方式

```bash
# 单元（含 7 条真库用例 SKIP）
cd /opt/cloudpivot/backend && cargo test --lib cost_uncollected

# 真库（演练库；夹具自清）
ERP_DB_URL='postgres://alun:***@127.0.0.1:5432/costb26_drill_1625844' \
  cargo test --lib cost_uncollected_db -- --test-threads=1

# HTTP 冒烟（任意有数据的库；profile=test 不强制 redis）
NIK_PROFILE=test NIK_SERVER_LISTEN=127.0.0.1:8038 NIK_DATABASE_NAME=<库> \
NIK_MIDDLEWARE_AUTH_JWT_SECRET=<≥32 字符> CLOUDPIVOT_STORAGE_MASTER_KEY=<64hex> X_CONFIG_SECRET_KEY=<任意> \
  ./target/debug/cloudpivot &
curl -s -H "Authorization: Bearer $TOKEN" \
  'http://127.0.0.1:8038/api/business/erp/cost-period/uncollected?period_code=2026-11'

# 前端
cd /opt/cloudpivot/frontend && pnpm typecheck && pnpm build
```

## 6. 证据文件

| 文件 | 内容 |
|---|---|
| `01_unit_tests.out` | `cargo test --lib cost_uncollected` → 28 passed / 0 failed（EXIT=0） |
| `02_real_db_tests.out` | 真库 7 passed / 0 failed（15.62s，逐条用例名） |
| `03_http.out` | 隔离实例 5 条请求：按期间 200（四块）、按批次 200（带 batch）、未知批次 404+10002、非法期间 400+10001、缺参 400+10001；启动日志「679 条 permission 规则」 |
| `04_frontend_gates.out` | `pnpm typecheck` TYPECHECK_EXIT=0 / `pnpm build` BUILD_EXIT=0 |
| `05_backend_gates.out` | `rustfmt --check` FMT_EXIT=0 / `cargo clippy --all-targets -- -D warnings` EXIT=0 |
| `06_residue.out` | 演练库 9 项前缀计数全 0 + `git status backend/migrations` = 0 行 |

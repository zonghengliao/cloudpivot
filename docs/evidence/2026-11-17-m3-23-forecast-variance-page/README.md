# M3-23【M3·销售】销售预测：前端页面（录入 + 对比图）—— 证据

- 卡：`t_2947440a`（M3-23）
- 上游：`t_31257c83`（M3-21，`GET /business/erp/sales-forecast/variance` 偏差分析接口）、`t_e33d2100`（M3-22，汇总口径单测）
- 日期：2026-11-17
- **零后端改动 / 零迁移 / 零新增权限码 / 零新增菜单行**：`git diff` 只有前端 4 个文件 + 1 个新单测 + 本证据目录。

## 一、这一卡补的是什么

页面 `/business/erp/sales-target`（「销售目标与预测」，菜单已在 `V0318`）原来只有三个页签：
达成率看板 / 目标维护 / 销售预测。M3-21 的偏差分析接口**没有界面**，只能 curl。本卡补两块：

| 补的东西 | 为什么 |
|---|---|
| ④「预测 vs 实际」页签（新组件 `ForecastVariancePanel.vue`） | 四粒度下拉 + 容差输入 + 汇总卡 + **对比图** + 明细表；M3-21 接口第一次有界面 |
| 销售预测录入弹窗补「预测数量」 | M3-20/V0444 早就加了 `forecast_qty`，后端入参也收，但**前端从来没传过**（弹窗只有金额）⇒ 数量偏差永远是 0、`variance_qty_rate` 恒 `null`，对比图的数量列等于废的。本卡把这一列接上，并加进列表列 |

图里的三条序列：**预测金额（柱）/ 实际金额（柱）/ 偏差率（右轴折线）**，配色走 `EChart.vue` 的配色引擎（页面不写死颜色，22 套配色跟着换）。

## 二、口径（照后端抄，前端不重算）

| 数 | 口径 | 出处 |
|---|---|---|
| 偏差金额 | 实际 − 预测（正 = 实际超出） | `sales_target_dto::variance_amount` |
| 偏差率 | （实际−预测）÷ 预测 × 100；**预测为 0 ⇒ `null`**，界面显示 `—` 不是 `0.00%` | `variance_rate` |
| 合计偏差率 | Σ实际 ÷ Σ预测（**不是**行率平均） | `summarize_variance` |
| MAPE | 行偏差率绝对值的平均，只算有预测金额的行 | `mean_absolute_pct` |
| 汇总卡四个数 | 全部直接取后端 `summary`，前端不加总 | M3-22 钉过「行相加 = summary」 |
| 图取几行 | 前 12 行（后端已按 |偏差金额| 倒序），**前端不重排**；表格是全量 | `CHART_ROW_LIMIT` |
| 容差 | 0~100 的百分数，最多 2 位小数；空 = 后端缺省 5 | `normalize_tolerance_pct` |

## 三、验证（全部真跑，原始输出在目录里）

### 3.1 单测（`node:test`，零新依赖）

- 新增 `frontend/tests/unit/erp-sales-forecast-variance.test.ts`：**24 条**，`pass 24 / fail 0`（`06-unit-new.txt`）
  - 纯逻辑 19 条：`null` 率不当 0、金额字符串转数字、横轴按**码点**截断（不劈开 emoji）、只切前 N 行不重排、三序列名/类型/右轴/`connectNulls=false`、空数据不抛、四档文案颜色、`—` vs `0.00%`、万级轴缩写、容差/数量尾零、汇总一句话、worst 人话、换粒度提示。
  - **源码即契约 5 条**（读后端 `.rs` 断言，前后端任一侧改名当场红）：
    ① 四粒度值与 `FORECAST_DIM_*` + `dimension_label` 逐字一致；
    ② 四档偏差值与 `DEVIATION_*` + `deviation_label` 逐字一致（含颜色约定）；
    ③ 后端确实有 `path = "/business/erp/sales-forecast/variance"`（GET）+ `permission = "business:erp:sales-forecast:list"`，前端路径与 `BASE=/api/business/erp` 一致；
    ④ `ForecastVarianceRow / Summary / Res / Worst / DimensionCount` 五个结构的字段集合与后端 DTO **双向齐全**；
    ⑤ 后端 query 只三个入参，前端不多发。
- 全量回归 `pnpm test:unit`：**2262 / 2262 pass，0 fail**（`07-unit-full.txt`；上一卡是 2238，+24 正好 = 本卡新增）。

### 3.2 编译链

| 命令 | 结果 |
|---|---|
| `pnpm typecheck`（vue-tsc） | exit 0（`08-typecheck.txt`） |
| `pnpm typecheck:unit`（含 tests/unit） | exit 0（`09-typecheck-unit.txt`） |
| `pnpm build` | exit 0，`✓ built in 49.14s`（`10-build.txt`） |

### 3.3 真后端 + 真库（145 上的空库首启，库名 `cp_m323_variance`，跑完 drop）

起实例：`./target/debug/cloudpivot` + `NIK_DATABASE_*` 指向 `172.18.10.145:5432/cp_m323_variance`（空库 → 全量迁移 + bootstrap），`/health` 200。

夹具（`11-db-fixture.txt`，SQL 见「五」）：3 客户 / 2 物料 / 3 条有效订单 / 4 条预测（其中 C1 有 v1+v2）。

真 HTTP（admin 登录后带 Bearer）：

```
GET /api/business/erp/sales-forecast/variance?period_code=2026-11&dimension=CUSTOMER&tolerance_pct=5  → 200
  summary: forecast_amount=170,000.00  actual_amount=180,000.00  variance=10,000.00
           variance_rate=5.88%  mape_pct=22.50%  (2 有预测 / 1 漏预测，超 1 低 1 中 0)
           worst: 北京华信科技 -30,000.00 / -25.00% / UNDER
  rows: 北京华信科技 120,000/90,000/-25.00% UNDER v2   ← v1 的 10 万**没有双计**
        广州立创电子 0/30,000/— NO_FORECAST                ← 漏预测行
        上海联通贸易 50,000/60,000/20.00% OVER
GET …&dimension=MATERIAL → 200（云服务器 X1 80,000/120,000 +50%；存储阵列 S2 漏预测 60,000）
GET …&dimension=CUSTOMER_MATERIAL / ALL → 200
```

原始报文：`04-curl-variance-customer.json`、`05-curl-variance-material.json`。
字段名与前端 `ForecastVariance*` 接口逐字对得上（契约单测 ④ 已从源码双向钉死）。

### 3.4 真浏览器（Chromium，vite dev :5173 代理 → 后端 :8023）

登录 admin → `/business/erp/sales-target` → 切「预测 vs 实际」：

| 截图 | 验证点 |
|---|---|
| `01-ui-variance-customer.png` | 按客户粒度：四张汇总卡（170,000 / 180,000 / 10,000 · 5.88% / MAPE 22.50%）、worst 提示条、换粒度提示条、**预测柱+实际柱+偏差率折线**、明细表（漏预测行偏差率显示 `—`，数量列 `0 / 30 （30）`，档位标签「漏预测」） |
| `02-ui-variance-material.png` | 下拉切「按物料」→ 自动重查：预测合计 80,000、合计偏差率 **125.00%**（(180,000−80,000)÷80,000）而 MAPE 50.00% —— 两个率在界面上同时可见且不互相冒充；提示条变成「按客户 2 条」 |
| `03-ui-forecast-create-qty.png` | 录入弹窗多了「预测数量」，列表也多了「预测数量」列（120/100/50/80 与夹具一致） |

交互实测（同一次会话里点的）：

1. 容差填 `150` 点「对比」→ 前端拦下「容差必须在 0 ~ 100 之间（收到 150；这是百分数，5 = ±5%）」，**上一次的结果不消失**（只报错不清屏）。
2. 录入：客户=广州立创电子 / 金额 30,000 / **数量 30** → 创建成功，列表出现 `SFC-20261117-000001 … 30,000.00 30 v1`（说明数量真的落库了，`11-db-fixture.txt` 里这条 `forecast_qty=30.0000`）。
3. 再把这条「删除」→ 回到「预测 vs 实际」，广州立创电子重新变成 `漏预测` 行 —— 软删的当前版本不再进对比（M3-20 视图口径在界面侧也对）。

## 四、改了哪些文件

| 文件 | 改了什么 |
|---|---|
| `frontend/src/api/business/erp/sales-target.ts` | 新增 `ForecastVarianceQuery/Res/Summary/Row/Worst/DimensionCount` + `fetchForecastVariance`（路径 `/api/business/erp/sales-forecast/variance`，GET）；头注释从「三块接口」补成四块 |
| `frontend/src/views/business/erp/sales-target/forecast-variance-logic.ts`（新） | 纯逻辑：`toNumber/moneyOf/moneyText/rateText/qtyText/shortName/pickChartRows/buildVarianceChartOption/deviationMeta/summaryHeadline/worstText/dimensionHint/toleranceText/axisMoneyText/varianceTone` + `DIMENSION_OPTIONS` |
| `frontend/src/views/business/erp/sales-target/ForecastVariancePanel.vue`（新） | ④ 页签本体：筛选（期间/粒度/容差）、汇总卡、worst/换粒度提示、`EChart` 对比图、明细表、空状态 |
| `frontend/src/views/business/erp/sales-target/index.vue` | 加第 4 个页签（`v-if` 懒挂载，首次点开才请求）；录入弹窗与列表补「预测数量」；头注释三页签→四页签 |
| `frontend/tests/unit/erp-sales-forecast-variance.test.ts`（新） | 24 条（19 纯逻辑 + 5 源码即契约） |

## 五、可复现的最小步骤（在 99 上）

```bash
HOST=172.18.10.145
PGPASSWORD=*** createdb -h $HOST -U cloudpivot cp_m323_variance   # 空库，交给服务自己迁移
cd backend && NIK_DATABASE_HOST=$HOST NIK_DATABASE_NAME=cp_m323_variance \
  NIK_ADMIN_PASSWORD='Admin@123456' NIK_MIDDLEWARE_AUTH_JWT_SECRET=... \
  CLOUDPIVOT_STORAGE_MASTER_KEY=... X_CONFIG_SECRET_KEY=... ./target/debug/cloudpivot
# 夹具：3 客户 / 2 物料 / 3 张 APPROVED 订单（含明细）/ 4 条预测（C1 有 v1+v2）
psql -h $HOST -U cloudpivot -d cp_m323_variance -f docs/evidence/…/fixture.sql
# 页面：cd frontend && pnpm dev --port 5173 ，浏览器 127.0.0.1:5173/business/erp/sales-target
```

> 演练库 `cp_m323_variance` 验证完已 `dropdb`；夹具 SQL 见本目录 `12-fixture.sql`（与真机跑的那份逐字一致）。

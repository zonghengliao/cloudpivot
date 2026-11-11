# COST-B4-1 成本计算单导出 xlsx（后端生成 + 前端下载）

卡：`t_fa26b1c7` · 批次 B4（收口）· 依据 `docs/architecture/ARCH-9-成本核算升级设计.md`
§5.2 #32（`GET /business/erp/cost-period/{id}/cost-sheet?format=xlsx`）/ §5.4「导出：成本计算单 xlsx」/ §4.6（口径）。

## 一句话

页面步③ 看到的成本计算单，一键导出同一张表的 xlsx（主表 20 列 + 「分摊明细」第二 sheet），
**数字与页面逐格一致**（同一份数据源 `cost_period_handler::detail_of`，导出侧只搬运不重算）；
1000 行明细真库实测 **0.475s**。

## 交付物

| 文件 | 作用 |
|---|---|
| `backend/src/business/erp/cost_period_sheet.rs`（新，770 行） | 导出主逻辑：20 列表头常量 / 数量·金额·单位成本·百分比格式化（与页面 `qtyText`/`unitText`/`report::money_text` 同口径）/ 主表拼装（表头信息行 + N 行计算单 + 合计行）/ 分摊明细 sheet / `build_workbook` / `file_name` / `normalize_format` / HTTP 入口 + 12 条单测 |
| `backend/src/business/erp/cost_period_sheet_model.rs`（新，133 行） | 「分摊」列与「分摊明细」sheet 的取数（`biz_cost_alloc_result` 按期间一次拿全，含 `biz_cost_alloc_rule`/`biz_cost_object` 两次 LEFT JOIN；全序 3 段）+ 2 条单测 |
| `backend/src/business/erp/cost_period_handler.rs`（+4/−1） | `detail_of` 提为 `pub(crate)`（导出与详情**共用同一个组装函数**）；接口清单第 32 条从 ⛔ 改 ✅ |
| `backend/src/business/erp/mod.rs`（+4） | 注册两个新模块 |
| `frontend/src/api/business/erp/cost-period.ts`（+57） | `exportCostSheet(id)`：`responseType: 'blob'` + 60s 超时；`blobError()` 把 4xx/5xx 的 Blob 错误体读回原文（否则拦截器只能报「网络异常」） |
| `frontend/src/views/business/erp/cost-period/cost-period-logic.ts`（+13） | `costSheetFileName(batchNo)`（与后端 `file_name` 同形） |
| `frontend/src/views/business/erp/cost-period/index.vue`（+42） | 抽屉头部「导出 xlsx」按钮（权限 `cost-period:list`、无批次禁用、导出中 loading、成功/失败 MessagePlugin） |
| `frontend/tests/unit/erp-cost-period.test.ts`（+9） | 文件名同形单测（含空/null/首尾空格） |
| `docs/evidence/2026-11-11-cost-b4-1-cost-sheet-export/` | 本目录：起停脚本（`00_up.sh`）/ 1000 行造数（`01_seed.sql`）/ e2e（`01_e2e.sh`）/ 逐格校验（`03_verify_xlsx.py`）/ 2 对象小样本（`01b_seed_small.sql`）/ 浏览器下载件校验（`08_verify_download.py`）/ 实测输出与样本件 |

**无迁移**（表结构不动）、**无新权限码**（复用 `business:erp:cost-period:list`，设计 §5.2 #32 明确「能看就能导」）。

## 验收项对照

| 卡面验收 | 证据 |
|---|---|
| 导出件含 成本对象 / 料 / 工 / 费 / 分摊 / 完工 / 在制 | 主表 20 列：`成本对象号` / `本期材料投入` / `本期人工投入` / `本期制造费用` / `其中：分摊转入` / `完工数量` + `完工转出成本` / `期末在制数量` + `期末在制完工进度(%)` + `期末在制成本`，另有 `约当产量（材料/转换成本）`、三项单位成本、`完工入库单价`。**分摊明细** 单独一个 sheet（对象 × 车间 × 规则 × 科目 × 基数 × 待摊总额 × 分摊金额 × 尾差）。列覆盖由单测 `headers_cover_all_required_dimensions` 与 `03_verify_xlsx.py` 逐字比对双重兜底 |
| 数字与页面一致（同一份数据源） | ① 导出走 `cost_period_handler::detail_of`（页面 `GET /cost-period/{id}` 的同一个函数）；`03_verify_xlsx.py` 把 **1000 行 × 20 列逐格**与详情接口 JSON 比对，0 处不一致；合计行 = `items_summary`（页面表尾现算值），并写进守恒结论 `投入 X = 完工转出 Y + 期末在制 Z ✓`。② **真浏览器实测：页面步③ 的 2 对象小样本 → 点「导出 xlsx」→ 浏览器真实落盘的文件**（`10_page_step3_calc_sheet.png` / `11_page_export_toast.png` / `12_browser_download.xlsx`），`08_verify_download.py` 逐格比对同一时刻的详情接口返回（`09_verify_download.out`，1 行 OK） |
| 大对象（1000 行明细）不超时 | 1000 对象 / 1000 计算单 / 1000 分摊行真库实测：详情 0.104s、**导出 0.475s / 101,506 字节**（阈值 5s，脚本硬断言）；单测 `thousand_rows_sheet_builds_fast_and_complete` 另兜住「退化成逐行查库 / O(n²)」的结构性回归。全程 5 次库读（详情 4：单头 / 计算单 / 现算合计 / 状态机边 + 分摊 1）+ 一次 O(n) 内存拼装，无 N+1 |
| 后端改动带 `#[cfg(test)] mod tests` | `cargo test --lib cost_period_sheet` → **12 passed / 0 failed**（`02_unit_backend.log`） |
| 前端 `pnpm typecheck && pnpm build` | `06_typecheck.out` exit=0、`07_build.out` exit=0（`pnpm build` 内含 typecheck）；`01_unit_frontend.out` 本文件 21 条单测全过 |
| `git commit` + `push origin main` | 见卡内汇报的 commit 哈希 |

## 真库实测（`bash 01_e2e.sh` → `02_e2e.out`，0 条失败）

隔离实例 `costb41_ui` / `:18051`（`00_up.sh` 起停，空库自动迁移 V0370~V0384）：

1. 登录（空库首登改密）→ 拿到令牌；
2. 灌 1000 个成本对象 / 1000 行计算单 / 1 条分摊规则 / 1000 行分摊结果（`01_seed.sql`，口径照 §4.7.5 算例 A：料 19000 + 工 4000 + 费 4000 = 27000，完工 22311.1104 + 在制 4688.8896，满足 `ck_biz_cost_item_cons`）；
3. `GET /cost-period/{id}` → 1000 行、`items_summary.total_cost = 27000000.0000`（1000 × 27000，**现算**），耗时 0.104s；
4. `GET /cost-period/{id}/cost-sheet?format=xlsx` → **200**，0.475s，101,506 字节，`Content-Type: application/vnd.openxmlformats-officedocument.spreadsheetml.sheet`，`Content-Disposition` 带 RFC 5987 中文名 `成本计算单_CS-202609-0001.xlsx`，落盘 `PK` 魔数；
5. 边界：`format=csv` → **400**（报文点名只支持 xlsx）/ 批次不存在 → **404** / `id` 非 UUID → **400** / 不带令牌 → **401**；
6. `03_verify_xlsx.py` 逐格校验：sheet `['成本计算单', '分摊明细']`、主表 20 列表头逐字一致、1000 行逐格等于详情、逐行守恒、合计行 = `items_summary` 且打勾、分摊明细 1000 行。

样本件：`04_sample_cost_sheet_1000rows.xlsx`（实测导出原件）、`05_sample_detail.json`（同一时刻的详情接口返回，
两者可用 `python3 03_verify_xlsx.py 04_... 05_...` 离线复跑）。

## 口径与设计取舍（写清，避免二开踩坑）

1. **分摊列不是计算单上的列**。`biz_cost_period_item` 没有分摊字段，摊进来的钱已经以 `source_type='ALLOC'` 落进 `biz_cost_collect.element='OVERHEAD'` 并汇总进 `oh_cur`。所以「其中：分摊转入」是导出时对 `biz_cost_alloc_result` 的**只读聚合**（按 `(period_code, cost_object_id)` 求和），只解释构成，不参与计算、不写回 —— 加了这一列就等于同一笔钱两处真相。
2. **金额/数量写成格式化文本而不是 Excel 数值**：与全仓其余导出（报表 / 账龄 / 现金流量）一致，也与页面显示逐字一致（验收要求「数字与页面一致」）；`NUMERIC(18,4)` 落成 Excel 浮点会丢精度（导出件比账面少一分钱是事故）。
3. **没有分摊结果时不写空 sheet**：只有表头的 sheet 会被读成「本期一分钱没摊」，而真实情况通常是「还没做过分摊」——两件事。
4. **`format` 只认 xlsx**：缺省即 xlsx，其它值 400 而不是静默降级（静默给 xlsx 会让调用方以为拿到了 csv）。
5. **`biz_cost_alloc_result` 没有 `deleted_at` / `status` 列**（V0372 文件头：分摊结果是计算产物，重算=按期间整删重灌），SQL 里写任何软删过滤都会运行期 500 —— 单测 `alloc_sql_does_not_filter_missing_soft_delete_column` 专门钉住这条回归。
6. 导出按**批次 id** 拉整张计算单（不受分页限制），没有前端筛选参数：页面看到什么就导出什么。

## §8.3 已知风险与本卡的关系（导出件不能放大风险）

| 风险 | 导出件的处理 |
|---|---|
| R-4 完工进度拍脑袋（`completion_pct` 手工填） | 导出列 `期末在制完工进度(%)` 原样带出手工值，不做任何「平滑/推测」；配合 `期末在制数量` + `期末在制成本` 一起看，人能自己判断进度是否合理 |
| R-5 批成本 ≠ 库存计价单价（移动加权） | 导出的 `完工入库单价` 是**制造口径**（完工转出成本 ÷ 完工数量 = 27.888888），不是 `biz_inventory.avg_cost`；本模块头写明了「导出只搬运不重算」，不会把两个口径混成一个 |
| R-9 大期间重算/导出锁表 | 导出是**纯读**（5 条 SELECT，无写、无锁 `biz_cost_collect`），且不受分页限制；1000 行实测 0.475s。风险本身在重算卡，不在导出 |
| R-1 人工来源为手工/未录 | 导出件不带 `labor_source`（批次级信息，页面步② 与详情头已有），避免在逐对象表里塞一个「每行都一样」的字段误导读者 |

## 复现

```bash
cd /opt/cloudpivot/backend && cargo build           # 产出 target/debug/cloudpivot
cd docs/evidence/2026-11-11-cost-b4-1-cost-sheet-export
bash 00_up.sh up                                   # :18051，库 costb41_ui（空库自动迁移）
bash 01_e2e.sh                                     # 造数(1000) → 详情/导出 → 边界 → 逐格比对（0 失败）
python3 03_verify_xlsx.py 04_sample_cost_sheet_1000rows.xlsx 05_sample_detail.json   # 离线复跑逐格校验

# 前端下载链路（真浏览器，见 10/11/12 三份证据）
PGPASSWORD=alun psql -h 127.0.0.1 -U alun -d costb41_ui -f 01b_seed_small.sql   # 2 对象小样本 CS-202608-0001
cd /opt/cloudpivot/frontend && BACKEND_ORIGIN=http://127.0.0.1:18051 pnpm dev --port 5201 --strictPort
# 浏览器加令牌（接口登录后写 localStorage 的 x_access_token / x_refresh_token）
#   → /business/erp/cost-period?batch_no=CS-202608-0001 → 步③ 头部「导出 xlsx」
#   → 落盘 成本计算单_CS-202608-0001.xlsx + toast「已导出 …」
python3 08_verify_download.py 12_browser_download.xlsx 13_detail_small.json     # 浏览器下载件逐格校验
bash 00_up.sh down                                 # 停实例 + dropdb
```

后端单测：`cd backend && cargo test --lib cost_period_sheet`（12 条）
前端门槛：`cd frontend && pnpm typecheck && pnpm build`；单测 `tsx --test tests/unit/erp-cost-period.test.ts`（21 条）

## 前端入口

成本核算页（`/business/erp/cost-period`）打开某批次的向导抽屉 → 抽屉头部「导出 xlsx」按钮。
权限与「看计算单」同一个码 `business:erp:cost-period:list`；没有批次时按钮禁用；失败时把服务端报文原文弹出来
（`blobError` 专门解 `responseType: 'blob'` 下的 JSON 错误体，403/404 不会退化成「网络异常」）。

## 遗留 / 范围外

- 「分摊明细」sheet 只在该期间**有**分摊结果时出现（B2 分摊卡的产出）；本卡不造分摊数据、不改分摊口径。
- 计算单本身由 calc 编排产出的字段沿用 B3 口径；本卡不重算任何数字。
- 未做导出行数上限：1000 行实测 0.475s，若未来单期对象数达到万级，需要改为流式写（另开卡评估）。

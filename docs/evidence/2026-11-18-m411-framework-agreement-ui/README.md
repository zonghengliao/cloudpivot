# M4-11 框架协议前端页面 —— 证据包（2026-11-18）

卡：`t_da2f0a9b`（M4-11【M4·采购】框架协议：前端页面 —— 协议台账 + 执行量）

## 一句话

新增 `/business/erp/framework-agreement` 页面（两个 Tab：**协议台账** / **执行量**），
只用 M4-07/08/09 已有的 **2 条只读接口**；顺带修掉演练当场撞出来的**后端 500**
（`v_framework_agreement_execution` 在「协议没有物料行」时除零，`V0453`）。

## 文件清单

| # | 文件 | 是什么 |
|---|------|--------|
| 01 | `01_drill_full.out` | 一键演练**原始输出**：62 条 PASS（9 环境/夹具 + 52 接口 + 1 Playwright 汇总），`RESULT: PASS=62 FAIL=0` |
| 02 | `01-ledger-list.png` | 协议台账 Tab（6 份夹具协议 / 只读页脚） |
| 03 | `02-execution-list.png` | 执行量 Tab（剩余「超 5.0000」标红 / 不限量写「不限」） |
| 04 | `03-execution-drill.png` | 超限协议下钻抽屉（KPI + 逐行 + 来源单据含作废单） |
| 05 | `04-unlimited-drill.png` | 不限量协议抽屉（剩余 / 执行率都是「不限」） |
| 06 | `10_playwright.out` | Playwright 7 条用例原始输出（`7 passed`） |
| 07 | `05_frontend_typecheck.out` | `pnpm typecheck`（`EXIT=0`） |
| 08 | `06_frontend_build.out` | `pnpm build`（typecheck + vite build，`EXIT=0`） |
| 09 | `07_backend_fmt_clippy.out` | `cargo fmt --check` + `cargo clippy --lib -- -D warnings`（`EXIT=0`，零 warning） |
| 10 | `20_teeth_view_fix.out` | **反向验证**：视图退回 V0452 → 不带筛选总览 500；应用 V0453 → 200 |

## 演练环境（可复跑）

```bash
# 99 开发机（145 上的 PG）；145 上跑加 DBHOST=127.0.0.1
bash scripts/verify/m411_framework_agreement_ui_drill.sh
SKIP_BROWSER=1 bash scripts/verify/m411_framework_agreement_ui_drill.sh   # 只跑接口段（59 条 PASS：9 环境/夹具 + 50 接口）
```

* 临时库 `cp_m411_e2e`（空库首启，自己跑完 **385** 条迁移含 V0450~V0453），脚本收尾 `stop_all` 停进程；
  库名不含 `drill/e2e/test` 时脚本直接拒绝造数。
* 后端 `profile=test` @ `127.0.0.1:18102`，前端 vite dev @ `127.0.0.1:5191`（`target-guard` 硬拒 prod）。
* **协议没有 CRUD 接口**，六份协议只能 SQL 造 —— 这本身就是本卡的口径（见下「为什么没有新建/编辑」）。

## 夹具（6 份协议 / 4 条采购单）

| 协议 | 状态 | 明细上限 | 采购单 | 期望 |
|------|------|----------|--------|------|
| M411 超限协议 | APPROVED · 生效中 | A 100 / B 50 | 60 + 50 + 45 + 作废 30 | 已用 155 / 封顶 150 → **剩余 −5**、OVER、释放 1 张 |
| M411 部分执行协议 | APPROVED · 生效中 | 单头 50、明细不限 | 真接口 20 + 5（5 作废） | 剩余 30、40.00%、PARTIAL |
| M411 不限量协议 | APPROVED · 生效中 | 明细 `max_qty` NULL | 真接口 20 | 剩余 / 执行率 **null（不限）**、UNLIMITED |
| M411 拟稿协议 | DRAFT | 30 | — | 有效期状态 DRAFT（日期区间对也不「生效中」） |
| M411 过期协议 | APPROVED · 2020 到期 | 30 | — | EXPIRED、「已过期 2146 天」 |
| M411 无物料协议 | APPROVED · 生效中 | 无明细 | — | 详情 **200 + items=[]**、`cap_qty` null |

## 演练抓到的真问题（本卡修掉）

`GET /business/erp/framework-agreement/execution/page`（**不带任何筛选**）在库里存在
「一行明细都没有的协议」时 **500**：`division by zero [22012]`。

* 根因：`V0452` 的 `item_agg` 是 `GROUP BY` 聚合，零明细的协议没有这一组 → `i.item_max_qty_sum_raw`
  是 NULL → `COALESCE(...,0)` 把「没有明细」变成 **上限合计 0** → `cap_qty = 0` → `0/0`。
  即使不报错，`executed(0) >= cap(0)` 也会让空协议显示「已用满」。
* 为什么容易被漏掉：带 `over_only` / `validity_state` / `keyword` 的查询恰好把那一行排掉，
  所以「用关键字测过」的走查是绿的（`20_teeth_view_fix.out` 第 2 行就是这条反证）。
* 修法：`V0453` 重定义单头视图 —— 明细合计分支加 `item_count > 0` 前置（与后端纯函数
  `framework_agreement_execution_dto::summary_of` 逐字一致），两个比率走 `NULLIF(…,0)`。
  迁移末尾 DO 块自带探针：造一份零明细协议 → 断言 `cap_qty/rate/remaining` 都是 NULL、
  `exec_state = UNLIMITED` → 删掉，不留残留行。
* 反证：`20_teeth_view_fix.out` —— 退回 V0452 定义 → 500；应用 V0453 → 200。

## 为什么没有「新建 / 编辑 / 提交」按钮

框架协议域至今只有：M4-07 表结构、M4-08 下单引用校验、M4-09 两个只读统计接口。
**没有** CRUD handler，库里也**没有** `…:create/update/delete` 权限码（`V0452` 只种了
`…:page` + `…:execution`），`docs/planning/20261113-新卡清单.md` 的 M4 段也没有这张卡。
前端照「应该有的接口」画按钮 = 用户点了必然 404（AGENTS 记录的已知坑）。
所以页面只画读写得到的入口，并在台账页脚写明「只读台账」；补齐维护链路另开卡。

## 页面渲染口径（每条都有前端单测钉住，`tests/unit/erp-framework-agreement.test.ts`）

1. `max_qty / total_max_qty / cap_qty / cap_amount / remaining_* / exec_qty_rate*` 为 **null = 不限**
   → 显示「不限」（写成 0 会被读成「一点都不剩」）。
2. `remaining_qty` **负数** → 「超 X」，标红、**不截断**；进度条封顶 100，倍数写在百分比里。
3. `exec_state` 五态由后端给，**不用 pct 反推**；`UNLIMITED` 用 primary 色与「未开工」区分。
4. `booked_drift ≠ 0` = 回写快照与单据链分叉 → 已用量下面一行**灰字**提示，不弹错。
5. 金额按**字符串**舍 2 位（不经浮点，亿元级不掉尾数）；数量原样显示（审计要对得上库里那几位）。
6. 作废 / 软删的采购单也在下钻里（`counted=false` + 原因），不染红（作废是正常业务）。
7. 空物料范围的协议是 **200 + `items=[]`**，抽屉给空态文案，不是错误。

## 自检结果

| 门 | 结果 | 证据 |
|----|------|------|
| 一键演练（52 条接口断言 + 7 条 Playwright 用例） | **PASS=62 / FAIL=0**，rc=0 | `01_drill_full.out` |
| 前端单测 | **2322 passed / 0 failed**（本卡新增 32 条） | `pnpm test:unit` |
| `pnpm typecheck` | EXIT=0 | `05_frontend_typecheck.out` |
| `pnpm build` | EXIT=0 | `06_frontend_build.out` |
| `cargo fmt --check` / `clippy --lib -D warnings` | EXIT=0，零 warning | `07_backend_fmt_clippy.out` |
| 反向验证（视图退回老定义） | 500 → 修复后 200 | `20_teeth_view_fix.out` |

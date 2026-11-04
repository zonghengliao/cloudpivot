# P2-4 库位做完整 —— 证据（2026-11-04）

> 卡：`t_72b6a4d4`【P2-4】库位做完整（收货永远进「未分配」桶，库位管理等于没有）
> 依据：`docs/architecture/ARCH-3-模块勾稽关系审计.md` §5 **DC-22**
> 口径文档：`docs/ERP-库位策略.md`
> 迁移：`backend/migrations/business/V0218__erp_p2_4_stock_in_item_location_dict.{up,down}.sql`

## 1. 改了什么（一句话）

收货明细新增「用户选的库位」`biz_stock_in_item.location_dict_id`（`biz_location.id`）：
建单只存意图 → 过账按「(物料, 仓库, 库位)」find-or-create 库位账行并回写 `location_id`。
不指定 → 用仓库默认库位（`is_default = 1`）→ 仍没有则进「未分配」桶 + 详情提示。
出库没指定库位 → **FIFO**（最早建账的库位先出），指定 → 精确扣该桶。容量做**预警不拦截**。

## 2. 真机（隔离库 `cloudpivot_p24e2e` + 真 HTTP + 真页面）

```bash
D1_DB=cloudpivot_p24e2e RECREATE=1 bash scripts/verify/d1_dbsetup.sh
D1_DB=cloudpivot_p24e2e bash scripts/verify/d1_migrate.sh 18097     # 迁移到 V0218
P24_DB=cloudpivot_p24e2e P24_PORT=18096 bash scripts/verify/p24_location_e2e_up.sh up
python3 scripts/verify/p24_location_e2e.py --db cloudpivot_p24e2e --port 18096
```

原始输出：[`e2e-output.txt`](e2e-output.txt)（**33/33 PASS**，末尾附 `erp_invariants.py`：**40/40 通过 · BLOCKER 0 · WARN 0**）

逐条对卡面 §四「真机」验收：

| 卡面验收 | 脚本断言 | 结果 |
|---|---|---|
| 建收货单指定库位 A → 过账 → A 有数量、「未分配」没变 | A1–A10 | PASS（A=200，未分配桶 **0 行**） |
| 不指定库位 → 自动用默认库位 | B1–B5 | PASS（建单回读 `location_dict_id = 默认库位`，过账落到它） |
| 仓库没有默认库位 → 进「未分配」并给出提示 | C1–C5 | PASS（`location_warnings` 含「未分配」，数量 50 进 NULL 桶） |
| 跨仓库库位被拒 | D1–D2 | PASS（400 + 点名两个仓库） |
| 出库库位扣减正确（FIFO / 指定） | E1–E9 | PASS（FIFO 扣 A 150、B 100 不动、明细回写选中行；指定 B 后 B=60/A=150） |

## 3. 页面证据（真页面 · 真接口 · Chromium）

```bash
BACKEND_ORIGIN=http://127.0.0.1:18096 pnpm dev        # 前端 :5173
PLAYWRIGHT_MODULE=… node scripts/verify/p24_shots.mjs http://127.0.0.1:5173 prod-ish
```

| 截图 | 看点 |
|---|---|
| `shots/01-stockin-form-location.png` | 收货表单新增「库位」列；仓库选定后**自动带出默认库位**（`P24-D-01 默认收货位（默认）`）；头部有「批量设库位 → 应用到全部行」 |
| `shots/02-stockin-detail-location.png` | 详情抽屉明细新增「库位」列（编码译名） |
| `shots/02b-stockin-detail-unassigned-warning.png` | 未指定库位的单据：明细显示「未分配」+ 顶部「库位提示（1 条）」告警条 |
| `shots/03-location-default.png` | 库位管理：「默认库位」列可见（`P24-D-01 默认收货位`） |
| `shots/04-inventory-location-ledger.png` | 库位账（库位库存）：`P24-A-01 = 150`、`P24-B-01 = 60`、`P24-D-01 = 100`、`未分配库位 = 50` —— 四个桶各自独立 |

断言汇总见 `shots/report.json`：`form_location_text = "P24-D-01 默认收货位（默认）"`、
`batch_control = 1`、`detail_warnings = "库位提示（1 条）"`、`console_errors = []`、`failed_requests = []`。

## 4. 单测（真库，`ERP_DB_URL` 打开）

```bash
ERP_DB_URL=postgres://alun:***@127.0.0.1:5432/cloudpivot_p24 cargo test --lib business::erp::stockin
```

- 模块全量：**143 passed / 0 failed**（439s，含本卡新增 10 条 + 既有 location/cancel/post 用例）
- 新增 10 条（`stockin_handler_tests_location_dict.rs`）：
  指定库位落账 / 默认库位兜底 / 无默认进未分配 + 提示 / 跨仓库被拒 / 停用+已删被拒 /
  改单换库位 / 两口径冲突被拒 / 容量预警不拦截 / 出库 FIFO / 出库指定库位
- 出库模块：`business::erp::stockout` **7 passed / 0 failed**

## 5. 不变式

`erp_invariants.py`（隔离库）：**40/40 通过 · BLOCKER 0 · WARN 0**
（卡面写 32/32；套件此后新增了盘点 / 审批 / 现金流量等检查，现为 40 条，口径不变——全绿）

## 6. 已知的跨模块测试串扰（不是本卡引入）

整进程跑 `cargo test --lib`（默认多线程）时，`cash_flow::r3_real_config_covers_all_accounts`
会被**别的模块的夹具**（`P2D1A` / `P2D1B` 两个科目行）串到，报「分类配置漏科目」；
单独跑 `cargo test --lib business::erp::cash_flow` 是 **20 passed / 0 failed**。
本卡不写 `biz_account`，与该断言无关；`voucher.rs` 里另一张卡已把 voucher / cash_flow 的
`DB_LOCK` 合成一把（未提交改动），本卡未触碰。

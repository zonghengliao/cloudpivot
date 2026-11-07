# P0-03 库存链端到端演练（145 测试机 · cloudpivot_dev）

> 目标：证明 **任何时刻，库存表的数字 = 流水的累计**；并覆盖期初 / 入库 / 调拨 / 盘点（盘盈+盘亏）/ 出库。
> 环境：145 `http://172.18.10.145`（nginx 80 → 后端 8033），库 `cloudpivot_dev`，账号 `admin`（密码取 `/etc/cloudpivot/cloudpivot-dev.env`）。
> 审批账号：`demo_lead`（DEPT_LEADER + WAREHOUSE_MANAGER，制单人≠审批人）。
> 结论：**8/8 步通过**；对账 0 差异；**发现并修复 1 个 P1（分库位库存永久漂移）**。

## 演练结果

### 链：期初 → 入库 → 调拨 → 盘点（盘盈/盘亏）→ 出库 → 全量对账

| 步骤 | 操作 | HTTP | DB 状态 | 结论 |
|---|---|---|---|---|
| ① | 建仓库 A/B：`POST /api/business/erp/warehouse`（WH-DVA / WH-DVB，level 1） | 200 | `biz_warehouse` 两行 `01a1b5d9-5e84-…` / `01a1b5d9-5e9c-…` | ✅ |
| ① | 建库位：`POST /api/business/erp/location`（LOC-DVA-01 / LOC-DVB-01，`is_default=true`） | 200 | `biz_location` 两行 | ✅ |
| ① | 建演练物料：`POST /api/business/erp/material`（DEMO-INV01 / DEMO-INV02，MOVING_AVG） | 200 | `biz_material` 两行 | ✅ |
| ② | 期初：`POST /api/business/erp/opening-balance/inventory`（2026-10，100 @10） | 200 | `biz_opening_inventory` 1 行；`biz_inventory` qty=100（**未分配桶**）；流水 `IN/OPENING` 100；凭证 `PZ-20261107-000010` 1000/1000 | ✅ |
| ③ | 直接入库：`POST /api/business/erp/stock-in`（`is_sourceless=true`，50 @12）→ `POST /{id}/submit-post` | 200/200 | `IN-20261107-000003` **POSTED**（金额 600 < 1000 → 免审自动过账）；库存 +50（**库位桶** LOC-DVA-01）；流水 `IN/PURCHASE_IN`；凭证 `PZ-20261107-000011` 600/600 | ✅ |
| ④ | 调拨：`POST /api/business/erp/transfer`（A→B，40 件）→ `PATCH /{id}/status PENDING` → `POST /api/business/flow/task/{tid}/approve`（demo_lead）→ `PATCH … IN_TRANSIT` → `PATCH … DONE` | 200×5 | `TK-20261107-000001` **DONE** 40/400；`biz_stock_transfer` + item（shipped/received=40、unit_cost=10、amount=400）；流水 `OUT/TRANSFER_OUT` + `IN/TRANSFER_IN` 各 40；**A 110、B 40、总量 150 不变** | ✅（同时发现 P1 bug） |
| ⑤ | 盘点盘盈：`POST /api/business/erp/stock-take`（FULL）→ `PUT /{id}`（实盘 65）→ `PATCH … PENDING_POST` → 审批 → `PATCH … POSTED` | 200×5 | `ST-20261107-000001` **POSTED**：账面 110 / 实盘 115 / 差异 **+5** / 金额 50；流水 `IN/STOCK_TAKE_GAIN`（60→65）；凭证 `PZ-20261107-000012` 50/50 | ✅ |
| ⑤ | 盘点盘亏：同流程（实盘 62） | 200×5 | `ST-20261107-000002` **POSTED**：账面 115 / 实盘 112 / 差异 **−3** / 金额 30；流水 `OUT/STOCK_TAKE_LOSS`（65→62）；凭证 `PZ-20261107-000013` 30/30 | ✅ |
| ⑥ | 出库：`POST /api/business/erp/stock-out`（ISSUE 20）→ `PATCH /{id}/status {"status":"POSTED"}` | 200/200 | `OUT-20261107-000004` **POSTED**，20 件 / 200 元；流水 `OUT/ISSUE_OUT`（62→42）；凭证 `PZ-20261107-000014` 200/200（出库**无审批流**，直接过账） | ✅ |
| ⑦ | 全量对账（见下） | — | 库存 714 = 流水 714，**差异 0** | ✅ |
| ⑧ | 库位库存一致性（见下） | — | 逐桶 0 差异 | ✅ |

### 关键口径（与卡面不同的地方，都以实测为准）

1. **`biz_inventory` 是「桶」表**：主键唯一键是 `(material_id, warehouse_id, COALESCE(location_id))`。
   期初落「未分配桶」（`location_id IS NULL`）、收货落「默认库位桶」（`location_id = 库位库存行 id`），
   所以同一物料同一仓**天然会有两行**。卡面 ⑦ 的 SQL 直接在 `biz_inventory` 上
   `GROUP BY material_id, warehouse_id` 再 JOIN 流水，会把每个桶都乘上整仓流水 → 假差异。
2. **流水表没有库位列**：`biz_inventory_txn` 只有 `(material_id, warehouse_id)`，
   且方向列叫 **`txn_type`（IN/OUT）**，不是 `direction`；`qty` 恒正（`ck_biz_inv_txn_qty: qty > 0`）。
   ⇒ 「库存表 = 流水累计」只能在 **(物料, 仓库)** 粒度验证；逐桶只能对「库位账 vs 成本账」。
3. 盘点差异流水的 `biz_type` 是 **`STOCK_TAKE_GAIN` / `STOCK_TAKE_LOSS`**（不是卡面写的 `ADJUST`），
   `txn_type` 分别 IN / OUT。
4. 盘点**没有** `/stock-take/{id}/confirm`；确认过账 = `PATCH /api/business/erp/stock-take/{id}/status {"status":"POSTED"}`，
   且 `DRAFT → POSTED` 是非法边（必须先 `PENDING_POST` 走完 `stock_take_post` 审批）。
5. 调拨真实路由是 `/business/erp/transfer`（不是 `stock-transfer`），状态机 6 态：
   `DRAFT→PENDING→APPROVED→IN_TRANSIT→DONE`（`DONE` 才落库存），审批流程 `stock_transfer` v2 需 WAREHOUSE_MANAGER。
6. 出库**没有**审批流程（`stockout_handler` 注释明确「出库还没有种子审批流程」），`PATCH POSTED` 直接过账。

## ⑦ 全量对账（完整输出，差异 0）

```
===== ⑦-0 全账总量：库存表合计 vs 流水累计 =====
 inv_qty  | txn_qty  |   diff
----------+----------+----------
 714.0000 | 714.0000 | 0.0000
(1 row)

===== ⑦-1 逐 (物料, 仓库)：库存表 qty vs 流水累计（全量历史）=====
 material_code | warehouse_code | inv_qty  | txn_qty  |  diff
---------------+----------------+----------+----------+--------
 DEMO-INV01    | WH-DVA         |  92.0000 |  92.0000 | 0.0000
 DEMO-INV01    | WH-DVB         |  40.0000 |  40.0000 | 0.0000
 DEMO-INV02    | WH-DVA         | 110.0000 | 110.0000 | 0.0000
 DEMO-INV02    | WH-DVB         |  40.0000 |  40.0000 | 0.0000
 DEMO-M001     | WH01           |  88.0000 |  88.0000 | 0.0000
 DEMO-M002     | WH01           |  94.0000 |  94.0000 | 0.0000
 DEMO-P101     | WH01           | 100.0000 | 100.0000 | 0.0000
 DEMO-P102     | WH01           | 150.0000 | 150.0000 | 0.0000
(8 rows)

===== ⑦-2 差异行（期望 0 行）=====
 material_code | warehouse_code | inv_qty | txn_qty | diff
---------------+----------------+---------+---------+------
(0 rows)

===== ⑦-3 金额：库存表金额合计 vs 流水入-出金额累计 =====
 inv_amount | txn_amount |   diff
------------+------------+----------
 23580.0000 | 23580.0000 | 0.0000
(1 row)

===== ⑦-4 流水自洽：after_qty - before_qty = ±qty（期望 0 行异常）=====
 id | doc_no | txn_type | qty | before_qty | after_qty
----+--------+----------+-----+------------+-----------
(0 rows)
```

⑦-5（演练物料逐笔流水，审计轨迹，实测原样）：

```
 material_code | warehouse_code | txn_type |    biz_type     |       doc_no        |   qty    | before_qty | after_qty | unit_cost |  amount   | period_code
---------------+----------------+----------+-----------------+---------------------+----------+------------+-----------+-----------+-----------+-------------
 DEMO-INV01    | WH-DVA         | IN       | OPENING         | OPENING-2026-10     | 100.0000 |     0.0000 |  100.0000 | 10.000000 | 1000.0000 | 2026-10
 DEMO-INV01    | WH-DVA         | IN       | PURCHASE_IN     | IN-20261107-000003  |  50.0000 |     0.0000 |   50.0000 | 12.000000 |  600.0000 | 2026-11
 DEMO-INV01    | WH-DVA         | OUT      | TRANSFER_OUT    | TK-20261107-000001  |  40.0000 |   100.0000 |   60.0000 | 10.000000 |  400.0000 | 2026-11
 DEMO-INV01    | WH-DVB         | IN       | TRANSFER_IN     | TK-20261107-000001  |  40.0000 |     0.0000 |   40.0000 | 10.000000 |  400.0000 | 2026-11
 DEMO-INV01    | WH-DVA         | IN       | STOCK_TAKE_GAIN | ST-20261107-000001  |   5.0000 |    60.0000 |   65.0000 | 10.000000 |   50.0000 | 2026-11
 DEMO-INV01    | WH-DVA         | OUT      | STOCK_TAKE_LOSS | ST-20261107-000002  |   3.0000 |    65.0000 |   62.0000 | 10.000000 |   30.0000 | 2026-11
 DEMO-INV01    | WH-DVA         | OUT      | ISSUE_OUT       | OUT-20261107-000004 |  20.0000 |    62.0000 |   42.0000 | 10.000000 |  200.0000 | 2026-11
 DEMO-INV02    | WH-DVA         | IN       | OPENING         | OPENING-2026-09     | 100.0000 |     0.0000 |  100.0000 | 10.000000 | 1000.0000 | 2026-09
 DEMO-INV02    | WH-DVA         | IN       | PURCHASE_IN     | IN-20261107-000004  |  50.0000 |     0.0000 |   50.0000 | 12.000000 |  600.0000 | 2026-11
 DEMO-INV02    | WH-DVA         | OUT      | TRANSFER_OUT    | TK-20261107-000093  |  40.0000 |   100.0000 |   60.0000 | 10.000000 |  400.0000 | 2026-11
 DEMO-INV02    | WH-DVB         | IN       | TRANSFER_IN     | TK-20261107-000093  |  40.0000 |     0.0000 |   40.0000 | 10.000000 |  400.0000 | 2026-11
```

## ⑧ 库位库存一致性（完整输出，逐桶 0 差异）

```
===== ⑧-1 逐 (物料, 仓库)：库位账合计 vs 库存账合计（期望 0 行）=====
 material_code | warehouse_code | cost_qty | loc_qty | diff
---------------+----------------+----------+---------+------
(0 rows)

===== ⑧-2 逐桶对账：(物料, 仓库, 库位字典) 成本账 vs 库位账（期望 0 行）=====
 material_code | warehouse_code | bucket | cost_qty | loc_qty | diff
---------------+----------------+--------+----------+---------+------
(0 rows)

===== ⑧-3 演练物料的桶明细（人工核对用）=====
 material_code | warehouse_code |   bucket   | cost_qty | avg_cost  |  amount  | loc_qty
---------------+----------------+------------+----------+-----------+----------+---------
 DEMO-INV01    | WH-DVA         | LOC-DVA-01 |  50.0000 | 12.000000 | 600.0000 | 50.0000
 DEMO-INV01    | WH-DVA         | (未分配)   |  42.0000 | 10.000000 | 420.0000 |
 DEMO-INV01    | WH-DVB         | (未分配)   |  40.0000 | 10.000000 | 400.0000 |
 DEMO-INV02    | WH-DVA         | LOC-DVA-01 |  50.0000 | 12.000000 | 600.0000 | 50.0000
 DEMO-INV02    | WH-DVA         | (未分配)   |  60.0000 | 10.000000 | 600.0000 |
 DEMO-INV02    | WH-DVB         | (未分配)   |  40.0000 | 10.000000 | 400.0000 |
```

> ⑧-3 里「未分配」行的 `loc_qty` 为空是**展示口径**：成本账的未分配桶 `location_id IS NULL`，
> 按 `il.id = i.location_id` 关联不上库位账行，数字隐藏在这一列之外的 ⑧-2 汇总里（那里按
> 「库位字典 → 未分配」归组，显示 0 差异）。

## 金额/数量勾稽

| 勾稽项 | 期望 | 实测 | 结论 |
|---|---|---|---|
| 期初金额 = Σ(期初数量 × 期初单价) | 100 × 10 = 1000 | 1000（`PZ-…000010` 1000/1000） | ✅ |
| 入库 50 @12 金额 | 600 | 600（`PZ-…000011` 600/600；库存桶 amount 600） | ✅ |
| 调拨前后总量不变 | 150 → 150 | 110(A) + 40(B) = 150；调出 100→60、调入 0→40 | ✅ |
| 调拨金额守恒 | 400 = 400 | OUT 400 / IN 400（原价转移，avg 10） | ✅ |
| 盘盈差异 = 实盘 − 账面 | 115 − 110 = +5 | +5，金额 50 = 5×10 | ✅ |
| 盘亏差异 | 112 − 115 = −3 | −3，金额 30 = 3×10 | ✅ |
| 出库结转成本 | 20 × 10 = 200 | 200（`PZ-…000014` 200/200） | ✅ |
| 期末库存数量 = 期初 + 入 − 出 ± 盘 | 100+50−40+5−3−20 = 92（WH-DVA） | 92.0000 | ✅ |
| 凭证借贷平衡 | 全账 0 张不平 | `HAVING sum(debit_amount)<>sum(credit_amount)` = 0 | ✅ |
| 库存表金额 = 流水金额累计 | 0 差异 | 23580 = 23580 | ✅ |

## 发现的问题

| # | 问题 | 严重度 | 复现步骤 | 建议 |
|---|---|---|---|---|
| 1 | **调拨结算分库位库存永久漂移（已修）**：同仓同时存在「整仓汇总桶」与「具体库位桶」时，明细带 `from_location_id` → 成本账按「汇总桶优先」扣汇总桶、库位账按明细库位扣具体库位桶。两本账**合计**仍相等（`assert_settle_invariants` 第 4 条看不出来），分库位数量永久错位。实测：调拨 40 件后库位桶 成本 50 / 库位账 10，汇总桶 成本 42 / 库位账 82 | **P1** | ①期初 100（落汇总桶）→ ②收货 50（落默认库位桶）→ ③调拨 40 且明细指该库位桶 → ④按 `(物料,仓,库位字典)` 对账 | **本卡已修**：`resolve_inventory_row` 一并返回库存桶的 `location_id`，调出/调入两侧的 `apply_location_qty` 与序列号落点都用「成本账实际动的那个桶」（commit `ce548eb`，含真库回归用例 `p003_settle_deducts_the_same_bucket_in_both_books`） |
| 2 | 库存是「桶」表、流水无库位列 → 卡面 ⑦ 的 SQL 口径不成立（同一物料同仓两行会被流水重复放大）。方向列名是 `txn_type` 不是 `direction`，盘点差异 `biz_type` 是 `STOCK_TAKE_GAIN/LOSS` 不是 `ADJUST` | P3（口径） | 直接跑卡面 SQL 会得到假差异 | 巡检/对账 SQL 统一按 (物料, 仓库) 聚合比较库存与流水；逐桶只比「库位账 vs 成本账」 |
| 3 | 期初落「未分配桶」、收货落「默认库位桶」—— 同仓两桶并存是**常态**，正是 #1 的触发条件；且现有汇总不变式（合计相等）对这种错位无感 | P2（设计） | 见 #1 | ①期初也落「默认库位桶」（与收货一致）；或②在 `assert_settle_invariants` / 巡检里加**逐桶**断言（本卡只在演练脚本里补，未改生产断言，避免引入新的结算失败点） |
| 4 | 调拨明细指定了具体库位，但「汇总桶优先」策略会静默改扣汇总桶：不报错、不提示，审批人看到的是库位级意图、账上动的是汇总桶 | P3 | 同 #1 场景，观察 `biz_inventory.location_id` 未变而汇总桶减少 | 要么改为「按库位精确匹配、无该桶才回退汇总桶」，要么在调拨详情/审批表单里显式标注「本次按汇总桶记账」 |
| 5 | 无源入库单（`is_sourceless=true`）的 `in_type` 仍是 `PURCHASE` | P4 | `POST /business/erp/stock-in` 无 `source_order_id` → 详情 `in_type=PURCHASE` | 无源场景建议用 `OTHER`/`OPENING`，便于报表区分 |
| 6 | 盘点审批通过只到 `PENDING_POST`，还要人工再点一次「过账」；卡面写的 `/stock-take/{id}/confirm` 不存在 | P4（文档） | `PATCH /stock-take/{id}/status {"status":"POSTED"}` | 前端/文档统一到真实路径；若希望审批通过即过账，需要流程回调里做过账（当前设计是两步，属安全考虑） |
| 7 | 单号取号器是全局单行，演练期间被并行卡/单测推进（本次调拨单号跳到 `TK-20261107-000093`） | P4 | 观察 `biz_stock_transfer.transfer_no` 跳号 | 无功能影响；若要求单号连续，需要按业务类型独立号段 |

## 修复与验证（第 1 条）

- 代码：`backend/src/business/erp/transfer_settle.rs`
  - `resolve_inventory_row` 返回 `InventoryBucket { inventory_id, location_id }`（原来只回 `id`）；
  - 调出侧：`apply_location_qty(..., out_bucket.location_id, -qty)`（原来是明细的 `from_location_id`）；
  - 调入侧：先解析目标桶（有汇总桶 → 汇总桶），库位账与序列号落点都用同一个桶（原来是明细的 `to_location_id`）。
- 回归用例：`transfer_handler_db_tests_settle.rs::p003_settle_deducts_the_same_bucket_in_both_books`
  （造「汇总桶 100 + 库位桶 6」并存，调拨 3 件后断言：汇总桶 97、库位桶 6、逐桶 0 差异）。
- 测试：`business::erp::transfer` 全量 **85/85 通过**（含 22 条 settle），`cargo check --all-targets` 通过，
  `cargo fmt --check` 通过；`cargo clippy --all-targets` 只剩 3 处**与本卡无关的既有告警**
  （`fx_revaluation.rs:788`、`sales_order_model.rs:699`、`year_end_linkage_tests.rs:214`，均为 `-D warnings` 下报错，HEAD 上已存在）。
- 部署：145 `git pull` → `cargo build`（debug）→ 重启 `cloudpivot-test`，health=200（新二进制 20:11:28）。
- 修复后真机复跑（DEMO-INV02，同一漂移场景：期初 100 汇总桶 + 收货 50 库位桶 + 调拨 40 指定库位桶）：
  `TK-20261107-000093` DONE 后 成本账 汇总 60 / 库位 50，库位账 汇总 60 / 库位 50 → **逐桶 0 差异**（修复前同场景为 42/82 vs 50/10）。
- 修复前那笔调拨在 DEMO-INV01 上留下的 40 件漂移，已用 `12_datafix_bucket_drift.sql`
  按成本账口径把这 40 件从「未分配桶」挪回「LOC-DVA-01 桶」修平（只动本卡自造的演练物料，前后对比见该文件输出）。

## 汇总

- 通过步骤: **8/8**
- 阻断性 bug: **0 个**（发现 1 个 P1，已在本卡修复并部署验证）
- 结论: **链路通畅**
- 数据副作用说明：本卡为跑回归用例把 `ERP_DB_URL` 指向了 145 `cloudpivot_dev`，残留的测试夹具库存行
  （夹具仓库 `WH-W3B-1/2`）已用 `15_purge_test_residue.sql` 清掉；取号器被推进若干号段（无功能影响）。

## 复现文件

| 文件 | 内容 |
|---|---|
| `00_env.sh` | 环境变量、登录、curl 封装 |
| `01_refs.sh` | 登录 + 参考数据（单位/供应商/物料/仓库） |
| `02_master.sh` | ① 物料 + 2 仓库 + 2 库位（幂等） |
| `03_opening.sh` | ② 库存期初 |
| `04_stockin.sh` | ③ 直接入库 + 过账 |
| `05_transfer_create.sh` / `05_transfer_submit.sh` / `05_transfer_flow.sh` | ④ 调拨建单/提交审批/审批/发运/收货（含前后库存快照） |
| `06_stocktake_create.sh` / `06b_stocktake_gain.sh` | ⑤ 盘盈 |
| `07_stocktake_loss.sh` | ⑤ 盘亏 |
| `08_stockout_create.sh` | ⑥ 出库建单 + 过账 |
| `09_reconcile.sql` / `10_reconcile_output.txt` | 修复前对账（含 P1 的漂移证据） |
| `11_fix_verify.sh` | 修复后同场景复跑（DEMO-INV02） |
| `12_datafix_bucket_drift.sql` | 修平修复前那笔调拨的 40 件漂移（仅演练物料） |
| `13_final_reconcile.sql` / `14_final_reconcile_output.txt` | 终局对账（差异 0） |
| `15_purge_test_residue.sql` | 清测试夹具残留 |
| `restart_145.sh` | 145 重启 + health 轮询（无人值守下 systemctl 必须走脚本） |
| `20_verify_test.sh` | 回归用例一键跑（含 SSH 隧道指向 145 库） |

# M2-18 仓库缺口收口 E2E（在途 + 库龄 + 安全库存）

- 卡：`t_659b64de`（M2-18【M2·仓库】仓库缺口收口：在途 + 库龄 + 安全库存 E2E）
- 上游：M2-01..07（在途账 / 发货过账 / 签收确认 / 查询 / 对账 / 页面）、
  M2-08..12（库龄计算 / 分段报表 / 呆滞料 / 单测 / 页面）、
  M2-13..17（水位字段 / 水位检查 / 补货建议 / 单测 / 设置页）
- 交付物（本卡**没有**改业务代码，只交验收链路与证据）：
  - `scripts/verify/m218_warehouse_gap_e2e.sh`（驱动：建库 → 起隔离实例 → 夹具 → 真接口断言 → 清理）
  - `scripts/verify/m218_warehouse_gap_fixture.sql`（阶段 1：库存现状）
  - `scripts/verify/m218_warehouse_gap_fixture_phase2.sql`（阶段 2：签收确认后的落库形态）
  - `scripts/verify/m218_warehouse_gap_e2e.py`（真接口断言，阶段 A / B）
  - 本 README

## 1. 一条链路（同一格库存上依次发生）

```text
① 入库四批（业务日期 今天−120 / −75 / −45 / −5）              → biz_inventory_txn 的 IN 流水
② 库龄：四档 40/30/20/10、合计 = 结存 100、期末快照可回溯        → M2-08/09（库函数 FIFO 唯一实现）
③ 呆滞：> 90 天那层 10 件被标出来                              → M2-10
④ 发货过账：两张发货单在途 30 + 30 = 60（账还没扣）            → M2-01/02/03
⑤ 在途查询 / 汇总 / 对账「在途 + 已签收 = 发货总量」对平        → M2-04/05
⑥ 补货建议：预计可用 = 100 − 60 + 100 = 140 < 水位 200         → M2-15（含 M2-13 水位）
   ⇒ 建议补到最高水位 300 = 160
⑦ 水位检查：账面结存 100 < 200 → 建待办（P2 / 负责人 = 仓管主管）→ M2-14
   ⇒ dry_run 只算不写、真跑建 1 条、复跑 skipped_open=1（三层幂等）
⑧ 签收确认（阶段 2 复刻落库形态）：结存 100→70、在途 60→30、
   库龄四档 → 40/30/0/0（FIFO 吃满最老两层）、呆滞清零
   ⇒ **预计可用 140 不变**（签收只是把「在路上」挪进「账上」）
⑨ 改水位（PUT 最低 50）→ 水位检查自动收口（closed=1、AUTO_RECOVERED）
   → 补货建议转 OK（不再出现）                                → M2-13/14/15/17
```

## 2. 夹具数字（脚本里逐个断言，改数必改断言）

| 入库批次 | 业务日期 | 数量 | 落哪档 | 呆滞（>90 天） |
|:--|:--|--:|:--|:--:|
| M218-IN-0001 | 今天 − 120 | 10 | 90 天以上 | ✅ |
| M218-IN-0002 | 今天 − 75 | 20 | 61-90 天 | — |
| M218-IN-0003 | 今天 − 45 | 30 | 31-60 天 | — |
| M218-IN-0004 | 今天 − 5 | 40 | 0-30 天 | — |
| **合计** | | **100** | = `biz_inventory.qty` | 10 |

在途：M218-SH-0001 发 30 / 签 0 = 在途 30；M218-SH-0002 发 50 / 签 20 = 在途 30（部分签收仍算在途）。
未清采购订单：M218-PO-0001 `APPROVED` 100（计入供给）、M218-PO-0002 `DRAFT` 999（**不计入**）。
水位：策略行 min 200 / max 300（三级回退的最高优先级）。

## 3. 实测（2026-11-17，99 开发机 · 库在 145）

```bash
# 空库首启（不用模板库）→ 服务自己应用 363 条迁移 → bootstrap admin → 夹具 → 真接口断言
SKIP_BUILD=1 bash scripts/verify/m218_warehouse_gap_e2e.sh
```

两次连跑输出（第二次是改完显示文案后的复跑）：

```text
== 汇总：PASS=24 FAIL=0（阶段 A 73 条真接口断言 + 阶段 B 44 条 + 本节 SQL 断言）==
ALL PASS
```

- 环境：`cp_m218_gap_e2e_<pid>` @ `172.18.10.145:5432`，隔离实例 `127.0.0.1:18218`，
  `schema_migrations.success = 363`，脚本退出即 `dropdb`（跑完 `pg_database` 里无 `cp_m218%`）。
- 断言合计 **141 条**：阶段 A 73（库龄 14 / 呆滞 5 / 在途 18 / 补货 13 / 水位检查 22 / 登录与建账号 4 …）、
  阶段 B 44、收口后库层 7 + 驱动自身 17。

阶段 A 关键实测值（与设计逐个对上）：

```text
库龄四档   0-30=40  31-60=30  61-90=20  90+=10   合计=100  层数=4  最老=120 天
期末快照   as_of=今天−60 → 合计 30、2 层
呆滞       90 天阈值 → 1 格 / 10 件 / 占比 0.10
在途       明细 2 行；汇总 60；发货 80 / 已签 20；对账 checked=2 matched=2 mismatched=0 balanced=true
补货       结存 100 / 在途 60 / 未清 100 → 可用 40 / 预计 140 → 目标 300（MAX）→ 建议 160（PURCHASE）
水位检查   dry_run: low=1 raised=0 action=DRY_RUN
           真跑   : raised=1 owner_source=ROLE action=RAISED（task_no 回显）
           复跑   : raised=0 skipped_open=1 closed=0 action=EXISTS
```

阶段 B 关键实测值：

```text
库龄       合计 70、层数 2（40/30/0/0）—— FIFO 吃满 −120 与 −75 两层
呆滞       1 格 → 0 格（最老层被吃掉，呆滞不是标签而是 FIFO 摊出来的）
在途       合计 60 → 30；默认明细 1 行（SH-0002）；status=RECEIVED 1 行（SH-0001，在途 0 / 已签 30）
对账       balanced 仍 true，SH-0001 已签收 30 / 在途 0
补货       结存 70 / 在途 30 / 未清 100 → 预计 140（**与签收前相同**）→ 建议仍 160
水位检查   低水位 1 →（PUT min=50）→ low=0 closed=1，待办 CANCELLED + AUTO_RECOVERED
补货建议   转 OK（include_covered=true 也不再出现，预计 140 ≥ 50）
```

库层复核（不依赖 HTTP 回显）：

```sql
-- 待办只 1 条、标题/优先级/负责人/水位来源都对
SELECT count(*), max(title), max(status||'|'||priority) FROM biz_task
 WHERE doc_type='INVENTORY' AND doc_id='a2180000-0000-4000-8000-000000000601';
-- Σ库龄层剩余 == 账面结存 == 70（库龄与结存同源）
WITH inv AS (SELECT qty FROM biz_inventory WHERE id='…0601'),
     l AS (SELECT sum(qty_remaining) q FROM biz_inventory_aging_layers(CURRENT_DATE,'…0101','…0010'))
SELECT (SELECT qty FROM inv)::numeric(18,4)||'|'||(SELECT q FROM l)::numeric(18,4);
```

## 4. 刻意不同、**不要合并**的两处口径

| 判定点 | 比的是 | 为什么 |
|:--|:--|:--|
| 水位检查（M2-14） | **账面结存** `biz_inventory.qty` | 它是「账上还剩多少」的提醒：给仓管一个「现在就得动手」的信号，不该被「在路上 / 已下单」稀释 |
| 补货建议（M2-15） | **预计可用** = 结存 − 发货在途 + 未清采购订单 | 它是「还要不要再下单」的决策：货已发走就不要再买一次，订单在路上就不要重复下单 |

阶段 A 就是这两种结论并存的形态（结存 100 > 0 却仍建议补 160），断言里把两个数字都钉住了；
谁把两者合成一个口径，本脚本会当场红。

另外三条同源口径（脚本也钉着）：
- 「在途」= `biz_inventory_in_transit.status='IN_TRANSIT'` 的 `qty_in_transit`（生成列 = 发货 − 已签，PG 保证）；
- 库龄层只来自 `biz_inventory_txn` 的 IN 流水，层日期取来源单据**业务日期**（`biz_stock_in.in_date`），
  **不是** `created_at` 过账时间 —— 所以期末快照（`as_of`）能还原「那天账上是什么样」；
- 在途对账的「发货总量」取 `biz_shipment_item.qty`（明细 SSOT），不取单头 `total_qty`。

## 5. 未覆盖 / 有意留给别处

| 事项 | 归属 |
|:--|:--|
| `POST /shipment/{id}/receive` 的**过账内核**（负库存闸 / 三本账 / 成本结转凭证 / 挂应收 / 在途清零 / 失败回滚可重试） | M2-03，由 `scripts/verify/m203_shipment_receipt_drill.sh` 的 5 条真库用例覆盖；本卡只验「签收之后三个查询面是否一致地跟着变」，阶段 2 用 SQL 复刻该落库形态 |
| 前端三个页面（在途库存 / 库龄分析 / 安全库存设置）的渲染与交互 | M2-07 / M2-12 / M2-17 各自的页面验收（`m207_transit_page_drill.sh`、前端单测、vite build） |
| 权限码矩阵与菜单结构 | `menu_invariants.sh` + `permission_audit_tests`（本卡不动菜单） |

## 6. 复跑方法与踩坑

```bash
# 99 → 145（默认）
SKIP_BUILD=1 bash scripts/verify/m218_warehouse_gap_e2e.sh
# 在 145 上跑
PG_HOST=127.0.0.1 bash scripts/verify/m218_warehouse_gap_e2e.sh
# 留库排查
KEEP_DB=1 bash scripts/verify/m218_warehouse_gap_e2e.sh   # 会打印库名与实例日志路径
```

坑（都在脚本里已处理）：

1. **不要用 `cloudpivot_test` 模板库做这个 E2E**：模板里 `admin` 的口令是别的演练改过的 argon2 哈希，
   没法反推 → 登录必然 401；而且模板带着几十行历史夹具（会污染「全量列表」类断言）。
   本脚本建**空库**让服务自己跑完 363 条迁移 + bootstrap，admin 口令 = `NIK_ADMIN_PASSWORD`（首登强制改密，脚本走真接口改）。
2. **水位待办要有「仓管主管」可派**：空库只有角色没有人 → `skipped_no_owner`。脚本先调
   `GET /system/role/page` 取 `WAREHOUSE_MANAGER` 的 id，再 `POST /system/user` 建一个演练账号（重跑时「已存在」也算过）。
3. **三处库层约束**（照抄夹具会直接 23514）：`biz_stock_in` 的
   `ck_biz_si_sourceless_pair`（`PURCHASE` 且无 `source_order_id` ⇒ `is_sourceless = true`）与
   `ck_biz_si_tax`（`total_with_tax = total_amount + total_tax`）；`biz_sales_order` 的
   `ck_sales_order_amount`（同样要求含税合计闭合）。
4. **对账需要发货单明细**：`biz_shipment_item.so_item_id` 是 NOT NULL 且无默认值 → 夹具必须同时造
   `biz_sales_order_item`；只造发货单头会让对账报 `ORPHAN_TRANSIT`。
5. **在途汇总恒为未签收在途**（`status='IN_TRANSIT'`）：签收后那张单不再出现在汇总里，
   `received_qty` 也只统计仍在途的行 —— 断言「已签收 30」要看明细（`?status=RECEIVED`）或对账，
   不要去汇总里找。

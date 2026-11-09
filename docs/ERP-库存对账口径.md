# ERP 库存对账口径（唯一口径 · FIX-7）

> 适用范围：任何「库存表 vs 流水」「成本账 vs 库位账」的核对 —— 报表取数、巡检脚本、
> 演练对账、数据修复前后验证。**照抄本文的 SQL 就能用**，其它地方（旧卡面 / 旧证据文档）
> 里出现过的写法一律以本文为准。
>
> 起因：卡 FIX-7（t_6207df79）。P0-03 的卡面给了两句错口径 ——
> ① 在 `biz_inventory` 上 `GROUP BY material_id, warehouse_id` 再 JOIN 流水；
> ② 把方向列写成 `direction`、把盘点差异 `biz_type` 写成 `ADJUST`。
> 两句都会出**假差异**，本文把正确口径固化下来，并写进巡检脚本
> `scripts/verify/erp_invariants.py`（B1 / B1b / B4）。

---

## 1. 三张表长什么样（这决定 SQL 怎么写）

| 表 | 粒度 | 关键列 | 说明 |
|---|---|---|---|
| `biz_inventory` | **桶**：唯一键 `(material_id, warehouse_id, COALESCE(location_id, 零 UUID))` | `qty` / `amount` / `avg_cost` / `location_id` | 同一物料同仓**天然多行**（期初落「未分配桶」、收货落「默认库位桶」） |
| `biz_inventory_location` | 库位账：`(material_id, warehouse_id, location_id)` | `qty` | `location_id` 是**库位字典 id**（`biz_location.id`，NULL = 未分配）；本表 `id` 才是被别的表引用的「库位库存行 id」 |
| `biz_inventory_txn` | 流水：**只有 `(material_id, warehouse_id)`** | `txn_type`(IN/OUT) / `qty` / `before_qty` / `after_qty` / `biz_type` / `doc_type` / `doc_id` | **没有库位列**；`qty` 恒正（`ck_biz_inv_txn_qty: qty > 0`），方向靠 `txn_type` |

三条由此推出的铁律：

1. **「库存 = 流水」只能在 `(物料, 仓库)` 粒度比**。流水没有库位维度，把它 JOIN 到每个库存桶上
   会把整仓流水乘上桶数 → 满屏假差异。
2. **逐桶只能比「成本账 vs 库位账」**（`biz_inventory` 的桶 vs `biz_inventory_location` 的行），
   且必须**按桶键 FULL JOIN**：`i.location_id` 为空的「未分配桶」在 `LEFT JOIN ... ON il.id = i.location_id`
   下必然关联不上，直接 `i.qty <> il.qty` 同样是满屏假差异。
3. **列名/取值别猜**：方向列是 `txn_type`（`IN`/`OUT`），不是 `direction`；
   盘点差异流水的 `biz_type` 是 `STOCK_TAKE_GAIN` / `STOCK_TAKE_LOSS`（不是 `ADJUST`）；
   销售出库流水是 `SALE_OUT`（不是 `OTHER_OUT`，见 §4）。

> ⚠️ **`location_id IS NULL` 的那一行是「未分配库位」这一个真实的桶，不是整仓汇总行。**
> 历史上有两处代码把它当整仓汇总（`COALESCE(MAX(qty) FILTER (WHERE location_id IS NULL), SUM(qty))`），
> 两桶并存时就报假差异、修的时候还会把库位桶抹掉（E2E-03 的 P1，见 §6）。
> 任何「整仓数量」都只能是 `SUM(qty)`。

---

## 2. 正确对账 SQL（两类，抄这两段就够）

### 2.1 库存表 vs 流水累计 —— 按 (物料, 仓库) 聚合

```sql
-- 差异行；期望 0 行
WITH inv AS (
    SELECT material_id, warehouse_id, SUM(qty) AS qty
      FROM biz_inventory GROUP BY 1, 2
), txn AS (
    SELECT material_id, warehouse_id,
           SUM(CASE WHEN txn_type = 'IN' THEN qty ELSE -qty END) AS qty
      FROM biz_inventory_txn GROUP BY 1, 2
)
SELECT m.material_code, w.warehouse_code,
       COALESCE(inv.qty, 0) AS inv_qty,
       COALESCE(txn.qty, 0) AS txn_qty,
       COALESCE(inv.qty, 0) - COALESCE(txn.qty, 0) AS diff
  FROM inv FULL JOIN txn USING (material_id, warehouse_id)
  JOIN biz_material  m ON m.id = COALESCE(inv.material_id, txn.material_id)
  JOIN biz_warehouse w ON w.id = COALESCE(inv.warehouse_id, txn.warehouse_id)
 WHERE COALESCE(inv.qty, 0) <> COALESCE(txn.qty, 0);
```

金额同口径：把 `qty` 换成 `amount` 即可（`SUM(CASE WHEN txn_type='IN' THEN amount ELSE -amount END)`）。

### 2.2 成本账 vs 库位账 —— 逐桶（物料 × 仓库 × 库位字典）

```sql
-- 差异行；期望 0 行
WITH cost AS (
    SELECT i.material_id, i.warehouse_id,
           COALESCE(il.location_id::text, '(未分配)') AS bucket,
           SUM(i.qty) AS qty
      FROM biz_inventory i
      LEFT JOIN biz_inventory_location il ON il.id = i.location_id
     GROUP BY 1, 2, 3
), loc AS (
    SELECT material_id, warehouse_id,
           COALESCE(location_id::text, '(未分配)') AS bucket,
           SUM(qty) AS qty
      FROM biz_inventory_location
     GROUP BY 1, 2, 3
)
SELECT m.material_code, w.warehouse_code, COALESCE(cost.bucket, loc.bucket) AS bucket,
       COALESCE(cost.qty, 0) AS cost_qty, COALESCE(loc.qty, 0) AS loc_qty,
       COALESCE(cost.qty, 0) - COALESCE(loc.qty, 0) AS diff
  FROM cost FULL JOIN loc
    ON cost.material_id = loc.material_id
   AND cost.warehouse_id = loc.warehouse_id
   AND cost.bucket = loc.bucket
  JOIN biz_material  m ON m.id = COALESCE(cost.material_id, loc.material_id)
  JOIN biz_warehouse w ON w.id = COALESCE(cost.warehouse_id, loc.warehouse_id)
 WHERE COALESCE(cost.qty, 0) <> COALESCE(loc.qty, 0);
```

> 为什么「合计相等」不够：2026-11-07 的 P1 事故（已修 `ce548eb`）就是**两本账合计相等、
> 逐桶永久错位** —— 调拨按「汇总桶优先」扣成本账、按明细库位扣库位账。只跑 2.1 看不出来，
> 必须跑 2.2。

### 2.3 流水自洽（`after_qty - before_qty = ±qty`）

```sql
SELECT id, doc_no, txn_type, qty, before_qty, after_qty
  FROM biz_inventory_txn
 WHERE (txn_type = 'IN'  AND after_qty - before_qty <> qty)
    OR (txn_type = 'OUT' AND before_qty - after_qty <> qty);   -- 期望 0 行
```

---

## 3. 巡检脚本在哪（不要自己再写一套）

`scripts/verify/erp_invariants.py` 的「B. 账账相符」组已经按本文口径固化了三条：

| 检查 | 口径 | 级别 |
|---|---|---|
| `B1` | 库位账合计 = 成本账 qty（按 `(物料, 仓库)` 聚合） | BLOCKER |
| `B1b` | **逐桶**成本账 = 库位账（`(物料, 仓库, 库位字典)` FULL JOIN） | WARN（合计相等但库位级错位时报警） |
| `B4` | 流水累计 = 当前库存（按 `(物料, 仓库)` 聚合，`txn_type` 加减） | BLOCKER |

用法（只读，可对任意库跑）：

```bash
NIK_DATABASE_HOST=127.0.0.1 NIK_DATABASE_PORT=5432 \
NIK_DATABASE_USER=cloudpivot NIK_DATABASE_NAME=cloudpivot \
NIK_DATABASE_PASSWORD=*** \
python3 scripts/verify/erp_invariants.py --group 账账
```

值域另有一条独立巡检：`scripts/verify/biz_type_domain.py`（按 `doc_kind.rs::COLUMN_DOMAINS`
查 `biz_inventory_txn.biz_type` 是否都在词表内）。

---

## 4. 出库流水的 `biz_type` 口径（FIX-7）

出库单有**两个正交维度**，`biz_type` 必须两者一起看：

* `out_type`（`ISSUE` / `SCRAP` / `LOSS` / `OTHER`）——「领用性质」，决定借方科目；
* `source_type`（`SALES` = 发货单过账自动生成；手工建单为空）——「谁生成了这张单」。

| 单据 | `out_type` | `source_type` | `biz_inventory_txn.biz_type` |
|---|---|---|---|
| 销售出库（发货单过账生成） | `OTHER` | `SALES` | **`SALE_OUT`** |
| 手工领用出库 | `ISSUE` | 空 | `ISSUE_OUT` |
| 手工报废出库 | `SCRAP` | 空 | `SCRAP_OUT` |
| 手工盘亏出库 | `LOSS` | 空 | `LOSS_OUT` |
| 手工其他出库 | `OTHER` | 空 | `OTHER_OUT` |
| 出库作废冲销（方向 IN） | — | — | `STOCK_OUT_CANCEL` |

> 取值唯一真相：`backend/src/business/shared/doc_kind.rs` 的 `TxnBizType`
> （前端镜像 `frontend/src/api/business/erp/doc-kinds.ts` 的 `TXN_BIZ_TYPES`）。
> 代码侧唯一入口是 `stockout_base::biz_type_of_doc(out_type, source_type)`。
>
> **分界点**：V0330 之前生成的销售出库流水记的是 `OTHER_OUT`，V0330 迁移把它统一刷成
> `SALE_OUT`（只刷 `biz_stock_out.source_type='SALES'` 的那些单，手工其他出库不受影响），
> 所以**没有「迁移前 / 迁移后」两套统计口径** —— 直接 `WHERE biz_type='SALE_OUT'` 统计销售出库即可。

按 `biz_type` 统计销售出库的验收 SQL：

```sql
SELECT count(*) AS txn_rows, COALESCE(SUM(qty), 0) AS qty
  FROM biz_inventory_txn
 WHERE biz_type = 'SALE_OUT' AND txn_type = 'OUT';
-- 与「销售出库单明细的已过账数量」逐单核对：
SELECT SUM(i.qty)
  FROM biz_stock_out s JOIN biz_stock_out_item i ON i.stock_out_id = s.id
 WHERE s.source_type = 'SALES' AND s.status = 'POSTED' AND s.deleted_at IS NULL;
```

---

## 5. 无源入库单的 `in_type`（FIX-7）

| 单据 | `in_type` | `is_sourceless` | 供应商 |
|---|---|---|---|
| 采购入库（绑采购单） | `PURCHASE` | false | 由采购单带出 |
| 销售退货回库 | `RETURN` | false | 空 |
| 其他入库（杂项） | `OTHER` | — | 空 |
| **无源入库（期初 / 复制 / 补录）** | **`SOURCELESS`** | **true** | **用户显式填** |

约束（V0330 起）：`ck_biz_si_supplier_scope` = 「`PURCHASE` / `SOURCELESS` 必须有供应商，
其余必须没有」。V0330 同时把存量无源单的 `in_type` 从 `PURCHASE` 迁到 `SOURCELESS`，
所以按 `in_type` 统计采购入库不会再把「没走采购的货」算进去。

---

## 6. 库存巡检（不变式③）：`book_qty` 口径与修复语义

接口 `GET /api/business/erp/inventory/audit?dry_run=true|false`（权限 `business:erp:inventory:audit`），
实现 `backend/src/business/erp/inventory_audit.rs`（修正语句在唯一写入口 `inventory_writer.rs`）。

### 6.1 判定口径（唯一）

```text
对每个 serial_required 物料 × 仓库：
    book_qty   = SUM(biz_inventory.qty)            -- 该物料×仓库的【所有桶之和】
    actual_qty = COUNT(biz_material_serial WHERE status='IN_STOCK' AND deleted_at IS NULL)
    不一致     = book_qty <> actual_qty            -- 正 = 账多，负 = 账少
```

**`book_qty` 必须是 `SUM(qty)`**，与 §2.1 同一个粒度。

❌ 错误写法（历史实现，`E2E-FIX-2` 修掉）：

```sql
COALESCE(MAX(qty) FILTER (WHERE location_id IS NULL), SUM(qty))
-- 含义变成「只要存在未分配桶，就只取那一桶」→ 库位桶被无视
```

实测（E2E-03 · 一物一码物料 MS）：未分配桶 2 + 默认库位桶 3，`Σqty = 5`；
旧口径报 `book = 2`、`actual = 5`、`diff = -3` 的**假差异**。

### 6.2 修复语义（`dry_run=false`，数据安全级）

| 该 `(物料, 仓库)` 的桶数 | 语义 | 为什么 |
|---|---|---|
| `0`（成本账一行都没有） | `INSERT` 建档，`qty = actual`，落未分配桶（`location_id = NULL`） | 没有可参照的桶，等价于「补建一笔账」 |
| `1`（单桶） | **差额调整** `qty += (actual - book)`，`amount = 新 qty × avg_cost` | 数学上等于 `qty = actual`，但不写死覆盖、将来加了列也不会被抹 |
| `≥2`（多桶并存） | **拒绝自动修**，报告标 `unfixable_reason = MULTI_BUCKET`，要人工处理 | 「差额该记哪个桶」本身有歧义：记未分配桶会让 §2.2 逐桶对账错位，记某个库位桶则是凭空猜 |

- `dry_run` **缺省 `true`**（只报不修），非法值（`?dry_run=treu`）回 400，不静默当 `false`；
- 没有仓库归属的在库序列号（`warehouse_id IS NULL`）写不进仓账 → `unfixable_reason = NO_WAREHOUSE`；
- 报告里的 `bucket_count` / `unfixable` / `unfixable_reason` 是给运维看的诊断字段：
  「报了但修不了」必须可见，不能静默跳过。

> ❌ 旧实现：`INSERT … ON CONFLICT (…) DO UPDATE SET qty = EXCLUDED.qty`，`INSERT` 恒写
> `location_id = NULL`。两桶并存时它把**未分配桶覆盖成 `actual`**、库位桶原样留着 →
> 账上总量变成 `actual + 库位桶`，**正确的账面被改错且不可逆**（需人工回修）。

### 6.3 与 ADR-20261031（批量生成落未分配桶）的关系

ADR-20261031 起「一物一码批量生成 = 一笔无来源入库」：成本账与库位账都按**未分配桶**落；
而**收货**按 ADR 之前的 FIX-4 桶策略落**默认库位桶**（`docs` 见 `inventory-bucket-policy`）。
于是同一个 `(物料, 仓库)` 天然会同时存在「未分配桶 + 默认库位桶」两种行 —— **这不是脏数据**，
是两条业务路径各自正确的落点。

由此得到两条结论：

1. 「同一物料同仓多行」是常态，**任何按整仓取数的 SQL 都必须 `SUM(qty)`**
   （本仓历史上把 NULL 桶当「整仓汇总」的假设已被 §6.1 的错误口径与
   `transfer_settle::assert_settle_invariants` 第 2 条假红两次反证）；
2. 「自动改账」在多桶时必须交人工 —— 因为桶的选择是业务语义（批量生成 vs 收货），
   代码没有依据替业务决定差额该落在哪一桶。

### 6.4 回归用例

`backend/src/business/erp/inventory_audit.rs::tests`（真库，需 `ERP_DB_URL`）：

- `book_qty_sums_all_buckets_no_false_drift`：两桶并存且总数正确 → `mismatched = 0`（不再假红）；
- `multi_bucket_mismatch_is_never_auto_fixed`：多桶 + 真差异 → `MULTI_BUCKET`、`dry_run=false` 一行不改；
- `single_bucket_fix_is_delta_not_overwrite`：单桶按差额修正到 `actual`，`amount = qty × avg_cost`；
- `dry_run_false_fixes_quantities`：单桶差额调整 + 零桶建档（反向巡检）都仍然可修。

---

## 7. 相关文件

| 文件 | 作用 |
|---|---|
| `backend/src/business/erp/inventory_audit.rs` | 库存巡检（不变式③）：`book_qty = SUM(所有桶)`、桶数诊断、修复分流 |
| `backend/src/business/erp/inventory_writer.rs` | 三本账唯一写入口（含巡检修正语句 `align_cost_qty_*`） |
| `scripts/verify/erp_invariants.py` | 巡检（B1 / B1b / B4 已按本文口径固化） |
| `scripts/verify/biz_type_domain.py` | `biz_type` 值域巡检（词表＝`doc_kind.rs`） |
| `backend/src/business/shared/doc_kind.rs` | 业务类型词表唯一真相 |
| `docs/evidence/20261107-p003-inventory-chain/09_reconcile.sql` / `13_final_reconcile.sql` | P0-03 演练对账 SQL（**已是正确口径**，与本文一致） |
| `backend/migrations/business/V0330__inventory_biz_type_and_sourceless_in_type.up.sql` | 本卡迁移（销售出库 `SALE_OUT` 回填 + `SOURCELESS` 语义） |

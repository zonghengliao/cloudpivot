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

> 取值唯一真相：`backend/crates/cloudpivot-doc/src/lib.rs` 的 `TxnBizType`
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

### 5.1 无源入库照样挂应付（口径 B：视同采购 · E2E-FIX-8 ①）

**结论**：`is_sourceless = true` 的入库单过账后**照旧**生成「借 存货（1405 等）/ 借 进项税 /
贷 2202 应付账款」的凭证与一张应付单。**不要**加「无源 → 不挂应付」的分支。

三条依据（都能在代码/DDL 里直接核）：

1. **「无源」= 没有采购单，不是没有供应商。** 同一条 `ck_biz_si_supplier_scope` 要求
   `SOURCELESS` **必须有** `supplier_id`，DTO 层还要求无源单写 `remark` 说明来源
   （`stockin_dto.rs`：AP-4 `66027` / AP-5 `66029`）。所以「欠款给谁」有明确答案 ——
   单头上的供应商，凭证贷方行的辅助核算挂的也是它（`purchase_in_lines` 的 `ctx.supplier_id`）。
2. **期初根本不走这张表。** 库存期初是 `POST /business/erp/opening-balance/inventory`
   （`biz_opening_inventory`，凭证 `借 1405 / 贷 3103`，见 `opening.rs`），不生成应付。
   所以「期初类无源入库不挂应付」的前提不成立：无源入库是「**有**供应商、**没**采购单」的
   补录（老系统迁来的未结清货款、货到票未到、样机/赠品补录……），货款确实要付。
3. **不挂应付会把缺陷变成人工。** 只借 1405 不贷记任何负债 → 试算平衡缺口、应付账龄与
   现金流量表各漏一笔真实负债，财务只能手工补一张应付单；而手工单与原始入库单没有
   `payable.voucher_id` / `biz_payable.source_no` 的勾稽，以后对账还得再核一次。

**选路表**（别再让「无源」当成筐；每行都在代码/库里有据）：

| 场景 | 走哪条路 | 挂应付？ |
|---|---|---|
| 旧系统期初的库存 | `POST /opening-balance/inventory`（期初凭证 1405/3103） | 不挂（本来就没有这笔负债） |
| 有采购单的收货 | 收货过账 `post_in_tx`，`in_type = PURCHASE` | 挂，供应商由采购单带出 |
| **无采购单但有供应商的补录** | 收货过账 `post_in_tx`，`in_type = SOURCELESS` | **挂（本口径）** |
| 销售退货回库 | **另一条过账事务** `sales_return_posting::post_in_tx`（`in_type = RETURN`，凭证 借 1405 / 贷 6401） | 不挂；145 dev 实测 `IN-20261109-000243` 就是这张形态（有凭证、无应付） |
| 杂项（`in_type = OTHER`） | 收货 API 不再落这个类型（`stockin_base::in_type_of` 只出 `PURCHASE` / `SOURCELESS`）；历史行若走 ⑦ | 不挂：`payable::create_for_stock_in` 对「无供应商」直接报错，不会静默生成一张挂不上辅助核算的应付 |

> 所以「无源入库挂应付」不是 `post_in_tx` 无差别对待所有类型的结果 —— 它是
> **收货过账这一条路**（PURCHASE + SOURCELESS，二者都必须有供应商）的固有语义。
> 销售退货有自己的过账事务与贷方科目（6401），不受本口径影响。

> 如果产品以后确实要「无源不挂应付」，改法**不是**关掉分支：得同时定「贷方记什么科目」
> （如 3103 本年利润 / 以前年度损益调整）、改 `B2`（库存金额 = 1405 余额）/`B3` 巡检口径，
> 并处理存量无源单已生成的应付。只关分支等于制造不平衡的凭证。

**回归钉住**：`receipt_refund` 那类「加列 + 口径」的变更都有纯逻辑用例；本条的判据在真库
（过账事务），由 `stockin_handler` 的过账用例覆盖（需 `ERP_DB_URL`），本卡未改代码，
只把口径写进本节与 `stockin_handler_post.rs` ⑦ 的注释指针。

---

## 6. 库存巡检（不变式③）：`book_qty` 口径与修复语义

接口 `GET /api/business/erp/inventory/audit?dry_run=true|false`（权限 `business:erp:inventory:audit`），
实现 `backend/app/src/business/erp/inventory_audit.rs`（修正语句在唯一写入口 `inventory_writer.rs`）。

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

`backend/app/src/business/erp/inventory_audit.rs::tests`（真库，需 `ERP_DB_URL`）：

- `book_qty_sums_all_buckets_no_false_drift`：两桶并存且总数正确 → `mismatched = 0`（不再假红）；
- `multi_bucket_mismatch_is_never_auto_fixed`：多桶 + 真差异 → `MULTI_BUCKET`、`dry_run=false` 一行不改；
- `single_bucket_fix_is_delta_not_overwrite`：单桶按差额修正到 `actual`，`amount = qty × avg_cost`；
- `dry_run_false_fixes_quantities`：单桶差额调整 + 零桶建档（反向巡检）都仍然可修。

---

## 7. 调拨**不出**会计凭证（E2E-FIX-8 ④ · 合理设计，不是漏做）

**结论**：仓库间调拨单（`biz_stock_transfer`）**不生成会计凭证**，表上也没有 `voucher_id` 列，
`biz_voucher.source_type` 里**没有** `STOCK_TRANSFER` 这个来源。巡检 / 审计看到「调拨单没有凭证」
请按本节判「正常」，不要当缺陷提单。

**为什么本来就没有 GL 影响**：

1. **借贷同科目**：调出仓与调入仓的存货都记 `1405`，仓间移位等于「借 1405 / 贷 1405」，
   对总账零影响；落一张这样的凭证只会污染 `biz_voucher` 与凭证号序列，没有任何信息量。
2. **成本按原价平移**：过账时 `transfer_settle::settle_transfer` 用调出桶的 `unit_cost`
   结转（`biz_stock_transfer.total_amount` / 明细金额快照），调入仓 `credit_inventory_row`
   按同一 `unit_cost` 入账 —— 金额在公司层面不变，不产生损益。
3. **结构上就没有钩子**：`backend/crates/cloudpivot-doc/src/lib.rs::VoucherSourceType` 的 19 个
   来源里没有调拨；`biz_stock_transfer` 的 DDL（V0140）只有 `approval_instance_id`（流程弱引用），
   没有 `voucher_id`；`transfer_settle.rs` / `transfer_handler.rs` 里 `voucher` 出现 **0** 次。

调拨**会**动的是库存三本账 + 两条流水（方向相反、数量相等、`doc_type='STOCK_TRANSFER'`）：

| 表 | 调拨做了什么 |
|---|---|
| `biz_inventory`（成本账） | 调出仓 `-qty/-amount`、调入仓 `+qty/+amount`（同 `unit_cost`） |
| `biz_inventory_location`（库位账） | 与成本账**同一个桶**扣/加（`out_bucket.location_id` / `in_bucket_location`） |
| `biz_inventory_txn`（流水） | `biz_type='TRANSFER_OUT'`（`txn_type='OUT'`）+ `'TRANSFER_IN'`（`'IN'`），同 `doc_id` |

**审计怎么核（可直接抄）**：

```sql
-- ① 调拨单不应有凭证来源（期望 0；非 0 = 有人绕过口径生成了凭证，要查）
SELECT count(*) FROM biz_voucher WHERE source_type = 'STOCK_TRANSFER';

-- ② 调拨流水必须成对且数量相等（期望 0 行）
SELECT doc_no, out_qty, in_qty FROM (
    SELECT doc_no,
           SUM(CASE WHEN biz_type = 'TRANSFER_OUT' THEN qty ELSE 0 END) AS out_qty,
           SUM(CASE WHEN biz_type = 'TRANSFER_IN'  THEN qty ELSE 0 END) AS in_qty
      FROM biz_inventory_txn
     WHERE doc_type = 'STOCK_TRANSFER'
     GROUP BY doc_no
) t WHERE out_qty <> in_qty;

-- ③ 成本守恒：同一调拨单调出金额 = 调入金额（`transfer_settle` 不变式第 3 条，期望 0 行）
SELECT doc_no, out_amt, in_amt FROM (
    SELECT doc_no,
           SUM(CASE WHEN biz_type = 'TRANSFER_OUT' THEN amount ELSE 0 END) AS out_amt,
           SUM(CASE WHEN biz_type = 'TRANSFER_IN'  THEN amount ELSE 0 END) AS in_amt
      FROM biz_inventory_txn
     WHERE doc_type = 'STOCK_TRANSFER'
     GROUP BY doc_no
) t WHERE out_amt <> in_amt;
```

145 dev 库实测（2026-11-09）：`source_type='STOCK_TRANSFER'` 凭证 **0** 张、`biz_stock_transfer`
无 `voucher_id` 列、DONE 单 3 张、调拨流水不配对 **0** 行。

**唯一的例外情形**（写清楚免得日后被「为什么不做」问倒）：跨法人 / 跨账套调拨、内部加价调拨
在会计上是内部交易（视同销售 / 内部往来），**需要**凭证；本系统当前不支持这两种形态，
现有 `biz_stock_transfer` 只表达「同一账套内的仓间移位」。真要做，得先加「内部结算价 /
往来科目」字段，而不是给现在的调拨单硬挂一张 1405/1405 的空凭证。

---

## 8. 在途库存对账（M2-05 · 在途 + 已出 = 发货总量）

> 适用范围：`biz_inventory_in_transit`（V0420）这本**在途账**对不对得上发货单。
> 接口 `GET /business/erp/inventory-transit/reconcile`（权限 `business:erp:inventory-transit:audit`），
> 实现 `backend/app/src/business/erp/inventory_transit/reconcile.rs`。

### 8.1 对账式（唯一口径，三侧比对）

```text
发货总量 item_qty       = Σ biz_shipment_item.qty                    -- 发货单明细（SSOT）
已出     received_qty   = Σ biz_inventory_in_transit.received_qty    -- status <> 'CANCELLED'
在途     qty_in_transit = Σ biz_inventory_in_transit.qty_in_transit  -- status <> 'CANCELLED'

① 行恒等（库层保证）：shipped_qty = received_qty + qty_in_transit    -- V0420 生成列
② 对账式（本文）   ：item_qty    = received_qty + qty_in_transit
   ⇒ 等价于 item_qty = shipped_qty（在途账记的发货量 = 发货单发的量）
```

**粒度必须是 `(发货单 × 物料)`**：

* `biz_inventory_in_transit` 的唯一键是 `(material_id, warehouse_id, shipment_id)`，
  一行就是「这张发货单的这个物料」；
* 「发货总量」取**明细**（`biz_shipment_item`），不取单头 `total_qty` —— 单头是明细的汇总
  字段（自己也可能被写坏），而且单头粒度回答不了「哪一行物料错了」。

### 8.2 差异码（六个，四个方向都要查）

| issue 码 | 含义 | 硬问题？ |
|---|---|:--:|
| `QTY_MISMATCH` | 两侧都有行，但发货量不等（过账漏写 / 明细被改 / 在途量被手工改） | ✅ |
| `MISSING_TRANSIT` | 有明细、**没有**在途行（在途写入被绕过；或 M2-02 之前过账的历史单） | ✅ |
| `ORPHAN_TRANSIT` | 有在途行、明细里没有这个物料（明细被删 / 物料挂错） | ✅ |
| `IDENTITY_BROKEN` | 行级 `shipped ≠ received + in_transit`（绕过生成列的写入，手工改库 / 刷数） | ✅ |
| `MULTI_WAREHOUSE` | 同一 `(发货单, 物料)` 在多仓都有在途行（同一批货记了两份） | ✅ |
| `CANCELLED_ONLY` | 只有在途作废行、没有有效行（整批作废：**不是错**） | ❌ |

`CANCELLED_ONLY` 只标注、不计入 `mismatched`、也不进对账式的量（V0420 口径：
作废行既不算在途也不算已出）。

### 8.3 范围的三个边界（写清楚，免得被当成 bug 提单）

1. **只对「已过账」的发货单对账**：明细侧 SQL 恒带 `s.status = 'POSTED'`。
   `DRAFT` / `PENDING_POST` 的单有明细但**本来就不该有在途行**（发货过账才进在途），
   不排除它们会让每张草稿单都报 `MISSING_TRANSIT`，满屏假红。
2. **`MISSING_TRANSIT` 会报历史单，这是诚实**：M2-02 之前发货过账 = 立即出库，
   没有在途账这一层（V0420 才建表）。那些历史单在本报告里必然表现为「有明细、无在途行」——
   不要「修」它（补一行假在途账等于伪造历史），**按 `ship_date_from/ship_date_to` 收到
   在途账上线之后的区间再对账**即可。
3. **未纳入在途账的组不进 `diff`**：它们单列成 `untracked` 组数与 `untracked_item_qty`
   发货量。把它们并进差额，真差异就会永远埋在一个非零数里。

### 8.4 只读 / 截断 / 汇总

* **只读**：`reconcile.rs` 一行写 SQL 都没有。对账的「修」是业务动作（重新过账 / 补签收 /
  改单据），三侧都「有理」，谁是真相只有业务知道 —— 这与 `inventory_audit`（一物一码巡检，
  有唯一参照物「序列号件数」、可 `dry_run=false`）刻意不同，**不要**给它加自动修复。
* **截断**：一次最多比对 2000 个 `(发货单 × 物料)` 组（多取一行判 `truncated`）。
  `truncated=true` 时 `balanced` **恒为 false**（没扫到的组里可能就有差异），
  调用方必须按发货日期区间收窄再对，绝不能把半份结论当全量。
* **`balanced` 的判据**：`零硬问题 && 对账差额 = 0 && 未截断`。

### 8.5 手工核对 SQL（没有接口时可直接抄）

```sql
-- 差异行；期望 0 行（口径与接口一致：明细 POSTED、在途非作废、FULL OUTER JOIN 两个方向）
WITH items AS (
    SELECT si.shipment_id, si.material_id, SUM(si.qty) AS item_qty
      FROM biz_shipment_item si
      JOIN biz_shipment s ON s.id = si.shipment_id
     WHERE s.status = 'POSTED'
     GROUP BY 1, 2
), transit AS (
    SELECT t.shipment_id, t.material_id,
           SUM(t.shipped_qty)     FILTER (WHERE t.status <> 'CANCELLED') AS shipped_qty,
           SUM(t.received_qty)    FILTER (WHERE t.status <> 'CANCELLED') AS received_qty,
           SUM(t.qty_in_transit)  FILTER (WHERE t.status <> 'CANCELLED') AS qty_in_transit,
           count(*)               FILTER (WHERE t.status <> 'CANCELLED') AS active_lines
      FROM biz_inventory_in_transit t
     GROUP BY 1, 2
)
SELECT s.ship_no, m.material_code,
       COALESCE(items.item_qty, 0)       AS 发货总量,
       COALESCE(transit.received_qty, 0) AS 已出,
       COALESCE(transit.qty_in_transit, 0) AS 在途,
       COALESCE(items.item_qty, 0)
         - COALESCE(transit.received_qty, 0)
         - COALESCE(transit.qty_in_transit, 0) AS 差额
  FROM items FULL JOIN transit USING (shipment_id, material_id)
  JOIN biz_shipment s ON s.id = COALESCE(items.shipment_id, transit.shipment_id)
  LEFT JOIN biz_material m ON m.id = COALESCE(items.material_id, transit.material_id)
 WHERE COALESCE(transit.active_lines, 0) > 0        -- 整批作废的组不算
   AND COALESCE(items.item_qty, 0)
       <> COALESCE(transit.received_qty, 0) + COALESCE(transit.qty_in_transit, 0);
```

演练：`bash scripts/verify/m205_transit_reconcile_drill.sh`（克隆库 + 迁移幂等/回滚 +
对账真库用例 10 条 + 三道权限/菜单闸与基线 diff）。

---

## 9. 相关文件

| 文件 | 作用 |
|---|---|
| `backend/app/src/business/erp/inventory_audit.rs` | 库存巡检（不变式③）：`book_qty = SUM(所有桶)`、桶数诊断、修复分流 |
| `backend/app/src/business/erp/inventory_writer.rs` | 三本账唯一写入口（含巡检修正语句 `align_cost_qty_*`） |
| `backend/app/src/business/erp/inventory_transit/reconcile.rs` | **在途对账（§8）**：三侧比对 + 六个差异码 + 截断/汇总口径（只读） |
| `backend/app/src/business/erp/shipment/shipment_transit.rs` | 在途账**唯一写入口**（M2-02 `record_shipped` / M2-03 `receive_open`） |
| `scripts/verify/erp_invariants.py` | 巡检（B1 / B1b / B4 已按本文口径固化） |
| `scripts/verify/m205_transit_reconcile_drill.sh` | 在途对账演练（迁移幂等 / 回滚 / 真库用例 / 权限菜单闸） |
| `scripts/verify/biz_type_domain.py` | `biz_type` 值域巡检（词表＝`doc_kind.rs`） |
| `backend/crates/cloudpivot-doc/src/lib.rs` | 业务类型词表唯一真相（零依赖 crate；`business/shared/doc_kind.rs` 已改为转发垫片） |
| `docs/evidence/20261107-p003-inventory-chain/09_reconcile.sql` / `13_final_reconcile.sql` | P0-03 演练对账 SQL（**已是正确口径**，与本文一致） |
| `backend/app/migrations/business/V0330__inventory_biz_type_and_sourceless_in_type.up.sql` | 本卡迁移（销售出库 `SALE_OUT` 回填 + `SOURCELESS` 语义） |
| `backend/app/migrations/business/V0420__inventory_in_transit.up.sql` | 在途账表（在途量 = 生成列，M2-01） |
| `backend/app/src/business/erp/stockin/stockin_handler_post.rs` | 收货过账 ⑦：PURCHASE / SOURCELESS 生成应付 + 凭证（§5.1 口径；注释里有指针） |
| `backend/app/src/business/erp/sales/sales_return_posting.rs` | 销售退货回库的**另一条**过账事务（借 1405 / 贷 6401，不挂应付） |
| `backend/app/src/business/erp/transfer/transfer_settle.rs` | 调拨过账：两条成对流水 + 三本账，**不生成凭证**（§7） |

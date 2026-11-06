# SO-08 · 发货单表 + 迁移（V0283）· 证据包

卡：`t_2e4166ae`（父卡 `t_1ae4c684` SO-07 销售订单前端）｜执行：backend-developer｜日期：2026-11-06 CST

## 1. 交付物

| 类型 | 文件 |
|------|------|
| 迁移（up） | `backend/migrations/business/V0283__sales_shipment.up.sql` |
| 迁移（down） | `backend/migrations/business/V0283__sales_shipment.down.sql` |
| 数据层（取号 + 状态域 + 超发守卫 + 单测） | `backend/src/business/erp/shipment_model.rs` |
| 模块注册 | `backend/src/business/erp/mod.rs`（`pub mod shipment_model;` + 文档表一行） |

**迁移号更正**：卡面写 `V0263__sales_shipment.up.sql`，但 V0263 已被
`V0263__sales_customer_menu_label`（SO-03 前端菜单卡）占用，当前最大号 V0282 ⇒ 本卡落 **V0283**。
（与 SO-04 同样的号段冲突，处理口径一致：顺延、不复用已应用号段。）

## 2. 两条硬验收

### ① 迁移 APPLY_OK（走引擎，不是手灌 SQL）

`REPO=/opt/cloudpivot ERP_DB=cloudpivot_so08 RECREATE=1 PORT=18110 bash scripts/verify/erp_clean_db.sh`
——干净库 → 一个实例启动 → 引擎把 `migrations/` 全部应用（含本次 V0283）：

```
==> 库指纹
    schema_migrations(max|count) = 283|211
```

`schema_migrations` 末 4 行（`migration-apply.txt` 末尾）：

| version | kind | applied_via | checksum16 | ms | success |
|---------|------|-------------|-----------|----|---------|
| 280 | versioned | executed | 28c53ef197f4f837 | 0 | t |
| 281 | versioned | executed | 99f1a25e21d6b3af | 5 | t |
| 282 | versioned | executed | dca0c4c33753de93 | 0 | t |
| **283** | **versioned** | **executed** | **68246cae1290ff60** | **13** | **t** |

即卡面要求：`kind=versioned`、`applied_via` 只有 `executed`。

结构实测 `schema.txt`（`\d` 原文）：15 列 / 10 列、5 + 3 条索引、3 条 CHECK、
3 条外键（头和明细各就位，`uk_biz_shipment_no` 是 `WHERE deleted_at IS NULL` 的部分唯一索引）、
`biz_seq(seq_type='SH')` = `SH|yyyyMMdd|-|1|1`。

### ② 超发校验生效 + 库层守卫

**(a) 应用层守卫**（`shipment_model::ensure_qty_within_remaining`，SO-09 建单必须调用）
`round-1-unit-db.txt` / `round-2-unit-db.txt`（真库，连跑两轮，各 **11 passed / 0 failed**）：

```
[SO-08] 超发探针：订单 10 / 已发 4 ⇒ 未发 6.0000
test result: ok. 11 passed; 0 failed; 0 ignored; 0 measured; 2740 filtered out
```

用例覆盖：7 拒 / 6 放行 / 6.0001 拒；草稿单占满 6 后 0.0001 也拒；改单排除本单后重新放行；
作废（CANCELLED）即释放占用；**POSTED 不重复扣**（已计入 `delivered_qty`，不再算占用）；
`shippable_items` 只返回未发完的行。

**(b) 库层探针**（`constraint-probes.sql` → `constraint-probes.out.txt`，干净库上逐条跑）：

| 探针 | 期望 | 实测 |
|------|------|------|
| A `status='SHIPPED'`（销售订单终态） | 拒 | `violates check constraint "ck_shipment_status"` |
| B `total_qty=-1` | 拒 | `violates check constraint "ck_shipment_total_qty"` |
| C 不传 status/total_qty | 通过 | `DRAFT / 0.0000` |
| D 明细 `qty=0` | 拒 | `violates check constraint "ck_biz_shipment_item_qty"` |
| E `so_item_id` 指向不存在订单行 | 拒 | `violates foreign key constraint "fk_biz_shipment_item_so_item"` |
| F 超发判定（10 − 4 = 6） | 7 → BLOCKED、6 → ALLOW、6.0001 → BLOCKED | 完全一致（`remaining=6.0000`） |
| G 发 6（合法）+ `serial_nos` 6 个 | 通过 | `INSERT 0 1`，`serial_count=6` |
| H 删被引用的订单行 | 拒 | `violates RESTRICT setting ... (23001)` |
| I 存活行重号 | 拒 | `duplicate key ... "uk_biz_shipment_no"` |
| J 软删后同号复插 | 通过 | 存活行 = 1 |
| K 删单头 | 明细级联消失 | `items_left = 0` |
| L 收尾 | 无残留 | `shipments=0, shipment_items=0, probe_orders=0, probe_customers=0` |

**(c) 回滚演练**（`rollback-drill.txt`，事务内 down → up → ROLLBACK，不留副作用）：
down 后两表 `<dropped>`、`biz_seq(SH)=0`；up 重放后表 / `uk_biz_shipment_no` / 3 条 CHECK / 取号行全部回来。

## 3. 门禁

| 门禁 | 结果 | 证据 |
|------|------|------|
| `cargo fmt -- --check` | rc=0（先 fmt 后 check） | `FMT_OK` |
| `cargo clippy --all-targets --features test-hooks -- -D warnings` | 见 §3 附注 | — |
| `cargo test --lib --features test-hooks business::erp::shipment_model`（真库 `ERP_DB_URL`，`--test-threads=1`） | **11 passed ×2 轮** | round-1 / round-2 |
| `cargo test --lib --features test-hooks`（全量，真库） | 见 §3 附注 | — |

前端无改动 ⇒ `pnpm typecheck && pnpm build` 不适用；无 API 改动（本卡只交付表 + 取号 + 超发守卫，
接口在 SO-09）⇒ 真实 token curl 不适用。

## 4. 设计口径与踩到的坑（下游卡必读）

1. **表名/列名以卡面为准**：`biz_shipment` / `biz_shipment_item` / `ship_no` / `so_id` / `so_item_id` /
   `total_qty`，与 ARCH-7 §3.1.5 一致，无第三种写法。
2. **卡面没点名但建了 `stock_out_id`**：ARCH-7 §3.1.5 有这一列，SO-10（过账 → 生成销售出库单）需要它
   回链出库单；现在不建就要在 SO-10 再开一次迁移改表。可空、无外键（否则过账事务成环）。
3. **外键口径**：`so_id` 与 `so_item_id` **ON DELETE RESTRICT**（单据期间不许物理删）；
   `shipment_id` **CASCADE**；`customer_id` / `warehouse_id` / `material_id` / `location_id` 是弱引用（无外键，
   主数据软删后历史单据仍可读）。
   ⚠ **`ON DELETE RESTRICT` 抛的 SQLSTATE 是 23001 `restrict_violation`，不是 23503
   `foreign_key_violation`** —— 迁移自检里只写 `WHEN foreign_key_violation` 会捕不到、整个 DO 块炸掉
   （首轮 APPLY 就栽在这，报 `query failed: : db error`，引擎把 RAISE 文案吞了）。
4. **超发是跨表判定，CHECK 表达不了**：库层只兜 `qty > 0` 与 `total_qty >= 0`；
   「不超过订单行未发货量」由 `shipment_model` 守（本仓迁移里零 TRIGGER，业务判定一律在 Rust）。
5. **未发货量口径**（唯一）：`订单行.qty − delivered_qty − 其他 DRAFT/PENDING_POST 发货单占用`。
   **POSTED 不能再算占用**（已通过 `delivered_qty` 体现），否则一张单过账后扣两次；
   CANCELLED 自动释放。判定必须先 `lock_so_item`（`SELECT … FOR UPDATE`）再算，否则并发建单会双通过。
6. **本卡不登记 `LEGAL_EDGES`**（与 SO-04 分工一致）：SO-09 落 API 时登记 `DocKind::Shipment` +
   `SHIPMENT_EDGES`（4 条边：`DRAFT→PENDING_POST→POSTED`、`DRAFT/PENDING_POST→CANCELLED`），
   边表域值必须与 `shipment_model::STATUSES` / `ck_shipment_status` 逐字一致（单测已钉住后者）。
7. **索引名统一带 `shipment` 全词**：PG 索引名 schema 级唯一，撞名 + `IF NOT EXISTS` 会被静默跳过
   （SO-04 踩过 `uk_biz_so_no` 被 `biz_stock_out` 占用）。
8. **`Value::Text` 不能直绑 numeric**：SQL 里写 `?::text::numeric`（真库用例已按此写）。

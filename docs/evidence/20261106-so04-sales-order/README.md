# SO-04 · 销售订单表 + 迁移（V0264）· 证据包

卡：`t_f5f501a7`（父卡 `t_3f4c05ae` 客户档案前端）｜执行：backend-developer｜日期：2026-11-06 CST

## 1. 交付物

| 类型 | 文件 |
|------|------|
| 迁移（up） | `backend/migrations/business/V0264__sales_order.up.sql` |
| 迁移（down） | `backend/migrations/business/V0264__sales_order.down.sql` |
| 数据层（取号 + 域守卫 + 单测） | `backend/src/business/erp/sales_model.rs` |
| 模块注册 | `backend/src/business/erp/mod.rs`（`pub mod sales_model;` + 文档表两行） |

**迁移号更正**：卡面写 `V0261__sales_order.up.sql`，但 V0261 已被 `V0261__sales_customer_permissions`（SO-02）
占用，V0262/V0263 被 SO-03 前端菜单占用 ⇒ 本卡落 **V0264**（父卡交接的 hotspot 也点名「后续从 V0264 起」）。

## 2. 三条硬验收

### ① 迁移 APPLY_OK（走引擎，不是手灌 SQL）

`bash scripts/verify/erp_clean_db.sh`（ERP_DB=cloudpivot_v0264_so04，RECREATE=1，PORT=18098）——
干净库 → 一个实例启动 → 引擎把 `migrations/business/` 全部应用：

```
==> 库指纹
    schema_migrations(max|count) = 264|206
```

`migration-applied-rows.txt`：

| version | kind | applied_via | checksum16 |
|---------|------|-------------|-----------|
| 262 | versioned | executed | 1ce2400b1115ef0c |
| 263 | versioned | executed | 2f3c117c2df7079b |
| **264** | **versioned** | **executed** | **a486432cf8b02b3a** |

即：`kind=versioned`、`applied_via=executed`（卡面要求 3），checksum 是引擎按文件内容算的 sha256。

表结构实测见 `schema-biz_sales_order.txt` / `schema-biz_sales_order_item.txt`；
索引 8 条全在位（`uk_biz_sales_order_no` 是 `WHERE deleted_at IS NULL` 的部分唯一索引）。

### ② 单据号能生成（真库，真 `biz_seq` 原子取号）

`round-1-unit-db.txt` / `round-2-unit-db.txt`（连跑两轮，各 10 passed / 0 failed）：

```
[SO-04] 取号实况：SO-20261106-000007 → SO-20261106-000008（period_key=20261106）
test result: ok. 10 passed; 0 failed; 0 ignored; 0 measured; 2700 filtered out
```

第二轮接着 `SO-20261106-000009 → SO-20261106-000010`（跨轮连续，说明号段没被重置/没撞号）。
落库 `biz_seq(seq_type='SO')`：`current_no=10, period_key=20261106`（CST 当天）。

### ③ 金额约束生效（`ck_sales_order_amount`）

`constraint-probes.sql` → `constraint-probes.out.txt`，每一条都是「插坏数据必须被拒」的行为断言：

| 探针 | 期望 | 实测 |
|------|------|------|
| A 100 + 13 = **114**（差 1） | 拒 | `ERROR: violates check constraint "ck_sales_order_amount"` |
| B 100 + 13 = 113 | 通过 | `INSERT 0 1`，读出 `DRAFT / 100.0000 / 13.0000 / 113.0000` |
| C `status='SHIPPED'`（技术架构旧稿值） | 拒 | `ERROR: violates check constraint "ck_sales_order_status"` |
| D `delivered_qty=11 > qty=10` | 拒 | `ERROR: violates check constraint "ck_sales_order_item_delivered"` |
| E `qty=0` | 拒 | `ERROR: violates check constraint "ck_sales_order_item_qty"` |
| F 存活行同 `so_no` 插两次 | 拒 | `ERROR: duplicate key value violates unique constraint "uk_biz_sales_order_no"` |
| G 软删后同号重插 | 通过 | 两次 `INSERT 0 1`，存活行 count=1 |
| H 删单头 → 明细 | 级联消失 | `items_left = 0` |
| I 收尾 | 无残留 | `orders=0, items=0` |

回滚演练：`rollback-drill.txt`（隔离库 down → 结构与取号行 `0 rows` → 再 up 重放成功）。

## 3. 门禁

| 门禁 | 结果 | 证据 |
|------|------|------|
| `cargo fmt -- --check` | rc=0 | 无 diff 输出 |
| `cargo clippy --all-targets --features test-hooks -- -D warnings` | rc=0，无警告 | `Finished dev profile in 3m 32s` |
| `cargo check --lib --features test-hooks` | rc=0 | — |
| `cargo test --lib business::erp::sales_model`（真库 ERP_DB_URL，`--test-threads=1`） | **10 passed / 0 failed ×2 轮** | round-1/round-2 |
| 闸④ `biz_type_domain.py --strict` | PASS | `gate04-biz_type_domain.txt` |
| 闸⑧ `role_code_gate.py` | PASS（无新增/改动角色码） | `gate08-role_code.txt` |
| 闸③ `erp_invariants.py`（同库） | 67/68，0 BLOCKER；唯一 WARN = 新库 `admin` 无部门（bootstrap 种子现状，非本卡引入） | — |

前端无改动 ⇒ `pnpm typecheck && pnpm build` 不适用；无 API 改动 ⇒ 真实 token curl 不适用
（本卡只交付表 + 取号函数，接口在 SO-05）。

## 4. 设计口径与踩到的坑（下游卡必读）

1. **表名/列名以卡面为准**：`biz_sales_order` / `so_no` / `so_id` / `delivered_qty`。
   `docs/ERP-批次2-技术架构.md` §2.2 写的是 `biz_sale_order` / `order_no` / `shipped_qty` —— 旧稿，别照抄。
2. **状态域 7 值取 PRD**：`DRAFT/SUBMITTED/APPROVED/PARTIAL_OUT/OUTBOUND/CLOSED/CANCELLED`。
   技术架构稿的 `PARTIAL_SHIPPED`/`SHIPPED` 与 PRD 全文矛盾，**不采纳**（迁移自检会把 `SHIPPED` 当越界值拒掉）。
3. **索引名撞名坑（本卡实测踩到）**：`uk_biz_so_no` / `idx_biz_so_status` / `idx_biz_so_date` 已被
   **既有 `biz_stock_out`** 占用（它有 `so_no` 列）。PG 索引名是 schema 级唯一，重名 + `IF NOT EXISTS`
   会被**静默跳过** ⇒ 唯一索引根本不建。已统一改成 `uk_biz_sales_order_no` / `idx_biz_sales_order_*`。
   **后续卡新建索引前先 `select indexname from pg_indexes where indexname='…'`。**
4. **`Value::Text` 不能直接绑 numeric 列**（`cannot bind parameter to column type numeric`）：
   SQL 里写成 `?::text::numeric`（与 `purchase_model::insert_items` 的 `NULLIF(?::text,'')::numeric` 同款）。
5. **`customer_id` 是弱引用（无外键）**：与 `biz_purchase_order.supplier_id` 同风格（客户软删，历史单据仍可读）。
   明细 `so_id` 是硬外键 **ON DELETE CASCADE**（明细离开单头无独立意义）。
6. **本卡不建状态迁移入口**：`LEGAL_EDGES` 的唯一真相在 `business/shared/state_machine.rs`，登记新实体要先在
   `shared/doc_kind.rs` 的 `DocKind` 加枚举值（被闸④逐字比对）。SO-05 落 API 时再登记
   `DocKind::SaleOrder` + `SALES_ORDER_EDGES`，届时边表域值必须与 `sales_model::STATUSES` 逐字一致。

# E2E-FIX-2 证据：库存巡检 `book_qty` 口径 + 修复路径（t_6a79f456）

来源：E2E-03 演练（t_540eae91）发现的 P1 —— `inventory_audit.rs` 的 `book_qty` 写成了
`COALESCE(MAX(qty) FILTER (WHERE location_id IS NULL), SUM(qty))`，两桶并存时报假差异；
`dry_run=false` 的修复会把未分配桶覆盖成 `actual`，**抹掉库位桶 → 账面被改错且不可逆**。

## 1. 根因（一句话）

`biz_inventory.location_id IS NULL` 是「未分配库位」这一个**真实桶**，不是整仓汇总行。
`docs/ERP-库存对账口径.md` §1/§2.1 早就定了整仓数量 = `SUM(qty)`；
`transfer_settle::assert_settle_invariants` 第 4 条（`SUM(库位账) == SUM(成本账)`）也是按桶相加。
写成「有未分配桶就取那一桶」等于把其余桶全部无视。

## 2. 改动

| 文件 | 改动 |
|---|---|
| `backend/src/business/erp/inventory_audit.rs` | `book` CTE 改 `SUM(i.qty)` + `COUNT(*) AS bucket_count`；`AuditItem` 增 `bucket_count` / `unfixable_reason`；`fixable = 有仓库 && bucket_count <= 1`；模块头改口径说明 |
| `backend/src/business/erp/inventory_writer.rs` | 修正拆两条集合语句：`align_cost_qty_delta_sql`（`bucket_count = 1` → `qty += actual - book`，`amount = 新 qty × avg_cost`）、`align_cost_qty_insert_sql`（`bucket_count = 0` → 建档落未分配桶）；`align_cost_qty_batch` 返回两者之和 |
| `backend/src/business/erp/transfer_settle.rs` | 同一口径的**第二处**：`assert_settle_invariants` 第 2 条的 `book_qty` 也改 `COALESCE(SUM(b.qty), 0)`（否则调拨结算在两桶并存时假红并整单回滚） |
| `backend/src/business/erp/handler.rs` | 接口文档注释同步（口径 / 两条修正语句） |
| `frontend/src/api/business/erp/index.ts` | `InventoryAuditItem` 增 `bucket_count` / `unfixable_reason` |
| `frontend/src/views/business/erp/inventory/{index.vue,inventory-logic.ts}` | 新增「成本账桶数」列；`auditFixableLabel(fixable, reason)` 区分「多桶并存·需人工」与「缺仓库归属」；结论文案同步 |
| `frontend/tests/unit/erp-inventory.test.ts` | 覆盖 `MULTI_BUCKET` 文案与结论文案 |
| `docs/ERP-库存对账口径.md` | §1 增「NULL 桶不是汇总行」警示；新增 §6（巡检口径 / 修复语义 / ADR-20261031 关系 / 回归用例），相关文件表补两行 |

**没有迁移**：本次只改 SQL 与 DTO，不动表结构（迁移数仍为 353 / 286 条）。

## 3. 真库单测（99 · 隔离库 `cloudpivot_e2efix2`，`ERP_DB_URL` 已注入）

```
cargo test --lib -- --test-threads=1 business::erp::inventory_audit
test result: ok. 16 passed; 0 failed; 0 ignored; 3847 filtered out; finished in 10.37s
```

新增/强化的 4 条（其余为原有回归）：

| 用例 | 断言 |
|---|---|
| `book_qty_sums_all_buckets_no_false_drift` | 两桶 2 + 1、在库 3 → `mismatched = 0`；`dry_run=false` 也不动账（两桶原样） |
| `multi_bucket_mismatch_is_never_auto_fixed` | 两桶 2 + 1、在库 4 → `book=3/actual=4/diff=-1`、`bucket_count=2`、`fixable=false`、`MULTI_BUCKET`；`dry_run=false` 后 `fixed=0` 且两桶逐字不变；再巡检仍报同一条 |
| `single_bucket_fix_is_delta_not_overwrite` | 单桶 9 → 修成 `qty=3.0000`、`amount=7.5000`（3×2.5），不新增第二个桶 |
| `dry_run_false_fixes_quantities`（原有） | 单桶差额 + 零桶建档（反向巡检）仍可修，`bucket_count` 分别为 1 / 0 |

`10.37s` 的耗时说明是真库执行（未注入 `ERP_DB_URL` 会静默 SKIP、一秒返回）。

## 4. 145 真机 curl 实测（隔离实例，不碰 `cloudpivot_dev` / `:8033`）

- 机器：`172.18.10.145`，全新库 `cp_e2efix2`（迁移 353 / 286 条），实例 `127.0.0.1:18318`，测完停实例、库保留。
- 场景全部走**真实 HTTP + 真实 token**（`POST /api/business/erp/serial/batch-generate`）：
  1. 一物一码物料批量生成 3 件（不带 `location_id` → 未分配桶）；
  2. 同仓再入库 3 件（带 `location_id` = 默认库位行 → 默认库位桶）。
- 断言（原始输出见 `live-145.txt`，PASS=7 / FAIL=0）：

| 验收项 | 实测 |
|---|---|
| ② `dry_run=true` | `checked=1, mismatched=0` —— 两桶 3+3、在库 6 → **零漂移**（旧口径 `book=3` → 假差异 -3） |
| ③ 多桶 + 真差异 `dry_run=false` | `bucket_count=2 / fixable=false / unfixable_reason=MULTI_BUCKET`，`fixed=0`，账面 `(未分配)=9.0000, 库位桶=3.0000` 前后逐字相同 |
| ④ 单桶修复 | 单桶物料账面改 9（在库 3）→ `fixed=1`，修后 `qty=3.0000 / amount=30.0000 / buckets=1` |
| 一致时 `dry_run=false` | `fixed=0`，两桶原样 |

同一库同一状态下的口径对比（SQL，`cp_e2efix2` 保留可复验）：

```
新口径 SUM(所有桶)             = 6.0000   （= 在库序列号 6）
旧口径 COALESCE(MAX(NULL桶),SUM) = 3.0000   （假差异 -3）
```

## 5. 为什么「多桶拒绝自动修」而不是「差额记到某一个桶」

桶的落点是**业务语义**（ADR-20261031 的批量生成落未分配桶、FIX-4 的收货落默认库位桶），
代码没有依据替业务决定差额该落在哪一桶：

- 记未分配桶 → `docs/ERP-库存对账口径.md` §2.2 的逐桶对账（B1b）立刻错位；
- 记某个库位桶 → 凭空猜。

所以多桶一律 `MULTI_BUCKET` 交人工，报告里 `bucket_count` 直接告诉运维「这里有几个桶」。

## 6. 复跑方式

```bash
# 单测（真库）
ERP_DB=cloudpivot_e2efix2 RECREATE=1 bash scripts/verify/erp_clean_db.sh
ERP_DB_URL='postgres://alun:alun@127.0.0.1:5432/cloudpivot_e2efix2' NIK_PROFILE=test \
  cargo test --lib -- --test-threads=1 business::erp::inventory_audit

# 145 真机（脚本：/tmp/e2efix2_live.sh，随卡未入库；逻辑见 §4）
```

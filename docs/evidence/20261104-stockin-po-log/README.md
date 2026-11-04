# stockin 夹具：采购单状态日志孤儿（卡 `t_8ec0a1af`）

> 一句话结论：**根因修复 + 断言补口 + RED 证明 + 3 轮干净库验收**全部落地。
> 干净隔离库 `cloudpivot_8ec0a1af`（迁移 max V0219 / 163 条、超管 1）上
> `--lib business::erp::stockin_handler -- --test-threads=4` 连跑 3 轮，
> 每轮 **134 passed / 0 failed**，轮间 psql 复查
> `biz_status_log(entity_type='PURCHASE_ORDER')` 孤儿 **恒为 0**（不是「不增长」，是 0）。

## 1. 现象（上游卡 `t_25926f63` 并发自检抓到）

同一隔库上跑 4 模块 ×28 次，`PURCHASE_ORDER` 状态日志孤儿线性增长：

| 时点 | stockin 轮次 | PURCHASE_ORDER 孤儿 |
|---|---|---|
| Phase A 结束 | 3 | 105 |
| Phase B SEED | 4 | 140 |
| Phase B 结束 | 7 | 245 |

⇒ 每轮 +35 条，无上界；而 STOCK_IN / PAYABLE / VOUCHER / CONTRACT / ACCOUNTING_PERIOD 均为 0。

## 2. 根因（行级）

`backend/src/business/erp/stockin_handler_tests_fixtures.rs` 的 `cleanup_fixtures()`：

```rust
// 修复前
"DELETE FROM biz_purchase_order WHERE supplier_id IN ('{SUPPLIER}'::uuid, '{A4_SUPPLIER_B}'::uuid)"
```

收货过账会推进采购单状态（`SUBMITTED → PARTIAL_RECEIVED / RECEIVED`，见
`stockin_purchase.rs`，走 `state_machine` 写 `biz_status_log`），而 `biz_status_log`
**没有外键**（刻意的审计留痕表）⇒ 采购单本体删了，状态日志留在库里。
`stockin_handler_tests_helpers.rs:191 / 795` 造单写的 `entity_no = B1-{id 后 8 位}`，
与上游证据里的抽样行完全对得上。

## 3. 改法（4 处）

### 3.1 根因：删采购单**之前**按 id 集合删日志（`stockin_handler_tests_fixtures.rs`）

```rust
format!(
    "DELETE FROM biz_status_log WHERE entity_type = 'PURCHASE_ORDER' AND entity_id IN \
     (SELECT id FROM biz_purchase_order WHERE supplier_id IN ({}))",
    fixture_suppliers()
),
```

删除集口径与同批修复卡 `t_bfa81333` 一致：根 = **夹具供应商 id**（`fixture_suppliers()`），
沿 `biz_purchase_order.id` 取本模块历次运行造过的 id 集合。**不是** `entity_no LIKE 'B1-%'`
前缀扫（单号是弱引用，审计 S4），**也不是**「全库孤儿扫」（会删掉兄弟模块正在删单的
毫秒级中间态）。

### 3.2 断言补口：`zz_module_namespace_has_no_residue` 纳入 `PURCHASE_ORDER`

```rust
let orphans = fixture_hygiene::settle_orphans(
    &["STOCK_IN", "PURCHASE_ORDER"], std::time::Duration::from_secs(5)).await;
assert_eq!(orphans, 0, "STOCK_IN / PURCHASE_ORDER 状态日志不许留孤儿行（收敛等待 5s 后仍非 0）");
```

判据只能落在整库：采购单行一删，孤儿日志的 `entity_id` 就再没有任何字段能反查所属模块
（没有供应商可回溯）。所以用 `settle_orphans`（先收口再断言、吸收他人毫秒级窗口），
口径没有放宽 —— 真孤儿等多久都还在。

### 3.3 自证用例：`purchase_order_status_logs_are_purged_by_cleanup`

造采购单 → 建收货单 → 过账（状态机写日志）→ **断言日志 > 0** → 调模块真实的
`cleanup_fixtures()` → 断言该 `order_id` 的日志为 0、采购单本体也为 0。
不是另写一套 SQL，走的就是生产清场路径。

### 3.4 顺带收敛：`purchase_handler.rs` 的「全库孤儿扫」

```rust
// 修复前（最后半句是全库扫：本模块每跑一次 setup 就全表写一遍）
"DELETE FROM biz_status_log WHERE entity_type = 'PURCHASE_ORDER' AND ( \
     entity_id IN (SELECT id FROM biz_purchase_order WHERE supplier_id = '{SUPPLIER}'::uuid) \
  OR entity_id NOT IN (SELECT id FROM biz_purchase_order))"
```

改成只按本夹具供应商 id 集合删。历史孤儿由各模块自己的 `zz_` 收口
（`fixture_hygiene::settle_orphans`）。

## 4. 验收矩阵（干净库 `cloudpivot_8ec0a1af`，同一库连跑不重建）

基线（未跑任何用例）：PO 孤儿=0 / PO 日志总数=0 / 采购单行=0 / STOCK_IN 孤儿=0

| 轮次 | 用例 | failed | 耗时 | 轮后 PO 孤儿 | 轮后 PO 日志总数 | 轮后采购单行 | 轮后 STOCK_IN 孤儿 |
|---|---|---|---|---|---|---|---|
| R1 | 134 | 0 | 419.72s | **0** | 0 | 0 | 0 |
| R2 | 134 | 0 | 406.61s | **0** | 0 | 0 | 0 |
| R3 | 134 | 0 | 415.92s | **0** | 0 | 0 | 0 |

（用例数 133 → 134 = 本卡新增 1 条自证用例；`FAILED`/`panicked` 行数三轮均为 0。）

轮后再用 psql **独立复查**（不经测试代码）：
`PURCHASE_ORDER 孤儿 = 0`、`PURCHASE_ORDER 日志总数 = 0`、`采购单行 = 0`。

### 附：`purchase_handler` 模块回归（同一个库）

`--lib business::erp::purchase_handler -- --test-threads=4` ⇒ **50 passed / 0 failed（81.60s）**，
前后 PO 孤儿均 0。

## 5. RED 证明（断言确实覆盖这个缺口）

把修复语句的 `entity_type` 换成永不匹配的假值（等价「回到修复前」），只跑新用例：

```
test business::erp::stockin_handler::stockin_handler_tests_fixtures::purchase_order_status_logs_are_purged_by_cleanup ... FAILED
panicked at src/business/erp/stockin_handler_tests_fixtures.rs:584:5:
assertion `left == right` failed: 清场后采购单状态日志必须为 0（`biz_status_log` 没有外键 ⇒ 必须在删采购单之前按 id 删）
  left: 1
 right: 0
test result: FAILED. 0 passed; 1 failed; ... finished in 3.62s
```

⇒ 修复前红、修复后绿，缺口被真正守住（见 `redproof.txt`，三轮轮间输出见 `accept.txt`，
`purchase_handler` 回归见 `purchase_handler_regression.txt`）。

## 6. 复现

```bash
# 1) 干净隔离库（二进制来自验证树，独立树 + 独立 target）
REPO=/root/8ec0a1af-tree BIN=/root/8ec0a1af-target/debug/cloudpivot \
ERP_DB=cloudpivot_8ec0a1af PORT=18097 RECREATE=1 \
bash /opt/cloudpivot/scripts/verify/erp_clean_db.sh

# 2) 三轮
export ERP_DB_URL='postgres://alun:***@127.0.0.1:5432/cloudpivot_8ec0a1af'
cd /root/8ec0a1af-tree/backend
CARGO_TARGET_DIR=/root/8ec0a1af-target \
  cargo test --lib business::erp::stockin_handler -- --test-threads=4

# 3) 轮间复查（必须恒 0）
psql -h 127.0.0.1 -U alun -d cloudpivot_8ec0a1af -Atc \
 "SELECT count(*) FROM biz_status_log WHERE entity_type='PURCHASE_ORDER'
    AND entity_id NOT IN (SELECT id FROM biz_purchase_order)"
```

## 7. 边界

* 本卡只跑 **stockin 模块 ×3** + **purchase_handler 模块 ×1**，**没有**跑全量 `cargo test`
  （全量仍在 `t_9a6e3d35` 的口径里）。
* 验证树 `/root/8ec0a1af-tree` = `git archive HEAD` + 本卡这两个文件；
  共享工作树 `/opt/cloudpivot` 上别的卡未提交的 WIP（opening / payable_payment /
  serial_handler / voucher / contract_approval_tests / period_* / fixture_hygiene）
  **没有被我 commit**：`purchase_handler.rs` 只把自己那一 hunk 用 `git apply --cached` 入索引，
  别人的改动仍是未暂存状态。
* `purchase_handler.rs` 里 `po_instance_where()` 还有一处同族的「全库孤儿扫」
  （`biz_id IS NULL OR biz_id NOT IN (SELECT id FROM biz_purchase_order)`），
  本卡按卡面范围只收敛了 `status_log` 那条；如需一并收敛建议另开卡（会动审批实例清场语义）。

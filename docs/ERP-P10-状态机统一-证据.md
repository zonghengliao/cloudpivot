# ERP P10 · 状态机统一 + 入账后只读 + 状态变更日志 —— 验收证据

> 卡：TSK-202610-000871「P10：状态机统一 + 入账后只读 + 状态变更日志（铁律 ③④）」
> 代码：`583e7a0`（主体）+ `fbb9c94`（实测收口）+ `072f6e1`（调拨建单留痕断言）
> 本文件所有数字均为本次复跑的真实输出，不含推算。

## 一、三个洞与对应修法

| 洞（开工实测） | 修法 | 验证口径 |
|---|---|---|
| ① 状态机可被绕过：`purchase_model::LEGAL_EDGES` 只有 4 条边，而 `RECEIVED` 由 `stockin_model::sync_purchase_status` 一句 `UPDATE … CASE` 直接写 | 新增 `backend/src/business/shared/state_machine.rs`（唯一入口 `transition()`）；`sync_purchase_status` 改为「锁单 → 算目标态 → 变了才 transition → CAS」 | 单测 ⑭/⑮/⑯ + e2e ④/⑤ |
| ② 没有状态变更日志：只有 `created_by/update_by/updated_at` | 迁移 `V0159__biz_status_log`（只增不改：无 `deleted_at`/`update_by`/`updated_at`，只有 INSERT）+ 五个实体建单与流转全部留痕 | 单测 ①④⑥⑦ + e2e ①/④/⑤ |
| ③ 入账后还能改：`POSTED/RECEIVED/SETTLED` 下金额/数量/明细无硬约束 | `state_machine::ensure_writable()` 挂在各 `update_*` 入口；白名单只放 `status`/`updated_at`/`update_by`/`remark`/`approval_instance_id`/`voucher_id`/`payable_id` | 单测 ⑧–⑬ + ⑰⑱ + e2e ②/②b/③ |

## 二、与卡面的两处差异（都不是放宽，必须留痕）

1. **采购订单边表是 10 条，卡面写 8 条**（`state_machine::PURCHASE_ORDER_EDGES`）。
   多出的两条是 `SUBMITTED→PARTIAL_RECEIVED` / `SUBMITTED→RECEIVED`。
   原因：`APPROVED` 在本仓**没有任何写入方**（`purchase_dto::TRANSITION_TARGETS` 只放行
   `SUBMITTED/CLOSED/CANCELLED`，审批也没接采购单），而全库现存采购单的提交态就是 `SUBMITTED`
   （P2-B1 的 `purchase_receivable` 白名单本来就收 `SUBMITTED`）。只登记卡面 8 条会让**所有真实采购单收货 409**，
   等于打断 P9/P2-B1 的收货链路。所以这两条是「把既有生产行为登记进状态机」。
   `DRAFT → RECEIVED` 仍非法（单测 ⑯ 钉死），自环 `PARTIAL_RECEIVED→PARTIAL_RECEIVED` **故意不登记**
   （继续部分收货不是状态变更，由调用方判「状态没变就不调 transition」，只写收货流水）。
2. **多两道「静默丢字段」的防线**（`CreateStockInReq` 显式收下 `total_qty/total_amount/total_tax`）。
   卡面 §五-3 的验收请求 `PUT {"total_amount": 999}` 在修前是**假绿 200**：未知键被 `serde` 静默丢弃 →
   请求退化成「什么都没改」→ 走 `update_remark` 返回 200，金额其实没动。现在入账态值不等 → 409 且点名
   `total_amount`；草稿态值不等 → 400「由明细派生」；单测 ⑰⑱ + e2e ②b 三条断言钉死。

## 三、真实输出

### 1. 不变量套件（生产库，144）

```text
# 生产仓自带脚本（b0bfb8b 快照，29 条口径）
结果：29/29 通过   🚨 BLOCKER 0   ⚠️ WARN 0
★ 全部不变量成立，账目干净。

# main 最新脚本（P11 加了 H1/H2/H3，32 条口径）
✅ [审批] H1 实例已终态 → 待办必须终态
✅ [审批] H2 任务已取消/完成 → 审批实例必须终态
✅ [审批] H3 待办必能追溯到任务（未软删）
结果：32/32 通过   🚨 BLOCKER 0   ⚠️ WARN 0
```

### 2. 单测（99 主仓，`cargo test --lib`）

```text
running 1439 tests
test result: ok. 1439 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 271.35s
```

其中：
- `business::erp::*` **336 passed**（P9 交付基线 323 → 本卡 +13）
- `business::shared::state_machine::*` **9 passed**

### 3. 本卡 16 条用例 ↔ 实现的对应表

| 卡面 | 用例 | 位置 |
|---|---|---|
| ① | `legal_edge_writes_complete_log_row` | state_machine.rs |
| ② | `illegal_edge_is_conflict_and_names_targets` | state_machine.rs |
| ③ | `reason_required_edge_without_reason_is_bad_request` | state_machine.rs |
| ④ | `two_transitions_write_two_ordered_rows` | state_machine.rs |
| ⑤ | `unknown_entity_type_is_rejected` | state_machine.rs |
| ⑥ | `trail_by_entity_no_is_complete` | state_machine.rs |
| ⑦ | `log_rolls_back_with_transaction` | state_machine.rs |
| ⑧ | `p10_posted_amount_change_is_conflict` | stockin_handler.rs |
| ⑨ | `p10_posted_qty_change_is_conflict` | stockin_handler.rs |
| ⑩ | `p10_posted_items_change_is_conflict` | stockin_handler.rs |
| ⑪ | `p10_posted_remark_change_is_allowed` | stockin_handler.rs |
| ⑫ | `p10_draft_amount_change_is_allowed` | stockin_handler.rs |
| ⑬ | `posted_voucher_summary_change_is_conflict` | state_machine.rs |
| ⑭ | `p10_receipt_advances_order_and_logs_ref` | stockin_handler.rs |
| ⑮ | `p10_partial_receipt_advances_to_partial_received` | stockin_handler.rs |
| ⑯ | `purchase_order_draft_to_received_is_conflict` | state_machine.rs |
| 额外 | `p10_posted_total_amount_field_alone_is_conflict` / `p10_draft_total_amount_direct_is_bad_request` | stockin_handler.rs |
| 额外 | 调拨建单 `NULL→DRAFT` 轨迹断言 | transfer_handler.rs |

### 4. 真机 e2e（隔离实例 `127.0.0.1:18090` + 库 `cloudpivot_p10`，迁移到 V0159）

```text
$ BASE=http://127.0.0.1:18090 DB_USER=p10role DB_NAME=cloudpivot_p10 DB_PASS=*** \
  bash scripts/verify/p10_state_machine.sh
[PASS] 登录成功（admin）
[PASS] 夹具就位
[PASS] 建单 HTTP（200） / [PASS] 过账 HTTP（200）
[PASS] 状态日志行数（2）
[PASS] 第一行日志（NULL→DRAFT）
[PASS] 第二行日志（DRAFT→POSTED）
[PASS] 操作人姓名落库（admin）
[PASS] 改金额 HTTP（409） / [PASS] 文案点「已入账」
[PASS] 只有 total_amount 的改单 HTTP（409） / [PASS] 文案点名 total_amount
[PASS] 库里金额一毫未动（10.0000）
[PASS] 改备注 HTTP（200） / [PASS] 金额未被顺手改掉（10.0000） / [PASS] 备注已落库（P10E 只改备注）
[PASS] 采购单状态（RECEIVED）
[PASS] 采购单推进日志（SUBMITTED→RECEIVED|IN-20261031-000547|STOCK_IN）
[PASS] 部分收货后采购单状态（PARTIAL_RECEIVED）
[PASS] 部分收货推进日志（SUBMITTED→PARTIAL_RECEIVED）
[PASS] 状态日志零残留（0） / [PASS] 收货单零残留（0）
==> 汇总：PASS=31 FAIL=0
```

### 5. 迁移

```text
V0159__biz_status_log.up.sql   → 建表 + 3 索引 + 自检块（表/索引计数/「不许有 deleted_at|update_by|updated_at」）
V0159__biz_status_log.down.sql → 有日志行时主动中止（审计证据链不许静默丢）
grep -rEn '^\\[a-z]' backend/migrations/  → 0 命中（无 psql 元命令）
cloudpivot_p10: select max(version::int) → 159（应用本迁移后随其它卡推进到 170）
```

## 四、怎么复跑

```bash
# 单测（真库用例需 ERP_DB_URL 指向「已迁到 V0159 及以上」的隔离库）
cd backend && ERP_DB_URL='postgres://p10role:***@127.0.0.1:5432/cloudpivot_p10' cargo test --lib

# 真机 e2e：先起隔离实例（NIK_PROFILE=test + 独立库/端口），再跑
BASE=http://127.0.0.1:18090 DB_HOST=127.0.0.1 DB_USER=p10role DB_NAME=cloudpivot_p10 \
  DB_PASS=*** bash scripts/verify/p10_state_machine.sh

# 生产不变量（144）
ssh root@172.18.10.144 'cd /opt/cloudpivot && set -a; . /etc/cloudpivot/cloudpivot.env; set +a; \
  python3 scripts/verify/erp_invariants.py'
```

## 五、已知边界

- `biz_status_log` **只增不改**：没有 `deleted_at`、没有 UPDATE 路径、`down` 在有日志时主动中止。
  任何「补记历史」的需求都要新开迁移，不许 UPDATE 既有行。
- P9 的 `cancel_in_tx` 六件套（库存/库位账/流水/凭证/应付/序列号）与 P7 的付款/红冲均未改动语义，
  只在它们的「状态写库」那一步改成走 `state_machine::transition()`（同事务）。
- 采购单收货的 CAS 落空（并发把单子推走）现在返回 `PO_STATUS_RACE|` 哨兵 → 409 并整笔回滚，
  不会再出现「日志说推进了、单据没动」的假轨迹。

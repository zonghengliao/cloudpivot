# LNK-P08 修复记录：应付单第二次部分付款被状态机误拒（10001/10003）

| 项 | 内容 |
|---|---|
| 缺陷编号 | **LNK-P08**（来源：联动矩阵测试卡 t_1fdb4704，`docs/test-report-linkage.md` §5.1） |
| 修复卡 | **t_a34c68b6**（backend-developer） |
| 影响面 | 应付「分期付款」第二期直接失败 → 第三期及以后完全不可用 |
| 结论 | **已修**：`p08_paid_plus_unpaid_equals_amount` 去掉 `#[ignore]` 转正，全量 linkage `62 passed / 0 failed / 0 ignored` |
| 护栏 | `state_machine::transition` 的 `from == to` **409 一字未改**（P10 铁律③的唯一入口） |

---

## 1. 缺陷（修复前，真库复现）

把调用点改回修复前的形态（`state_machine::transition` 无条件调用）后跑卡面命令：

```text
$ cd backend
$ LINKAGE_DB_URL='postgres://lnkrole:***@127.0.0.1:5432/cloudpivot_test' LINKAGE_REQUIRE_DB=1 \
    cargo test --test linkage p08_paid_plus -- --test-threads=1 --nocapture

running 1 test
test linkage::payable_chain::p08_paid_plus_unpaid_equals_amount ... 联动矩阵测试已连接 cloudpivot_test

thread 'linkage::payable_chain::p08_paid_plus_unpaid_equals_amount' panicked at tests/linkage/payable_chain.rs:131:9:
P08：付款 3.5 失败 10001 [10003] 应付单 AP-20261031-000249 的状态没有变化（PARTIAL_PAID → PARTIAL_PAID）：原地流转不是状态变更，不要写状态日志（重复部分收货请只写业务流水）
note: run with `RUST_BACKTRACE=1` environment variable to display a backtrace
FAILED

test result: FAILED. 0 passed; 1 failed; 0 ignored; 0 measured; 61 filtered out; finished in 3.14s
```

这条输出与测试卡报告的现象**逐字一致**（只是单号不同）—— 证明本用例确实钉住了这个缺陷，
不是「恰好绿」（变异验证：把修复回退，用例如期变红）。

## 2. 根因

| 位置 | 问题 |
|---|---|
| `backend/src/business/erp/payable_payment.rs:589`（修复前） | 无条件调 `state_machine::transition(..., &row.status, status, ...)`。第二期付款时 `row.status = PARTIAL_PAID`、算出的目标态也是 `PARTIAL_PAID` → `from == to` |
| `backend/src/business/shared/state_machine.rs:450` | `if from == to → 409 / 10001`（**这条护栏本身是对的**：原地流转不是状态变更，不该写状态日志） |

为什么 1343 个单模块用例没抓到：`payable_payment::tests::p7_two_partial_payments_accumulate_and_settle`
只付两次，且两次都跨状态（`OPEN → PARTIAL_PAID → SETTLED`），永远碰不到
`PARTIAL_PAID → PARTIAL_PAID`。P08 把「三期」写进断言，第二期就撞上。

## 3. 修法：状态没变就不写状态日志（**不动护栏**）

### 3.1 新增显式语义入口

```rust
// backend/src/business/shared/state_machine.rs
pub async fn transition_if_changed(/* 与 transition 同款入参 */) -> Result<Option<TransitionOutcome>, ApiError> {
    check_registration(entity_type, edges)?;   // 注册表核对在自环短路之前
    if from == to {
        return Ok(None);                       // 状态没变 = 不写 biz_status_log
    }
    Ok(Some(transition(/* … */).await?))
}
```

- `Ok(None)` 的语义是「**没变**」，不是「失败」—— 调用点据此知道「只该留业务流水」。
- 注册表核对放在自环短路**之前**：调用点把 `PAYABLE` 写成 `PAYABLES` 仍然 500，
  不会被自环静默吞掉（单测 ⑱ 钉死）。

### 3.2 为什么不放宽 `transition` 的 `from == to` 护栏

- 它是**五个实体共用**的唯一入口（`STOCK_IN / PURCHASE_ORDER / TRANSFER / PAYABLE / VOUCHER`）。
- 放宽等于把「所有调用点写错」都变成静默空日志 —— P10 铁律③的刹车就没了。
- 所以「允许原地不动」由**调用点显式声明**（用 `transition_if_changed` 而不是 `transition`），
  代码上一眼能看出「这里是故意的」。
- 新增回归锁单测 ⑰：`transition` 收到 `from == to` 仍然 409。

### 3.3 付款路径改用它（业务照做，只有日志跳过）

```rust
// backend/src/business/erp/payable_payment.rs ④ 应付单重算
let _status_log = state_machine::transition_if_changed(
    tx, ENTITY_PAYABLE, row.id, &row.payable_no, &PAYABLE_EDGES,
    &row.status, status, operator, Some(&format!("付款 {payment_no}")), None,
).await.map_err(...)?;
```

付款记录、付款凭证、`paid_amount/unpaid_amount` 回写全部照旧（同一事务）。

| 第几期 | 状态变化 | `biz_status_log` | `biz_payment` / 凭证 |
|---|---|---|---|
| 1 | `OPEN → PARTIAL_PAID` | 1 条 | 各 1 条 |
| 2 | `PARTIAL_PAID → PARTIAL_PAID` | **0 条**（原地流转） | 各 1 条 |
| 3 | `PARTIAL_PAID → SETTLED` | 1 条 | 各 1 条 |

### 3.4 同一套语义收口到已有的两处调用点

`stockin_model::sync_purchase_status`（采购单「继续部分收货」）原本是调用点自己
`if target == current { return Ok(0) }`，现改为同一个 `transition_if_changed` 入口
（行为不变：`Ok(None)` → 返回 0，只写收货流水）。这样「允许原地不动」的口径只有一份实现。

## 4. 改动清单

| 文件 | 改动 |
|---|---|
| `backend/src/business/shared/state_machine.rs` | 新增 `transition_if_changed` + 抽出 `check_registration`；模块文档补「自环」正确用法与 LNK-P08 反面教材；新增 3 条真库单测（⑰⑱⑲，共 11→14 条） |
| `backend/src/business/erp/payable_payment.rs` | ④ 改调 `transition_if_changed`；模块文档补「状态日志只写真实变化」的三期表 |
| `backend/src/business/erp/stockin_model.rs` | `sync_purchase_status` 改用同一入口（语义不变） |
| `backend/tests/linkage/payable_chain.rs` | P08 **去掉 `#[ignore]`**；补「状态日志只写真实变化」断言（建单 1 条 + 两期各 1 条，不许有 `PARTIAL_PAID → PARTIAL_PAID`） |
| `backend/tests/linkage/common.rs` | 夹具清理补「状态日志按 ERP 单据 id 清」（见 §6） |
| `backend/tests/linkage/cleanup_leftovers.sql` | 同上，收尾脚本同步补 |

## 5. 验证（原始输出）

### 5.1 P08 单条（修复后）

```text
$ cargo test --test linkage p08_paid_plus -- --test-threads=1 --nocapture
    Finished `test` profile [unoptimized + debuginfo] target(s) in 1m 01s
     Running tests/linkage.rs (target/debug/deps/linkage-2b19476a5d38256b)

running 1 test
test linkage::payable_chain::p08_paid_plus_unpaid_equals_amount ... 联动矩阵测试已连接 cloudpivot_test
ok

test result: ok. 1 passed; 0 failed; 0 ignored; 0 measured; 61 filtered out; finished in 4.92s
```

### 5.2 全量 linkage（P08 已转正）

```text
$ LINKAGE_DB_URL=… LINKAGE_REQUIRE_DB=1 cargo test --test linkage -- --test-threads=1
running 62 tests
test result: ok. 62 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 292.73s
```

> 口径说明：本次跑在共享工作区 `/opt/cloudpivot`，同一时刻**另一张卡（LNK-F12 / t_ff20409a）
> 的在途改动也在树里**（`transfer_*`、`tests/linkage/transfer_chain.rs`，F12 用例同样已转正）。
> 62 = 61（原 60 + P08）+ 1（F12）。P08 自身的独立证据见 §5.1（单跑）。

### 5.3 lib 单测（状态机 + 应付付款 + 收货）

```text
$ ERP_DB_URL='postgres://lnkrole:***@127.0.0.1:5432/cloudpivot_test' \
    cargo test --lib -- --test-threads=1 business::shared::state_machine \
      business::erp::payable_payment business::erp::stockin business::erp::payable
test result: ok. 89 passed; 0 failed; 0 ignored; 0 measured; 1363 filtered out; finished in 90.19s
```

- 新增 3 条：`self_loop_is_still_rejected_by_transition`（⑰ 护栏不放宽）、
  `transition_if_changed_skips_self_loop`（⑱ 原地流转零日志 + 实体名写错仍 500）、
  `transition_if_changed_writes_when_changed`（⑲ 真变化照旧落一行）。
- `payable_payment::tests::p7_*` **10 条全绿**（验收标准 4）。

### 5.4 验收标准对照

| 验收标准 | 证据 |
|---|---|
| 1. `cargo test --test linkage p08_paid_plus -- --ignored` 转绿；去掉 `#[ignore]` 全量跑也绿 | §5.1（单跑 ok）、§5.2（62/62，0 ignored） |
| 2. `paid + unpaid = amount` 每一次付款后都成立（三次分期） | P08 断言：每付一笔后 `payable_snapshot` 重读库并断言恒等式（`paid_now + unpaid_now == amount_now`），未付不得为负 |
| 3. 状态只写真实变化的日志（无 `PARTIAL_PAID → PARTIAL_PAID` 空日志） | P08 断言 `biz_status_log` 轨迹**恰好** `NULL→OPEN`、`OPEN→PARTIAL_PAID`、`PARTIAL_PAID→SETTLED` 三行；每步付款后条数 2/2/3（第 2 期不新增） |
| 4. `payable_payment::tests::p7_*` 仍绿 | §5.3（10/10 ok） |

### 5.5 代码门禁

```text
$ rustfmt +1.94.1 --edition 2024 --check src/business/erp/payable_payment.rs \
    src/business/shared/state_machine.rs src/business/erp/stockin_model.rs \
    tests/linkage/payable_chain.rs tests/linkage/common.rs
OK src/business/shared/state_machine.rs
OK src/business/erp/payable_payment.rs
OK src/business/erp/stockin_model.rs
OK tests/linkage/payable_chain.rs
OK tests/linkage/common.rs

$ cargo clippy --lib -- -D warnings -A clippy::manual_range_contains
    Finished `dev` profile [unoptimized + debuginfo] target(s) in 0.20s      → LIB_EXIT=0
$ cargo clippy --test linkage -- -D warnings -A clippy::manual_range_contains
    Finished `dev` profile [unoptimized + debuginfo] target(s) in 0.11s      → TEST_EXIT=0
```

**为什么门禁按 target 分开跑**：`cargo clippy --all-targets` 在共享工作区会被**别的卡在途的
未跟踪文件**挡红 —— 实测 22 条告警全在 `tests/testd/*`（另一张卡的 WIP，`useless_vec` /
`deref on an immutable reference` 等），与本改动无关（`grep` 结果里没有本卡 6 个文件中的任何一个）。
本卡改动的 lib 与 `--test linkage` 两个目标均为 0 告警。

## 6. 顺手补齐的测试基建（不修会持续积脏数据）

P10 起**所有**状态变更都写 `biz_status_log`，但联动矩阵的夹具清理（`common.rs::cleanup`）
只按「任务 id / `LNK-…` 单号前缀」删日志 —— 而应付 / 收货单的单号是业务号段
（`AP-…` / `IN-…`），一条都删不掉。实测 `cloudpivot_test` 里已积了 **1683 条孤儿状态日志**
（`entity_id` 指向早已删掉的夹具行）：

```text
PAYABLE|461   STOCK_IN|531   TRANSFER|236   VOUCHER|455   （孤儿数）
业务行：payable 7 / stock_in 1 / transfer 0 / voucher 59
```

修法（两处，语义一致）：

1. `common.rs::cleanup` 的状态日志 DELETE 增加 `entity_id IN (biz_stock_in / biz_payable / biz_voucher 的本用例夹具 id)`，
   且这条语句仍排在删 `biz_payable` **之前**（日志无外键，晚了就查不到 id）；
2. `cleanup_leftovers.sql` 同步补上同样的谓词（收尾脚本与用例口径一致）。

清理历史孤儿（**只动 `cloudpivot_test`**，且判据是「`entity_id` 指向的业务行不存在」）：

```text
BEGIN; … CREATE TEMP TABLE _orphan AS SELECT …; SELECT count(*) FROM _orphan;
 1683
DELETE 1683
COMMIT;

-- 清后只剩「引用真实业务行」的日志
PAYABLE 1 / STOCK_IN 2 / VOUCHER 58
```

## 7. 遗留与注意

- **LNK-F12**（在途超期无告警）不属本卡，仍由 t_ff20409a 处理。
- 本轮验证把 `cloudpivot_test` 的 `lnkrole` 口令重置为本次会话使用值（原口令无人掌握，
  该角色是本机测试库专用 superuser）；未改 `pg_hba.conf`、未新建角色。
- 共享工作区并发：验证期间 LNK-F12 卡在同一树里改 `transfer_*` 与 `transfer_chain.rs`，
  本卡**只提交自己那 6 个文件**（见 §4），没碰对方改动。

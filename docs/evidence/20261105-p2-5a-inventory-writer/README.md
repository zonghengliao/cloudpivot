# P2-5A 库存多写入点收口：13 个文件直写 `biz_inventory` → 唯一写入口

> 任务：`t_f088a8db`｜工作区：`dir:/opt/cloudpivot`｜本轮改动提交见卡面。

## 一、结论

| 验收项 | 结果 |
|---|---|
| 唯一写入口建立 | ✅ `backend/src/business/erp/inventory_writer.rs`（1262 行，三张表全部写 SQL 都在这里） |
| 13 个文件全部迁移 | ✅ 见 §四（13 个里 6 个是生产文件 → 已迁；2 个只在 `#[cfg(test)]` 里写、5 个是测试文件 → 按设计豁免，闸门放行） |
| 零直接 UPDATE | ✅ `scripts/verify/inventory_write_gate.py`：扫描 341 个非测试 `.rs`，**0 处越界**；写入口内 22 条写语句 |
| 全量测试绿 | ✅ `business::erp` 1045/1045、`business::flow::linkage` 35/35，跑两轮（干净库各起一轮） |
| 不变量 | ✅ 干净迁移库上 `erp_invariants.py` **52/52 通过、0 BLOCKER、0 WARN**（卡面写的 40/40 是套件旧版本；现行版本共 52 条） |
| git commit | ✅ 见卡面 |

## 二、评审计数复现（收口前的真实数字）

```bash
# biz_inventory（成本账）：13 个文件
$ grep -rlE "(UPDATE|INSERT INTO)[[:space:]]+biz_inventory[[:space:](]|INTO biz_inventory AS" \
    --include=*.rs backend/src | sort
business/erp/inventory_audit.rs        business/erp/model_tests.rs
business/erp/reminder_tests.rs         business/erp/serial_handler.rs
business/erp/serial_model.rs           business/erp/stockin_cancel.rs
business/erp/stockin_handler_tests_cancel.rs  business/erp/stockin_write.rs
business/erp/stockout_model.rs         business/erp/stocktake_tests.rs
business/erp/transfer_handler.rs       business/erp/transfer_settle.rs
business/flow/linkage/tests.rs
```

`biz_inventory_location`（库位账）同样 **13 个文件**，`biz_inventory_txn`（流水）**8 个文件** ——
与评审给的 13 / 13 / 8 **逐一对上**。按「生产 / 仅测试」拆开：

| 类别 | 文件 |
|---|---|
| 生产写（6） | `inventory_audit` `serial_model` `stockin_cancel` `stockin_write` `stockout_model` `transfer_settle` |
| 只在 `#[cfg(test)]` 里写（2） | `serial_handler`（`mod tests` 从 419 行起）`transfer_handler`（1304 行起） |
| 纯测试文件（5） | `model_tests` `reminder_tests` `stockin_handler_tests_cancel` `stocktake_tests` `linkage/tests` |

另外有三个生产文件只写「库位账 / 流水」，不在上面 13 个成本账文件里，但**同属这次收口范围**：
`stockin_location`（库位账 find-or-create）、`stocktake_model`（盘点流水）、`opening`（期初流水）。

## 三、设计：唯一写入口

`erp/inventory_writer.rs` 把三张表的**全部** `INSERT/UPDATE` 搬进来，按语义命名：

| 账 | 函数 | 语义（为什么必须是它自己，不能合并） |
|---|---|---|
| 成本账 | `lock_cost` / `lock_cost_bucket` | 锁读（`FOR UPDATE`）；`_bucket` 版「汇总桶优先」 |
| | `add_cost` | 入库加法（upsert，`ROUND(…,6)`，金额权威不重算） |
| | `credit_cost` | 调拨调入（先锁后改，均价随金额/数量重算） |
| | `subtract_cost` | 收货作废冲减（按桶键减 + 归零夹 0） |
| | `deduct_cost` | 出库扣减（按行 id，**整桶取完金额归零**） |
| | `try_deduct_cost` | 调拨调出（`AND qty >= ?` 条件扣减，均价不动） |
| | `restore_cost` | 出库作废回加（金额按原额，缺行建档） |
| | `sync_cost_qty_from_serials` | 一物一码数量派生（两条 UPDATE 必须成对） |
| | `align_cost_qty_sql` / `align_cost_qty_batch` | 巡检批量修正（一条语句，读侧 CTE 由巡检提供） |
| 库位账 | `lock_location` / `add_location_qty` / `subtract_location_qty` / `deduct_location_qty` / `restore_location_qty` / `ensure_location_row` / `sync_location_qty_from_serials` | 数量账，两分支（库位账行 id / 未分配桶）都在这里 |
| 流水 | `insert_txn`（入参 `TxnRecord`，22 列） | 只增不改；`period_code` 支持显式覆盖（期初按会计期间） |

分层：`handler`（事务边界）→ 业务 model（算什么、什么时候写）→ **`inventory_writer`（唯一落 SQL）** → 三张表。
业务侧保留同名函数（`apply_cost` / `reverse_cost` / `deduct_cost` …）作为**薄包装**，
签名与语义不变 —— 所以 handler、单测、e2e 一行调用都没改。

## 四、13 个文件的处置清单

| # | 文件 | 收口前直写 | 处置 |
|---|---|---|---|
| 1 | `business/erp/stockin_write.rs` | 成本账 upsert + 库位账 upsert + `IN` 流水 + 锁读 | 4 个函数体改调 `add_cost` / `add_location_qty` / `insert_txn` / `lock_cost` |
| 2 | `business/erp/stockin_cancel.rs` | 成本账冲减 + 库位账冲减（两分支）+ `OUT` 流水 | `subtract_cost` / `subtract_location_qty` / `insert_txn`；`SHORTFALL_PREFIX` 移到写入口后 `pub use` 重导出 |
| 3 | `business/erp/stockout_model.rs` | 成本账扣减 + 回加（含建档）+ 库位账扣减 + 回加 + 2 条流水 + 锁读 | `deduct_cost` / `restore_cost` / `deduct_location_qty` / `restore_location_qty` / `lock_location` / `insert_txn` |
| 4 | `business/erp/transfer_settle.rs` | 调出条件扣减 + 调入加/建档 + 调拨流水 | `try_deduct_cost` / `credit_cost` / `insert_txn` |
| 5 | `business/erp/serial_model.rs` | 数量派生 3 条 UPDATE + 序列号流水 | `sync_cost_qty_from_serials` + `sync_location_qty_from_serials` + `insert_txn`（仍保留「影响行数必须为 1」自检） |
| 6 | `business/erp/inventory_audit.rs` | 巡检批量修正 `INSERT … ON CONFLICT DO UPDATE` | `align_cost_qty_batch`（写语句在写入口，读侧 CTE 留在巡检，口径单一） |
| 7 | `business/erp/stockin_location.rs` | 库位账行 find-or-create | `ensure_location_row` |
| 8 | `business/erp/stocktake_model.rs` | 盘点流水 | `insert_txn` |
| 9 | `business/erp/opening.rs` | 2 条期初流水（`IN` / 取消 `OUT`） | `insert_txn`（**按会计期间**落 `period_code`） |
| 10 | `business/erp/serial_handler.rs` | 只在 `#[cfg(test)]`（419 行起）里直写夹具 | 豁免（夹具要造「号账不符」的脏状态） |
| 11 | `business/erp/transfer_handler.rs` | 只在 `#[cfg(test)]`（1304 行起）里直写夹具 | 豁免 |
| 12 | `business/erp/stocktake_tests.rs` | 测试文件 | 豁免 |
| 13 | `business/flow/linkage/tests.rs` | 测试文件 | 豁免 |
| — | `business/erp/model_tests.rs` / `reminder_tests.rs` / `stockin_handler_tests_cancel.rs` | 测试文件 | 豁免 |

`inventory_writer.rs` 里三张表的写语句合计 **22 条**（成本账 11 / 库位账 10 / 流水 1）。

## 五、闸门（CI 检查）

`scripts/verify/inventory_write_gate.py`，已接进 `scripts/check.sh`（在「闸③ ERP 不变量」之前，秒级、无库依赖）：

```text
============================================================================================
库存三本账唯一写入口闸（P2-5A）
扫描 341 个非测试 .rs（豁免 61 个测试文件）
唯一写入口 backend/src/business/erp/inventory_writer.rs：22 条写语句
============================================================================================

★ 零越界：非测试代码里没有任何一处直写库存三本账。
EXIT=0
```

判据：`backend/src` 下**非测试**代码里出现 `INSERT INTO|UPDATE|DELETE FROM biz_inventory(_location|_txn)`
即判红（注释行、文件名含 `test` 的文件、文件内 `#[cfg(test)]` 之后的代码豁免 —— 理由写在脚本头与
`docs/后端二开指南.md` 第十四节）。

负例验证（闸门真的会红）：把 `stockout_model.rs` 里一行改回直写即可复现 —— 见 §七 实测。

## 六、实测（两轮 + 真接口 e2e）

环境：隔离库 `cloudpivot_p25a`（`createdb -O alun` → 起一次 `target/debug/cloudpivot` 自动跑完
**182 个迁移** + 种 `SUPER_ADMIN`），`ERP_DB_URL` 指向它，`--test-threads=1`。原始记录见 `test-runs.txt`。

| 轮次 | 命令（`cargo test --lib --`） | 结果 |
|---|---|---|
| 第 1 轮（干净库） | `business::erp` | **1045 passed / 0 failed**（1133.6s） |
| | `business::flow::linkage` | **35 passed / 0 failed**（13.6s） |
| 第 2 轮（重建空库再来一遍） | `business::erp` | **1045 passed / 0 failed**（1160.8s） |
| | `business::flow::linkage` | **35 passed / 0 failed**（13.7s） |

- 第 2 轮 `SKIP: 未设置 ERP_DB_URL` 出现 **0 次** —— 真库用例全部真跑，不存在「静默 SKIP 假绿」。
- 首轮（收口过程中、修 `period_code` 之前）曾实测到 1 条红：
  `opening::tests::r5_09_inventory_opening_writes_location_and_txn` 断言 `period_code = "2034-09"`
  实际 `"2026-11"` —— 这正是「流水期间码必须由调用方给（期初按会计期间）」的证据，修完即绿（§八 ②）。
- 静态检查：`cargo fmt --check` 通过；`cargo clippy --lib -- -D warnings` 通过；
  `cargo clippy --all-targets -- -D warnings` 通过。

### 六.1 真接口 e2e（走被收口的写路径 + 事后跑不变量）

`e2e-real-flow.txt`：另起一个干净库 `cloudpivot_p25a_e2e2` 与实例，
**真实 HTTP** 建收货单（无源入库 A4 口径）→ 过账 → 直接查库断言三本账 → 重复过账 → 跑不变量。
**11 项断言全过，0 失败**：

```text
[PASS] 建收货单（无源入库 A4 口径）（200）
[PASS] 过账（200）
[PASS] 成本账 qty = 100（100.0000）
[PASS] 成本账 amount = 200（不含税）（200.0000）
[PASS] 成本账 avg_cost = 2.000000（2.000000）
[PASS] 库位账合计 = 成本账 qty（B1）（100.0000）
[PASS] 流水 IN / before 0 → after 100 / 期间码=当月（IN|PURCHASE_IN|100.0000|200.0000|0.0000|100.0000|2026-11）
[PASS] 重复过账 409（409）
[PASS] 重复过账没再加库存（100.0000）
结果：52/52 通过   🚨 BLOCKER 0   ⚠️ WARN 0
★ 全部不变量成立，账目干净。
inventory=1 location=1 txn=1
== 汇总：PASS=11 FAIL=0 ==
```

即：**真实业务流水落库之后**（不是空库），三本账一致、不变量 52/52 —— 这是「收口没有改变任何口径」的最直接证据。
（作废 / 红冲的反向路径由单测覆盖：`stockin_handler_tests_cancel`、`stockout_tests`、`transfer_handler::db_tests`
里的反向用例都在上面两轮里全绿。）

## 七、负例：闸门抓不抓得到

`neg-gate-check.txt`：往 `stockout_model.rs` 的 `deduct_cost` 里临时塞一行
`let _probe = "UPDATE biz_inventory SET qty = qty";`，闸门立刻判红：

```text
✗ 有 1 处越界写入（必须改走 inventory_writer）：
  · backend/src/business/erp/stockout_model.rs:644  [biz_inventory]  let _probe = "UPDATE biz_inventory SET qty = qty"; …
闸门 EXIT=1
还原后 md5 一致：YES（01a72c7f62a6590438bac8500a61e8d5）
```

## 八、收口时顺手修掉的两个真 bug

1. **`stockout_model::restore_cost` 的建档分支参数少绑一个**（原有代码）：
   `INSERT INTO biz_inventory …` 有 **12 个 `?`**，`vec![]` 只传 **11 个**参数 —— 一旦走到「成本账行被硬删过」
   的历史缺口分支，就会把 `now` 时间戳绑到 `amount` 列上直接报错。收口时逐位对齐并加了注释。
2. **期初流水的 `period_code` 不能用 `now` 折算**：期初按**会计期间**录入（`2034-09`），
   `now` 是当天。写入口的 `TxnRecord.period_code` 因此做成 `Option<&str>`：
   `None` = 按 `now` 折算（所有日常业务），`Some` = 用调用方给的期间（期初两条流水）。

## 九、边界与未做

- **测试夹具不迁移**（有意）：巡检用例需要直写脏数据（`UPDATE biz_inventory SET qty = 999`）来证明
  「巡检能抓到」，走写入口反而造不出来。闸门对这类代码明确放行，理由写在脚本头。
- **没有合并「语义相近但口径不同」的写入**（出库扣减 vs 调拨调出、入库加法 vs 调拨调入）：
  合并会让巡检 C4/B2 的恒等式变红，见 §三 表。
- **生产库（144）未动**：本卡只改代码，迁移无新增（`V0242` 未使用），线上更新走 `deploy.sh`。
- 卡面写的「不变量 40/40」是套件旧版本的条数；现行 `scripts/verify/erp_invariants.py` 共 **52 条**，
  干净库实测 **52/52**、0 BLOCKER（见 §一）。在「刚跑完单测残留库」上会看到 6 条 BLOCKER，
  那是夹具故意留下的脏账（出库/作废用例不清理），不是本卡引入 —— 同一个库在收口前后的不变量集合一致。

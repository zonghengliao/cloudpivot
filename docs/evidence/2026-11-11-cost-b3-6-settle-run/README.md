# COST-B3-6 · 结算：状态回写 + 事务内 6 条断言

- **卡**：`t_02fec6bf`（B3 批次 · ARCH-9 §8.1 B3-6）
- **设计**：`docs/architecture/ARCH-9-成本核算升级设计.md` §4.8 ①–⑥（结算事务）、§4.11（C-COST-1~4）、§4.7.5（算例 A 四张凭证与 4001 余额）
- **代码**：`backend/src/business/erp/cost_settle_run.rs`（编排 + 6 条断言）、`cost_settle_run_db_tests.rs`（真库用例）
- **对外入口**：`POST /business/erp/cost-period/{id}/settle`（设计 §5.2 #29，权限 `business:erp:cost-period:settle`，本轮由本卡接线）

## 1. 做了什么

一次事务（`settle_period`）内顺序执行，任何一步失败整笔回滚：

| 步 | 动作 | 谁实现 |
|---|---|---|
| ① | `biz_cost_period FOR UPDATE` + 状态 `APPROVED` + 期间未结账 + 无红冲在跑 | 本卡 |
| ② | 完工入库（`stock_in(PRODUCTION)` + 明细 + 三本账 + 计算单回绑） | B3-4 `cost_settle_finish::generate` |
| ③ | 三 / 四张凭证（人工计提 / 手工台账 / 制造费用结转 / 完工结转，全走 `post_voucher`） | B3-5 `cost_settle_voucher::post_all` |
| ④ | 状态机 `APPROVED→SETTLED` + CAS 回写单头（3 个 voucher_id / 6 个合计 / 对象数 / 结算人） | 本卡 `write_back` |
| ⑤ | 事务内 6 条断言（C-COST-1~4 的事务内版） | 本卡 `run_asserts` |
| ⑥ | 提交后：**没有任何**会失败的账务动作（通知不在 B3 范围） | — |

### 6 条断言（`ASSERT_IDS`，顺序是契约）

| # | 编号 | 判据 | 失败码 |
|---|---|---|---|
| 1 | `C-COST-1` | 当期池 = 当期 `4101.*` 借方发生额，按 (车间,科目) 逐组（复用 `cost_calc_reconcile::reconcile_in_tx(Stage::Settle)`） | `66065` |
| 2 | `C-COST-2` | `Σ 生产入库单金额` = 完工结转凭证 **1405 借方** = `Σ item.finished_cost` | `66077` |
| 3 | `C-COST-2·行` | `completed_qty > 0` ⇒ 计算单 `SETTLED` + 绑单 + 入库单数量/金额逐行相等；入库单张数 = 有完工的对象数 | `66077` |
| 4 | `C-COST-3` | `4001.01/.02/.03` 本期净发生 = `Σ(wip_end_cost − wip_beg_cost)` | `66077` |
| 5 | `C-COST-4·清零` | 每个 (车间, 科目) 的 `4101.*` 本期借方 = 贷方 | `66077` |
| 6 | `C-COST-4·守恒` | `Σ biz_cost_alloc_result.alloc_amount` = `Σ biz_cost_collect(OVERHEAD, 池行)` | `66077` |

新增错误码：`66077 ERP_COST_SETTLE_ASSERT`（409，已登记 66xxx 码值稳定性数组 + 模块头码段表）。

## 2. 实测输出

| 文件 | 内容 |
|---|---|
| `01_real_db_tests.out` | `ERP_DB_URL=…mig_replay_b34` 真库 10 用例全过（含 6 条断言逐条实测值，见下） |
| `02_full_lib_tests.out` | `cargo test --lib`（不连库）全仓结果 |
| `03_gates.out` | `cargo fmt --check` / `cargo clippy --all-targets -D warnings` / 库存闸 |
| `04_residue.out` | 夹具残留全 0（14 张表逐表计数） |

主用例（算例 A）6 条断言的实测值（原文摘录自 `01_real_db_tests.out`）：

```
[断言] C-COST-1 当期池 = 4101.* 借方发生额（逐 (车间,科目) 组） —— 期间 2805-11 结算口径：池 4000（其中待结算过账 0），应已入账 4000，账面 4000，差额 0，不平 0 组
[断言] C-COST-2 生产入库金额 = 完工结转凭证借方 1405 = Σ 完工成本 —— 入库单合计 22311.1104 / 完工凭证 1405 借方 22311.1104 / Σ完工成本 22311.1104
[断言] C-COST-2·行 入库单 ↔ 计算单逐行同源（数量 / 金额 / 状态） —— 有完工的计算单 1 行 / 本批次入库单 1 张，逐行不符 0 行
[断言] C-COST-3 4001 本期净发生 = Σ(期末在制 − 期初在制) —— 4001 本期净发生 4688.8896 / Σ(期末在制 4688.8896 − 期初在制 0.0000) = 4688.8896
[断言] C-COST-4·清零 每个 (车间,科目) 的 4101 本期借贷相等（池清零） —— 4101.* 本期 2 个 (车间,科目) 组，未清零 0 组
[断言] C-COST-4·守恒 Σ 分摊结果 = Σ 池行（分摊守恒 / 无漏摊） —— 池 4000.0000 / 已分摊 4000.0000（差额 0.0000）
```

设计 §4.7.5 逐项对平（同一用例断言）：

| 科目 | 实测余额 | 设计值 |
|---|---|---|
| 4001.01 | 3800.0000 | 3800.00 |
| 4001.02 | 444.4448 | 444.4448 |
| 4001.03 | 444.4448 | 444.4448 |
| 合计 | 4688.8896 | = `wip_end_cost` |
| 4101.01 / 4101.02 | 0 / 0 | 清零 |

「第 6 条失败 ⇒ 前面全部回滚」的实测（`sixth_assertion_rolls_back_everything`）：

```
[第 6 条断言] 结算事务断言 C-COST-4·守恒「Σ 分摊结果 = Σ 池行（分摊守恒 / 无漏摊）」不平，
整个结算已回滚：池 4000.0000 / 已分摊 3000.0000（差额 1000.0000）…
```
回滚后实测：入库单 0 张、结算凭证 0 张、库存金额 0、`biz_cost_period.status` 仍 `APPROVED`、
`settled_at` 空、`finish_voucher_id` 空、计算单未绑单、批次状态日志 0 条，
**被删的那一行分摊结果也随回滚回来（2 行）**。

## 3. 用例清单

| # | 用例 | 验的什么 |
|---|---|---|
| 1 | `settle_end_to_end_in_one_transaction` | 验收①：一次事务入库 + 4 张凭证 + 状态回写 + 6 条断言全过 |
| 2 | `sixth_assertion_rolls_back_everything` | 验收②③：第 6 条红 ⇒ 前面全部回滚（零写入） |
| 3 | `settle_requires_approved_status` | 状态闸（`DRAFT` ⇒ `66073`） |
| 4 | `missing_material_debit_is_caught_by_the_wip_assertion` | 断言 4 正面证据：料钱没进 `4001.01` ⇒ `66077` |
| 5 | `bypassed_4101_entry_is_caught_before_settling` | 断言 1 正面证据：旁路记 4101 ⇒ `66065` |
| 6 | `second_settle_is_rejected` | 幂等：二次结算 `66073`，凭证/入库单条数不变 |
| 7 | `payroll_source_settles_with_three_vouchers` | `PAYROLL` ⇒ 3 张凭证，断言照样全过 |
| 8 | `empty_batch_is_rejected` | 无计算单不能结算（409，零写入） |
| 9 | `zero_output_batch_is_rejected_by_the_finish_voucher_gate` | 全在制（完工 0）⇒ `66075`，零写入 |
| 10 | `fixture_constants_are_aligned` | 与 B3-4/B3-5 常量对齐（防上游漂移） |

## 4. 已知偏差 / 未做

1. **断言 4 取「本期净发生」形态**（`期末 − 期初`），不是设计原文的「全库期末余额」。
   事务内取等价形态的两个理由（①免疫别的期间/年度凭证；②红冲重结时旧蓝字+红字同期间互抵）
   写在 `cost_settle_run.rs` 模块头；**全库余额版留给 B3-10 巡检**（C-COST-3 的权威实现）。
2. **提交后的通知（站内信 / 待办完成）未做**（设计 §4.8 ⑦）：B3 批次里没有通知卡，
   且它失败只打 `warn`、不影响单据状态，不构成约定 11 的死单风险。
3. **断言 1 复用 `66065`**（与 calc 第 4 段同一个码 / 同一套报文），没有另造新码。
4. **无迁移**：本卡不改 schema（6 条断言只读现有表）⇒ 卡面「迁移幂等 + `.down.sql`」N/A。
5. **无前端改动**：结算按钮属 B3-11（前端成本核算页 5 步向导），本卡只开 #29 接口。
6. **完成数量 0 的计算单不置 `SETTLED`**（沿用 B3-4 口径：`ck_biz_cost_item_settled` 要求
   `SETTLED` 必有入库单）—— 断言 3 只要求反向（`completed_qty > 0` ⇒ 必须已绑单）。

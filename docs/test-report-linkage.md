# 跨模块联动矩阵测试报告（测试A · t_1fdb4704）

> 任务卡：**【测试A】跨模块联动矩阵测试（补 1343 个测试的盲区，60+ 用例）**
> 执行：测试工程师（test-enginner）　日期：2026-10-31
> 代码位置：`backend/tests/linkage.rs` + `backend/tests/linkage/`（6 个文件，4200+ 行）
> 运行入口：`cargo test --test linkage -- --test-threads=1`

---

## 0. TL;DR

| 项目 | 结果 |
|---|---|
| 用例总数 | **62**（T01–T18 / S01–S16 / F01–F12 / P01–P08 / V01–V08） |
| 通过 | **60** |
| 失败 | **0** |
| 已知问题转 `#[ignore]` | **2**（LNK-P08 真缺陷、LNK-F12 功能缺失） |
| 抓到的**新**缺陷 | **1 个**（LNK-P08：应付第二次部分付款被状态机误拒 → 付款路径第三次分期直接断） |
| 抓到的功能缺失 | **1 个**（LNK-F12：在途仓无超期告警/扫描） |
| 卡面口径与实现不一致（非缺陷，需产品定） | **5 处**（T10 / T11 / F03 / F08 / F09） |
| 老板报的 TSK-202610-000149（任务取消后孤儿待办） | **已被 P11 修复，本套用例 T15 作为回归护栏转绿** |
| 夹具残留 | 跑绿后 **0**（真库逐表核对，见 §7） |

```
running 62 tests
test result: ok. 60 passed; 0 failed; 2 ignored; 0 measured; 0 filtered out; finished in 312.10s
```

---

## 1. 环境与复现

```bash
cd /opt/cloudpivot/backend

# 真库（红线：只碰 cloudpivot_test，common.rs 里有硬校验，指错库直接 panic）
export LINKAGE_DB_URL='postgres://lnkrole:***@127.0.0.1:5432/cloudpivot_test'
export LINKAGE_REQUIRE_DB=1          # 没连上库就失败，而不是静默 SKIP

cargo test --test linkage -- --test-threads=1
```

- `LINKAGE_DB_URL` 缺省 + 无 `LINKAGE_REQUIRE_DB` 时按仓内既有约定打印 `SKIP` 并返回（无库 CI 仍绿），
  但**验收必须带 `LINKAGE_REQUIRE_DB=1`**，否则「全部跳过」也会显示 ok —— 这正是本套用例要防的假绿。
- `--test-threads=1` **是必需的**：T17 会临时关掉 `TASK_ACCEPT` 种子流程的 `is_default`（模拟「没配流程」），
  并发跑时别的 T 用例会读进这个窗口。这一点已写进用例注释。

---

## 2. 测试设计方法：状态变更 × 关联物矩阵

单模块用例回答「这个模块自己对不对」；本套用例回答「**A 变了，B 有没有跟着变**」。
下表每一行都至少有一条**正向**（该发生的发生了）+ 一条**反向**（不该动的没动）配对 ——
反向那半才是抓「孤儿待办」这类缺陷的。

| 主体变更 | 谁引用了它 | 引用者该变成 | 撤销函数 | 实际调了吗 | 覆盖用例 |
|---|---|---|---|---|---|
| 任务 → CANCELLED | `biz_flow_instance` | CANCELLED | `cancel_pending_of_biz` | ✅（P11 已接） | T02 |
| 任务 → CANCELLED | `biz_flow_task` | CANCELLED + `completed_at` 非空 | 同上 | ✅ | T03 |
| 任务 → CANCELLED | `biz_flow_instance_node` | CANCELLED | 同上 | ✅ | T04 |
| 任务 → CANCELLED | 子任务 | CANCELLED | `cascade_cancel_descendants` | ✅ | T06 |
| 父任务 CANCELLED | 子任务的审批实例 | CANCELLED | 级联里再走一遍 | ✅ | **T07**（最容易漏的一行） |
| 任务 → DONE | 审批实例 | **保持 APPROVED**（不动） | — | ✅ 反向 | T05 |
| 任务软删 | 审批实例 | CANCELLED | `cancel_on_task_deleted` | ✅ | T08 |
| 任务 CANCELLED 后投票 | 任务 / 实例 / 待办 | 全部不变（幂等分支） | — | ✅ 反向 | T09 / T10 |
| 任务 CANCELLED 后 | 审批人的「待我审批」列表 | 查不到（不是查到但点不动） | — | ✅ | **T15（老板报的那条）** |
| 收货单 POSTED→CANCELLED | `biz_inventory` | qty/金额减回 | `reverse_cost` | ✅ | S06 |
| 同上 | `biz_inventory_location` | qty 减回 | `reverse_location_qty` | ✅ | S07 |
| 同上 | `biz_inventory_txn` | 追加 OUT / `PURCHASE_IN_CANCEL` | `write_cancel_txn` | ✅ | S08 |
| 同上 | `biz_voucher` | 红冲 + `reversed_by` 回写 | `reverse` | ✅ | S09 |
| 同上 | `biz_payable` | CANCELLED | `cancel` | ✅ | S10 |
| 同上 | `biz_material_serial` | 软删 | `void_serials_of` | ✅ | S11 |
| 收货单 POSTED→CANCELLED | 四账同时 | 回到基线 | — | ✅ 总闸 | **S12** |
| 收货单过账 DRAFT→POSTED | 库存/库位账/流水/凭证/应付/序列号 | 各写一笔 | — | ✅ 正向 | S01–S05 |
| 调拨 DRAFT→CANCELLED | 库存 | **不动**（未结算） | — | ✅ 反向 | F06 |
| 调拨 PENDING→CANCELLED | `biz_flow_instance` | 终止在跑实例 | `cancel_pending_of_biz` | ✅ | F07 |
| 调拨 IN_TRANSIT→DONE | 调出仓 / 调入仓 | 双写且总量守恒 | `settle_transfer` | ✅ | F04 / F05 |
| 应付 SETTLED | 付款凭证（借 2202 / 贷 1002） | 生成且借贷平衡 | — | ✅ | P04 |
| 应付 paid/unpaid | amount | `paid + unpaid = amount` 恒等 | — | ⚠️ **P08 红**（见 §5.1） | P08 |
| 凭证红冲 | `biz_voucher_entry` / `reversed_by` | 镜像分录、指针回写、状态仍 POSTED | `reverse_voucher` | ✅ | V01 / V02 |
| IN_TRANSIT 长期压货 | 告警 / 扫描任务 | 应能被查出并推给人 | **没有实现** | ❌ | F12（ignore） |

---

## 3. 用例清单（62 条）

### 领域 1 · 任务 ↔ 审批 ↔ 待办（18）

| 编号 | 用例 | 断言核心 | 结果 |
|---|---|---|---|
| T01 | `t01_submit_review_starts_instance_and_todo` | 提交验收 → 实例 PENDING + 待办 PENDING，两者 `biz_no/biz_id` 对得上 | ok |
| T02 | `t02_task_cancelled_cancels_instance` | 任务取消 → 实例 CANCELLED + `ended_at` 非空 | ok |
| T03 | `t03_task_cancelled_cancels_todo_with_timestamp` | 待办 CANCELLED 且 `completed_at` 非空（审计要时间） | ok |
| T04 | `t04_task_cancelled_cancels_instance_node` | 实例节点也 CANCELLED（画布不留下「进行中」节点） | ok |
| T05 | `t05_task_done_keeps_instance_approved` | 任务完成 → 实例仍是 APPROVED（**不改**，反向） | ok |
| T06 | `t06_parent_cancel_cascades_to_child` | 父任务取消 → 子任务 CANCELLED | ok |
| T07 | `t07_parent_cancel_cancels_child_instance` | 子任务的审批实例一并 CANCELLED | ok |
| T08 | `t08_task_soft_delete_cancels_instance` | 软删 → 实例 CANCELLED（P11 逻辑回归） | ok |
| T09 | `t09_approve_after_cancel_keeps_task_cancelled` | 拿**已撤销的待办 id** 点同意 → 任务仍 CANCELLED | ok |
| T10 | `t10_approve_after_cancel_is_rejected` | 同上 → 实例/待办/任务**零状态变化**（口径见 §6.1） | ok |
| T11 | `t11_cancel_is_idempotent` | 再取消一次 → 零副作用 + 干净 4xx（口径见 §6.2） | ok |
| T12 | `t12_approved_todo_survives_later_cancel` | 已 APPROVED 的待办不被改 CANCELLED（保留审计） | ok |
| T13 | `t13_submitter_has_no_todo` | 提交人自己的待办列表为空 | ok |
| T14 | `t14_approver_sees_todo` | 审批人看得到 | ok |
| T15 | `t15_cancelled_task_disappears_from_approver_todo` | **取消后审批人列表里查不到** | ok（P11 修复后转绿） |
| T16 | `t16_outsider_sees_nothing` | 无关用户看到 0 条 | ok |
| T17 | `t17_missing_flow_falls_back_to_two_level_acceptance` | 关掉默认流程 → 提交不报错，走两级验收 | ok（**必须串行**） |
| T18 | `t18_second_submit_does_not_create_second_instance` | 重复提交 → 不产生第二个实例 | ok |

### 领域 2 · 收货单全链路（16）

| 编号 | 用例 | 断言核心 | 结果 |
|---|---|---|---|
| S01 | `s01_post_writes_all_six_ledgers` | 过账 → 库存 +qty、库位账 +qty、流水 1 条 IN、凭证 1 张、应付 1 张（六账齐） | ok |
| S02 | `s02_voucher_is_balanced_with_expected_accounts` | 借 1405 = 价、借 2221.01 = 税、贷 2202 = 价税合计；借贷平衡 | ok |
| S03 | `s03_payable_amount_equals_stock_in_total` | 应付金额 = 收货单 `total_with_tax` | ok |
| S04 | `s04_post_creates_serials_for_serial_material` | 一物一码物料 → 生成 N 个序列号（IN_STOCK） | ok |
| S05 | `s05_invariant_b1_location_equals_cost` | 不变量 B1：库位账合计 = 成本账 | ok |
| S06 | `s06_cancel_rolls_back_inventory` | 作废 → 库存 qty/金额回滚 | ok |
| S07 | `s07_cancel_rolls_back_location_ledger` | 作废 → 库位账回滚 | ok |
| S08 | `s08_cancel_writes_reverse_txn` | 作废 → 追加 OUT / `PURCHASE_IN_CANCEL` 流水 | ok |
| S09 | `s09_cancel_reverses_voucher` | 作废 → 红字凭证 + 原单 `reversed_by` 回写 | ok |
| S10 | `s10_cancel_cancels_payable` | 作废 → 应付 CANCELLED | ok |
| S11 | `s11_cancel_voids_serials` | 作废 → 序列号软删（不物理删） | ok |
| S12 | `s12_cancel_returns_all_four_ledgers_to_baseline` | **四账 + 金额一次归位**（本套最重要的总闸） | ok |
| S13 | `s13_cancel_without_reason_is_bad_request` | 缺 reason / 空白 reason → 400，状态不变 | ok |
| S14 | `s14_cancel_after_shipping_is_conflict` | 货已出库再作废 → 409，文案点名「货已出库/库存不足」 | ok |
| S15 | `s15_double_cancel_is_conflict` | 重复作废 → 409，且四账快照完全不变（不许退两次） | ok |
| S16 | `s16_failed_cancel_rolls_back_everything` | 作废失败 → 四账 + 序列号 + 应付全快照不变（整事务回滚） | ok |

### 领域 3 · 调拨全链路（12）

| 编号 | 用例 | 断言核心 | 结果 |
|---|---|---|---|
| F01 | `f01_submit_creates_pending_instance` | DRAFT → PENDING 生成审批实例 | ok |
| F02 | `f02_approval_moves_to_approved` | 审批通过 → APPROVED | ok |
| F03 | `f03_ship_records_transit_warehouse_without_touching_stock` | 发货只回填 `transit_warehouse_id`，**不动库存**（口径见 §6.3） | ok |
| F04 | `f04_receive_moves_qty_between_warehouses` | 收货 → 调出仓 -qty / 调入仓 +qty | ok |
| F05 | `f05_total_qty_and_cost_conserved_after_done` | **两仓总量 + 金额守恒** | ok |
| F06 | `f06_draft_cancel_leaves_stock_untouched` | DRAFT 取消 → 库存不动（反向） | ok |
| F07 | `f07_pending_cancel_terminates_running_instance` | PENDING 取消 → 实例终止；重新提交是 409 而不是撞唯一键 500 | ok |
| F08 | `f08_manual_approve_while_pending_is_blocked` | 审批中人工直推 APPROVED → 409 门闸（口径见 §6.4） | ok |
| F09 | `f09_over_issue_is_rejected_at_settlement` | 超发在结算处被拒（409），状态留在 IN_TRANSIT（口径见 §6.5） | ok |
| F10 | `f10_concurrent_transfers_do_not_deadlock` | 同物料并发调拨不死锁 | ok |
| F11 | `f11_serials_follow_the_transfer` | 一物一码序列号跟着调拨走（明细关联表） | ok |
| F12 | `f12_in_transit_stuck_shipment_should_alert` | 压 30 天的在途单应可被查出/告警 | **ignored**（LNK-F12） |

### 领域 4 · 应付 ↔ 付款（8）

| 编号 | 用例 | 断言核心 | 结果 |
|---|---|---|---|
| P01 | `p01_full_payment_settles_payable` | 全额付款 → SETTLED + `unpaid = 0` | ok |
| P02 | `p02_partial_payment_marks_partial_paid` | 部分付款 → PARTIAL_PAID | ok |
| P03 | `p03_two_partial_payments_accumulate_to_settled` | 两次部分付款（第二次结清）→ SETTLED | ok |
| P04 | `p04_payment_generates_balanced_voucher` | 付款凭证 借 2202 / 贷 1002，借贷平衡 | ok |
| P05 | `p05_over_payment_is_bad_request` | 超未付余额 → 400，且**不留半成品凭证** | ok |
| P06 | `p06_pay_settled_is_conflict` | 对 SETTLED 再付款 → 409 | ok |
| P07 | `p07_cancel_paid_payable_is_conflict` | 已付款的应付作废 → 409 | ok |
| P08 | `p08_paid_plus_unpaid_equals_amount` | 三次分期，每步 `paid + unpaid = amount` | **ignored**（LNK-P08 真缺陷） |

### 领域 5 · 凭证 ↔ 分录（8）

| 编号 | 用例 | 断言核心 | 结果 |
|---|---|---|---|
| V01 | `v01_reverse_creates_mirrored_red_voucher` | 红冲 → 红字凭证，金额/借贷方向镜像 | ok |
| V02 | `v02_reverse_backfills_pointer_and_keeps_posted` | 原单 `reversed_by` 回写，状态仍 POSTED（不许改 CANCELLED） | ok |
| V03 | `v03_double_reverse_is_conflict` | 重复红冲 → 409 | ok |
| V04 | `v04_reverse_red_voucher_is_conflict` | 红冲红字凭证 → 409 | ok |
| V05 | `v05_unbalanced_manual_voucher_is_bad_request` | 手工凭证借贷不平 → 400，文案点名差额 | ok |
| V06 | `v06_balanced_manual_voucher_is_posted` | 平衡 → POSTED，`entry_count` 正确 | ok |
| V07 | `v07_every_voucher_is_balanced` | 逐张 `Σ借 = Σ贷`（**不扫全库**，见 §6.6） | ok |
| V08 | `v08_each_entry_has_exactly_one_non_zero_side` | 每条分录有且仅有一边非零 | ok |

---

## 4. 这套用例的「不是假绿」证据

60 条 ok 有没有可能其实是「连不上库 → 全部 return → 显示 ok」？做了两道验证：

**(1) 连接证据**：带 `--nocapture` 跑单条，首行是 `联动矩阵测试已连接 cloudpivot_test`。

**(2) 变异测试（mutation）**：临时把两条断言改成错值，确认它们会**红**，然后原样还原：

```text
# 把 T15 的前置「取消前应有 1 条待办」改成 2
thread 't15_cancelled_task_disappears_from_approver_todo' panicked:
assertion `left == right` failed: MUTATION 前置：取消前审批人应当能看到这条待办
  left: 1

# 把 S12 的「过账后库存 12」改成 13
thread 's12_cancel_returns_all_four_ledgers_to_baseline' panicked:
assertion `left == right` failed: MUTATION 中途：库存 12：期望 13，实际 12.0000
  left: 12.0000

test result: FAILED. 0 passed; 2 failed
```

断言真的在读库、真的是实测数字（不是 `assert!(true)` 式陪跑）。

---

## 5. 缺陷记录

### 5.1 LNK-P08（**真缺陷 · 中高**）应付单第二次部分付款被状态机误拒

| 项 | 内容 |
|---|---|
| 用例 | `linkage::payable_chain::p08_paid_plus_unpaid_equals_amount`（当前 `#[ignore]`） |
| 期望 | 分三期付款：每付一笔 `paid += x`、`unpaid -= x`，`paid + unpaid = amount` 恒等，付清自动 SETTLED |
| 实际 | **第二期直接失败**，付款不落库 |
| 严重度 | 中高 —— 普通业务路径（分期付款/分次报销）走不通，且第 3 期以后完全不可用 |
| 修复卡 | **t_a34c68b6** |
| 状态 | 记录在案，**未改业务代码**（P10/P11 在改这块，按卡面要求只报告） |

真实报错（真库复现，非推测）：

```text
P08：付款 3.5 失败 10001 [10003] 应付单 AP-20261031-000153 的状态没有变化
（PARTIAL_PAID → PARTIAL_PAID）：原地流转不是状态变更，不要写状态日志
（重复部分收货请只写业务流水）
```

复现步骤：

1. 建一张收货单（qty 4 × 单价 2.5，税率 13%）→ 过账 → 生成应付 `amount = 11.30`（OPEN）；
2. 登记付款 `1.00` → OPEN → PARTIAL_PAID ✅（`unpaid = 10.30`）；
3. 再登记付款 `3.50` → **失败 10001/10003**，`paid` 仍是 1.00 ❌

根因指针（未改动）：

- 调用点 `backend/src/business/erp/payable_payment.rs:589`：无条件调用
  `state_machine::transition(..., &row.status, status, ...)`，
  「已 PARTIAL_PAID 再部分付款」时 `from == to`；
- 拒绝点 `backend/src/business/shared/state_machine.rs:452`：`if from == to → 409 / 10001`
  （这条护栏本身是**对的**，原地流转不该写状态日志）。

为什么 1343 个单模块测试没抓到：`payable_payment` 的 `p7_*` 只测了「两次付款且**两次都跨状态**」
（OPEN→PARTIAL_PAID→SETTLED），恰好绕开了「同状态再付一次」这一步。本用例把「三期」写进断言，
第一步就撞上。

> **评审注意**：修复方向应是「状态没变时**不调** `transition`」，而不是放宽 `state_machine` 的
> `from == to` 护栏 —— 护栏是 P10 铁律③的唯一入口，放宽会连带影响所有实体。

### 5.2 LNK-F12（**功能缺失 · 中**）在途仓 `IN_TRANSIT` 超期无告警、无扫描

| 项 | 内容 |
|---|---|
| 用例 | `linkage::transfer_chain::f12_in_transit_stuck_shipment_should_alert`（`#[ignore]`） |
| 期望 | 发货后长期未收货的在途单能被查出并推给人（阈值可配，默认 7 天） |
| 实际 | 全仓没有任何实现 |
| 严重度 | 中 —— 库存既不在调出仓也不在调入仓，账实差异只能靠人肉发现 |
| 修复卡 | **t_ff20409a** |

已核实的证据（不是"看起来没有"）：

```bash
grep -rn "IN_TRANSIT" --include=*.rs src/ | grep -i "timeout|alert|超时|告警"   # 零命中
grep -rn "transit" src/ migrations/ | grep -i "job|schedule"                    # 零命中
```

`transfer_model` 只在发货时回填 `transit_warehouse_id`，没有任何超时判定/扫描任务/告警入口。

---

## 6. 口径差异（**不是缺陷**，需产品/架构拍板）

卡面写的是「预期」，实现是「现状」。这几条我按**正确行为**写断言，并把差异记在这里，
免得下一张卡重新猜。

### 6.1 T10：取消后点同意 → 实例「变 APPROVED」？
卡面写「当前行为，标注为缺陷」。实测 P11 撤销落地后，待办已是 `CANCELLED`，
`engine::vote` 对非 PENDING 待办走**幂等分支**（`ActionOut{ idempotent: true }`，
架构 §4.5.1 第 ① 步的既定行为）。所以断言写成「**零状态变化**」：
实例仍 CANCELLED、待办仍 CANCELLED、任务仍 CANCELLED —— 行为正确，只是不返回 Err。

### 6.2 T11：重复取消 → 幂等返回成功 还是 409？
卡面写「幂等，不报错」。实测返回 `409 + 63010 任务已处于该状态，无需流转`。
两种做法都满足真正的风险点（**零副作用**），显式 409 还能让前端提示"已经是已取消"，
比静默 200 更不易误导。用例断言「零副作用 + 干净 4xx（不是 500、不是二次退库存）」。

### 6.3 F03：发货时「调出仓扣减 + 在途仓增加」？
实现不是这样：库存只在 `IN_TRANSIT → DONE` 的结算里双写（`settle_transfer`），
发货只回填 `transit_warehouse_id`，**没有实物在途台账**。F03 断言的是这个记账口径；
F04/F05 才是"数量真的动了"的地方。若产品确需"在途仓真实持仓"，属于新需求（与 LNK-F12 同族）。

### 6.4 F08：审批中人工直推 APPROVED「当前可」？
实测 P5 起已有门闸：挂着在跑审批单时人工推 APPROVED 被 `409 + 65030` 拦住。
用例断言**正确行为**（拦住 + 状态与实例都不动）。

### 6.5 F09：超发在「建单」还是「结算」被拒？
实测校验在 `IN_TRANSIT → DONE` 的结算里（条件 UPDATE 返回 0 行 → 整笔回滚），
不在建单时。用例一路推到 IN_TRANSIT 再断言收货失败且状态/库存不变。

### 6.6 V07：卡面要求「全库扫描 Σ借 = Σ贷」
**故意不扫全库**：`cloudpivot_test` 是并发共享库（P9/P10/P11 同时在造数据），
全库扫描会被别人跑到一半的中间态污染 → 偶发红（P9 已踩过"全局计数漂数"）。
改为「本用例可归属的凭证全集」逐张校验 —— 覆盖同样的不变量，但不依赖别人的进度。

---

## 7. 测试基础设施（`backend/tests/linkage/common.rs`）

| 要求 | 实现 |
|---|---|
| 每用例独立前缀 | `LNK-<TAG>-<uuid8>`（取 UUIDv7 **末 8 位随机段**：前 8 位是时间戳高段，同毫秒会撞唯一键——已实测踩过） |
| 自清理 | `Ctx::cleanup()` 按本用例 **id 精确删净**，外键反序；不用 `LIKE '%'`、不做全局 `COUNT(*)` |
| 不依赖别的用例数据 | 主数据（角色/三用户/单位/供应商/两仓/两物料/库位）也自己造，不共用种子 |
| 只用 `cloudpivot_test` | `ensure_db()` 里硬校验连接串必须含 `/cloudpivot_test`，否则 **panic**（不做"尽力而为"） |
| 无库不假绿 | `LINKAGE_REQUIRE_DB=1` 时没 URL 直接失败；否则打印 `SKIP` 并返回（与仓内既有约定一致） |
| 全局池 × 多 runtime | `max_lifetime = 0` / `idle_timeout = 0`，逼池子每次借出新建连接（W1b/P2-B1 已验证做法） |
| **清理自检（不许静默留夹具）** | `cleanup()` 末尾按本用例 id/前缀在 22 张表上数残留，非 0 或任一 DELETE 报错 → **当场 panic**（理由见下） |

> **为什么加了这条自检（实测踩出来的）**：第一次跑绿后我去真库数残留，发现 F11 留了
> `biz_material / biz_warehouse / biz_material_serial` 各若干行，而用例是 **ok** 的。
> 原因是 `cleanup()` 里「删 `biz_material_serial`」排在「删 `biz_stock_transfer_item_serial`」**之前**，
> 撞外键 → 序列号删不掉 → 物料、仓库连带删不掉；而失败信息是 `println!("WARN: ...")`，
> **被 libtest 的 stdout 捕获吞掉**，全绿的用例根本看不到。两个问题都修了：
> ① 调换删除顺序（先删关联表）；② 把「清理干净」变成硬断言，不再靠人肉看日志。

### 夹具残留核对（跑完后在真库逐表数，全部 0）

```sql
SELECT 'user',count(*) FROM sys_user WHERE username LIKE 'LNK-%'
UNION ALL SELECT 'role',count(*) FROM sys_role WHERE role_code LIKE 'LNK-%'
...  -- 共 18 张表
```

上一次跑完之后的结果：

```text
         t          | count
--------------------+-------
 flow_instance      |     0
 inventory          |     0
 inventory_location |     0
 inventory_txn      |     0
 location           |     0
 material           |     0
 payable            |     0
 role               |     0
 serial             |     0
 status_log         |     0
 stock_in           |     0
 supplier           |     0
 task               |     0
 transfer           |     0
 unit               |     0
 user               |     0
 voucher            |     0
 warehouse          |     0
(18 rows)
```

### 已知限制（写清楚，不装看不见）

1. **断言 panic 时 `cleanup()` 不执行**（Rust 没有 async 析构）。失败那一轮会留一套夹具：
   编码带随机后缀，**不会污染别的用例**，但会累积。收尾工具：
   `backend/tests/linkage/cleanup_leftovers.sql`（整份在一个事务里，任一步外键失败则全回滚）。
   该脚本第一版有顺序 bug —— `biz_material_serial` 早于 `biz_stock_transfer_item_serial` 被删，
   整批在第一个外键就回滚（表面"跑完没报错"，实际一行没删）。已修正为「先删关联表」并加了收尾核对查询。
2. **必须串行跑**（`--test-threads=1`）：T17 会临时改 `TASK_ACCEPT` 的 `is_default`。
3. F10「并发不死锁」是在**单个用例内**起并发任务，验证的是行锁顺序；不是多进程压测。

---

## 8. 顺带发现的仓库级问题（与本次改动无关，报备）

`cargo clippy --all-targets -- -D warnings` 在**当前 main** 就红，全部落在业务源码（不是我改的文件）：

```text
src/business/impexp/definition.rs:493   manual `!RangeInclusive::contains` implementation
src/business/task/handler.rs:2987      manual `!RangeInclusive::contains` implementation
src/business/task/template.rs:398      manual `!RangeInclusive::contains` implementation
```

即 `scripts/check.sh` 现在过不了。原因是工具链 clippy 版本比写代码时新（新增 `manual_range_contains` 规则）。
本套用例自己的 clippy 是干净的：

```bash
cargo clippy --test linkage -- -D warnings -A clippy::manual_range_contains   # Finished, 0 warnings
cargo fmt -- --check                                                          # 通过
```

建议单独开卡统一修（`!(a..=b).contains(&x)`），别混在业务卡里。

---

## 9. 后续（已建卡，不自己动手改业务代码）

| 卡 | 内容 | 承接 |
|---|---|---|
| `t_a34c68b6` | 应付第二次部分付款被状态机误拒（LNK-P08） | backend-developer |
| `t_ff20409a` | 在途仓 `IN_TRANSIT` 超期告警缺失（LNK-F12） | backend-developer |

两张卡修好后，把对应用例的 `#[ignore]` 去掉即转正（注释里已写明原因与卡号）。

---

## 10. 真实运行输出（粘贴）

```text
$ LINKAGE_DB_URL='postgres://lnkrole:***@127.0.0.1:5432/cloudpivot_test' LINKAGE_REQUIRE_DB=1 \
  cargo test --test linkage -- --test-threads=1

running 62 tests
test linkage::payable_chain::p01_full_payment_settles_payable ... ok
test linkage::payable_chain::p02_partial_payment_marks_partial_paid ... ok
test linkage::payable_chain::p03_two_partial_payments_accumulate_to_settled ... ok
test linkage::payable_chain::p04_payment_generates_balanced_voucher ... ok
test linkage::payable_chain::p05_over_payment_is_bad_request ... ok
test linkage::payable_chain::p06_pay_settled_is_conflict ... ok
test linkage::payable_chain::p07_cancel_paid_payable_is_conflict ... ok
test linkage::payable_chain::p08_paid_plus_unpaid_equals_amount ... ignored, 缺陷 LNK-P08：应付单第二次同状态部分付款被状态机误拒（10001/10003），待修复卡处理
test linkage::stockin_chain::s01_post_writes_all_six_ledgers ... ok
...
test linkage::transfer_chain::f12_in_transit_stuck_shipment_should_alert ... ignored, 功能缺失：IN_TRANSIT 在途超时告警未实现（无扫描任务、无告警表），见 docs/test-report-linkage.md LNK-F12
...
test result: ok. 60 passed; 0 failed; 2 ignored; 0 measured; 0 filtered out; finished in 312.10s
```

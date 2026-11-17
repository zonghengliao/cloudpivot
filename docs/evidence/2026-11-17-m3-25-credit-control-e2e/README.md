# M3-25 销售缺口 E2E：信用控制 · 超限场景（证据）

- 卡：`t_76dcfc44`（M3-25【M3·销售】销售缺口 E2E：信用控制 / 超限场景）
- 日期：2026-11-17
- 结论：**12 步信用链一次跑通，演练 35/35 PASS、0 失败；有牙演练（改坏期望）与 SKIP 守卫自检均确认会红；零迁移 / 零权限码 / 零前端改动。**

## 1. 这一卡补的是什么缺口

信用额度前面已经有五张卡（M3-09 策略三态、M3-10 实时快照、M3-11 超信用拦截、M3-12 核销释放、
M3-13 并发单测），但它们的夹具是**各管一段**的：

| 上游卡 | 夹具形态 | 断在哪 |
|:--|:--|:--|
| M3-09 `sales_order_handler::tests` | 订单 SQL 直插 `DRAFT`，客户额度手改 | 提交守卫对；提交之后的占用没人验 |
| M3-11 `sales_approval_bridge::tests` | 实例 + 订单都是 SQL 直插 | 审批闸对；「额度是不是这张单吃掉的」没人验 |
| M3-12 `credit_release_tests` | 订单 / 应收 SQL 直插，`MANUAL` 应收 | 核销释放对；「真实出库挂的应收能不能核销释放」没人验 |
| M3-13 `credit_concurrency_tests` | 同上 | 并发 CAS 对；整条业务链上并发不重复 |

于是「**客户额度从 0 被一张真审批的订单吃掉、下一张单被真的拦住、钱真的收回来后额度真的还了**」
这件事，在库里从来没有被一条用例走过。本卡用一个客户、七张单、一次收款把它串起来。

## 2. 链路与断言（12 步 · 112 处断言点）

```text
① 客户额度 1000 / BLOCK；订单 A(800) 建单（草稿不占）→ 提交 → 真派真批 → APPROVED ⇒ 占用 800
② 订单 B(300) 提交：409 + 10003，单据留 DRAFT、零审批实例、额度不动、零占用台账
   └ 快照：verdict=BLOCK / allowed=false / total=1100 / over=100 / available=200
③ 额度调 1100（走客户编辑真接口）→ B 重提：800+300=1100 **边界等号算通过** → 审批 ⇒ 占用 1100
④ 订单 C(300) 提交 409：额度真被吃满
⑤ 发货 A → 过账（在途）→ 签收确认 → 应收 800 OPEN；**占用仍是 1100**（挂账≠释放）
⑥ 登记收款 800 → 核销 → **核销即释放**：占用 1100→300，回执 released=800，台账 AR_SETTLED -800
⑦ C 重提（300+0+300 ≤ 1100）→ 审批 ⇒ 占用 600（被拦的单是活的）
⑧ 策略 WARN：D(700) 提交放行 + credit_warning + 台账 delta=0 → 审批 ⇒ 占用 1300（台账幂等只有 1 行）
⑨ 策略 PASS：E(500) 静默放行（warning=None、台账不增）→ 审批 ⇒ 占用 1800
⑩ 审批点闸：额度 5000 → F(1000) 提交通过 → 额度调低到 100 → 审批 409+10003
   └ 待办原样保留、单据仍 SUBMITTED、额度不动
⑪ 过审兜底（约定 11）：绕过前闸 approve → 回调发现超限 → **打回 DRAFT** + remark 写原因，
   实例 APPROVED、额度不动、不记占用台账；额度调回 5000 重提 → 审批通过 ⇒ 占用 2800（不留死单）
⑫ 不控额度：额度 0 + BLOCK → G(5000) 照过（UNCONTROLLED / available=null）→ 审批 ⇒ 占用 7800
收口：缓存列 == 公式（S9）、台账 Σdelta == 缓存列、无 PENDING 实例、七张单终态一致、零残留
```

单据/审批/收款全部走真实入口：`sales_order_handler::create_sales_order / submit_approval`、
`doc_reader::{guard_self_approval_for_task, guard_approve_for_task}` + `engine::approve` +
`doc_reader::dispatch_settled`（与 HTTP `POST /business/flow/task/{id}/approve` 逐行同款）、
`shipment_handler::{create_shipment, change_shipment_status, confirm_receipt}`、
`receipt::{create, allocate}`、`customer_handler::update_customer`、`credit_handler::credit_snapshot_of`。

## 3. 交付物

| 文件 | 作用 |
|:--|:--|
| `backend/app/src/business/erp/sales/credit_control_e2e_tests.rs`（新，1233 行） | 超限全链用例（1 条 12 步 / 112 处断言点）+ 收尾零残留用例（1 条） |
| `backend/app/src/business/erp/sales/mod.rs` | 挂 `#[cfg(test)] mod credit_control_e2e_tests;` |
| `scripts/verify/m325_credit_control_e2e_drill.sh`（新，224 行） | 克隆演练库 → 补 V0405…V0445 → 前置断言 → 本卡 + 信用域 + 销售域 + 发货域 → SKIP=0 + 正向标记 → 夹具零残留 → DROP；`M325_BAD_URL=1` 为 SKIP 守卫自检 |
| `docs/evidence/2026-11-17-m3-25-credit-control-e2e/*` | 本目录：主跑 / 步骤轨迹 / SKIP 自检 / 库侧快照 / 有牙 |

**零迁移、零权限码、零菜单、零前端改动**（纯测试卡）。

## 4. 真跑输出

### 4.1 演练脚本（`10_drill_full.out`）

```text
== 汇总：PASS=35 FAIL=0 ==
ALL PASS
```

| 项 | 结果 |
|:--|:--|
| 模板克隆 + 补 41 条业务迁移（V0405…V0445） | PASS |
| 前置：`biz_credit_log` / `biz_inventory_in_transit` / `biz_receipt_allocation` | PASS |
| 前置：`biz_customer.credit_policy` 列 + `ck_biz_customer_credit_policy` + `so_approve` + `SALES_SUPERVISOR` | PASS |
| 前置：信用链五张单据没有被人为关掉的状态机边 | PASS |
| 本卡 `credit_control_e2e_tests` | **2 条全绿，SKIP=0，正向标记在位** |
| 信用域整组 `business::erp::credit` | **44 条全绿，SKIP=0** |
| 销售域整组 `business::erp::sales` | **361 条全绿，SKIP=0** |
| 发货域整组 `business::erp::shipment` | **49 条全绿，SKIP=0** |
| 夹具清场（客户/账号/订单/发货/出库/审批实例/凭证/应收/收款/核销明细/台账/三本账/在途）15 项 | 全 0 |

> 本机在 99（开发机），库在 145（`172.18.10.145:5432`），符合卡面「场景 A」。

### 4.2 步骤轨迹（`11_e2e_nocapture.out`，节选）

```text
[M3-25] 步 1/12 建订单 A（含税 800 / 额度 1000）→ 提交 → 真审批 → APPROVED
[M3-25] 步 2/12 订单 B（含税 300）超限：提交必须 409 + 10003，单据留 DRAFT
[M3-25] 步 3/12 额度调高到 1100（走客户编辑接口）→ B 重提：800 + 300 = 1100 边界通过
[M3-25] 步 5/12 发货 A → 过账 → 签收确认 → 挂应收 800（此刻额度**不释放**）
[M3-25] 步 6/12 登记收款 800 → 核销该应收 → 占用 1100 → 300（核销即释放）
[M3-25] 步 10/12 额度调 5000 → 订单 F(1000) 提交通过 → 额度调低到 100 → 审批被 409 拦
[M3-25]   节点 n_sales 的审批前闸已拦下（符合预期）
[M3-25] 步 11/12 绕过前闸直接 approve → 回调发现超限 → 打回 DRAFT（可重提）
[M3-25] 全链通过：M325-CUST 额度 1000→1100→0 的 12 步信用链，终态占用 7800.00
test result: ok. 1 passed; 0 failed
```

### 4.3 库侧取证（`13_db_snapshot.txt`，夹具保留版 `M325_KEEP_FIXTURES=1`）

```text
① 缓存列 vs 公式：  credit_used 7800.0000 / 公式 7800.0000 / 一致 t
② 七张单终态：      OUTBOUND, APPROVED ×6；F 的 remark 保留「审批通过时信用额度不足…已退回草稿」
③ 信用台账（9 行）：
   ORDER_OCCUPY    SO-…0001     0 →   800   Δ  +800
   ORDER_OCCUPY    SO-…0002   800 →  1100   Δ  +300
   AR_SETTLED      RC-…0001  1100 →   300   Δ  -800   ← 核销即释放
   ORDER_OCCUPY    SO-…0003   300 →   600   Δ  +300
   OVER_LIMIT_WARN SO-…0004   600 →   600   Δ     0   ← WARN 放行留痕（delta=0）
   ORDER_OCCUPY    SO-…0004   600 →  1300   Δ  +700
   ORDER_OCCUPY    SO-…0005  1300 →  1800   Δ  +500   ← PASS 静默放行（无警示行）
   ORDER_OCCUPY    SO-…0006  1800 →  2800   Δ +1000   ← 打回草稿后重提才占用
   ORDER_OCCUPY    SO-…0007  2800 →  7800   Δ +5000   ← 不控额度
④ 台账 Σdelta 7800.0000 == 缓存列 7800.0000
⑤ 应收 AR-…0001 800 已收 800 / SETTLED，核销额 800，收款单 RC-…0001
⑥ 审批实例 8 张（七张单 + F 被打回那次）全 APPROVED
⑦ 成本结转凭证 PZ-…0001 借 400 / 贷 400（80 件 × 成本 5，2 条分录）
⑧ PENDING 实例 = 0（无死单）
```

## 5. 守卫有牙（两次反证）

| 反证 | 做法 | 结果 | 证据 |
|:--|:--|:--|:--|
| 断言有牙 | 把「核销后占用」期望 300 改成 301 | `left: 300.0000 / right: 301` + `汇总：PASS=32 FAIL=2` + 退出码 1；**演练库照样被 DROP**（`cp_m325%` 只剩开发库） | `20_teeth_break_expectation.out` |
| SKIP 守卫有牙 | `M325_BAD_URL=1` 把连接串故意写错 | 本卡组 `出现 1 行 SKIP` + `缺正向标记「[M3-25] 全链通过」`；四组共 5 条 FAIL、退出码 1 | `12_skip_guard.out` |

> 为什么单独做第二档：真库用例「连不上 → 打印 `SKIP:` → 早退」在 Rust test harness 里仍然记
> `ok`（`0.04s / 2 passed`）。`--nocapture` 下测试名与 `SKIP:` 打在同一行，
> `grep '^SKIP'` 会数成 0 —— 这正是上游 `m324` 演练里埋着的假绿口径。本卡改成
> `grep 'SKIP:'` **加**一条正向标记（真跑过的用例一定打 `[M3-25] 全链通过`），两条同时绿才算过。

## 6. 门禁

```text
cargo fmt -- --check                                  通过
cargo clippy -p cloudpivot --lib --tests -- -D warnings   0 warning
python3 scripts/lint/path_refs_gate.py                PASS（1014 脚本 / 0 处失效路径）
```

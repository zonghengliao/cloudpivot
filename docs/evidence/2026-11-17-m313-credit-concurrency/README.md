# M3-13 客户信用：单测（并发下单、部分收款）· 含竞态

- 卡：`t_5fc52acf`（M3-13【M3·销售】客户信用：单测（并发下单、部分收款），含竞态）
- 上游：`t_464b7265`（M3-09 策略三态）、`t_8729b656`（M3-10 实时占用）、
  `t_8a08ce48`（M3-11 超信用拦截）、`t_f4f53ec4`（M3-12 核销即释放）
- 下游：`t_5f9041b6`（M3-14 客户信用页）

## 一、这张卡要的是什么

M3-09/10/11/12 建的都是**单线程语义**：一次请求、一个事件、一个断言。真实销售链不是这样跑
的 —— 同一客户名下的「占用」（下单 / 审批）与「释放」（收款核销）是**并发事件**。本卡把并发
口径钉成真库用例，断言的是「并发之后仍然成立的不变量」，不是某一种特定交错。

**不改生产代码、不加迁移、不造权限码、不动前端**（本卡只做单测；下一个可用迁移号仍是 `V0438`）。

## 二、交付物

| 类别 | 文件 | 说明 |
|------|------|------|
| 真库并发用例 | `backend/app/src/business/erp/credit_concurrency_tests.rs`（新，7 条 / ~1090 行） | `multi_thread` 运行时 + `Barrier` 同刻起跑 + 连接池预热；断言敞口 / 缓存列==公式 / 台账链 / 重放幂等四条不变量 |
| 注册 | `backend/app/src/business/erp/mod.rs` | `#[cfg(test)] mod credit_concurrency_tests;` |
| 演练脚本 | `scripts/verify/m313_credit_concurrency_drill.sh`（新，29 断言） | 模板克隆 + 补 V0405…V0437 + 静态防假绿断言 + 5 组真库用例 + 收尾残留/S9 对拍 |
| 证据 | `docs/evidence/2026-11-17-m313-credit-concurrency/{README.md,00-drill.out,01-backend-lint.out}` | 本文件 |

## 三、竞态模型：三道闸各自承诺什么（**结论，不是假设**）

| 闸 | 入口 | 语义 | 并发下的真实行为（本卡实测） |
|---|---|---|---|
| ① 提交闸 | `sales_order_handler::ensure_submittable` | **入场预估**，不是承诺 | 读「已提交快照」（`credit_used` + `SUBMITTED` 合计），两条提交并发时彼此看不见 ⇒ **可双双进在审**；但 `credit_used` 一分不涨（占用唯一时机 = `APPROVED`） |
| ② 审批闸 | `sales_approval_bridge::guard_credit_on_approve` + `settle`（= `credit::decide` / `assess_order`） | **承诺** | 在审单互相计入 `pending` ⇒ **合计超限时一张都落不了 `APPROVED`**（方向保守：宁可两张都拦，也不许超卖） |
| ③ 核销 | `receipt::allocate` → `credit::sync` | **行锁 + CAS** | 收款单 / 应收行 `FOR UPDATE`（按 `receivable_id` 排序防交叉死锁）；额度同步 CAS 重试（`SYNC_ATTEMPTS = 5`） |

于是「并发下单」的结论是：**提交闸会超收在审单（入场预估），但敞口由审批闸守住；代价是两张
单都审不过、撤一张即可放行另一张（可恢复，不是死单）**。用例 ① 把这条如实钉住，包括「3 轮内
必须命中一次竞态」的护栏 —— 命中不了就说明竞态断言成了死代码。

## 四、**关键发现：不预热连接池，并发用例就是假绿**

池是**惰性建连**的（`min_connections = 1`）。冷池时第二条任务的**第一条**语句要付一次完整
握手（99 → 145 实测 ~20ms），于是两条「并发」任务被错开成**串行** —— 守卫总能读到对方
**已提交**的状态，竞态分支永远不执行，用例照样全绿。

实测对照（同一条用例、同一台库）：

| 场景 | 两条任务的起止 | 结果 |
|---|---|---|
| 不预热 | `0ms→24ms` / `1ms→24ms`（第二条在第一条 commit 之后才读到守卫） | `SUBMITTED / DRAFT`（串行分支，竞态没测到） |
| 预热 2 条连接 | `0ms→24ms` / `1ms→24ms`（真重叠） | `SUBMITTED / SUBMITTED`（**命中竞态**，审批闸两张都拦） |

所以 `warm_pool(n)` 在**每个并发段之前**都调，且脚本把它做成静态断言（`warm_pool(` ≥ 8 处）。
生产环境池是热的，并发是常态 —— 用例必须模拟那个状态。
另外：`current_thread` 运行时下 `tokio::spawn` 不会真并行，7 条用例全部 `multi_thread`（也是静态断言）。

## 五、7 条用例各自钉什么

| # | 用例 | 断言（是「等于多少」，不是「跑通了」） |
|:--|:--|:--|
| ① | `db_concurrent_submit_is_admission_only` | 两张 600 同时提交（额度 1000）：敞口 0、`credit_used` 0、无 `APPROVED`；命中竞态时两张都进在审且**每张 `assess_order` 都被拦、超限额精确 200**；撤一张后另一张可审 ⇒ 敞口 600 |
| ② | `db_concurrent_approve_within_limit_both_land` | 两张 400 同时审批（合计 800 ≤ 1000）：**两张都落 `APPROVED`**、占用 800（丢更新会停在 400）、台账 2 行链 `0→400→800`、cause=`ORDER_OCCUPY`、ref_no=订单号、trigger=`API` |
| ③ | `db_concurrent_approve_over_limit_neither_lands` | 两张 600 同时审批（合计 1200 > 1000）：**零落库**、两张都留在 `SUBMITTED`、拦截理由来自信用闸、**台账为空**（没有假放行记录）；撤一张后另一张可审 ⇒ 600 |
| ④ | `db_concurrent_same_receipt_allocate_releases_once` | 同一张收款单并发两次核销 300：应收 300/700 `PARTIAL`（**不是 600**）、占用 700、台账 2 行链 `0→1000→700`、`delta=-300`、**只有一份回执**（另一次增量 0 ⇒ 无回执无台账） |
| ⑤ | `db_concurrent_multi_receipts_serialize_on_one_receivable` | 4 张收款并发各核 100：应收 400/600、占用 600、台账 5 行链 `0→1000→600`、4 笔释放合计 `-400` |
| ⑥ | `db_concurrent_allocate_over_balance_one_wins_one_rejected` | 2 张收款并发各核 600（应收 1000）：**恰好 1 成功 1 被余额闸拒**、应收 600/400 `PARTIAL`、占用 400、台账只多 1 行；未结金额**从不为负** |
| ⑦ | `db_concurrent_occupy_and_releases_converge` | 同一客户并发：审批通过 +300 与两笔各 100 释放：占用 400→**500**、台账 4 行链 `0→400→700→600→500`、`Σdelta=+100`、两笔核销都带回执（各 100） |

公共不变量（每条用例都跑）：

1. **敞口 ≤ 额度**：`APPROVED/PARTIAL_OUT/OUTBOUND` 含税合计不超过 `credit_limit`；
2. **缓存列 == 公式**（S9 同源，容差 0.01）：`credit::stored == credit::usage_of().used == 期望值`；
3. **台账首尾相接**：`before_used(i) == after_used(i-1)`，首行 `before = 0`，末行 `after = 最终值`，
   `Σdelta == 末值 − 初值` —— 这是 CAS 真把并发写串行化了的**直接证据**（没有 CAS 就会两行同 `before`）；
4. **重放不双扣**：同一笔核销并发重放，`received_amount` 不被写两份、额度只释放一次。

## 六、验证（全部真实执行，日志在 00/01）

| 项 | 结果 |
|---|---|
| `cargo fmt -p cloudpivot -- --check` | 0 差异（见 `01-backend-lint.out`） |
| `cargo clippy -p cloudpivot --lib --tests -- -D warnings` | 0 warning（比卡面要求多了 `--tests`，因为新用例在 test target 里） |
| `bash scripts/verify/m313_credit_concurrency_drill.sh` | **ALL PASS，PASS=29 FAIL=0**（见 `00-drill.out`） |
| └ `business::erp::credit_concurrency_tests` | 7 条全绿，SKIP=0（第 1 轮即命中竞态） |
| └ `business::erp::credit_release_tests` | 7 条全绿，SKIP=0（M3-12 回归） |
| └ `business::erp::credit` | 44 条全绿，SKIP=0 |
| └ `business::erp::receipt` | 35 条全绿，SKIP=0 |
| └ `business::erp::receipt_refund` | 24 条全绿，SKIP=0 |
| └ 收尾 S9 对拍 | M313 客户 `credit_used` 与公式 **0 漂移**；夹具订单/应收/收款/台账/状态日志 全 0 |
| 演练库 | 145 上 `cp_m313%` 残留 **0**（脚本自 DROP；手工建的临时库也已删） |

演练库按卡面约束建在 **145（`172.18.10.145:5432`，用户 `cloudpivot`）**，库名含 `drill`；
模板库 `cloudpivot_test` 落后 33 条业务迁移，脚本按 `V0405…V0437` 逐条幂等补齐（与 M3-10/11/12 同法）。

## 七、决策

1. **不在本卡改提交闸的并发语义**（不给它加锁 / 不用咨询锁串行化）：卡面是「单测」，改闸等于改
   生产行为（会把并发的提交串行化、拉长响应），且当前敞口由审批闸守住，没有超卖。本卡把
   「提交闸 = 入场预估」这件事**用用例钉住**，并留下「撤一张即可放行」的可恢复性断言。
2. **审批闸的并发用 `credit::assess_order` 直测**，不启流程引擎：M3-11 已用真流程（实例 / 待办 /
   回调 / 打回草稿）覆盖单线程语义，本卡只压「闸 → 写状态 → 落额度」这一脚的数据层竞态。
3. **断言成对写**：既断言「该发生的发生了」（两张都落、4 笔都释放），也断言「不该发生的没发生」
   （不是 600、不是 -100、未结不为负、台账为空、只有一份回执）。
4. **台账链断言抽成一个函数**（`assert_ledger_chain`），5 条用例复用：并发正确性的证据只写一份，
   避免每个用例各写一套松紧不一的检查。
5. **`warm_pool` 写进用例并在模块头解释「不预热=假绿」**：这是本卡最重要的工程教训，写在代码里
   比写在汇报里更容易被下一个人看见。

## 八、已知边界（**不是**漏了）

1. **提交闸并发会超收在审单**（入场预估），代价是两张都审不过、需要撤一张 —— 方向安全，钱不超。
   真要做到「提交即原子预约」，得给提交路径加按客户的咨询锁（或把守卫挪进事务并锁客户行），
   那会改变响应时延与现有错误码分布，属独立卡。
2. **并发审批的「双双被拦」是保守结果**：两张合计超限时，本来就没有「先到先得」的语义 ——
   谁先批都可能被告知额度不足，此时的处置是调额 / 收款 / 撤单（闸的文案已经这么写）。
3. **预收（收款未核销）不释放额度**（口径沿用 M3-12 / PRD 4.7.2）：钱没挂到应收上，公式里
   `received_amount` 没变。
4. **坏账（`WRITTEN_OFF`）仍占额度**：口径边界沿用 SO-38（保守方向，不会超卖），属坏账卡。
5. **`.worktrees/`、README、frontend 的其它在途改动**不是本卡的（同仓多 worker），提交时只按路径
   `git add` 本卡的 5 个文件。

## 九、给下游卡

- **M3-14（客户信用页）**：页面主数据仍走 `GET /business/erp/credit/{customer_id}`（M3-10），
  核销 toast 读 `credit_release`（M3-12，金额是定点字符串）。并发口径对页面没有新增要求：
  提交 / 审批被拦时的报文已经是「额度 / 已占 / 在审 / 本单 / 超出」五个数。
- 任何后续改**提交闸**的卡（要「原子预约」的话）：本文件的用例 ① 就是验收前置 ——
  改完之后它会从「双双进在审」变成「一张被拦」，届时需要同步改断言与模块头口径。

# P1-FIX-5C：跨单据联动触发点接线清单与「防误配」设计

> 卡：`t_21dc81c3`（architect）· 父卡：`t_53c23814`（P1-FIX-5B）
> 关联实现卡：`t_befea97a`（backend-developer，同一 P1-FIX-5C 的代码落地面）
> 判据来源：`backend/src/business/flow/linkage/spec.rs::TRIGGER_SPECS`（唯一真相）
> 日期：2026-11-05

---

## 0. 一句话结论

白名单里的 15 个触发点，**只有 3 个真的有人调联动引擎**（`STOCK_IN` / `STOCK_OUT` / `TASK`），
其余 12 个「能保存、能启用、永远不触发」。本卡的选择是**短期防误配 + 中期分批接线**：

1. 给 `TriggerSpec` 加 `wired`（真实调用的才有），配置页据此标注 + 强制二次确认，
   写入口再硬门一次（未接线 + 启用 + 未显式确认 ⇒ `400`）——**不许静默保存**；
2. 12 个未接线触发点按业务价值排成 4 批，每批用同一套接线标准形 + 同一套准入门槛，
   **没有端到端证据的接线不算接线**（不接受只加一个 `run(...)` 调用）。

> 关键区分：`wired`（触发点有没有人调引擎）与 `implemented`（动作接管没有）是**两个独立的门槛**。
> 一个配置真正生效必须两个都过。当前 15×11 的组合里，同时满足的只有 3 个触发点 × 4 个动作（见 §5）。

---

## 1. 事实与证据（先摆现场）

### 1.1 改动前的实测结论（P1-FIX-5B）

`docs/evidence/20261105-p1fix5b-linkage-presets/README.md`「偏差与范围说明」第 2 条：

> 只有 2/15 个触发点真接线（TASK、STOCK_IN），其余 13 个触发点上配规则永不触发且配置页无提示。

5B 修掉的是其中一条**真断链**：`STOCK_IN/POSTED` 原来只走 ⑦ 步凭证专用路由
（`linkage::engine::stockin_voucher`），通用引擎的 `run` / `run_deferred` 在这个触发点上
**一次都没被调用** ⇒ 在收货过账上配 `SEND_NOTICE` 等于配了白配。修法是调用方补第 ⑧ 步
`run_excluding(..., &[GENERATE_VOUCHER])` + commit 后 `run_deferred`。

### 1.2 代码事实（本卡开工时实测，非引用）

| 事项 | 实测值 | 取证命令 |
|---|---|---|
| 白名单触发点 | 15 个（13 个状态机实体 + `TRIGGER_TASK` / `TRIGGER_FLOW_INSTANCE`，这两个在 `TRIGGERS` 尾部） | `grep -n "pub const TRIGGERS" -A 25 spec.rs` |
| 已接线 | 3 个：`STOCK_IN`、`STOCK_OUT`、`TASK` | `spec::wired_triggers()` |
| 真实调用引擎的生产文件 | 4 个：`erp/stockin_handler_post.rs`、`erp/stockout_handler.rs`、`erp/stockout_linkage.rs`、`task/task_handler_notice.rs` | `tests_wiring::wired_triggers_match_call_sites` 反向扫描 |
| 白名单动作 | 11 个 | `spec::ACTIONS` |
| 已接管动作 | 4 个：`SEND_NOTICE`、`CREATE_TASK`、`GENERATE_VOUCHER`、`UPDATE_RECEIVED_QTY` | `spec::implemented_codes()` |
| 占位动作 | 7 个（写入口 `400`，运行期 ERROR + 跳过） | 同上 |

**这两组数字是本文全部判断的基础**：白名单 ≠ 接线（触发点侧），白名单 ≠ 可用（动作侧）。
P1-FIX-5C 之前，配置页只把「动作」这一侧的占位项置灰了，触发点这一侧**一个提示都没有**。

### 1.3 为什么这是「最坏的失败模式」

- 静默：没有报错、没有日志、没有告警——管理员在页面上看到「已启用」，业务上什么都没发生；
- 延迟暴露：往往要等到「该发通知没发」「该建任务没建」被业务方投诉才发现，中间已经过了几个业务周期；
- 归因困难：现场证据里只有「规则表里有一条 enabled 的规则」，没有任何东西说明它挂在没接线的触发点上。

> 结论：**允许保存一条永远不会触发的规则**，比**禁止保存**代价高得多。
> 所以默认行为必须是拒绝，把「我明确知道现在不生效」变成一次显式确认。

---

## 2. 设计决策

### D1：短期走「防误配」，不追求一次补齐 12 个触发点

| 方案 | 做法 | 为什么不选 / 为什么选 |
|---|---|---|
| A. 全量补齐 | 本卡把 12 个触发点全接上 | ❌ 每个触发点都要改事务边界（哪一步 `InTx`、哪些动作排除、commit 后做什么）+ 端到端真机证据。12 个一次做 = 12 份回滚风险同时上线，且 5B 已证明「接线最容易错的是排除清单」（重复扣库存 / 一张单两张凭证） |
| B. 直接禁选 | 未接线触发点从下拉里删掉 | ❌ 会挡掉「为下周上线先备好配置」的真实需求；且历史脏行（已经配了的）会变成看不见的幽灵数据 |
| **C. 标注 + 二次确认 + 写入口硬门** | `wired` 字段下发；下拉标注；表单双确认；写入口 `400` 兜底 | ✅ 三层都不依赖前端自觉；`allow_unwired` 只表达「我已知晓它当前不触发」，不是「解除限制」 |

**决策：C。** 判定标准写进代码（`spec::TRIGGER_SPECS`），页面与写入口都读它，避免两份真相。

### D2：`wired` 的判据 = 「生产代码里有没有调用引擎入口」，且必须可自动核对

手写一个 `wired: true` 一定会漂移：重构把调用删了没人发现，或者偷偷多接一个触发点却没登记。
所以每个触发点额外声明**证据对**（文件 + 文件里必须出现的调用片段），并由单测双向核对：

- **正向**：`wired = true` 的触发点，`proofs` 点名的文件必须存在且必须含该片段（删了调用 → 测试红）；
- **反向**：扫描 `src/business` 下全部生产代码（排除联动模块自身与测试文件），
  找到的所有「调联动引擎」的文件集合，必须**恰好等于** wired 触发点点名的文件集合
  （多接一个没登记 → 红；登记了没调用 → 红）。

守卫文件：`backend/src/business/flow/linkage/tests_wiring.rs`。

> **为什么反向扫描是文本匹配而不是语法树**：本仓库没有把 `syn` 拉进 dev-dependencies，
> 为一条守卫引入解析器不值得；引擎入口名（`run` / `run_excluding` / `run_deferred` /
> `status_notice` / `stockin_voucher`）在这份代码里是稳定契约。宁可因为「注释里提到引擎」
> 而让人来更新表（fail-closed），也不要漏掉真实调用。

**这套守卫的边界（诚实说明）**：它证明的是「有人调了引擎入口」，**不**证明
「调了之后行为正确」（排除清单对不对、上下文构造对不对、幂等键对不对）。
后者只能靠端到端证据——这就是 D3 的准入门槛。

### D3：中期接线走「准入制」，四批推进

补 12 个触发点不是「在 handler 里加一行 `engine::run(...)`」。5B 的经验是**接线点有五个必答问题**，
任何一个答错都会变成账目错误而不是功能缺失：

1. **事务归属**：这个状态变更在哪个事务里？哪一步之后库存/凭证已经落完？
2. **排除清单**：这个触发点上，调用方自己已经做了哪些白名单动作？（重复执行 = 重复落账）
3. **提交后动作**：通知 / 建任务必须在 commit 之后（发通知失败不能回滚已过账），
   上下文必须在 `InTx` 里算好（主键、单据号、收件人策略、幂等键）。
4. **幂等键**：同一单据同一状态被重放（重试 / 并发）时，通知不能发两遍、任务不能建两个。
5. **端到端证据**：源单据动作 → 目标产物（凭证 / 通知 / 任务）真实落库，且**停用规则后行为回退**。

**没有 ⑤ 的接线一律不算接线**——这条是 5B 用「真断链」换来的教训：代码上看起来调了引擎，
但配了规则不生效，比没接还糟（因为没人会再去查）。

### D4：配置页三层防线（每一层都不依赖上一层自觉）

| 层 | 位置 | 行为 | 绕过难度 |
|---|---|---|---|
| ① 字典 | `admin.rs::catalog_triggers` → `TriggerRes.wired / wiring` | 下拉项文案带「· 未接线」；`wiring` 直接显示后端写的「为什么没接 / 接的时候要动哪里」 | 前端可忽略字段，但页面就没提示了 |
| ② 表单 | `index.vue` | 选中未接线触发点 ⇒ 顶部 warning alert + **第二道勾选**（不勾不能保存）；列表行贴「未接线」标签 | 可以手搓请求绕过 |
| ③ 写入口 | `model.rs::upsert_rule` | 未接线 + 启用 + 未 `allow_unwired` ⇒ `400`，错误文案带「当前可用（已接线）的触发点」清单 | ✅ 无法绕过（数据库之外唯一写入口） |

三层都从 `spec::TRIGGER_SPECS` 取值，**前端不维护第二份解释文案**（`wiring` 由后端下发）。

---

## 3. 15 个触发点逐个清单

### 3.1 总表

> 表按**业务价值**排序（不按 `TRIGGERS` 的代码顺序），便于排批次；覆盖性由
> `tests_wiring::trigger_specs_cover_whitelist_in_order` 钉住（15 条、同集合），不靠这张表。

| # | 触发点 | wired | 状态变更的唯一入口（实测） | 调用方已做的落账（= 排除清单初稿） | 业务价值 | 批次 |
|---|--------|-------|---------------------------|--------------------------------|---------|------|
| 1 | `STOCK_IN` | ✅ | `erp/stockin_handler_post.rs::post_in_tx` ⑦⑧ | 库存三写 + 成本 + 凭证（专用路由） | 高 | 已接（5B） |
| 2 | `STOCK_OUT` | ✅ | `erp/stockout_handler.rs::post_out_tx` ⑥ | 库存三写 + 成本快照 + 成本结转凭证 | 高 | 已接（5C） |
| 3 | `TASK` | ✅ | `task/task_handler_notice.rs::notify_status_change` | 旧通知逻辑（SHADOW/TAKEOVER 由配置决定） | 高 | 已接（第 2 步） |
| 4 | `PAYABLE` | ❌ | `erp/payable_payment.rs::settle_payment_in_tx`（L≈1148，`transition_if_changed`）+ 取消三处 + `erp/payable.rs::cancel_for_stock_in` | 付款记录、余额/状态维护、付款凭证 | **最高** | A |
| 5 | `EXPENSE_REPORT` | ❌ | `erp/expense_handler.rs::report_submit` / `report_cancel` / `report_pay`（付清时 `transition_if_changed`） | 应付单建设 + 凭证 + 付款金额回写 | 高 | A |
| 6 | `STOCK_TAKE` | ❌ | `erp/stocktake_handler.rs::post_tx`（盘盈盘亏落库存）/ `cancel_tx` / `submit_for_approval` / `withdraw` | 盘盈盘亏库存三写、成本、凭证（`stocktake_bridge`） | 高 | A |
| 7 | `PURCHASE_ORDER` | ❌ | `erp/stockin_purchase.rs::sync_purchase_status`（`transition_if_changed`）+ `erp/purchase_model.rs::apply_status` | 累计收货量派生（只校验不落库） | 高 | A |
| 8 | `STOCK_TRANSFER` | ❌ | `erp/transfer_handler_status.rs::apply_transition`（L≈55，`IN_TRANSIT→DONE` 同事务结算） | 库存双写（`transfer_model::settle_transfer`） | 中 | B |
| 9 | `CONTRACT` | ✅ | `erp/contract_handler.rs::contract_change_status`（提交审批分支 + 通用分支）+ `erp/contract_bridge.rs::settle`（审批通过 / 驳回） | 审批实例发起 + 状态机（合同**不落账**） | 高 | 已接（LNK-07） |
| 10 | `VOUCHER` | ✅ | `erp/voucher_write.rs::transition_voucher_in_tx`（L≈535，审核/记账/反记账共用）+ `manual_in_tx` / `reverse_in_tx` / `reverse_after_approval` | 分录行 + 记账戳 + 期间闸 | 中 | 已接（LNK-06） |
| 11 | `DEPRECIATION` | ✅ | `erp/depreciation_model.rs::run_batch`（⑤ 步，`CALCULATED → POSTED`）/ `reverse_batch`（⑥ 步，`POSTED → REVERSED`）—— 手动计提与定时任务共用同一条 `run_batch` | 折旧明细批量写 + 多借方凭证 + 卡片回写（凭证专用路由，通用引擎排除） | 中 | 已接（LNK-10） |
| 12 | `ACCOUNTING_PERIOD` | ✅ | `erp/period.rs::close_period`（L≈1311，结账）/ `reopen_period`（L≈1400，反结账）；开账 `open_period` / `batch_open` 是建行，**不接**（见 §3.2） | 期间状态 CAS + 期末汇兑重估凭证（结账第 ⑤ 步） | 中高 | 已接（LNK-08） |
| 13 | `FIXED_ASSET` | ✅ | `erp/asset_bridge.rs::execute` 第 ⑤ 步（审批通过 → `DISPOSED`）/ `erp/asset_handler.rs::asset_dispose_cancel` 第 ⑤ 步（→ `IN_USE`）；**停用/启用没有 HTTP 入口**（见 §3.2），建卡是建行不接 | 处置凭证（1~3 张）+ 红冲 + 处置痕迹 + 状态 | 中 | 已接（LNK-09） |
| 14 | `ACCOUNTING_YEAR` | ❌ | `erp/year_end.rs::close_year`（L≈1382）/ `reopen_year`（L≈1520） | 年度结转凭证 + 下年期初台账（**长事务**） | 中 | D |
| 15 | `FLOW_INSTANCE` | ❌ | `flow/engine.rs`（`model::bump_instance`，L≈882）← `handler::approve_task` / `reject_task` / `withdraw_instance` | 实例状态 + 流程任务级联取消 + **业务单据状态（bridge）** | 需先定序 | D |

> 表中「L≈」是开工时实测行号，会随重构漂移——**判据只看 `spec.rs` 的 `wired` 与 `tests_wiring` 的双向扫描**，不要拿行号当契约。

### 3.2 三条已接线的调用点与排除清单（照抄用的范本）

**`STOCK_IN`**（最完整的范本，5B 修的断链）

```
post_in_tx（同一事务）
  ③ 库存/成本/流水（专用实现）
  ⑦ linkage::engine::stockin_voucher        ← 凭证专用路由（旧逻辑，等价接管）
  ⑧ linkage::engine::run_excluding(..., &[GENERATE_VOUCHER])   ← 通用引擎，排除凭证动作
返回 LinkageRun（deferred 上下文在事务内算好）
commit 之后
  run_stockin_deferred_linkage → linkage::engine::run_deferred
```

**排除清单为什么必须有**：一条规则若同时接管「凭证」和「发通知」，⑦ 与 ⑧ 都跑会一张单两张凭证。
排除放在**调用方**而不是写死进引擎——换个触发点（如 `PAYABLE/SETTLED`）通用路线仍要能跑 `GENERATE_VOUCHER`。

**`STOCK_OUT`**（5C 新增，同一形状）

```
post_out_tx（同一事务）
  ③⑤ 库存/成本/成本结转凭证（专用实现）
  ⑥ run_excluding(spec::STOCKOUT_EXCLUDED_ACTIONS)
      = [WRITE_INVENTORY, WRITE_INVENTORY_TXN, UPDATE_COST, GENERATE_VOUCHER]
commit 之后
  erp/stockout_linkage.rs::run_deferred_linkage → run_deferred
```
单测 `stockout_post_wires_generic_engine_and_excludes_dedicated_actions` 把「必须用 `run_excluding`」
「必须传 `STOCKOUT_EXCLUDED_ACTIONS`」「四个动作一个都不能漏」「SEND_NOTICE 不能被排除」逐条钉住。

**`TASK`**（专用路由，不是通用引擎）

`task_handler_notice::notify_status_change` 用 `task_notice_draft` 算好草稿 → `linkage::engine::status_notice`：
`SHADOW` 只做并行对比（旧逻辑照发）、`TAKEOVER` 由配置发（旧逻辑不发），校验不过一律退回旧逻辑。
**它证明的不是「通用引擎能跑」，而是「触发点可以有自己的路由」**——这也是 `VOUCHER` / `FLOW_INSTANCE`
将来更可能采用的形状。

### 3.3 12 条未接线：为什么、接的时候要动哪里

#### A. 高价值，优先接（批次 A）

**`PAYABLE`（应付单 · 付款/结清）** — 业务价值最高：付款是钱出去的最后一跳。

- 入口：`payable_payment.rs::settle_payment_in_tx` 用 `transition_if_changed` 推进
  `OPEN/PARTIAL_PAID → PARTIAL_PAID/SETTLED`（第二次部分付款是原地流转，不写状态日志）；
  取消路径三处（`cancel_body_in_tx` / `submit_cancel_tx` / `restore_after_cancel_rejection`）；
  `payable.rs::cancel_for_stock_in` 还会从收货侧取消应付。
- 接线要动：**四处都要接**（否则「取消付款」「收货取消」这些边永远走不到联动），
  事务内 `run_excluding(&[UPDATE_PAYABLE_BALANCE])`（余额已由同事务维护，再跑一次是重复加减），
  commit 后 `run_deferred`。
- 上下文：`payable_id` / `payable_no` / `amount` / `paid_amount` / `supplier` / `status`。
- 典型用法：付款完成 → 通知采购对接人 + 建对账任务。

**`EXPENSE_REPORT`（报销单 · 提交/通过/付清/作废）**

- 入口：`expense_handler.rs` 的 `report_submit`（L≈920）/ `report_cancel`（L≈1214）/
  `report_pay`（L≈1410，`APPROVED → PAID` 用 `transition_if_changed`，部分付款不改状态）。
- 接线要动：三处事务内 + commit 后；`report_pay` 里已经有应付/凭证落账，排除清单要包含凭证类动作。
- 典型用法：付清 → 通知报销人「钱已到账」；通过 → 给出纳建待付款任务。

**`STOCK_TAKE`（盘点单 · 过账/作废）**

- 入口：`stocktake_handler.rs::post_tx`（L≈810，盘盈盘亏由 `stocktake_model::apply_status` + 库存写入落账）、
  `cancel_tx`（L≈993）、`submit_for_approval`（L≈519）、`withdraw`（L≈643）。
- 接线要动：**与 `STOCK_IN` 同形**（第 ⑦⑧ 步 + commit 后），排除清单至少包含
  `WRITE_INVENTORY` / `WRITE_INVENTORY_TXN` / `UPDATE_COST` / `GENERATE_VOUCHER`
  （盘盈盘亏由专用实现落账，通用引擎再跑一次就是重复调账）。
- 典型用法：盘亏过账 → 建调查任务 + 通知仓储主管。

**`PURCHASE_ORDER`（采购单 · 部分收货/收货完成）**

- 入口：两个——`stockin_purchase.rs::sync_purchase_status`（收货过账时 `transition_if_changed`
  把 PO 推到 `PARTIAL_RECEIVED` / `RECEIVED`）与 `purchase_model::apply_status`（手工改状态）。
  **两处都要接**，否则「部分收货 → 全部收货」这条边走不到联动。
- 排除清单：`UPDATE_RECEIVED_QTY` 的动作语义已退化为「只校验」（累计收货量是派生值
  `POSTED_RECEIVED_SQL`），不排除也不会重复落库，但二次校验会白白多一次 SQL——建议仍排除。
- 典型用法：收货完成 → 通知采购员跟进对账；部分收货 → 给跟单员建任务。

#### B. 中价值（批次 B）

**`STOCK_TRANSFER`（调拨单 · 在途/完成）**

- 入口：`transfer_handler_status.rs::apply_transition`（L≈55）；`IN_TRANSIT → DONE` 时同事务调
  `transfer_model::settle_transfer` 做调入调出双写。
- 接线要动：结算之后 `run_excluding`（库存三写 + 凭证）→ commit 后 `run_deferred`。
- 风险：结算失败要整笔回滚（含刚写的 `DONE`），联动加在结算之后不改变这个语义，但在事务内跑
  关键动作会拉长事务——本触发点只建议先开 `SEND_NOTICE` / `CREATE_TASK`。

**`CONTRACT`（合同 · 提交审批/审批通过/驳回/生效/完成/终止）— LNK-07 已接**

- 入口：`contract_handler.rs::contract_change_status` 的提交审批分支（`DRAFT → PENDING_APPROVAL`）
  + 通用分支（`SIGNED → ACTIVE` / `ACTIVE → COMPLETED` / `SIGNED|ACTIVE → TERMINATED` /
  撤回 `PENDING_APPROVAL → DRAFT`），以及 `contract_bridge.rs::settle`（审批回调
  `PENDING_APPROVAL → SIGNED|DRAFT`）。唯一 hook 是 `erp/contract_linkage.rs`
  （`run_excluding(spec::CONTRACT_EXCLUDED_ACTIONS)` + commit 后 `run_deferred`）。
- **重复触发**：审批通过有两条路（flow 回调 / `PATCH /status` 直改），两条都只写同一列
  `biz_contract.status` 且带 CAS —— 后到的那条在 `settle_guard`（`AlreadySettled`）或
  状态机边判定（409）处被挡掉，联动只跑一次；通知另有幂等键
  `LNK:CONTRACT:<状态>:<合同 id>`。
- **排除清单**：`GENERATE_VOUCHER`（合同不产生凭证；配置期另有
  `spec::CONTRACT_FORBIDDEN_ACTIONS` 直接 400，避免「配了永远不执行」）。
- 典型用法：合同生效 → 建履约计划任务 + 通知商务；终止 → 通知制单人。
- 物证：`docs/evidence/20261107-lnk07-contract/`（真机 HTTP：提交/审批/生效各一条通知、
  重复审批幂等且通知不增、配 GENERATE_VOUCHER 400、合同名下 0 张凭证）。

**`ACCOUNTING_PERIOD`（会计期间 · 结账/反结账）— LNK-08 已接**

- 入口：`period.rs::close_period`（四项校验 + 汇兑重估 + 状态 CAS 成功后，`OPEN → CLOSED`）与
  `period.rs::reopen_period`（后续期间校验 + 状态 CAS 成功后，`CLOSED → OPEN`）。唯一 hook 是
  `erp/period_linkage.rs`（`run_excluding(spec::PERIOD_EXCLUDED_ACTIONS)` + commit 后
  `run_deferred_after_commit` → `run_deferred`）。
- **事务边界是这张卡的重点**：结账是长事务（全期间凭证聚合 + 汇兑重估），通知一律在
  `tx.commit()` 之后发 —— 事务内发通知一旦回滚就是「财务收到月结完成、实际那个月还开着」。
  `tests_wiring::period_wires_generic_engine_and_excludes_ledger_actions` 用源码切片钉住
  「`run_in_tx` 在 commit 前 / `run_deferred_after_commit` 在 commit 后」。
- **排除清单**：`GENERATE_VOUCHER`（结账第 ⑤ 步 `fx_revaluation::revalue_period` 自己出期末
  汇兑重估凭证）+ `WRITE_INVENTORY` / `WRITE_INVENTORY_TXN` / `UPDATE_COST` /
  `UPDATE_PAYABLE_BALANCE`（期间没有明细行 / 余额列，封账是只读开关，不是业务单据）。
- ⚠ **开账不接**：`open_period`（`NOT_OPENED → OPEN`）、`batch_open`、启动时
  `ensure_year_periods` 都是**建行 / 批量补行**，挂引擎就是启动扇出 N 条通知。所以
  `PERIOD/OPEN` 状态只在**反结账**上触发，`spec::TRIGGER_SPECS` 的 `wiring` 文案里写明了。
- 幂等：通知幂等键 `LNK:PERIOD:<状态>:<期间 id>`；重复结账被状态闸 409 挡住、联动不跑第二遍。
- 真库用例：`erp/period_linkage_tests.rs` 6 条（结账触发 / 反结账触发 / **事务回滚与 commit
  失败都不发通知** / 重复结账不重复触发 / 排除动作确实不跑）。

**`VOUCHER`（凭证 · 落草稿/审核/反审核/记账/红冲）— LNK-06 已接**

- 入口：`voucher_write.rs::transition_voucher_in_tx`（L≈535，三条状态边共用）+
  `manual_in_tx`（DRAFT）、`reverse_in_tx` / `reverse_after_approval`（红字凭证 POSTED）。
  唯一 hook 是 `erp/voucher_linkage.rs`（`run_excluding` + commit 后 `run_deferred`）。
- ⚠ **环风险**：在凭证状态上配 `GENERATE_VOUCHER` = 凭证生凭证。两道闸：
  运行时 `spec::VOUCHER_EXCLUDED_ACTIONS` 排除，配置期 `spec::VOUCHER_FORBIDDEN_ACTIONS`
  + `spec::forbidden_action_reason` 让 `model::upsert_rule` / `admin::check_draft` 直接 400。
  这是唯一一个「不自带排除清单就会产生逻辑环」的触发点。
- 口径：只接凭证自身的人工 / 红冲状态流转；来源单据自动落的 `POSTED` 凭证不重复触发
  （避免嵌套联动）。
- 典型用法：记账完成 → 通知财务主管；反记账 → 建异常跟进任务。

**`DEPRECIATION`（折旧批次 · 过账/红冲）— LNK-10 已接**

- 入口：① 过账 `erp/depreciation_model.rs::run_batch` 第 ⑤ 步（明细 + 多借方凭证 +
  回写卡片 + 批次 CAS `CALCULATED → POSTED` 之后、commit 之前）；
  ② 红冲 `erp/depreciation_model.rs::reverse_batch` 第 ⑥ 步（红字凭证 + 明细 `REVERSED` +
  卡片回滚 + 批次 CAS `POSTED → REVERSED` 之后）。唯一 hook 是
  `erp/depreciation_linkage.rs`（`run_excluding(spec::DEPRECIATION_EXCLUDED_ACTIONS)`
  + commit 后 `run_deferred_after_commit`）。手动计提（`depreciation_handler`）与定时任务
  （`run_scheduled`）都走同一条 `run_batch`，接线点只有一个。
- **批量操作 + 聚合口径（卡面重点）**：一次计提可能涉及几百台资产，但**触发单据是批次**
  （`ctx.doc_id = 批次 id`），300 台的批次和 1 台的批次都只跑一遍规则、只发**一条**通知，
  台数写在正文里（「涉及 N 台资产」）——不存在「一人收 300 条」的扇出。
- **通知一律 `AfterCommit`**：计提 / 红冲都是长事务（扫几百台 + 逐台回写 + 出凭证），
  事务内发通知一旦回滚就是「财务收到某月已计提、实际没提」；`run_in_tx` 只跑 `Phase::InTx`
  动作，提交后才 `run_deferred_after_commit` 发通知 / 建任务。
- **排除清单**：`GENERATE_VOUCHER`（计提凭证由 `voucher_multi::post_multi_debit` 出一张
  多借方凭证，再跑一张 = 累计折旧科目翻倍）+ `REVERSE_VOUCHER`（红冲由
  `voucher::reverse_posted` 做，再冲一次撞「已经红冲」→ 500）。库存 / 余额类动作不列
  （批次明细只有资产 / 金额 / 部门，与 `FIXED_ASSET` / `CONTRACT` 同口径）。
- **不接的两条边**：`CALCULATED` 是一次计提的中间态（同事务里紧接着置 `POSTED`，挂上去
  一次计提发两条 = 刷屏）；`SKIPPED` 是合法空跑（0 台 / 0 金额 / 不出凭证，没有账要通知谁）。
- 幂等：通知幂等键 `LNK:DEPRECIATION:<状态>:<批次 id>:<本次事件凭证 id>` —— 同一笔过账
  重放只发一条；红冲后重跑是新的一笔（新凭证），会再发一条。
- 真库用例：`erp/depreciation_linkage_tests.rs` 5 条（过账触发聚合成 1 条 / 重复计提不重复
  触发 / 红冲触发 + 重复红冲不增 / 事务回滚不发通知 / 排除动作确实不跑、凭证只有一张）。

#### C. 中价值但风险/复杂度更高（批次 C）

**`ACCOUNTING_PERIOD`（会计期间 · 开关/结账/反结账）**

- 入口：`period.rs::open_period`（L≈783）/ `close_period`（L≈1253）/ `reopen_period`（L≈1343）。
- 典型用法：月结完成 → 通知财务全组；反结账 → 上报财务负责人。
- 风险：结账是「全系统只读」的开关，联动做错的影响面是全员，先只接通知类动作。

**`FIXED_ASSET`（固定资产 · 处置 / 作废处置）— LNK-09 已接**

- 入口：① 处置 `erp/asset_bridge.rs::execute` 第 ⑤ 步（审批通过 → 处置凭证 + 状态机
  `IN_USE|IDLE → DISPOSED` + 卡片回填 + 变动留痕之后、commit 之前）；
  ② 作废处置 `erp/asset_handler.rs::asset_dispose_cancel` 第 ⑤ 步（红冲 + `DISPOSED → IN_USE`
  + 恢复在役 + 留痕之后）。唯一 hook 是 `erp/asset_linkage.rs`
  （`run_excluding(spec::FIXED_ASSET_EXCLUDED_ACTIONS)` + commit 后 `run_deferred_after_commit`）。
- **处置的真实入口是审批回调，不是 `POST /asset/{id}/dispose`**：提交只是发起审批，
  卡片仍在役（真机取证里「提交后通知 = 0」）。只接 submit 会在生产环境（配了已发布
  `asset_disposal` 流程）里永远不响。
- **排除清单**：`GENERATE_VOUCHER`（处置凭证由 `post_disposal_vouchers` 出 1~3 张）+
  `REVERSE_VOUCHER`（作废处置走 `reverse_disposal_vouchers` 原路红冲；再跑一次撞
  「已经红冲」→ 500）。库存 / 余额类动作不列（资产卡片没有这些列，与 `CONTRACT` 同口径）。
- **停用 / 启用（`IN_USE ⇄ IDLE`）不接：这两条边目前没有 HTTP 入口**。`state_machine`
  里有 mandatory 边、`doc_kind` 有 `IDLE`/`ENABLE` 变动类型，但 `asset_handler.rs` 的 14 个
  接口里没有状态变更接口（`asset_model::apply_asset_status` 的调用方只有单测），
  前端资产页也只有转移 / 换人 / 处置 / 作废处置四个按钮。补接口属于资产模块二开（新功能），
  本卡不虚造调用点；`spec::TRIGGER_SPECS.wiring` 已把这一点写给配置页看
  （`FIXED_ASSET/IDLE` 上的规则「可保存、暂无触发源」）。
- 建卡（`NULL → IN_USE`）是**建行**不接（与开账同口径：导入 N 张卡不能扇出 N 条通知）。
- 幂等：通知幂等键 `LNK:FIXED_ASSET:<状态>:<卡片 id>`；重复审批 / 重复处置被
  `disposable()` + `asset_mark_disposed` 的 CAS 挡住，联动不跑第二遍。
- 真库用例：`erp/asset_linkage_tests.rs` 5 条（处置触发 / 重复处置不重复触发 /
  作废处置触发 / 事务回滚不发通知 / 排除动作确实不跑、凭证只有一套）。
- 物证：`docs/evidence/20261107-lnk09-asset/`（真机 HTTP：审批通过 → 卡片 DISPOSED +
  通知 1 条 + 3 张处置凭证；重复 approve 幂等且通知/凭证不增；作废处置 → IN_USE + 通知 1 条）。

#### D. 必须单开设计卡（批次 D）

**`ACCOUNTING_YEAR`（年结）**

- 入口：`year_end.rs::close_year`（L≈1382）/ `reopen_year`（L≈1520）。
- 事务非常长（年度结转凭证 + 下年期初台账 + 期间留痕）。年结失败的回滚代价最大，
  必须**单独一张卡**做真机全量验证 + 大事务耗时基线；不允许和别的触发点混在一张卡里上。

**`FLOW_INSTANCE`（审批实例）**

- 入口：`flow/engine.rs` 里的 `model::bump_instance`（L≈882）← `handler::approve_task` /
  `reject_task` / `withdraw_instance`；到终态时还会级联取消未完成的流程任务。
- ⚠ **必须先定序**：审批通过会顺带改业务单据状态（各 `*_bridge`），因此
  「`FLOW_INSTANCE/APPROVED`」与「`CONTRACT/APPROVED`」这类单据触发点**天然重叠**。
  建议的职责划分：
  - `FLOW_INSTANCE` 只挂**审批侧**动作（通知发起人 / 审批留痕 / 建后续审批任务）；
  - **业务侧**动作（改单据状态、生成凭证）归各单据触发点；
  - 若将来确实需要在实例终态跑业务动作，必须在文档与配置页写清「两者会各触发一次」。
- 这条不接线不是能力问题，是**不能带着歧义接线**。

---

## 4. 为什么不能「顺手都接上」——三条硬理由

1. **排除清单不是可选项**：5B 的断链修复证明，通用引擎跑在已有专用落账的触发点上，
   不做排除就是重复写账（库存扣两次 / 一张单两张凭证）。12 个触发点里至少 6 个
   （STOCK_TAKE、TRANSFER、STOCK_OUT 已接、VOUCHER、DEPRECIATION、EXPENSE）自带落账。
2. **通知不能进事务**：`SEND_NOTICE` / `CREATE_TASK` 是 `AfterCommit`；在事务里跑，
   通知发出去了业务回滚 → 幽灵通知；上下文必须在事务内算好，否则 commit 后拿不到主键/收件人。
3. **一条卡做 12 个接线 = 12 份端到端证据**：本仓库的验收标准是真机端到端（源单据 → 目标单据），
   不是单测。12 条 × (建单 + 过账 + 断言 + 停用回退) 不可能在一张卡里做扎实。

---

## 5. 双门槛矩阵：触发点 × 动作（当前实际可用的组合）

只有 ✅×✅ 的格子才「配了就生效」：

| 动作 \ 触发点 | STOCK_IN | STOCK_OUT | TASK | 其余 12 个 |
|---|---|---|---|---|
| `SEND_NOTICE`（已接管） | ✅ | ✅ | ✅ | ❌ 触发点未接线 |
| `CREATE_TASK`（已接管） | ✅ | ✅ | ✅ | ❌ |
| `GENERATE_VOUCHER`（已接管） | ✅（已排除专用路由，需 SHADOW/TAKEOVER 明确） | ⚠ 排除在专用路径（成本结转已发） | ❌ 语义不成立（任务无凭证） | ❌ |
| `UPDATE_RECEIVED_QTY`（已接管，语义=校验） | ✅（收货过账唯一合理点） | ❌ | ❌ | ❌ |
| 其余 7 个占位动作 | ❌ 写入口 400 | ❌ | ❌ | ❌ |

> 读法：**当前线上能真正生效的联动 = 3 个触发点 × 2~3 个动作**。
> 这也是为什么「防误配」必须先落地：矩阵的空格子太多，而页面原来一格提示都没有。

---

## 6. 接线标准形（下一张卡照抄）

```
① 找到该状态变更的唯一事务入口（必须是 state_machine::transition / transition_if_changed 的调用点，
   不要在 handler 里新增一条旁路写状态）
② 列出这个入口自己已经做完的落账动作 → 写成排除清单常量（放 src/business/flow/linkage/spec.rs，
   由调用方显式传给 run_excluding，不写死进引擎）
③ 在「落账完成之后、事务提交之前」调 run_excluding(...)，返回值里的 deferred 上下文带出事务
④ commit 之后调 run_deferred（通知/建任务在事务外；失败只记日志，不回滚业务）
⑤ 两个单测 + 一份端到端证据：
   - 漂移守卫：spec::TRIGGER_SPECS 加 wired=true + proofs（文件 + 片段），
     tests_wiring 的正反向扫描自动覆盖
   - 排除清单守卫：include_str! 钉住「必须调 run_excluding」「必须传 XX_EXCLUDED_ACTIONS」「不含 SEND_NOTICE」
   - 端到端：源单据动作 → 目标产物落库；停用规则后行为回退（不发/退回旧逻辑/不漏发）
```

**DoR（开工前必须答得出）**：唯一事务入口在哪；该点已做的落账有哪些；commit 后能拿到哪些上下文；
幂等键是什么；端到端脚本怎么造数据。
**DoD（不满足不许合）**：`run_excluding` + `run_deferred` 都在；排除清单有单测；
端到端证据真机通过（含停用回退）；`cargo clippy -D warnings` / `fmt` 干净；
`wired=true` + `wiring` 文案 + 本文第 3 节更新；配置页文案不需要改（它读后端）。

---

## 7. 分批计划（按业务价值，不按代码难度）

| 批次 | 触发点 | 理由 | 建议卡 |
|---|---|---|---|
| A | `PAYABLE`、`EXPENSE_REPORT`、`STOCK_TAKE`、`PURCHASE_ORDER` | 直接关系钱与库存差异，业务最痛；排除清单清楚（付款余额 / 盘盈亏库存 / 采购累计量） | 一张卡一格，可并行 |
| B | `STOCK_TRANSFER`、`CONTRACT`、`VOUCHER`、`DEPRECIATION` | 中等价值；`VOUCHER` 有环风险、`DEPRECIATION` 有批量扇出风险，各自要专门断言 | 一张卡两格起 |
| C | `ACCOUNTING_PERIOD`、`FIXED_ASSET` | `ACCOUNTING_PERIOD` 影响全员只读；`FIXED_ASSET` 要先统一「停用启用算不算状态变更」的口径 | 先设计再实现 |
| D | `ACCOUNTING_YEAR`、`FLOW_INSTANCE` | 长事务 / 触发点重叠，必须单开设计卡 | 设计卡 → 实现卡 |

**每批的前置**：上一批的端到端证据已归档到 `docs/evidence/`，且 `wired_triggers()` 与本表一致
（`tests_wiring` 会强制）。

---

## 8. 配置页交互（已落地的行为）

1. **下拉**：未接线项文案 = `收货单（STOCK_IN）· 未接线`——**可选但一眼可辨**（不禁选，允许预置）。
2. **表单**：选中未接线触发点 ⇒ 顶部 `warning` alert：
   「『X』这个触发点还没有接线：代码里没有任何状态变更点会调用联动引擎，在这里配的规则保存后永远不会触发。」
   + 后端下发的 `wiring`（为什么没接 / 接的时候要动哪里）。
3. **保存**：未接线时必须多勾一道「我已知晓该触发点未接线：这条规则保存后不会触发，只是先把配置存下来」，
   否则保存按钮 `disabled`；勾了之后请求才带 `allow_unwired: true`（后端硬门）。
4. **列表**：未接线规则贴 `warning` 标签「未接线」+ 一行说明，
   与「配置异常」（动作不在白名单 / 参数非法，`danger`）**分开显示**——两者性质不同。
5. **影响预览**：`trigger_wired = false` 时结论条用 `warning` 而不是 `success`
   （「可以通过草稿校验」的绿条不能盖过「配了不生效」这条最重要的结论）。
6. **文案单一来源**：所有解释性文字来自后端 `TriggerSpec.wiring`，前端不维护第二份。

---

## 9. 验收对照（本卡三个 □）

| 验收项 | 证据 |
|---|---|
| 15 个触发点逐个「已接线 / 未接线（原因）」→ 落到代码与文档 | 代码：`spec.rs::TRIGGER_SPECS`（wired + wiring + proofs，15 条齐全，`tests_wiring::trigger_specs_cover_whitelist_in_order` 钉住同序同数）；文档：本文 §3 + §5 |
| 未接线触发点在配置页不可静默保存（置灰或二次确认，截图） | 三层防线（§2 D4）：`TriggerRes.wired/wiring` 下发 + 表单双确认 + `model::upsert_rule` 硬门 400；截图与接口实测见 §10 |
| 若补齐某些触发点：每个都要端到端证据 | 本卡补齐 `STOCK_OUT`（5C 与实现卡同批）：`run_excluding(STOCKOUT_EXCLUDED_ACTIONS)` + commit 后 `run_deferred` + 漂移/排除清单双单测 + 端到端证据见 §10 |

---

## 10. 独立验收记录（本卡实测，非引用）

<!-- VERIFY:BEGIN -->
**时间**：2026-11-05 22:15–22:30 CST · **环境**：共享工作区 `/opt/cloudpivot` + 隔离实例（库 `cloudpivot_lnk5b`，端口 18132）+ 前端 dev server（`BACKEND_ORIGIN=http://127.0.0.1:18132`，端口 5173）。

| # | 验收项 | 命令 | 实测结果 |
|---|--------|------|----------|
| 1 | 后端单测（真库，含接线漂移守卫） | `ERP_DB_URL=…/cloudpivot_lnk5c_arch_ut cargo test --lib business::flow::linkage -- --test-threads=1` | **59 passed / 0 failed，EXIT=0**；其中 `tests_wiring` 5 条全 ok（`trigger_specs_cover_whitelist_in_order` / `every_trigger_declares_wired_and_reason` / `wired_trigger_proofs_exist_in_source` / `wired_triggers_match_call_sites` / `stockout_post_wires_generic_engine_and_excludes_dedicated_actions`） |
| 2 | 前端类型检查 | `pnpm typecheck`（vue-tsc --noEmit） | **EXIT=0**（修掉上一版遗留的 4 个 `TS6133` 未使用变量：`isHealthy` / `previewAlertTheme` / `wiredTriggerCount` / `unwiredTriggerCount` —— 后三者已接进页面，`isHealthy` 移除多余 import） |
| 3 | 前端逻辑单测 | `npx tsx --test tests/unit/flow-linkage.test.ts` | **9/9 pass，EXIT=0** |
| 4 | 配置面「防误配」接口探针（13 断言） | `LNK_PORT=18132 python3 scripts/probe_config_guard.py` | **PASS=13 FAIL=0**（明细见下表） |
| 5 | 配置页交互实测（5 张截图） | 浏览器直连 5173 → `/flow/linkage` | 下拉 12 项带「· 未接线」；未接线触发点表单顶部 warning；第二道勾选未勾时「创建规则」**disabled**；勾上才解禁；草稿预览写「保存会被拒绝：尚未接线」 |
| 6 | 三方一致性交叉核对 | 单测 + 探针 A2 + 页面下拉文案 | `spec::wired_triggers()` = `admin::catalog_triggers()` 下发 = 前端标注 = **`["STOCK_IN","STOCK_OUT","TASK"]`（3 个）**，未接线 12 个，三处逐字一致 |

**接口探针 13 条断言逐条原文**（`docs/evidence/20261105-p1fix5c-architect-config-guard/logs/probe_config_guard.txt`）：

```
A1 触发点字典 15 条全部下发                 HTTP 200 count=15
A2 已接线恰好 3 个                          wired=['STOCK_IN','STOCK_OUT','TASK']
A3 未接线 12 个                             unwired=['STOCK_TRANSFER','PURCHASE_ORDER','PAYABLE','VOUCHER',
                                            'ACCOUNTING_PERIOD','ACCOUNTING_YEAR','CONTRACT','STOCK_TAKE',
                                            'FIXED_ASSET','DEPRECIATION','EXPENSE_REPORT','FLOW_INSTANCE']
A4 每个触发点都下发「接线位置 / 未接线原因」
A5 STOCK_OUT 标注已接线并点明 post_out_tx
A6 已接管动作 4 个（CREATE_TASK 已转真实现）  ['GENERATE_VOUCHER','UPDATE_RECEIVED_QTY','CREATE_TASK','SEND_NOTICE']
B1 未接线触发点（STOCK_TRANSFER/DRAFT）保存   HTTP 400「触发点 STOCK_TRANSFER/DRAFT 尚未接线…」（不静默成功）
B2 显式 allow_unwired=true 才可预置           HTTP 200 id=01a1ac07-…
B3 未带 allow_unwired 的「启用」同样被拒      HTTP 400（停用放行，不属变更）
B4 探针规则清理                              HTTP 200（可重复跑）
C1 未接线预览 trigger_wired=false            STOCK_TRANSFER
C2 预览 warnings 点名「未接线」
C3 已接线预览 trigger_wired=true（对照组）    STOCK_IN/POSTED enabled_rules=2
PASS=13 FAIL=0
```

**后端单测尾部原文**（`…/logs/linkage_unit.txt`）：

```
test business::flow::linkage::tests_wiring::wired_triggers_match_call_sites ... ok
test business::flow::linkage::tests_wiring::wired_trigger_proofs_exist_in_source ... ok
test business::flow::linkage::tests_wiring::stockout_post_wires_generic_engine_and_excludes_dedicated_actions ... ok
test business::flow::linkage::tests_wiring::trigger_specs_cover_whitelist_in_order ... ok
test business::flow::linkage::tests_wiring::every_trigger_declares_wired_and_reason ... ok
test result: ok. 59 passed; 0 failed; 0 ignored; 0 measured; 2605 filtered out; finished in 14.04s
EXIT=0
```

**截图**（`docs/evidence/20261105-p1fix5c-architect-config-guard/shots/`）：

| 文件 | 内容 |
|------|------|
| `01-trigger-dropdown-unwired-labels.png` | 新增规则 → 源单据类型下拉：15 项中 12 项带「· 未接线」，顶部计数「触发点已接线 3 个 / 未接线 12 个」 |
| `02-form-unwired-warning.png` | 选中「库存调拨单（STOCK_TRANSFER）· 未接线」→ 橙色 warning，正文是后端下发的 `wiring`（为什么没接 + 接的时候要动 `erp/transfer_handler_status.rs`） |
| `03-unwired-second-ack-blocks-save.png` | 只勾第一道确认、未勾「我已知晓该触发点未接线」→「创建规则」按钮 **disabled**（不可静默保存） |
| `04-second-ack-enables-save.png` | 勾上第二道确认后才解禁（请求会带 `allow_unwired: true`，后端仍可 400） |
| `05-draft-preview-rejects-unwired.png` | 点「预览影响（不保存）」→ 红色「保存会被拒绝：触发点 STOCK_TRANSFER/DRAFT 尚未接线」 |

**诚实边界**：

1. 本卡**未改 Rust 源码**（`wired` 机制与 `STOCK_OUT` 接线由实现卡 t_befea97a 落地），因此没有重复跑 clippy；
   `cargo test` 已在当前共享工作区的树上编译通过，间接证明代码可编译。
2. 5B/5C 的**端到端**证据（出库过账 → 任务 / 通知，19 条断言）由实现卡提供，
   本卡只复核其入口存在性（A2/A5 + `tests_wiring` 反向扫描）与配置面行为，**不重复贴**。
3. 生产 144 未部署：本卡与实现卡均无迁移，发布只换二进制 + 前端。
<!-- VERIFY:END -->

### 10.1 第二轮复测（commit `072b614`，本卡收尾时独立复算）

> 为什么要有第二轮：第一轮（上表）打在**共享工作区当时那棵树**上；同一仓库还有别的卡在并行改文件。
> 所以收尾时在**本卡提交的树**（独立 worktree `/root/p1fix5c-arch-wt`，`HEAD=072b614`）上重算一遍。

| # | 项 | 命令 | 结果 | 日志 |
|---|----|------|------|------|
| 1 | 配置面探针（重跑） | `LNK_PORT=18132 python3 scripts/probe_config_guard.py` | **PASS=13 FAIL=0**（与第一轮逐条一致：wired 3 / unwired 12、未接线保存 400、`allow_unwired` 才放行、启用同拦、预览 `trigger_wired`） | `logs/probe_config_guard_round2.txt` |
| 2 | 后端 linkage 单测（重跑，真库） | `ERP_DB_URL=…/cloudpivot_lnk5c_arch_ut cargo test --lib business::flow::linkage -- --test-threads=1` | **59 passed / 0 failed，EXIT=0**（含 5 条 `tests_wiring` 守卫） | `logs/linkage_unit_round2.txt` |
| 3 | 前端类型检查（**本卡提交树**） | `git worktree add /root/p1fix5c-arch-wt 072b614` → `frontend && ./node_modules/.bin/vue-tsc --noEmit` | **EXIT=0** | `logs/frontend_typecheck_round2_worktree.txt` |
| 4 | 前端逻辑单测（同一 worktree） | `npx tsx --test tests/unit/flow-linkage.test.ts` | **9/9 pass，EXIT=0** | 同上 |
| 5 | 截图目视复核 | 人工读 `shots/01`、`shots/03` | 01：下拉 `STOCK_IN`/`STOCK_OUT` 无标注、`STOCK_TRANSFER`…`STOCK_TAKE` 逐项带「· 未接线」；03：第一道勾选已勾、第二道未勾 ⇒ 「创建规则」**灰化不可点** | `shots/` |

**一条必须记下来的现场**：收尾时共享工作区的 `pnpm typecheck` 是 **RED**，报的是
`src/api/business/notice/index.ts` 的 `TS6133` / `TS2552`（另一个卡未提交的 notice SSE 改动），
**与联动配置页无关**。同一条命令在本卡提交树上 EXIT=0（上表第 3 行），因此判本卡前端无类型问题；
被污染的那次输出也原文留档（`logs/frontend_typecheck_shared_tree_polluted.txt`），避免下次有人把它当成本卡的锅。

---

## 11. 风险、回滚与观测

| 风险 | 现状 | 处置 |
|---|---|---|
| 历史脏行（已配在未接线触发点上的规则） | 生产 `biz_flow_linkage_rule` 目前 0 行（5B 交接说明），V0253/V0254 未记账 | 配置页对历史行显示「未接线」标签；`rule_problems` 也会报，`isHealthy` 变红 |
| `allow_unwired` 被当成「解除限制」 | 语义已写进 DTO 注释：表达「我已知晓它当前不触发」 | 页面文案明确「保存后不会触发」；后端错误文案给出已接线清单 |
| 守卫误报（注释里提到引擎名） | 反向扫描按文件粒度 | fail-closed：宁可让人来更新表，也不漏真实调用 |
| 接线后行为变更（凭证生成权 / 通知发送方） | 5B 已把 `STOCK_IN` 凭证权切到配置路径 | 发版按「行为变更」复核；`SHADOW` 模式可先影子对比再 `TAKEOVER` |
| 生产未部署 | 生产 144：`biz_flow_linkage_rule` 0 行，V0253/V0254 未记账 | 下次 `deploy.sh` 一并应用；勿与 5C 接线混在同一次发布 |

**观测**：`linkage` 引擎的每次执行都落 `runs` / `deferred`；接线后若「规则 enabled 但 `runs` 长期为 0」，
就是新的沉默断链信号——建议后续加一条巡检（配置页已有 `problems`，可扩展成全局看板）。

---

## 12. 诚实清单（没做 / 不确定的）

1. **12 个触发点未接线**：本卡只做防误配 + 分批计划，没有在这些触发点上加任何 `run(...)` 调用。
2. **`wired` 证明的是「调了引擎」，不是「行为正确」**：排除清单 / 上下文 / 幂等键的正确性
   只能由端到端证据保证（§6 DoD ⑤）。守卫对这三件事无能为力。
3. **`FIXED_ASSET` 的口径未定**：停用/启用走 `asset_model` CAS + `log_event`，不经状态机。
   「它们算不算触发点」需要产品与财务一起定，本卡只列风险。
4. **`FLOW_INSTANCE` 的职责划分只是建议**：审批侧 vs 业务侧的分工需要一张设计卡确认后
   才能写进 `spec::wiring`，现在 `wired=false` 是**正确的**（不是因为懒）。
5. **动作侧仍有 7 个占位**：即使把 12 个触发点全接上，「写库存 / 红冲 / 改状态 / 锁单」
   这些动作仍然配置不了。触发点接线与动作接管是两条并行线，不要互相当成前置。
6. **截图 / 实测的时效**：本卡的验收是打在**共享工作区当时那棵树**上的；实现卡若在同一批文件上继续改动，
   以 `tests_wiring` + 重新实测为准。

# P3-07 前端配套：「审批通过但未执行」的付款 → 「执行付款」按钮

卡片：`t_41801d7f`（前端）· 后端来源：`t_49dcd588` → `9d05e34` feat / `d2a960b` docs
交付提交：`4559f92`（本目录所在提交的父提交链上的 feat 提交）
后端补字段子卡：`t_6f2200e1`

---

## 1. 做了什么

| # | 位置 | 改动 |
|---|---|---|
| ① | `frontend/src/views/business/erp/payable/index.vue` 付款记录行 | 新增「执行付款」按钮（`v-privilege="[PAY_PERMISSION]"`，`PAY_PERMISSION = business:erp:payable:pay`），只在「审批通过但还没执行」时渲染 |
| ② | 同上，`DocActionBar` | 该状态下后端 `blocked_reason` 仍是「已有一笔付款在审批中…请等审批结果」（审批其实已结束），换成「付款 PM-xxx 审批已通过、钱还没付出去：请让另一位有付款权限的账号点『执行付款』」 |
| ③ | `payable-write-api.ts` | `executePayment()`（`POST /api/business/erp/payable/payment/{id}/execute`）+ `PaymentExecuteResult`；`PayablePayment` 加可选 `awaiting_executor` |
| ④ | `payable-logic.ts` | `isAwaitingExecutor` / `executorProbeInstanceId` / `executePaymentConfirmText` 三个纯函数（单测直接打） |
| ⑤ | `tests/unit/p307-awaiting-executor.test.ts` | 16 条：判据真值表 + 探针去重 + 文案 + 源码契约（权限码 / 接口路径逐字对齐后端注解） |

交互：点按钮 → 二次确认（「这笔钱还没付出去 / 执行后立即应付回写 + 付款凭证 / 审批人不能执行自己审批的」）
→ `POST …/execute` → 成功提示 **后端 `data.message`** 并重取详情 + 列表；失败**原样弹后端 `msg`**
（409 + `66013` 是内控中文明文，一个字不改）。

## 2. 判据来源（卡面 §3 的 A/B 二选一，实际是 **B 契约 + A 兜底**）

`GET /payable/{id}/payment-info` 的 `payments[]` 里**没有**「审批通过但还没执行」这个事实
（只有 `approval_status` / `approval_status_label` / `pending` / `approval_instance_id`），
而两种 `PENDING` 从付款记录本身无法区分：

- 开关**关闭**：审批通过即自动记账 → 付款直接 `APPROVED`；
- 开关**打开**：审批通过 → 付款仍 `PENDING`（账一分没动）。

所以：

1. **首选后端字段**：`awaiting_executor`（方案 B）。已开子卡 `t_6f2200e1` 给 backend-developer。
2. **老后端兜底**：`approval_status === 'PENDING'` 且用 `approval_instance_id` 查到的实例是 `APPROVED`。
   这一发请求**常态下不额外发**：审批进度块（`loadApprovalProgressOf`）本来就会拉同一张实例，
   顺手把 `instance.status` 记进缓存，`loadExecutorGate` 命中缓存即返回。
   实测（`raw/43_request_count.txt`）：真后端下对同一实例只 **1** 发 `GET /business/flow/instance/{id}`
   —— 就是进度块那一发；兜底探针如果另发一次会是 2 发。
3. `awaiting_executor` 一旦下发，`isAwaitingExecutor` 直接采信（`true`/`false` 都信），
   `executorProbeInstanceId` 返回 `null` → 探针整段跳过。前端零改动、只多一个字段。
   同一次实测里用拦截层模拟了将来态：`awaiting_executor=true` → 按钮出现；
   `=false`（哪怕实例是 `APPROVED`）→ **按钮不出现**，即「后端说了算」。

判据写成纯函数而不是模板内联，是为了这条「两源优先级」能被单测钉死（写错不会报错，只会
按钮该出不出现 / 不该出现乱出，`vue-tsc` 抓不到）。

## 3. 门禁

| 门禁 | 结果 | 证据 |
|---|---|---|
| `pnpm typecheck`（`vue-tsc --noEmit`） | **exit 0** | `raw/10_typecheck.txt` |
| 本卡单测 `tests/unit/p307-awaiting-executor.test.ts` | **16 / 16 pass** | `raw/20_unit_p307.txt` |
| 全量单测 `pnpm test:unit` | 1065 / 1067 pass | `raw/21_unit_full.txt` |
| ↳ 2 条红是 **HEAD 既有问题** | 在 `d2688fb` 的**干净 worktree**（`git status` 0 文件）上同样红 | `raw/22_unit_baseline_pristine.txt` |
| `pnpm check:palettes` | 全部通过 ✓（本卡只用 TDesign 主题，无裸 hex） | `raw/30_palettes.txt` |
| 真机全链 | **PASS 19 / FAIL 0** | `raw/40_e2e.txt` |

全量单测那 2 条红的归属（不是本卡文件）：

- `erp-sales-order.test.ts` › `create payload keeps dates and folds empty remark to null`
- `flow-biz-link.test.ts` › `后端 FlowBizType 的落库字面量 = 前端 FLOW_BIZ_TYPES（逐字、同序，15 个）`

## 4. 真机验收（隔离实例，不碰生产）

环境：`vite dev :5217`（源码即产物）→ 反代 `:18312` 后端（`/opt/cloudpivot/backend/target/debug/cloudpivot`，
md5 `35292ce8`，含 `POST /payable/payment/{id}/execute` 路由）→ 空库 `cloudpivot_p307fe`。

一次性准备（`raw/setup.sh` + `raw/fixture.sql`/`fixture2.sql`，全部可重跑）：

- 空库首启（273 条迁移 + 种子流程/科目/期间）→ admin 改密 → 建两个账号：
  - `p307.pay`（`FINANCE_MANAGER`）= 发起付款人；
  - `p307.mgr`（`DEPT_LEADER` + `FINANCE_MANAGER`）= `payable_pay` 流程「部门主管审批」节点的审批人；
- 通过**系统参数接口**打开 `erp.separation_of_duties.approve_execute`（`PUT /api/support/config/{id}`）；
- 3 张应付单是 **SQL 夹具**（收货过账链路与本卡无关），付款 / 审批 / 执行**全部真接口**。

那 3 张应付单是用 SQL 直接插的（收货过账链路与本卡无关，只为给「付款 / 审批 / 执行」一个载体）；
**付款、审批、执行、状态回读全部走真实接口**，断言里的 `paid_amount` / 付款状态也全部来自接口回读。

### 用例与结果

| 用例 | 断言 | 结果 |
|---|---|---|
| 准备① | AP-P307-0001 发起付款 → 202 `PENDING_APPROVAL`；部门主管审批 → 实例 `APPROVED`；随后付款仍 `PENDING` 且 `paid_amount=0`（= 开关真的生效，账一分没动） | PASS ×3 |
| ①-1 | 详情付款记录行出现「执行付款」按钮（`v-privilege` 命中） | PASS |
| ①-2 | 动作条提示是「钱还没付出去 / 请另一位有付款权限的账号执行」，不是「等审批结果」 | PASS |
| ①-3 | 二次确认含「还没付出去 / 应付回写 / 付款凭证 / 审批与执行分离」四要素 | PASS |
| ①-4 | 确认后 `POST /execute` **200** + `data.message` | PASS |
| ①-5 | 成功提示 = 后端原话「付款 PM-20261108-000001 已执行：应付回写 + 付款凭证已生成」 | PASS |
| ①-6 | 付款记录「审批中」→「已通过」 | PASS |
| ①-7 | 应付未付 1200 → 0.00（**账真的动了**，凭证号 `PZ-20261108-000001` 已生成） | PASS |
| ①-8 | 执行后按钮消失 | PASS |
| ②-1 | 审批人自己（也有付款权限）同样看到按钮 | PASS |
| ②-2 | 他点 → 后端 **409 + 66013** | PASS |
| ②-3 | 弹的是后端 `msg` 原文：「系统已开启「审批与执行分离」，付款 PM-… 由你本人审批，不能自己执行。请交给其他有权限的账号处理，或在系统参数中关闭该开关（erp.separation_of_duties.approve_execute）。」 | PASS |
| ②-4 | 拦下后账一分没动（付款仍 `PENDING`、`paid=0`） | PASS |
| ③-1 | 系统参数关掉 `approve_execute` | PASS |
| ③-2 | 审批通过即自动记账：付款直接 `APPROVED`、`paid=800` | PASS |
| ③-3 | 开关关闭时「执行付款」按钮**不出现**（回归：审批通过即已生效） | PASS |

截图（`shots/`）：

- `01-awaiting-executor-button.png` — 详情：付款记录「审批中」+「执行付款」按钮 + 动作条提示；
- `02-confirm-dialog.png` — 二次确认（写明钱还没付 / 立即记账 / 审批人不能执行）；
- `03-executed.png` — 执行后：付款「已通过」、凭证号出现、未付 0.00、按钮消失；
- `04-66013-toast.png` — 审批人本人点：右下角 toast = 后端 66013 中文原文；
- `05-switch-off-no-button.png` — 开关关闭：审批通过即「已通过」，无按钮；
- `06-row-alignment.png` — 按钮在付款记录行里的排版（620px 抽屉必然换行，`align-self:center` 对齐）。

复现：`bash raw/setup.sh`（空库 + 后端已起）→ `node raw/p307e2e.mjs`。

## 5. 已知边界（写清，别当成没问题）

1. **普通执行人可能看不到按钮（老后端）**：兜底判据要读审批实例，而
   `GET /business/flow/instance/{id}` 只对「发起人 / 审批人 / 抄送人 / `business:flow:admin`」开放。
   执行人按设计不是参与人 → 403 → 按钮不出现（**不会误显示**，点下去也只会 409）。
   本次真机两条 UI 用例用的是**参与人**令牌（发起人执行 / 审批人被拦），所以能验；
   普通财务岗看到按钮这件事要等 `t_6f2200e1` 下发 `awaiting_executor`。
2. **验收账号的既有权限缺口（与本卡无关）**：验收里 `GET /api/support/dict/items/pay_method` 回 403
   （`support:dict:list` 不在业务角色矩阵里），是纯财务账号的既有缺口、与本卡无关
   —— 只影响「付款方式」下拉候选，不影响付款/执行链路。
3. 开关默认 `false`，**平时看不到这个按钮**（审批通过即已生效）；要复现必须在「系统参数 → ERP 内控」打开。
4. 未部署 145/144：本卡只提交代码；上线后开关仍默认关闭，无需迁移动作（V0340 已随 P3-07 落地）。

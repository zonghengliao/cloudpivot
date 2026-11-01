# P-FLOW-3B 前端配套证据（作废/红冲「审批中」徽标 + 撤回按钮）

> 卡：`t_c10da409`（前端配套，父卡 `t_50b3bdc5` 已交付后端 0828a76）
> 分支/提交：`p3bfe`（基于 `0828a76`）；真机环境：隔离后端 `127.0.0.1:18096` + 库 `cloudpivot_3b_e2e` + vite dev `127.0.0.1:5190`
> 生产 dist **未发布**（后端还有 3 处契约缺口，见 §五；一并补齐后发布更安全）

## 一、卡面四条要求 → 落地对照

| 卡面要求 | 落地 | 关键文件 |
|---|---|---|
| ① 应付列表/详情支持 `PENDING_CANCEL`，「作废」「付款」禁用，改显示「撤回作废审批」 | 状态标签 `作废审批中`（warning 橙）；`canPayPayable` / `canCancelPayable` 对该状态均为 false；行内与抽屉都有「撤回作废审批」；抽屉顶部 warning 条说明「欠款一分没动」+ 审批进度（标题「作废审批进度」）；状态筛选下拉新增「作废审批中」 | `payable/payable-logic.ts`、`payable/index.vue` |
| ② 凭证列表/详情显示「红冲审批中」，「红冲」禁用，改显示「撤回红冲审批」 | 凭证号单元格 + 详情头并列一个 warning 标签（**不顶替**「已记账」，凭证状态不变）；`canReverseVoucher` 对该状态 false；行内/详情头/详情警告条三处「撤回红冲审批」 | `voucher/voucher-logic.ts`、`voucher/index.vue` |
| ③ 送审后提示「已提交审批，等待财务经理复核」并刷新列表，不许说「已作废/已红冲」 | `submitFeedback()` 按 `outcome` 出文案：`PENDING_APPROVAL` → **info**（文案带单号 + 「仍是未付/仍是已记账，审批通过才…」）；`CANCELLED`/`REVERSED`（未配流程降级直做）→ success 老文案；未知 outcome → 用后端 `message` 兜底。列表/抽屉都刷新 | `shared/approval-flow.ts` |
| ④ 单测：`outcome` 分支文案 + 按钮可见性（D7 的纯逻辑抽法） | `tests/unit/pflow3b-cancel-reverse.test.ts` **17 条**；另修既有 `erp-payable-voucher-ledger.test.ts` 的筛选项断言（5→6 项） | 同左 |

## 二、门禁（本地，`repo/frontend`）

```text
pnpm typecheck      → 0 error（vue-tsc --noEmit）
pnpm test:unit      → 445 passed / 0 failed（既有 428 + 本卡 17）
pnpm check:palettes → 全部通过 ✓（330 条色阶断言，0 失败；本次只用 TDesign 语义主题，无裸 hex）
vite build          → 见 build.log（typecheck 已先行通过；SFC 已在 dev 下真实编译运行）
```

单测钉的 8 组口径（每条都在 `tests/unit/pflow3b-cancel-reverse.test.ts` 里）：

```text
① 状态映射：PENDING_CANCEL → 「作废审批中」+ warning；并与后端 payable.rs / payable_query.rs 字面量互相对照
① 筛选下拉 6 项（含 PENDING_CANCEL）
② 按钮矩阵：作废审批中 → 付款 ✗ / 作废 ✗ / 撤回 ✓（无权限 ✗ + 说明缺哪个码）
③ canStartPay 第四道闸：后端 can_pay=true 但 PENDING_CANCEL → 不显示（后端口径缺口的兜底）
④ 应付送审文案：含「已提交审批，等待财务经理复核」，**不含「已作废」**
⑤ 撤回文案：应付「回到「未付」」/ 凭证「仍是「已记账」」
⑥ 凭证送审文案：**不含「已红冲」「红字凭证 PZR」**
⑦ 凭证按钮：红冲审批中 → 红冲 ✗ / 撤回 ✓；红字/已红冲/非 POSTED 老判据不变
⑧ 兜底映射：实例分页 → 凭证 id/实例 id（只认 VOUCHER_REVERSE + PENDING）；outcome 常量与后端一致
```

## 三、真机流程（Playwright，2 条用例全绿）

`verify/tests/pflow3b-fe.spec.ts`，`2 passed (8.6s)`。环境是**真后端 + 真库 + 真流程**（vite dev 5190 反代 18096），
夹具由 `make_fixtures.py` 走真链路造（收货单 800 元 → 过账 → 自动落应付 + 进项凭证）。

### ① 应付作废（`AP-20261102-000008`）

| 步骤 | 断言（DOM/接口/库） | 截图 |
|---|---|---|
| 初始 | 状态标签「未付」、有「作废」「付款」、无「撤回」 | `01-payable-open.png` |
| 点作废 → 填原因 → 提交 | `POST /payable/{id}/cancel` 200，响应 `data.outcome = PENDING_APPROVAL`；toast 原文见下 | —— |
| 送审后列表 | 标签「作废审批中」；「撤回作废」出现；「作废」「付款」都消失；库 `status = PENDING_CANCEL` | `02-payable-pending-cancel.png` |
| 详情抽屉 | 出现「作废审批中」warning 条 + 「撤回作废审批」按钮；「发起付款」按钮消失并给出「作废审批中不能付款」 | `03-payable-drawer-pending.png` |
| 点撤回 → 确认 | `POST /payable/{id}/withdraw-cancel` 200；标签回「未付」；「作废」回来、「撤回」消失；库 `status = OPEN` | `04-payable-withdrawn.png` |

toast 原文（真机捕获）：`已提交审批，等待财务经理复核（AP-20261102-000008）：应付单仍是「未付」，审批通过才会真正作废`

### ② 凭证红冲（`PZ-20261102-000018`）

| 步骤 | 断言 | 截图 |
|---|---|---|
| 初始 | 状态「已记账」、有「红冲」、无「红冲审批中」 | `05-voucher-posted.png` |
| 点红冲 → 填原因 → 提交 | `POST /voucher/{id}/reverse` 200，`outcome = PENDING_APPROVAL`，**`status` 仍是 `POSTED`** | —— |
| 送审后列表 | 出现「红冲审批中」标签、状态仍「已记账」、「撤回红冲」出现、「红冲」消失；审批引擎里确有一条 `VOUCHER_REVERSE / PENDING / biz_id=凭证 id` 的实例 | `06-voucher-pending-reverse.png` |
| 详情抽屉（送审后自动打开） | 「红冲审批中」标签 + warning 条 + 「撤回红冲审批」；状态仍是「已记账」 | `07-voucher-drawer-pending.png` |
| **整页刷新后重查** | 「红冲审批中」仍在（走的是「后端没下发字段 → 审批实例兜底」这条路，见 §四） | `08-voucher-pending-after-reload.png` |
| 点撤回 → 确认 | `POST /voucher/{id}/withdraw-reverse` 200；标签消失、「红冲」回来、状态「已记账」；引擎里已无 PENDING 实例 | `09-voucher-withdrawn.png` |

toast 原文（真机捕获）：`已提交审批，等待财务经理复核（PZ-20261102-000018）：凭证仍是「已记账」，审批通过才生成红字凭证`

复跑说明：`make_fixtures.py` 造夹具、`setup_approver.py` 建财务经理账号（否则 ROLE 解析不到人 → 内联跳过 → 实例直接 APPROVED，看不到「审批中」），
spec 的 `beforeAll` 会先自愈（把上一轮残留的审批中撤回），因此可反复运行。

## 四、契约缺口与前端兜底（→ 子卡 t_p3bfe_be_gap）

动手前按「卡面口径 vs 后端枚举，先 grep 后端」逐条核对了 REST 契约，发现 4 处缺口，前端都做了**可撤掉**的兜底：

| # | 缺口 | 前端现状 | 后端补上后 |
|---|---|---|---|
| 1 | `voucher_query.rs::VoucherRes`（列表 + 详情）**没有** `pending_reverse_instance_id` / `reverse_reason`（`VoucherRow`、`biz_voucher` 列都有） | 字段声明为**可选**；缺席时用 `GET /business/flow/instance/page?status=PENDING&biz_type=VOUCHER_REVERSE` 把 `biz_id → 实例 id` 补回行上（补的是**真实例 id**，不是真假值）。多 1 个请求/次列表加载；没 `business:flow:instance:read` 或接口失败时静默降级（最坏是暂时不显示徽标，点红冲会被 409 挡住，不会重复送审） | 字段一出现即为权威值，兜底**自动不再发请求**（判据 `rows.every(row => row.pending_reverse_instance_id !== undefined)`） |
| 2 | V0191 的 4 个新权限码只授 `SUPER_ADMIN`；老码 `business:erp:payable:cancel` / `voucher:reverse` 才授了 `ADMIN/FINANCE/ACCOUNTANT`（V0158） | 「提交」仍打**老 URL** `/cancel` `/reverse`（后端同一函数、同样走审批），否则财务角色点作废/红冲会 403；「撤回」只有新 URL，所以撤回入口**按权限显示**，无权限时抽屉里说明缺哪个码 | 权限矩阵对齐后，把两个 URL 换成 `/submit-*` 只是一行；撤回入口自动对财务角色可见 |
| 3 | `payable_payment.rs::payment_capability` 没有 `PENDING_CANCEL` 分支 → 该状态 `can_pay` 仍为 `true` | `canStartPay(info, hasPerm, status)` 增加第 4 个条件：`status === PENDING_CANCEL` 一律 false（真机已验证按钮消失 + 「作废审批中不能付款」提示） | 这道前端闸变成冗余（不会误伤） |
| 4 | `PayableRes` 没有 `cancel_instance_id` / `cancel_reason` → 重新打开详情时拿不到作废审批实例 | 送审那一刻用响应里的 `approval_instance_id` 显示「作废审批进度」；**重新打开抽屉后进度区不显示**（不是 bug，是字段没下发） | 详情可直接拉进度 + 显示作废原因 |

## 五、行为补充（容易被误判的两点）

- **凭证状态永远不动**：红冲审批中 `status` 仍是 `POSTED`（后端硬红线 3：报表取数写死 `WHERE status='POSTED'`）。所以徽标必须与「已记账」**并列**，不能顶替。
- **送审 ≠ 生效**：`outcome = PENDING_APPROVAL` 时只弹 info，且文案明确「仍是未付 / 仍是已记账」；只有未配流程降级直做时才用 success 与老文案（`已作废：` / `已红冲：生成红字凭证 PZR-…`）。这条由单测 ④⑥ 双向断言（含 `not.toContain('已作废')` / `not.toContain('已红冲')`）。

## 六、改动清单

```text
frontend/src/api/business/erp/index.ts                        PayableStatus + PENDING_CANCEL
frontend/src/views/business/erp/shared/approval-flow.ts       新增：outcome→文案 纯逻辑（含撤回）
frontend/src/views/business/erp/payable/payable-logic.ts      状态映射/筛选/按钮判据/canStartPay 第四道闸
frontend/src/views/business/erp/payable/payable-write-api.ts  CancelSubmitResult + withdraw-cancel
frontend/src/views/business/erp/payable/index.vue             徽标/撤回入口/抽屉提示/送审反馈
frontend/src/views/business/erp/voucher/voucher-logic.ts      红冲审批中判据 + 实例兜底映射
frontend/src/views/business/erp/voucher/voucher-write-api.ts  pending 字段 + ReverseSubmitResult + withdraw-reverse
frontend/src/views/business/erp/voucher/index.vue             徽标/撤回入口/抽屉提示/送审反馈/兜底加载
frontend/tests/unit/pflow3b-cancel-reverse.test.ts            新增 17 条
frontend/tests/unit/erp-payable-voucher-ledger.test.ts        筛选项 5→6
```

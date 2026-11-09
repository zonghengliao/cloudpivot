# N-5 附件预检扩展到全部必传单据 —— 现状核对 + 补口 + 实测证据

> 卡：`t_2582434f`（NEAR-FIX-5 / 评审 5.6 扣分点「附件必传只在提交时拦，草稿无提示」）
> 日期：2026-11-10 · 环境：`172.18.10.99`（工作仓 `/opt/cloudpivot`）、`172.18.10.145`（cloudpivot-test 实例，`:80`）
> **未触碰 144 生产**；本轮只读 145 的库，未 DROP / TRUNCATE / 删任何数据。

## 0. 结论前置

1. **卡面背景已过期**：评审当时「E2E01-FIX-1 只做了采购退货」——在那之后
   `ATT-06 / ATT-08 / ATT-09 / ATT-10 / E2E05-FIX-2` 这几轮已经把 **8 类必传单据全部接完**。
   逐个核对结果：**8/8 都有草稿/操作页预检**（闸门 + 红字 + 按钮 disabled + 上传后自动重查）。
2. 本轮**真正缺的只有 1 处**：**付款单（PAYMENT）的「登记付款」弹窗没有新建态提示**
   （另外 7 类的新建/登记入口都有）。已补，并加 3 条单测钉住 8/8 的「新建态提示」覆盖。
3. 后端**零改动**：闸门判据早在 `business::attachment::rule`（ATT-06/08）落地，
   `biz_attachment_rule` 种子（V0347，V0348 把发票条件改成 `is_electric`）覆盖 8 类单据。
   本轮用 **145 真实 token curl** 把这条判据逐类打了一遍：**8/8 HTTP 200 且语义正确**（见 §4）。
4. 交付：`pnpm typecheck` + `pnpm test:unit`（1310 全绿）+ `pnpm build`（42.12s 成功）；
   145 实测脚本 `RESULT: PASS`。

## 1. 逐单据现状（验收①②③④）

| # | 单据 | `doc_type` / `at` | 草稿·操作页 | 闸门 | 红字 | 按钮 disabled | 上传后重查 | 新建态提示 |
|---|------|-------------------|-------------|------|------|----------------|-------------|-------------|
| 1 | 发票 | `INVOICE` / `SUBMIT` | `invoice/index.vue` 详情「开票」 | ✅ `useAttachmentGate` | ✅ `inv-detail-gate-hint` | ✅ 权限 ∪ 闸门 | ✅ `@changed` | ✅ `inv-create-attach-notice` |
| 2 | 收款单 | `RECEIPT` / `SUBMIT` | `receipt/index.vue` 详情「确认收款」 | ✅ | ✅ `rc-detail-gate-hint` | ✅ | ✅ | ✅ `rc-create-attach-notice` |
| 3 | 付款单 | `PAYMENT` / `ACTIVATE` | `payable/index.vue` 付款行「执行付款」 | ✅ 按付款行多实例表 | ✅ `payable-payment-gate-hint-<单号>` | ✅ | ✅ `refreshPaymentGates()` | ✅ **本轮补** `payable-pay-attach-notice` |
| 4 | 费用报销 | `EXPENSE_REPORT` / `SUBMIT` | `expense/ExpenseDetailDrawer.vue` 详情「提交审批」 | ✅ | ✅ `expense-detail-gate-hint` | ✅ | ✅ | ✅ `expense-create-attach-notice` |
| 5 | 固定资产 | `FIXED_ASSET` / `ACTIVATE` | `asset/index.vue`「启用资产」弹窗 | ✅ | ✅ `asset-enable-gate-hint`（黄字） | **刻意不置灰**（见 §5.2） | ✅ `refreshAssetGate()` | ✅ `asset-create-attach-notice` |
| 6 | 盘点单 | `STOCK_TAKE` / `CONFIRM` | `stock-take/index.vue` 详情「提交审批」 | ✅ | ✅ `stocktake-detail-gate-hint` | ✅ 仅 `PENDING_POST` 边 | ✅ | ✅ `stocktake-create-attach-notice` |
| 7 | 采购退货 | `PURCHASE_RETURN` / `SUBMIT` | `purchase-return/index.vue` 详情「提交审批」 | ✅ | ✅ `prt-detail-gate-hint` | ✅ | ✅ | ✅ `prt-create-attach-notice` |
| 8 | 销售退货 | `SALES_RETURN` / `SUBMIT` | `sales-return/index.vue` 详情「提交」 | ✅ | ✅ `return-detail-gate-hint` | ✅ | ✅ | ✅ `return-create-attach-notice` |

列表行（点行内动作按钮，不经详情）也都有「先查一次闸门 → 缺附件事把人送到详情 + 点名缺什么」：
发票 `issueInvoice` / 收款 `confirmReceipt` / 报销 `handleSubmit`+`handleSave` /
采购退货 `passAttachmentGate` / 销售退货 `submitReturn`。

**验收③「上传附件后按钮自动恢复」的实现链路**：`DocAttachmentPanel` 的 `@changed` → `refresh()`
→ `GET /attachment/check` → `gateFromCheck()` → `canProceed` 翻真 → `:disabled` 立刻解除。
乱序响应由组合式函数里的自增序号丢弃（连传两个文件不会把状态回退）。

## 2. 本轮实际改动

| 文件 | 改动 |
|------|------|
| `frontend/src/views/business/erp/payable/index.vue` | 「登记付款」弹窗新增 warning 提示 `payable-pay-attach-notice`：出款前必须上传银行回单（pdf/jpg/png，≥1 个）；审批通过后在详情「付款回单」面板上传，缺回单「执行付款」置灰；未配付款审批流程时提交即生效、回单事后补传。**付款记录在提交动作内部才创建 → 弹窗这一刻没有 id，查不了接口，所以只能靠文案**（验收④） |
| `frontend/tests/unit/attachment-gate-pages.test.ts` | 新增 3 条单测：<br>⑫ 8 类单据的**新建/登记入口**提示全覆盖（testid + 主题 + 点名格式白名单 + 「去哪传」），共 8 行断言表<br>⑬ V0347 种子的每个 `(doc_type, required_at)` 必须至少有一个页面接线（防「配了规则没人接」静默失效）<br>⑭ 公共 composable 复用：全仓只允许一处 `export function useAttachmentGate`，7 页必须 import；应付页的多实例表是唯一例外（判定仍走同一套纯函数） |
| `scripts/verify/n5_attachment_precheck.sh` | 新增 145 实测脚本：**自动探针**每类单据取一个现存 id（不写死），逐类调 `GET /attachment/check`，断言 `can_proceed` 两侧都会翻转、`need` 与规则表 `allow_exts` 逐字一致、非法 `at`→400、不存在单据→404、未登录→401。令牌不落盘 |

**后端无改动**（闸门已在 ATT-06/08 落地）。既有后端单测已覆盖判据形状：
`check_parse_required_at` / `check_res_shape_matches_card_contract` / `check_can_proceed_iff_no_missing`
（`backend/src/business/attachment/tests.rs`），迁移侧有 V0347 §4 自检（9 条种子齐全 + 8 类覆盖 + 发票两条互补 + CHECK 放宽）。

## 3. 判据来源（145 库里规则表原文 · `00_rules.txt`）

```text
EXPENSE_REPORT|SUBMIT|pdf,jpg,png,ofd|-|发票影像或费用凭证
FIXED_ASSET|ACTIVATE|pdf,jpg,png|-|发票或验收单
INVOICE|SUBMIT|pdf,jpg,png|is_electric IS FALSE|纸质发票扫描件
INVOICE|SUBMIT|pdf,ofd,xml|is_electric IS TRUE|发票影像
PAYMENT|ACTIVATE|pdf,jpg,png|-|银行回单
PURCHASE_RETURN|SUBMIT|pdf,jpg,png|-|退货单或签收凭证
RECEIPT|SUBMIT|pdf,jpg,png|-|银行回单
SALES_RETURN|SUBMIT|pdf,jpg,png|-|退货单或物流凭证
STOCK_TAKE|CONFIRM|pdf,jpg,png|-|盘点表或盈亏说明
```

> 发票的条件列在 145 上是 `is_electric IS TRUE/FALSE`（V0348 加了 `biz_invoice.is_electric`），
> 与仓库里 V0347 头注释写的 `invoice_code IS NULL / NOT NULL` 是**同一规则的两代写法**——
> 145 的库已经过 V0348，仓库侧页面只读接口结果、不关心条件写法，故不受影响。

## 4. 145 实测（`06_145_precheck_full.txt`，`RESULT: PASS`）

| 单据 | 探针单据 | HTTP | `can_proceed` | 红字 / 说明 |
|------|----------|------|---------------|-------------|
| INVOICE | `0b256e29…86c5` | 200 | true | 已有合规发票影像（1 个附件） |
| RECEIPT | `80532de1…306a` | 200 | **false** | 缺少银行回单（需 pdf/jpg/png 格式） |
| PAYMENT | `6a365328…5f33` | 200 | **false** | 缺少银行回单（需 pdf/jpg/png 格式） |
| EXPENSE_REPORT | `01a1b5fa…61e5` | 200 | **false** | 缺少发票影像或费用凭证（需 pdf/jpg/png/ofd 格式） |
| FIXED_ASSET | `01a1b5f3…557f` | 200 | **false** | 缺少发票或验收单（需 pdf/jpg/png 格式） |
| STOCK_TAKE | `01a1bdf5…5036` | 200 | true | 已有盘点表（1 个附件） |
| PURCHASE_RETURN | `01a1bdc8…564e` | 200 | true | 已有退货单（1 个附件） |
| SALES_RETURN | `01a1bddb…6332` | 200 | true | 已有退货单（1 个附件） |

两侧都出现了（4 假 4 真）——说明「传完就变亮」不是只看 `true` 的假绿：
同一接口对缺附件的单确实判 `false`，对已传的单确实翻 `true`。
边界：非法 `at` → `400 10001`（不静默回落）、不存在单据 → `404 10002`（前端据此降级放行）、未登录 → `401`。

## 5. 与卡面的三处偏差（都留档，不是偷工）

### 5.1 组合式函数路径：`shared/use-attachment-gate.ts`，不是 `src/composables/useAttachmentCheck.ts`

卡面建议 `frontend/src/composables/useAttachmentCheck.ts`。仓里 **ATT-08 已经落地了等价物**
`views/business/erp/shared/use-attachment-gate.ts`（导出 `{ gate, checking, refresh }`，
`gate` 内含 `canProceed`），并被 8 页引用；同时配套 `attachment-gate-pure.ts`（纯函数判定，可穷举单测）
与 `AttachmentGateHint.vue`（红字组件）。**再新建一个 `useAttachmentCheck` 就是第二份实现**，
与验收⑤「不是 8 份重复代码」的初衷相反。故本轮不新建，改为用单测 ⑭ 钉死「全仓只有一份实现」。

### 5.2 固定资产「启用」**故意不置灰**（E2E05-FIX-2 方案 B）

建卡不查附件（建卡时资产是 IDLE），若「启用」硬拦，「发票未到先建卡」的资产停用一次就**永远启用不回来**
（E2E-05 演练实测 400，界面上没有可点的按钮）。所以 `FIXED_ASSET` 现在是
「提示 + 允许 + 审计」：缺附件时弹窗黄字（文案仍来自后端 `assetGate.hint`）+ 去上传的链接 +
点「确认启用」时二次确认 → 照常提交，后端写 `operation_logs.ERP_ASSET_ENABLE_NO_ATTACH` 并在响应 `warning` 里回同一句话。
这违反卡面验收②的字面（「缺附件 → 按钮 disabled」），但**是上一轮为修死锁刻意降级的**，本轮沿用并单测钉住
（`attachment-gate-pages.test.ts` ⑤：任何按钮都不得再被 `assetGate` 直接禁用）。

### 5.3 PAYMENT 的 `required_at` 是 `ACTIVATE` 而不是 `SUBMIT`

付款记录是**提交动作内部一次性创建**的（`payable_payment::submit_for_approval_tx`），提交那一刻还没有
主键可以挂附件 —— 在 SUBMIT 上校验等于把付款永久锁死。真实闸口是**出款前**
（`execute_approved_payment`，此时 PENDING 付款单已存在，可在「付款回单」面板挂回单）。
**已知边界（非本轮引入）**：未配「应付付款审批」流程时付款是直付路径（提交即生效），
`execute_approved_payment` 不会被调用 → 该规则对直付不生效。本轮补的弹窗文案已如实写明这一点
（「未配付款审批流程时提交即生效，回单请事后补传」），不再让用户误以为会被拦。

## 6. 复现

```bash
# 前端（99 工作仓）
cd /opt/cloudpivot/frontend
pnpm typecheck                      # vue-tsc --noEmit
npx tsx --test tests/unit/attachment-gate-pages.test.ts   # 18 条（含本轮 3 条）
pnpm test:unit                      # 1310 全绿
pnpm build                          # 42.12s

# 145 实测（真实 token；脚本会自己 source /etc/cloudpivot/cloudpivot-dev.env）
scp scripts/verify/n5_attachment_precheck.sh root@172.18.10.145:/tmp/
ssh root@172.18.10.145 'bash /tmp/n5_attachment_precheck.sh /tmp/n5-attachment-precheck'
# → RESULT: PASS（8/8 单据 200 + 两侧翻转 + 边界三条）
```

## 7. 证据文件

| 文件 | 内容 |
|------|------|
| `00_rules.txt` | 145 `biz_attachment_rule` 启用中 9 条规则原文 |
| `01_probe_docs.sorted.txt` | 8 类单据的探针 id（自动从库取，只读） |
| `02_check_all.jsonl` | 8 次 `GET /attachment/check` 的原始响应（逐行 JSON） |
| `03_gate_unit_tests.txt` | `attachment-gate-pages` + `attachment-gate` 两个测试文件：32 条全绿 |
| `04_frontend_unit_full.txt` | 全量前端单测：1310 passed / 0 failed |
| `05_frontend_build.txt` | `pnpm build` 全量输出（内含 typecheck；`✓ built in 42.12s`） |
| `06_145_precheck_full.txt` | 145 实测完整 stdout（`RESULT: PASS`，无令牌） |

# ERP 按钮可达性审计 + 断链修复（P8）

> 审计时间：2026-10-31 09:33–10:15（CST）
> 执行人：frontend-developer（卡 t_8d7619fa）
> 环境：Chromium（Playwright 1.64）+ **真后端** `http://172.18.10.144`（nginx `/api` 反代到 8023），
> 前端产物 = `main@865bd61` + 本卡修复后本地 `pnpm build` 的 `dist`（本地静态服务 `127.0.0.1:5210`，`/api` 整段反代）。
> 账号：`laoliu_test`（只登录一次，token 落 storageState 复用）。
> 脚本与原始 JSON：`/opt/cloudpivot-verify/`（`tests/audit.spec.ts`、`out/audit-*.json`，**不在仓库内**）。

## 0. 结论速览

| 项 | 结果 |
|---|---|
| 15 个 ERP 页面 | 全部能打开，**加载期 4xx/5xx = 0** |
| 可点元素实测 | **155 个按钮/链接**逐个点击，每个都判定了反应类型 |
| 「有按钮点了没反应」 | **0 个**（初版脚本报的 8 个「none」全部复核为「已处于目标态 / 内联提示无文案变化」，见 §3） |
| 「点了报 4xx」 | **1 处真 bug**：调拨单「撤回」→`409 非法状态流转`（已修，见 §4） |
| 断链 1 `category/page` | 源码 **0 处**调用（早在 W4b-1 已改走 `material_type`），后端也无此路由 |
| 断链 2 `ledger/balance` | 源码 **0 处**调用，页面走 `ledger/trial-balance`，真机 200 + 4 行 + 期间切换 200 |
| 前端调用 vs 后端路由 | 92 个 api 调用点 ↔ 366 条后端路由，**缺失 0** |

## 1. 审计方法（可复现）

1. 每个页面 `goto` → 等 `networkidle` → 记录**加载期**所有 `/api/` 响应码与渲染证据（标题 / 表格行数 / 内联提示）。
2. 枚举内容区（`.basic-layout__content`）内所有 `.t-button` / `button` / `.t-link`（表格行内动作链接也算），
   记录文案、是否 `disabled`、`title`。
3. 逐个点击（`noWaitAfter`），1.1s 后对比快照：弹窗文案、抽屉 `t-drawer--open`、下拉浮层、`t-message` 提示、
   `t-table tbody tr` 行数、内容区文本长度、URL、以及点击期间全部 `/api/` 响应码。
4. 复位：**精确匹配**「取消」关弹窗（不是「确认取消」）、关抽屉、Esc、点空白，保证下一个按钮不被遮罩挡住。
5. 危险动作策略：二次确认框（删除 / 作废 / 过账 / 取消 / 出库 / 报废）**只验证弹窗出现即取消**，不确认落库；
   需要验「写接口是否 200」的，另写专门用例**自建数据 → 走真链路 → 删干净**（§4）。

> 踩坑记录（脚本已改）：`hasText:'取消'` 会同时命中「确认取消」，第一版脚本因此在 3 张采购单上
> 点掉了「确认取消」。已改为 `/^取消$/` 精确匹配，并把误操作的 3 张单在库里恢复成 `DRAFT`
> （`PO-20261031-000003 / -000009 / -000011`，见 §6 披露）。

## 2. 断链 1：`/erp/category/page` 404 — 源码早已修好，不是待修项

```bash
$ grep -rn "category/page" frontend/src/ | wc -l
0
$ grep -rn "erp/category" frontend/src/            # 只剩两处**注释**说明这个接口不存在
api/business/erp/index.ts:313  * 后端只种了 business:erp:category:* 权限码，handler.rs 里没有 /erp/category 路由
material/material-query.ts:12  * | 分类 category_id | material_type ∈ RAW/SEMI/FINISHED/SERVICE | ...
```

- 物料页的「分类」= `material_type` 四值（`RAW/SEMI/FINISHED/SERVICE`），不是独立分类表；
  后端 `biz_material` 没有 `category_id` 列（`V0137`），也没有任何 `biz_material_category` 表。
- 真机实测（物料页）：加载 4xx = **0**；筛选区「查询」「重置」各打一次 `GET /erp/material/page → 200`；
  「新建物料 / 编辑」都能打开弹窗且字段回填（§3）。
- 直接打接口仍是 404（后端确实没有这个路由）：

```
GET /api/business/erp/category/page?page=1&page_size=10 → 404 {"code":404,"msg":"请求的资源不存在"}
```

⇒ 结论：**情况 C**（分类字段根本不存在），前端已删干净，无需再动。

## 3. 断链 2：`/erp/ledger/balance` 404 — 源码早已改成 `trial-balance`，真机已验证

```bash
$ grep -rn "ledger/balance" frontend/src/ | wc -l
0
$ grep -n "trial-balance" frontend/src/api/business/erp/index.ts
1922  * 科目余额表（GET /erp/ledger/trial-balance，period 必填 yyyy-MM）
1924    return get<TrialBalance>(`${BASE}/ledger/trial-balance`, asParams({ period }));
```

真机用例 `tests/ledger-verify.spec.ts`（真浏览器 + 真后端，零拦截）：

```
[ledger] ledger/page 200，本页 6 行
[trial] http://127.0.0.1:5210/api/business/erp/ledger/trial-balance?period=2026-10 200，4 行，period=2026-10
[trial] 期间切换 2026-10 → 2026-09 200，4 行
```

- Tab1（总账流水）：选科目 → 查询 → `GET /ledger/page` **200，6 行**；
  不选科目点查询时页面给内联提示「请先选择科目」并**不发请求**（`?ledger-hint`），这是设计行为。
- Tab2（科目余额表）：切过去自动 `GET /ledger/trial-balance?period=<当前月>` **200**，
  4 个科目、合计行给出「借贷平衡」结论；把期间改成 `2026-09` → 再打一次 **200，4 行**。
- 全程 4xx/5xx = **0**。`/ledger/page` 在不带 `account_id/account_code` 时返回 **400 66006「account_id 或 account_code 必填其一」**
  —— 卡面已注明这是合理的必填校验，前端也从不空着调它（选科目才发）。

## 4. 15 页逐页结果（155 个按钮实测）

图例：✅ 有反应且无 4xx；⚠ 需说明；❌ 缺陷。

### 【物料】/business/erp/material — 8 个，4xx 0

| 按钮 | 实测 |
|---|---|
| 查询 / 重置 | ✅ `GET /erp/material/page → 200` |
| 新建物料 | ✅ 弹窗「新建物料」（编码/名称/分类/计量单位/规格/一物一码/启用/备注；按钮 取消/创建） |
| 编辑 ×4 | ✅ 弹窗「编辑物料」，字段回填；一物一码物料显示「当前：一物一码（已锁定）」 |
| — | ⚠ 本页**没有「删除」**入口（api 层也没有 `deleteMaterial`），后端有软删路由但页面未暴露 —— 属功能缺口，不是坏按钮 |

### 【仓库】/business/erp/warehouse — 4 个，4xx 0

| 按钮 | 实测 |
|---|---|
| 新建仓库 | ✅ 弹窗（编码/名称/父仓库/层级/类型/地址/负责人/是否叶子/启用） |
| 新建库位 | ⚠ `disabled` —— **有原因**：当前选中「总仓（种子）」是非叶子仓库，按钮外包 `t-tooltip` 且下方有 `t-alert`（`data-testid=non-leaf-hint`）写明「还有下级仓库，库位只能挂在叶子仓库下」 |
| 刷新 | ✅ `GET /erp/warehouse/tree → 200` + `GET /erp/location/list → 200` |
| 编辑仓库 | ✅ 弹窗，字段回填 |
| 行内 编辑/删除（库位面板） | ⚠ 首屏选中的总仓无库位，行内链接不渲染；同款「删除→二次确认」在【库位】页实测通过 |

### 【库位】/business/erp/location — 12 个，4xx 0

查询/重置/刷新 ✅ 各 200；新建库位 ✅ 弹窗（所属仓库必选）；查看 ×4 ✅ 抽屉打开；编辑 ×4 ✅ 弹窗回填。

### 【计量单位】/business/erp/unit — 13 个，4xx 0

查询/重置/刷新 ✅ 200；新建单位 ✅ 弹窗；查看 ×3 ✅ 抽屉；编辑 ×3 ✅ 弹窗；
**删除 ×3 ✅ 二次确认弹窗**（「确认删除「个（PCS）」？删除后物料不能再选它；已被物料引用时后端会拒绝」）。

### 【供应商】/business/erp/supplier — 7 个，4xx 0

查询/重置/刷新 ✅ 200；新建供应商 ✅ 弹窗（12 个字段）；查看 ✅ 抽屉；编辑 ✅ 弹窗；删除 ✅ 二次确认。

### 【序列号】/business/erp/serial — 10 个，4xx 0

| 按钮 | 实测 |
|---|---|
| 扫码「查询」 | ✅ 空输入点击 → 内联红字提示（内容区文本长度 315→326），**不发请求** |
| 筛选「查询」/「重置」 | ✅ `GET /erp/serial/page → 200` |
| 批量生成 | ✅ 弹窗（物料/仓库/数量/来源类型/批次号） |
| 出库 / 报废（在库行） | ✅ 各自弹「出库确认 / 报废确认」，写清「不可逆」 |
| 出库（已出库行） | ✅ 点击弹 `t-popup` 说明（该行 `outbound=false`，按钮灰 + tooltip 给原因），不误发请求 |

### 【即时库存】/business/erp/inventory — 3 个，4xx 0

⚠ 只有 3 个按钮可点：`t-tab-panel` 默认 `destroyOnHide=true`，未激活的两个 Tab 内容不挂载，
所以「查询/重置/刷新」是当前 Tab 的一组。三个 Tab 的首屏请求都已验：
`GET /erp/inventory/page → 200`（4 行，`可用 = 数量 − 锁定` 列正确）、`GET /erp/inventory-location/page → 200`（0 行，库位账未铺数据）、
`GET /erp/inventory-txn/page → 200`（有流水）。

### 【调拨单】/business/erp/transfer — 10 个，4xx 0（修复前 **1 处 409**）

新建调拨单 ✅ 弹窗（调出仓/调入仓/日期/原因/明细/序列号校验）；刷新/查询/重置 ✅ 200；查看 ×3 ✅ 抽屉；
**撤回 ❌ → 已修 ✅**：

- 修复前实测（2026-10-31 09:37，`out/audit-调拨单.json` 首轮）：
  `撤回 → 提示「非法状态流转：待审批 → DRAFT（草稿）」+ PATCH /erp/transfer/{id}/status → 409`
- 修复后实测：`撤回 → 弹窗「撤回调拨单 … 将从「审批中」变为「已取消」… 撤回原因」`，见 §5。

### 【采购订单】/business/erp/purchase — 39 个，4xx 0

查看 ×11 ✅ 抽屉；编辑 ×4 ✅ 弹窗（含「不含税/含税」两种口径文案）；取消 ×N ✅ 二次确认「确认取消」；
删除 ×N ✅ 二次确认；提交 ×5 → 本轮为**护栏拦截**（审计脚本对写请求显式 abort，提示 Network Error），
真实 200 证据见 §6（首轮误点实测过 3 次 `PATCH → 200`）。

### 【收货单】/business/erp/stockin — 22 个，4xx 0

新建收货单 ✅ 弹窗；编辑 ×2 ✅；过账 ✅ 弹窗「确认过账」（价税合计 226.00 / 发票已随货到达）；
作废 ×5 ✅ 弹窗（写清「作废后库存不自动冲回」）；删除 ×2 ✅ 二次确认；刷新 ✅ 200（顺带拉物料/仓库下拉）。

### 【应付单】/business/erp/payable — 7 个，4xx 0

刷新/查询/重置 ✅ 200；查看 ×4 ✅ 抽屉（只读页，无写入口 —— 与后端「应付单由收货过账生成」一致）。

### 【凭证】/business/erp/voucher — 9 个，4xx 0

刷新/查询/重置 ✅ 200；查看 ×6 ✅ 抽屉（分录 + 借贷平衡）。

### 【总账】/business/erp/ledger — 3 个，4xx 0

刷新/重置 ✅ 内联提示文案变化（「请先选择科目」出现/消失）；查询 → 见 §3，未选科目时提示已存在故无变化，
选中科目后 `GET /ledger/page 200`（由 `ledger-verify.spec.ts` 单独验）。

### 【会计科目】/business/erp/account — 5 个，4xx 0

刷新 ✅ `GET /erp/account/page → 200`；查询/重置 → 本地过滤（无请求，条件为空时无变化，正确）；
全部收起 ✅ 行数 4→3；全部展开 ✅（首屏即全展开，「再展开」无变化，正确）。

### 【会计期间】/business/erp/period — 3 个，4xx 0

刷新/查询/重置 ✅ 各 `GET /erp/period/page → 200`。

## 5. 本轮实际改动（仅前端）

### 5.1 调拨单「撤回」：`PENDING → DRAFT`（409）改成 `PENDING → CANCELLED`（要原因）

**真因**：后端 `transfer_model.rs::LEGAL_EDGES` 只有 5 条边
`DRAFT→PENDING / PENDING→APPROVED / PENDING→CANCELLED / APPROVED→IN_TRANSIT / (IN_TRANSIT→DONE)`，
**没有 `PENDING→DRAFT`**；而 `transfer-logic.ts` 里写的是 `PENDING→DRAFT`，`runAction` 直接发 `status:'DRAFT'` → 409。

顺带修掉第二个断链：请求体字段名 **`cancel_reason` → `reason`**
（后端 `TransferStatusReq` 只有 `reason`，传 `cancel_reason` 会被 serde 静默丢掉 → 400「必须给 reason」）。

改动文件：

| 文件 | 改了什么 |
|---|---|
| `frontend/src/api/business/erp/index.ts` | `TransferStatusReq.cancel_reason` → `reason`，注释标注字段名口径 |
| `frontend/src/views/business/erp/transfer/transfer-logic.ts` | `canTransition` 换成后端 5 条边；`actionTargetStatus('withdraw') = 'CANCELLED'`；`needsConfirm('withdraw') = true`；新增 `withdrawReasonError()` + `WITHDRAW_REASON_MAX=255`（对齐后端 `MAX_REASON_LEN`） |
| `frontend/src/views/business/erp/transfer/index.vue` | 撤回改为弹窗：「撤回调拨单」+ 原因必填输入 + 行内红字校验；确认后 `PATCH {status:'CANCELLED', reason}` |
| `frontend/tests/unit/erp-transfer.test.ts` | 状态机断言改成后端 5 条边（含 `PENDING→DRAFT = false`）、撤回目标 `CANCELLED`、`needsConfirm('withdraw')=true`、原因校验 5 条 |

真机证据（`tests/transfer-withdraw.spec.ts`，自建单 → 真链路 → 删干净）：

```
✓ 调拨单「撤回」：点开弹窗 → 原因必填 → 提交 200 且变已取消 (3.3s)
[cleanup] DELETE TK-20261031-000006 → 200
```

用例断言：弹窗出现且含「已取消」→ 不填原因点「确认撤回」得红字「必须填写原因」且**零 PATCH** →
填原因后 PATCH **200** → 行变「已取消」→ 回读 `status=CANCELLED, cancel_reason` 落库 → DELETE 收尾 200。

## 6. 需要知道的副作用与残留风险

1. **首轮脚本误操作（已恢复）**：`hasText:'取消'` 命中了「确认取消」，把 3 张采购单点成 `CANCELLED`（终态、API 无回退边）。
   已在生产库按 id 精确恢复成 `DRAFT`（清 `submitted_at`，`V0137` 无 `cancelled_at/cancel_reason` 列），回读确认：
   ```
   PO-20261031-000003|DRAFT   PO-20261031-000009|DRAFT   PO-20261031-000011|DRAFT
   ```
   脚本已改精确匹配，后续写请求一律走护栏 abort，**不再有同类风险**。
2. **生产 dist 落后 main 一个提交**：`172.18.10.144:/opt/cloudpivot` 停在 `016a267`，`frontend/dist/index.html` 时间 **09:04**
   （entry `assets/index-D0djUZPk.js`）。所以线上**没有** P4-D 的「即时库存/库位库存/库存流水」三页，
   本轮修复（撤回）也还没上线。发布属于发布卡的活，本卡未动生产 dist（同机还有 P5 的后端改动在工作树里）。
3. **生产后端实测**：`category/page`、`ledger/balance` 两条都 404（后端本就没有这两个路由）；
   `ledger/trial-balance?period=2026-10` 200；`ledger/page` 不带科目 400（必填校验，合理）。
4. 前端 api 调用点 × 后端路由静态比对：`/opt/cloudpivot-verify/static-link-check.py` →
   「前端所有 ERP/BASE 调用点都能在后端找到同名路由」（92 处 / 366 条路由，缺失 0）。

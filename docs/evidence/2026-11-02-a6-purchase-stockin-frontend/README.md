# A6 采购单 / 入库单前端配合 · 真机验收证据（2026-11-02）

卡：`t_60b43879`【A6】采购单/入库单前端配合（采购类型 + 合同选择器 + 供应商只读）

## 一、门禁数字（本机实跑）

| 项 | 命令 | 结果 |
|---|---|---|
| 类型检查 | `npx vue-tsc --noEmit` | **0 错误** |
| 构建 | `npx vite build --outDir <scratch>/a6/dist --emptyOutDir` | **成功**（产物直接用于下面的真机验收，**没有**写 `/opt/cloudpivot/frontend/dist`） |
| 单测（全量） | `pnpm test:unit` | **487 passed / 0 failed**（基线 473，本卡新增 14） |
| 单测（本卡） | `tests/unit/erp-a6-purchase-contract.test.ts` | **14/14 pass**（卡面要求 ≥10） |

> 构建与验收都在仓库外的一份 scratch 目录里跑：`<scratch>/a6/dist` 由 `serve.mjs` 静态服务 + `/api` 反代到隔离后端，
> **未 replace 生产 `frontend/dist`、未碰 144**。

## 二、前置验证（卡面要求：任何一条不通就停）

用真实 token 打三条接口（脚本 `prereq.sh`）：

| 检查 | 结果 |
|---|---|
| `GET /api/business/erp/contract/page` | **200**，能拿到合同（原库只有 TERMINATED / DRAFT，故新建一张并走 `DRAFT→SIGNED→ACTIVE`，`HT-20261102-000023`） |
| `POST /api/business/erp/purchase-order`（CONTRACT 型，带 `contract_id`，**不带 `supplier_id`**） | **200**，返回 `contract_no_snapshot = HT-20261102-000023` + 派生出的 `supplier_id` / `supplier_name` |
| `POST /api/business/erp/stock-in`（PURCHASE 型，**不带 `supplier_id`**） | **200**，返回派生出的 `supplier_id = d1370005-…`、`source_order_no = PO-…`、`is_sourceless=false` |

## 三、真机环境

```
后端：/opt/cloudpivot/backend/target/debug/cloudpivot（含 A3/A4 committed 代码）
      NIK_PROFILE=test NIK_SERVER_LISTEN=127.0.0.1:18086
      NIK_DATABASE_NAME=cloudpivot_a6_iso（从 cloudpivot_a5 克隆，schema_migrations 最大 V0200）
      CLOUDPIVOT_STORAGE_MASTER_KEY=…（profile=test 启动守卫要求）
前端：<scratch>/a6/dist（本卡代码构建）→ serve.mjs 静态服务 + /api 反代 → http://127.0.0.1:5216
驱动：Playwright（chromium headless，1600×1000），verify.mjs 49 条断言
```

## 四、全链路（verify.mjs，**49/49 PASS**，原文见 `verify-output.txt` / `checks.json`）

| # | 卡面 | 实跑断言 | 结果 |
|---|---|---|---|
| 15 | 新建合同采购单 | 默认「合同采购」+ 合同选择器在 + 供应商只读（`readonly` 属性在）+ 说明文案 + 选中合同后供应商自动带出乙方名 + 合同号可见 + 保存 200 + 服务端回填 `contract_no_snapshot` + `purchase_type=CONTRACT` | PASS ×10 |
| 16 | 列表「合同采购」+ 合同号 | 类型徽标 = 合同采购；合同号列 = 快照合同号（`HT-20261102-000023`） | PASS ×2 |
| 17 | 详情底部还有审批过程 | 采购类型=合同采购；审批过程区块三选一存在（草稿单走「未发起审批」空态）；提交审批后顶部「审批单 FLW-xxx」链接出现（count=1） | PASS ×3 |
| 18 | 点合同号 → 合同详情 | 跳 `/business/erp/contract?contract_id=…`，抽屉打开且是同一张合同 | PASS |
| 19 | 生成收货单 → 供应商灰、显示采购单号 | 供应商只读 + 「供应商来自采购单，不可修改」+ 采购单号 `PO-…` + 供应商值来自采购单 + 仓库随单带出且只读 | PASS ×6 |
| 20 | 保存收货单 → 过账 → 三账 | 保存 200（前端没传 `supplier_id`，后端派生）+ 过账 200 → `POSTED` + `voucher_id`/`payable_id` 均非空 + 凭证借贷平衡（406.80/406.80，3 行）+ 应付金额 = 收货单价税合计（406.80）+ 库存流水 IN×1 + 即时库存 3→6 | PASS ×9 |
| 21 | 新建现货采购单（手填供应商） | 切现货 → 合同选择器消失（count=0）+ 供应商回到可编辑下拉 + 保存 200 + `purchase_type=SPOT` 且无合同号 + 手选供应商生效 | PASS ×5 |
| 22 | 非 PURCHASE 型入库单 → 无源标记 | 未选采购单时供应商显示「请先选择采购单」；切「无源单据」→ 出现标记 + 供应商转手选；不填备注被前端拦住（文案点明要写来源）；填了 → 保存 200 且 `is_sourceless=true`、无 `source_order_id`；列表打标记；详情抽屉显示标记 | PASS ×8 |
| 23 | 控制台 / 网络 | 页面级 JS 异常 **0**；采购 / 收货 / 合同自身接口 4xx-5xx **0** | PASS ×2 |
| 23c | 既有噪声 | 控制台只有全局 SSE `/api/business/notice/stream?token=` 的 401（`alun 0.4.2` 未实现查询令牌白名单，A5 已单独取证），与本卡无关 | PASS ×2 |

## 五、回归（regression.sh，**14/14 PASS**，原文见 `regression-output.txt`）

现货采购不吃合同这条老链路逐环跑通：

1. 现货下单（`SPOT`，供应商手填）✔
2. 提交审批 → `APPROVED` ✔ 再收货（不带 `supplier_id`，后端派生）✔，来源单号正确 ✔
3. 过账 → `POSTED` ✔，生成凭证 ✔ / 应付 ✔
4. 已过账单作废入口可用 ✔（单账号环境被「复核人 ≠ 发起人」内控挡下 `65019`，属既有业务规则，不是本次改动）
5. 无源草稿直接作废 → `CANCELLED` ✔
6. 采购单作废 → `CANCELLED` ✔
7. `SPOT` 带 `contract_id` → **400** ✔（DTO 守卫仍在）
8. `CONTRACT` 缺 `contract_id` → **400** ✔

## 六、截图（9 张，`docs/evidence/2026-11-02-a6-purchase-stockin-frontend/`）

| 文件 | 内容 |
|---|---|
| `01-purchase-form-contract.png` | 采购单表单（合同型）：采购类型单选 + 合同下拉 + 合同号 + 供应商只读，明细 3×120 算出 406.80 |
| `02-purchase-list.png` | 采购列表：新增「采购类型」「合同号」两列 |
| `03-purchase-detail.png` | 采购详情抽屉：采购类型 / 合同号（可点）+ 明细 + 底部审批过程区块 |
| `04-stockin-from-po.png` | 收货页（从采购单生成）：供应商 + 仓库双只读且灰、采购单号可见、待收数量上限 |
| `05-purchase-form-spot.png` | 现货采购表单：合同选择器消失、供应商可手选 |
| `06-stockin-sourceless.png` | 无源单据模式：橙色「无源单据」标记 + 供应商可手选 + 备注必填拦截 |
| `07-contract-detail-from-po.png` | 从采购详情点合同号 → 合同详情抽屉 |
| `08-stockin-posted.png` | 过账后的收货单 |
| `09-stockin-sourceless-detail.png` | 无源单据详情：单据来源 = 无源单据标记 |

## 七、本卡与卡面的口径出入（都按「已落库代码」实现）

1. **合同可选状态**：卡面写「只列 `status = ACTIVE`」。后端 `resolve_contract` 实际接受
   `contract_model::USABLE_FOR_ORDER`（`SIGNED` / `ACTIVE`），前端跟后端走（否则会出现「可用却选不出」）。
   单测里逐字读 Rust 常量比对。
2. **合同下拉「剩余可用金额」**：`GET /erp/contract/page` 没有已下单额 / 剩余额字段，
   只能显示**合同总额**并明确标注「合同总额」，不冒充剩余额度（A5 已报告该接口无聚合）。
3. **无源单据的供应商**：卡面说「供应商字段隐藏或置灰」，但 A4 后端要求无源单**自己指定供应商**
   （`无源入库单必须自己指定供应商`）。所以无源模式下供应商=**可编辑手选**，并给出「需手工指定 + 备注写来源」的说明；
   「无源单据」标记照卡面显示。
4. **入库单没有 `in_type` 选择器**（本仓收货单从来只有 PURCHASE 语义），所以「非 PURCHASE 型」在本仓的
   真实含义就是 A4 的 `is_sourceless = true`；界面按此实现并加了显式的「单据来源」单选。
5. **列表「采购单号」列**：`source_order_no` 列本来就存在（D4 就加了），本卡只在无源行补了标记，未重复造列。

## 八、给后续卡的报告（没顺手改）

- **入库单「仓库」原来可手填**：带源单时后端要求 `warehouse_id` 与采购单一致，否则 400；
  本卡已把带源单时的仓库置灰并带出（卡面 §二.2 也要求「选中后自动带出仓库」），**这条已顺手做掉但不改后端**。
- **税率**仍是逐行手填（明细行税率来自采购明细 `tax_rate` 带出，从采购单生成时已自动带，手工建单时仍手填）。
- 采购详情顶部「审批单 FLW-xxx」链接与新增的「合同号」链接**并存**（两个独立元素、两个 testid），单测 `⑭` 钉死。
- 详情底部审批过程区块（P1-FIX-3 的「挪到最底部」改动）此前在 worktree 里**未提交**，
  本卡验收 17 依赖它，因此一并在本提交里带上了（见提交信息 body）。

## 九、本卡新增/改动的文件

```
frontend/src/api/business/erp/index.ts                     # PurchaseType / 合同快照字段 / stock-in is_sourceless
frontend/src/views/business/erp/purchase/purchase-logic.ts # 采购类型 / 合同选择 / 校验 / 请求体
frontend/src/views/business/erp/purchase/index.vue         # 表单三件套 + 列表两列 + 详情两项（+ P1-FIX-3 底部审批块）
frontend/src/views/business/erp/stockin/stockin-logic.ts   # 来源方式 / 供应商只读 / 采购单选择 / 校验 / 请求体
frontend/src/views/business/erp/stockin/index.vue          # 单据来源单选 + 采购单选择器 + 供应商只读 + 无源标记
frontend/src/views/business/erp/contract/index.vue         # 只加 ?contract_id= 深链（跨页跳转必需，未动 A5 其它行为）
frontend/tests/unit/erp-a6-purchase-contract.test.ts       # 本卡 14 条单测
frontend/tests/unit/d4-purchase-to-stockin.test.ts         # 采样数据补 purchase_order_id（A4 后「有源必带源单」）
frontend/tests/unit/erp-purchase-stockin.test.ts           # 校验用例显式走 SPOT（默认类型已变 CONTRACT）
```

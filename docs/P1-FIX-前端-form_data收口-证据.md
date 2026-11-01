# P1-FIX 前端收口证据：审批详情 / 待办卡片改用后端 `form_data` 快照

> 卡片：`t_b006af57`（前端·P1-FIX 收口）
> 前置：后端 `t_27385a1c` 已交付并部署 144（`dbc99c3` 功能 + `9d8d63d` 证据，零迁移）
> 验证方式：**隔离实例**（main 代码 + 干净库 + 真审批链）+ 真浏览器 DOM + 真 HTTP；生产只做只读核对与 dist 发布
> 日期：2026-11-02

## 一、四件活落地情况

| # | 卡面要求 | 落地 | 位置 |
|---|---|---|---|
| 1 | 补 5 个名称键标签 | `supplier_name` 供应商 / `warehouse_name` 收货仓库 / `from_warehouse_name` 调出仓库 / `to_warehouse_name` 调入仓库 / `unit_name` 单位 | `form-field-labels.ts::FORM_FIELD_LABELS` |
| 2 | 同名 `_name` 在场就藏 `_id` | 新增 `NAME_OVER_ID_KEY`（4 对），**值非空才藏**；嵌套形状（`form.supplier_id`）也认同一父级 | `form-field-labels.ts::isRedundantId` / `resolveSiblingValue` |
| 3 | 摘要白名单补名称键 | `SUMMARY_PRIORITY` 名称键排在对应 `_id` **前面**；摘要里名字在场时 `_id` 也不再重复出现 | `form-field-labels.ts::formSummary` |
| 4a | 删 `fillTodoFormData` | 删除（连同 `SUMMARY_FETCH_LIMIT=20`、`getFlowInstance` 导入）；待办 / 已办直接读 `row.form_data` | `task/index.vue::taskToCard` |
| 4b | 明细改读 `form_data.items` | `flow-business-items.ts` 重写为**纯函数** `readItemSnapshot`，不再 `import fetchPurchaseOrderItems`；详情页明细只来自快照 | `flow-business-items.ts` / `instance/detail.vue` |

配套：`FlowTaskItem` 新增 `form_data: Record<string, unknown>`（后端 `handler::task_res` 用 `unwrap_or_default` 兜底，`{}` = 无表单，绝不 `null`）。

## 二、真机 DOM 证据（隔离实例：main 代码 + 干净库 + 真审批链）

环境：`cloudpivot_p1fixfe`（空库 + 全量迁移 + 种子）、main 二进制（含 `dbc99c3`）、`vite dev`（源码即产物）、
审批人账号 `p1fix_approver` **只挂 `DEPT_LEADER`**：`business:erp:*` 授权数 = **0**，对
`/business/erp/{supplier,warehouse,material}/page` 实测 **403**。

### 1) 审批详情（采购 `PO-20261102-000001`，`form_data` 含 `supplier_name` / `warehouse_name` / `items`）

```text
采购单号    PO-20261102-000001
供应商      P1FIX 演示供应商
收货仓库    P1FIX 演示仓库
金额口径    不含税
价款        ¥8,000.00
价税合计    ¥9,040.00
数量合计    100
明细（1 行）
行号 物料编码 物料名称 数量 单位 单价 税率(%) 金额
1    P1FIX-M-1  P1FIX 演示物料  100  箱  ¥80.00  13  ¥8,000.00
```

- 整页 `(ID: …)` 出现次数 = **0**（改前：`供应商 (ID: 01a19810…)`、`收货仓库 (ID: …)` 两行 + `制单人 (ID: …)`）；
- 表头全中文，含「单位」；数值格式仍是 `100` / `¥80.00` / `13` / `¥8,000.00`；
- 明细表数据来自 `form_data.items`（页面**没有**发 `/business/erp/purchase-order-item/page`）。

### 1b) 调拨审批（`TK-20261102-000001`）

```text
调拨单号    TK-20261102-000001
调出仓库    P1FIX 演示仓库
调入仓库    P1FIX 二号仓
数量合计    5
明细（1 行）… 数量 5 单位 箱 …
```

`(ID: …)` 出现次数 = **0**（`from_warehouse_id` / `to_warehouse_id` 已让位）。

### 2) 审批中心待办卡片摘要

```text
供应商：P1FIX 演示供应商 · 收货仓库：P1FIX 演示仓库 · 价税合计：¥9,040.00
```

卡面验收要的两段（`供应商：P1FIX 演示供应商`、`价税合计：¥9,040.00`）都在，**不再有 `(ID: …)`**。
（卡面样例只写了这两段，实际按第 3 条要求也把「收货仓库」放进了白名单，所以摘要共 3 段 —— 与改前的
`供应商：(ID: …) · 收货仓库：(ID: …) · 价税合计：¥9,040.00` 同结构，只是 ID 换成了名字。）

### 3) 「打开审批中心」不再逐单查详情

```
performance.getEntriesByType('resource').filter(e => e.name.includes('/business/flow/instance/')).length
→ 0     （1 张卡片；2 张卡片同样 0）
```

改前：每张卡片 1 次 `GET /business/flow/instance/{id}`（上限 20 单）。现在摘要数据随
`GET /business/flow/task/todo` 一次带回（后端 `page_my_tasks` 的 SQL 直接 JOIN 出 `form_data`）。

### 4) 老实例降级（`form_data` 无 `items` / `*_name`）

把 `PO-20261102-000002` 的迭代按老形状处理（`form_data - items - supplier_name - warehouse_name`）后：

```text
采购单号    PO-20261102-000002
供应商      (ID: 01a19830…)
收货仓库    (ID: 01a19830…)
金额口径    不含税
价款        ¥30.00
价税合计    ¥33.90
数量合计    3
明细：—（该审批单未冻结明细）        <- 一行灰字，**不是空表格**
```

- 页面不报错、字段区照旧、其余内容正常（`hasFields: true`，无错误提示）；
- 老实例的待办卡片摘要仍走 ID 兜底（`供应商：(ID: …) · 收货仓库：(ID: …) · 价税合计：¥33.90`），不空白。

判定口径（`readItemSnapshot`）：

| `form_data.items` | `legacy` | 页面 |
|---|---|---|
| 对象数组（非空） | false | 渲染明细表（顺序 = 后端 `line_no` 升序，前端不重排） |
| 空数组 | false | 按「无明细」处理，不渲染表格、不提示老实例 |
| 缺失（有明细的业务类型） | true | 显示 `明细：—（该审批单未冻结明细）` |
| 脏数据（元素不是对象） | true | 同上（宁可显示 `—`，也不渲染半张表） |

「有明细的业务类型」白名单 = `PO_APPROVE` / `STOCK_IN_POST` / `STOCK_TRANSFER`（真值取自后端
`purchase_bridge::BIZ_TYPE`、`stockin_model::POST_BIZ_TYPE`、`transfer_model::published_transfer_flow`）。

## 三、门禁

| 门禁 | 结果 |
|---|---|
| `pnpm typecheck`（`vue-tsc --noEmit`） | 0 错 |
| `pnpm test:unit`（`tsx --test`） | **428 passed / 0 failed**（新增 ⑰ / ⑰b / ⑰c / ⑱ / ⑲ / ⑳ / ⑳b / ㉑ 共 8 条） |
| `pnpm check:palettes` | 22 套 × 明暗「全部通过 ✓」 |
| `pnpm build`（含 typecheck） | 成功；`dist/assets/detail-*.js` 含「该审批单未冻结明细」 |
| 明细接口不再被引 | `grep -c "purchase-order-item" dist/assets/detail-*.js` → **0** |

新增单测覆盖（卡面要求的五类都在）：

| 用例 | 钉住什么 |
|---|---|
| ⑰ | 5 个名称键的中文标签；`supplier_id`/`warehouse_id` 让位；整块字段区无 `(ID: …)`；「供应商」只出现一次 |
| ⑰b | 调拨 `from_/to_warehouse_id` 让位，只剩调出/调入仓库中文名 |
| ⑰c | 制单人译名拿不到 → 整行不显示；拿到 → 照常显示姓名；**别的**未知 uuid 字段仍兜底（不丢信息） |
| ⑱ | 名字为空 / 缺键 / `null` → `*_id` 照常显示；嵌套 `form.supplier_id` 认同一父级名字 |
| ⑲ | 新实例摘要 = 3 段中文名、无 `(ID: …)`；调拨摘要「调出仓库 · 调入仓库」；老实例摘要口径不变 |
| ⑳ | 快照明细 8 键 → 表头全中文（含「单位」）、数值 `100`/`¥80.00`/`13`/`¥8,000.00` |
| ⑳b | `items: []` / 无 items / `null` → 不渲染表格、不报错 |
| ㉑ | `readItemSnapshot` 四条边界（含脏数据降级） |

## 四、口径变化说明（两处，都不是卡面写的）

1. **制单人（`create_by`）**：卡面没说，但真机 DOM 上它是最后一处 `(ID: …)` —— 审批人没有
   `system:user:list`，译名必然 403。规则改成「译名拿到就显示姓名，拿不到整行不显示」
   （`AUDIT_OWNER_KEYS`）；那个名字本来就不在 `form_data` 里，详情页头部已显示「发起人 姓名」，
   所以不构成丢信息。**未知 uuid 字段仍按硬红线 3 兜底**（单测 ⑰c 钉住）。
2. **摘要 3 段而非 2 段**：见 §二.2，是多加了 `warehouse_name` 的必然结果，与卡面「放在对应 `_id` 前面」
   的要求一致。

## 五、生产侧

- 生产库（144）当前 150 条审批实例**全部**是 P1-FIX 后端上线前的老数据
  （`form_data ? 'supplier_name'` = 0 行、`? 'items'` = 0 行），因此生产上暂时**没有**能验「中文名」的真实新单；
  卡面第 5 条的「真实审批单复核」在第 6 节给出口径。
- 本卡只读核对生产（不写测试数据），dist 发布见第 6 节。

## 六、复核清单（下一张新审批单提交后照此核对）

1. 用只有审批岗位的角色打开该单详情 → 供应商 / 收货仓库（调拨看调出/调入）只显示中文名，页面无 `(ID: …)`；
2. 明细表头全中文且含「单位」，数值形如 `100` / `¥80.00` / `13`；
3. 打开审批中心 → `performance.getEntriesByType('resource')` 过滤 `/business/flow/instance/` = 0；
4. 老单打开后显示 `明细：—（该审批单未冻结明细）`，其余字段照旧。

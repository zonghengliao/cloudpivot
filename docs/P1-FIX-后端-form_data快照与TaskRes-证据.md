# P1-FIX 后端配套：审批 `form_data` 冻结显示名/明细 + `TaskRes` 下发 `form_data` · 实测证据

> 卡：`t_27385a1c`（后端·P1-FIX 配套）｜前置：`t_5326e4b4`（前端呈现层 `ac41c22`）
> 日期：2026-11-02｜执行：backend-developer

## 0. 一句话结论

审批 `form_data` 现在**自带人看的显示名与明细**（`supplier_name` / `warehouse_name` /
`from_warehouse_name` / `to_warehouse_name` / `items`），审批岗位账号（`business:erp:*` 权限 0 条）
不查任何主数据也能看懂单据；`GET /business/flow/task/todo` 的每一行自带同一份 `form_data`，
待办卡片摘要不再需要逐单补详情。

**但**：呈现层还剩两件前端活（本卡按卡面「别在本卡里改前端」留给后续前端卡），见 §7：
`*_id` 与 `*_name` 会同时显示（ID 那行仍降级成 `(ID: …)`、名称那行的标签是原始键名
`supplier_name`），以及 `formSummary` 的白名单只认 `supplier_id`。

## 1. 环境与方法

| 项 | 值 |
|---|---|
| 代码 | `/opt/cloudpivot`（`ac41c22` + 本卡增量），工具链 cargo 1.94.1 |
| 数据库 | 本机 PG，独立库 `cloudpivot_p1fixb`（空库 → 启动即应用 **134 个迁移** + 种子，零新增迁移） |
| 后端 | 本卡代码编出的 `target/debug/cloudpivot`，监听 `127.0.0.1:18299`，`NIK_PROFILE=test` |
| 前端 | `vite` dev（`BACKEND_ORIGIN=http://127.0.0.1:18299`，`127.0.0.1:5199`），真浏览器 DOM 断言 |
| 脚本 | `/tmp/p1fixv/{start.sh,verify.sh}`（真 HTTP）、`/tmp/p1fixb/*`（单测） |

真机验收用的是**真库 + 真 HTTP + 真浏览器**：不是构造返回值，也不是截图臆测。

## 2. 代码改动

| 文件 | 改动 |
|---|---|
| `backend/src/business/erp/form_snapshot.rs`（新增，~420 行含单测） | 显示名 / 明细快照查询层：`supplier_name` / `warehouse_name` / `purchase_items` / `stock_in_items` / `transfer_items`；`ITEM_KEYS` 是与前端 `FORM_FIELD_LABELS` 对齐的键契约 |
| `purchase_handler.rs` | `purchase_form_data` 变 async，补 `supplier_name` / `warehouse_name` / `items` |
| `stockin_handler.rs` | `stock_in_form_data` 变 async，补 `supplier_name` / `warehouse_name` / `items` |
| `payable_payment.rs` | 抽出 `payment_form_data`，补 `supplier_name` |
| `transfer_handler.rs` | 抽出 `transfer_form_data`，补 `from_warehouse_name` / `to_warehouse_name` / `items` |
| `flow/dto.rs` | `TaskRes` 新增 `form_data: Value`（`{}` = 无表单） |
| `flow/model.rs` | `MyTaskRow` 新增 `form_data`；`page_my_tasks` 的 SELECT 直接带出 `i.form_data::text`（同一页 SQL，零 N+1） |
| `flow/handler.rs` | `TaskCtx.form_data`；`my_task_res` / 详情时间线复用实例行；抄送路径传 `None` |

设计要点（都写进了代码注释）：

1. **查不到就不写这个键**（`None` ≠ 空串）：前端按 `*_id` 兜底，不会出现「供应商：」后面空空的假象。
2. **查询失败不阻断提交**：`tracing::warn!` 降级，审批该发还得发得出去。
3. **显示名查询刻意不过滤 `deleted_at`**：这是「显示名快照」，主数据被软删后历史审批仍应显示当时的名字。
4. **金额口径不变**：`amount` 仍是 JSON **数字**（条件边 `as_f64()` 比较），明细里的金额是十进制字符串（前端 `toNumber()` 认）。

## 3. 验收标准 1：提交审批后库里能看到显示名与明细

真实调用链（admin 建单 → 提交审批 → 查库）：

```
=== 6) 建采购单 8000 元（100 × 80，13%）并提交审批（制单人 admin ≠ 审批人 p1fix_approver） ===
po_id=01a19811-fa7e-7247-9c49-758431a2f7a1 po_no=PO-20261102-000028
{"code":0,"msg":"ok","status":"SUBMITTED","instance":"01a19811-fa8d-7533-a176-75bd3dfc2a92"}

=== 7) 验收标准 1：查库 select form_data from biz_flow_instance where biz_no=<采购单号> ===
{
  "items": [
    {
      "qty": "100.0000",
      "price": "80.000000",
      "amount": "8000.0000",
      "line_no": 1,
      "tax_rate": "13.0000",
      "unit_name": "箱",
      "material_code": "P1FIX-M-1",
      "material_name": "P1FIX 演示物料"
    }
  ],
  "amount": 9040.0,
  "order_id": "01a19811-fa7e-7247-9c49-758431a2f7a1",
  "order_no": "PO-20261102-000028",
  "create_by": "01a19806-ddd5-...",
  "total_qty": "100.0000",
  "price_mode": "EXCL",
  "supplier_id": "01a19810-71f7-74d8-beeb-63664a067c8f",
  "total_amount": "8000.0000",
  "warehouse_id": "01a19810-726a-77e6-a0d1-fd5a8305edbe",
  "supplier_name": "P1FIX 演示供应商",
  "total_with_tax": "9040.0000",
  "warehouse_name": "P1FIX 演示仓库"
}
-- 键清单 --
items, amount, order_id, order_no, create_by, total_qty, price_mode, supplier_id,
total_amount, warehouse_id, supplier_name, total_with_tax, warehouse_name
-- items 键 --
qty, price, amount, line_no, tax_rate, unit_name, material_code, material_name
-- form_data 字节数 --
661
```

`items` 的 8 个键与前端 `FORM_FIELD_LABELS` 逐字一致（前端按「存在的键」出列，所以多一个键就多一列、少一个键就少一列）。
整份 `form_data` 661 字节 —— 待办列表按页取（默认 20 行）的体积增量可忽略。

## 4. 验收标准 3：`TaskRes` 下发 `form_data`

同一个账号（`p1fix_approver`，只有 `DEPT_LEADER`）调 `GET /business/flow/task/todo`：

```
=== 8) 验收标准 3：GET /business/flow/task/todo（该账号）→ 每行自带 form_data ===
{"code":0,"total":2}
FLW-20261101-7FE924B0 | 采购订单审批（标准·金额分级） | 供应商=P1FIX 演示供应商 价税合计=9040.0000 明细0=P1FIX 演示物料
FLW-20261101-9BFB59D7 | 采购订单审批（标准·金额分级） | 供应商=P1FIX 演示供应商 价税合计=9040.0000 明细0=P1FIX 演示物料
带 supplier_name 的待办行数=2
```

也就是说：卡片摘要要的「供应商 / 价税合计 / 明细」在**列表那一次请求里就齐了**，
不再需要 `GET /business/flow/instance/{id}` 逐单补（原来还有 20 单上限）。

`GET /business/flow/instance/{id}` 仍然一致（同一份 form_data）：

```
=== 9) 审批详情接口 GET /business/flow/instance/{id}（该账号） ===
{"code":0,"status":"PENDING","form_keys":["amount","create_by","items","order_id","order_no","price_mode",
"supplier_id","supplier_name","total_amount","total_qty","total_with_tax","warehouse_id","warehouse_name"],"timeline":2}
supplier_name=P1FIX 演示供应商 warehouse_name=P1FIX 演示仓库 items=1行
```

## 5. 验收标准 2 的前提：「只有审批岗位角色」的账号确实没有 ERP 读权限

```
=== 4) 该账号的实际权限（erp_perms 必须是 0） ===
DEPT_LEADER|0|16          <- erp_perms=0，total=16（本库种子里含 business:flow:instance:read 等）

=== 10) 反证：该账号访问 ERP 主数据（应 403） ===
GET /business/erp/supplier/page -> HTTP 403
GET /business/erp/warehouse/page -> HTTP 403
GET /business/erp/material/page -> HTTP 403
```

生产库（144，只读查询）的真实情况比卡面写的更狠：**四个审批岗位角色在 `sys_role_menu` 里一条授权都没有**
（卡面的 `total = 1` 是 `LEFT JOIN` 造出来的那条 NULL 行，换 `JOIN` 就是 0 行）：

```
role_code          | 菜单授权数
GENERAL_MANAGER    | 0
WAREHOUSE_MANAGER  | 0
DEPT_LEADER        | 0
PURCHASE_MANAGER   | 0
```

生产里能打开审批中心的账号（`henry.liao` / `laoliu_test`）同时还挂着 `SUPER_ADMIN`，所以以前没人踩到这条。
本卡不改权限模型（方案 B 是产品/架构决策），只在**数据侧**（方案 A）解决 —— 一旦给审批岗位补上
`business:flow:instance:read` 这类最小权限，页面就能直接看懂，不需要再给 `business:erp:*`。

## 6. 真机 DOM：审批详情页（浏览器真文本，不是截图）

浏览器（`p1fix_approver` 登录）打开 `/flow/instance/01a19811-fa8d-…` 的 `document.body.innerText`：

```
审批内容
PO-20261102-000028 采购订单审批
采购单号      PO-20261102-000028
供应商        (ID: 01a19810…)
收货仓库      (ID: 01a19810…)
金额口径      不含税
价款          ¥8,000.00
价税合计      ¥9,040.00
数量合计      100
制单人        (ID: 01a19806…)
supplier_name      P1FIX 演示供应商
warehouse_name     P1FIX 演示仓库
明细（1 行）
行号  物料编码   物料名称        数量   单价    税率(%)  金额       unit_name
1     P1FIX-M-1  P1FIX 演示物料  100    ¥80.00  13       ¥8,000.00  箱
```

**后端这一半已经生效**：中文名与明细都在页面上，且该账号对 ERP 主数据全部 403（上面第 5 节）。

**前端这一半还没收口**（所以「供应商」那行仍是 `(ID: …)`、名称那行的标题是原始键名 `supplier_name`、
明细最后一列表头是 `unit_name`）—— 前端 `form-field-labels.ts` 的 `FORM_FIELD_LABELS` 里没有
`supplier_name` / `warehouse_name` / `unit_name` 这些键，也没有「同名 `*_name` 在场就藏 `*_id`」的规则。

## 7. 前端残留（后续前端卡要做的四件事，已开卡）

1. `FORM_FIELD_LABELS` 补：`supplier_name` 供应商 / `warehouse_name` 收货仓库 /
   `from_warehouse_name` 调出仓库 / `to_warehouse_name` 调入仓库 / `unit_name` 单位。
2. 「同名 `*_name` 在场就藏 `*_id`」：照 `INTERNAL_REF_NO_KEY`（`order_id` ↔ `order_no`）的现成写法扩一张表，
   藏掉的只是**重复**信息（名字还在），这样审批人才不会先看到一串 `(ID: …)`。
3. `SUMMARY_PRIORITY` 补 `supplier_name` / `from_warehouse_name` / `to_warehouse_name` / `warehouse_name`
   —— 现在白名单只认 `supplier_id`，摘要因此仍是「供应商：(ID: …)」。
4. 删 `task/index.vue` 的 `fillTodoFormData`（上限 20 单的补丁），明细改读 `form_data.items`，
   去掉对 `GET /business/erp/purchase-order-item/page` 的依赖（`flow-business-items.ts`）。

现状基线（真浏览器计数，dev 页面）：审批中心 1 张卡片 → 额外 1 次
`GET /business/flow/instance/{id}`（`performance.getEntriesByType('resource')` 过滤
`/business/flow/instance/`）；10 张卡片就是 10 次，旧实现还封顶 20 单。

## 8. 门禁

| 检查 | 命令 | 结果 |
|---|---|---|
| 格式 | `cargo fmt -p cloudpivot -- --check` | 通过 |
| 静态检查 | `cargo clippy -- -D warnings` | 通过（0 告警） |
| 编译 | `cargo check` / `cargo check --all-targets` | 通过 |
| 单测 | `ERP_DB_URL=… cargo test --lib -- --test-threads=1` | **1822 passed / 0 failed**（481s，真库） |

新增单测 7 条：

| 用例 | 钉住什么 |
|---|---|
| `form_snapshot::display_names_resolve_and_degrade_to_none` | 名称查得到给中文名、查不到给 `None`（不是空串） |
| `form_snapshot::purchase_items_freeze_every_display_key` | 采购明细 8 键齐全、按 `line_no` 升序、码/名/单位/数量/单价/金额/税率逐值 |
| `form_snapshot::empty_order_yields_empty_array` | 空单 → `[]`（不是 `null`） |
| `form_snapshot::stock_in_items_freeze_every_display_key` | 收货明细 8 键齐全（单位取 `unit_id`） |
| `form_snapshot::transfer_items_freeze_every_display_key` | 调拨明细：`price` = `unit_cost`、`tax_rate` 恒 `0`、单位取物料的计量单位 |
| `purchase_handler::p1fix_po_form_data_freezes_display_names_items_and_task_carries_it` | **端到端**：真审批链提交后库里 `form_data` 有名称/明细，且「我的待办」行自带同一份 |
| `purchase_handler::p1fix_po_form_data_omits_unknown_display_names` | 名称查不到时**不写该键**，其余字段（ids/金额/明细）照旧 |

另有 2 条改动的既有用例（`handler::tests::task_res_carries_d5_display_fields` /
`my_task_res_degrades_missing_initiator_name`）补了 `form_data` 断言（透传 / `{}`）。

## 9. 边界与未做

- **不改权限模型**：没有给四个审批岗位授 `business:erp:*`（方案 B 属产品/架构决策）。
- **不改前端**：按卡面第 5 条，前端的四件事留给后续前端卡。
- **只在采购链上做了端到端真机验证**：收货 / 调拨 / 付款三条链的 `form_data` 构造已改并各有单测覆盖
  快照查询（真库 SQL），但没有像采购那样跑完整「建单 → 提交审批 → 页面」链路（收货要过账、调拨要有库存、
  付款要有应付单，都是别的卡的领域）；下一次碰到这类单据提交时可以顺手核对。
- **生产历史实例**的 `form_data` 仍是老格式（无名称/明细）—— 本卡不回溯补数据，前端对老实例的兜底保持不变。

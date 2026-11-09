# ERP API 契约速查表（演练卡照这个写）

> **用途**：P0-01~06 六条链与后续演练卡里「卡面写 A、接口实际是 B」的坑，集中在这一页。
> 每条都附**真实出处**（路由宏 / DTO / 迁移 / DB 实测），改代码后请同步改本页。
>
> **读法**：卡面示意图里出现下面任一行，先用本页翻译成真实契约再写脚本。

## 0. 全局口径

| 项 | 口径 |
|---|---|
| 前缀 | 路由宏内写 `/business/erp/...`，`[router].prefix = /api` 注入 ⇒ **curl 必须写全 `/api/business/erp/...`**（漏 `/api` 会命中 SPA fallback 返回 HTML，报「响应体格式不符合」） |
| 鉴权 | `POST /api/system/auth/login` 拿 `data.access_token`，之后 `Authorization: Bearer <token>` |
| 权限码 | `{模块}:{资源}:{动作}`，如 `business:erp:stock-in:post`；`SUPER_ADMIN` 短路全部校验 |
| 响应 | `{"code":0,"msg":"ok","data":...}`；业务错误 `code != 0` + HTTP 4xx/5xx |
| 金额 | `NUMERIC(18,4)`，JSON 里按字符串传（`"1000.0000"`），别传浮点 |
| 单据状态 | 只能走状态机：`PATCH /{id}/status` 或专用入口；非法边一律 **409** 并点名可选去向 |

---

## 1. 销售发货：**没有** `/sales-order/{id}/ship`

```text
❌ 卡面：POST /api/business/erp/sales-order/{id}/ship      → 404（路由不存在）
❌ 卡面：POST /api/business/erp/shipment/{id}/confirm      → 404（没有 confirm 这个动作）
```

真实链路是**两张单据 + 状态机**：

```text
① POST  /api/business/erp/shipment           建发货单（DRAFT，取号 SH-yyyyMMdd-000001）
       body: {"so_id":"<销售订单 id>", "warehouse_id":"<仓库 id>", "items":[]}
       items 留空 = 从订单带出全部未发完的行（各行数量 = 该行未发量）
② PATCH /api/business/erp/shipment/{id}/status   body: {"target_status":"PENDING_POST"}  提交待过账
③ PATCH /api/business/erp/shipment/{id}/status   body: {"target_status":"POSTED"}        过账（动三本账）
```

- 合法边只有 4 条（`DRAFT→PENDING_POST`、`PENDING_POST→POSTED`、`DRAFT→CANCELLED`、`PENDING_POST→CANCELLED`，后者 `reason` 必填）：`backend/src/business/shared/state_machine.rs::SHIPMENT_EDGES`。
- 其余接口：`GET /shipment/page`、`GET /shipment/{id}`、`GET /shipment/shippable/{so_id}`（该订单**未发完的行**，前端「从订单带出」的唯一读数）。
- 出处：`backend/src/business/erp/shipment_handler.rs` 文件头接口清单 + `#[nik::post("/business/erp/shipment")]`。

## 2. 收款方式白名单= 4 个值，`BANK` → 400

```text
❌ 卡面：{"method":"BANK"}       → 400（不在白名单）
✅ 合法：BANK_TRANSFER / CASH / CHECK / OTHER
```

- 落库约束 `ck_biz_receipt_method`：`method IS NULL OR method IN ('BANK_TRANSFER','CASH','CHECK','OTHER')`。
- 代码里的白名单常量：`backend/src/business/erp/receipt_refund.rs::METHODS`（同一份 4 值，退款单 `ck_biz_receipt_refund_method` 逐字一致）。
- 登记收款：`POST /api/business/erp/receipt`，body 关键字段 `customer_id / amount / method / receipt_date / bank_account / currency_code`；核销走 `POST /receipt/{id}/allocate`，状态迁移走 `PATCH /receipt/{id}/status`（`DRAFT→CONFIRMED→POSTED`）。

## 3. 付款：`POST /payable/{id}/pay` 返回 **202 PENDING_APPROVAL**，不是 200 直接生效

```text
POST /api/business/erp/payable/{id}/pay
  body: {"amount":"2000.0000", "pay_date":"2026-11-08", "pay_method":"BANK_TRANSFER"}
  → 202 + {"outcome":"PENDING_APPROVAL", "payment_no":"PM-…", "approval_instance_id":"…"}
    （付款单要审批后才记账，此刻账还没动）
```

- 有已发布的 `payable_pay` 流程 ⇒ **202**，响应带 `approval_instance_id` / `payment_no`（`outcome = PENDING_APPROVAL`），钱还没动；
- 没配流程 ⇒ **409 + `APPROVAL_FLOW_NOT_CONFIGURED`（fail-closed）**，只有「持 `business:erp:skip-approval` + `skip_reason` ≥10 字」才能显式跳过直付，理由进 `biz_status_log`；
- 想看这单能付多少：`GET /api/business/erp/payable/{id}/payment-info`（详情抽屉「发起付款」的唯一读数）。
- 出处：`backend/src/business/erp/payable_payment.rs::submit_payment` 文档注释（"卡面 ③：改成先发起审批，返回 202"）。

## 4. `received_qty` **不是表列**（已收数量是派生值）

```text
❌ 卡面：SELECT received_qty FROM biz_purchase_order_item      → 列不存在（V0217 已删列）
✅ 接口：GET /api/business/erp/purchase-order/{id}/pending-items   → 每行 pending_qty / received_qty
✅ 接口：GET /api/business/erp/purchase-order-item/page            → 同样带 received_qty
```

真库口径（唯一真相，`backend/src/business/erp/purchase_model.rs::POSTED_RECEIVED_SQL`）：

```sql
COALESCE((SELECT SUM(si.qty) FROM biz_stock_in s
            JOIN biz_stock_in_item si ON si.stock_in_id = s.id
           WHERE s.source_order_id = i.order_id
             AND si.source_item_id = i.id
             AND s.status = 'POSTED' AND s.deleted_at IS NULL), 0)
```

即「只算 **POSTED** 收货单、按**采购明细行**（`source_item_id`）匹配」。自己写 SQL 核对时照抄这段，别按 `material_id` 聚合（同一物料可以有多行）。

## 5. 预算：路径与报表入参

```text
❌ 卡面：GET /api/erp/budget                 → 404
✅ 真实：GET /api/business/erp/budget/page?fiscal_year=2026&dept_id=…&account_id=…

❌ 卡面：GET /api/business/erp/budget/analysis
✅ 真实：GET /api/business/erp/budget/report?fiscal_year=2026      （预算执行表：已用 / 可用 / 执行率）
```

- `budget/report` 只吃筛选参数（`fiscal_year` / `dept_id` / `account_id` / `change_type` / `keyword`），**没有分页**，返回数组；
- 其余入口：`POST /budget`（建 DRAFT + v1 + 12 月分解）、`PUT /budget/{id}`、`POST /budget/{id}/submit`（送 `budget_approve`）、`POST /budget/{id}/adjust`（新版本直接送审）、`GET /budget/{id}`（含占用记录 + 版本链）。
- 出处：`backend/src/business/erp/budget_handler.rs` 文件头接口清单 + `#[nik::get("/business/erp/budget/report")]`。

## 6. `biz_budget` **没有 `used_amount` 列**（已用是现算的）

```text
❌ 卡面：SELECT used_amount FROM biz_budget          → 列不存在
✅ 真库：已用 = biz_budget_usage 里该「部门 + 科目 + 年度」槽位的占用合计
✅ 接口：GET /api/business/erp/budget/report?fiscal_year=2026 → 直接给 used / available / 执行率
```

- `biz_budget` 实际列（实测 `information_schema.columns`）：`budget_no, fiscal_year, dept_id, dept_name, account_id, account_code, account_name, total_amount, control_mode, version_no, supersedes_id, change_type, change_reason, status, approval_instance_id, …`——**没有任何「已用 / 余额」列**。
- 占用流水在 `biz_budget_usage`；12 月分解在 `biz_budget_period`；科目映射在 `biz_budget_subject_map`。
- 直接查库做断言时：`select coalesce(sum(amount),0) from biz_budget_usage where dept_id=… and account_id=… and fiscal_year=…`（口径与 `budget_guard::usages_of_slot` 一致）。

## 7. 盘点：**没有** `/stock-take/{id}/confirm`，过账是 `PATCH /{id}/status`

```text
❌ 卡面：POST /api/business/erp/stock-take/{id}/confirm       → 404
✅ 真实：PATCH /api/business/erp/stock-take/{id}/status
         {"status":"POSTED"}          ← 过账（动三本账 + 盘点差异凭证）
         {"status":"PENDING_POST"}    ← 提交审批（DRAFT → 待过账，挂 stock_take_post 实例）
         {"status":"CANCELLED","reason":"…"}  ← 作废（已过账的作废要红冲）
```

- 「审批通过」**不改单据状态**（实例 `APPROVED` 后单据仍是 `PENDING_POST`），真正过账是上面那一句；未审批就过账 → **409**（审批结论闸）。
- 字段名 `status` 与 `target_status` 都收，两个都给时必须相等：`backend/src/business/erp/stocktake_dto.rs::StockTakeStatusReq`。
- 其余：`POST /stock-take`、`PUT /stock-take/{id}`、`DELETE /stock-take/{id}`（草稿）、`GET /stock-take/page`、`GET /stock-take/{id}`。
- 口径细节见 `docs/ERP-盘点审批过账口径.md`（FIX-8）。

## 8. 资产：年限 / 残值率来自**分类**，状态是 `IN_USE`，停启用走 `POST /asset/{id}/status`

```text
❌ 卡面：建卡时传 useful_life / residual_rate   → 字段不存在（`CreateAssetReq` 里压根没有），传了会被 serde 静默丢弃
✅ 真实：建卡只传 category_id，年限 / 残值率**只能**由分类带出（不可在卡片上覆盖）
        卡片落库时快照成 useful_life_months / salvage_rate
❌ 卡面：status = ACTIVE
✅ 真实：状态白名单 IN_USE / IDLE / DISPOSED / CANCELLED（新建 = IN_USE）
❌ 卡面：PUT /asset/{id} 改状态
✅ 真实：POST /asset/{id}/status  {"target_status":"IDLE","reason":"闲置待调配"}  ← 停用（status/IDLE）
        POST /asset/{id}/status  {"target_status":"IN_USE","reason":"重新投用"}  ← 启用
```

- 建卡必填（`backend/src/business/erp/asset_dto.rs::CreateAssetReq`）：`asset_name`、`category_id`、`use_dept_id`、`original_value`、`purchase_date`、`pay_mode`（`CREDIT` 赊购贷 2202 / `CASH` 现付贷 1002）；`buy_mode` = `DIRECT_BUY` / `FROM_STOCK`（收货单来源强制 `FROM_STOCK`）。建卡**同时生成购入凭证**。
- **列名**（实测 `biz_fixed_asset`）：`useful_life_months`（月数）/ `salvage_rate`（净残值率 0≤r<1）/ `residual_value` / `depreciable_base` / `monthly_depreciation` / `accumulated_depreciation` / `net_value` / `depreciation_months_done`——**没有** `useful_life`、`residual_rate` 这两个名字。
- 分类表 `biz_asset_category` 才是来源：`useful_life_months`（1..=600）+ `salvage_rate`（默认 0.05）+ 三个科目（1601 / 1602 / 5602.01）。
- 金额与分类**不可改**（`PUT /asset/{id}` 只放行名称 / 规格 / 地点 / 备注）；状态变更只走状态机（`POST /asset/{id}/status`，`target_status` 只收 `IDLE` / `IN_USE`，`reason` 必填、`change_date` 可空），停用 / 启用**不出凭证**、折旧照提。
- 处置走审批：`POST /asset/{id}/dispose`（`asset_disposal` 流程）、作废 `POST /asset/{id}/dispose/cancel`。

---

## 9. 合同：归档是 `COMPLETED`（**没有** `CLOSED`）；路径②「登记签订」会终止审批实例

```text
❌ 卡面：PATCH /api/business/erp/contract/{id}/status  {"target_status":"CLOSED"}   → 400 + 10001
✅ 真实：{"target_status":"COMPLETED"}（前端 contract-logic.ts 的 EDGE_LABELS 同口径）
```

- 合同是 **6 态**：`DRAFT / PENDING_APPROVAL / SIGNED / ACTIVE / COMPLETED / TERMINATED`
  （`ck_biz_contract_status`；`backend/src/business/shared/state_machine.rs::CONTRACT_EDGES`）。
  `CLOSED` 是**采购单 / 销售单**的状态，不是合同的 —— 两个域的字面量别混用。
- `COMPLETED` 的语义是**归档（不再接新单）**，不是「履约完成」：是否履行完要看详情的
  `open_order_count == 0`（`biz_contract` 没有「已履行」列）。
- **归档不硬拦在途单据**（设计决策 **E4**：`ACTIVE → COMPLETED` 一律放行 200），
  前端在确认弹窗上显示「仍有 N 张未关闭单据」（数据源 = 详情接口 `open_order_count`）。
- **路径②**（审批中心之外的登记签订，`PENDING_APPROVAL → SIGNED`）：接口返回 200 后
  **在跑的审批实例已终止** —— 实例 `CANCELLED`、`PENDING` 待办 `CANCELLED`、
  节点收口，状态日志 `reason` 写明「审批中心外登记签订：审批实例已终止」；
  合同上的 `approval_instance_id` 保留作审计。**不要再用「审批中心」去推进这张单**
  （待办已经没了），要重审只能走 `TERMINATED` 后新签。
- 乙方类型只有 `CUSTOMER` / `SUPPLIER`：`PURCHASE`/`SERVICE`/`LEASE` → `SUPPLIER`、
  `SALES` → `CUSTOMER`。**服务商 / 出租方都建在供应商档**（决策 D1，见
  `docs/合同类型开放口径-决策.md` §二）；传 `CUSTOMER` 给服务/租赁类一律 400（`66021`）。
- 出处：`backend/src/business/erp/contract_handler.rs::contract_change_status`、
  `contract_bridge.rs::terminate_for_out_of_band_sign`、`contract_dto.rs::ContractStatusReq`。

---

## 10. 序列号批量生成：`unit_cost` **必填**（不带 → 400 + 10001）

```text
POST /api/business/erp/serial/batch-generate     权限码 business:erp:serial:generate
  body: {"material_id":"<UUID>", "count":3, "unit_cost":"12.34",
         "warehouse_id":"<UUID>", "location_dict_id":"<UUID>", "batch_no":"B001"}

❌ 不带 unit_cost（或 null / "" ）→ 400 + 10001
   「unit_cost 必填：批量生成按「期初/其他入库」记账，必须录成本单价（不含税）」
```

- **为什么必填**：批量生成自 `docs/adr-20261031-erp-serial-batch-generate-semantics.md` 起
  等于**无来源入库（期初 / 其他入库）**，没有采购单可挂 —— 成本只能由操作人录。
  不录就会在成本账留下 `qty > 0 / avg_cost = 0` 的行（巡检 **C3** 必红），
  所以后端**不设默认 0**（缺省 0 等于把缺陷原样保留）。
- 单价口径：**不含税**、`> 0`、最多 6 位小数、`< 1e11`；四舍五入口径与
  `stockin_model::unit_price` 一致（`MidpointAwayFromZero`，与 PG `ROUND(numeric,6)` 同源）。
  收到的值先 6 位舍入再校验，所以 `0.0000001` 会报「收到 0」。

| 场景 | HTTP / code | 报文 |
|---|---|---|
| 没传 | 400 / 10001 | `unit_cost 必填：批量生成按「期初/其他入库」记账，必须录成本单价（不含税）` |
| 非数字 | 400 / 10001 | `unit_cost 不是合法数字：「abc」` |
| `≤ 0` | 400 / 10001 | `unit_cost 必须 > 0（收到 0）：零成本入库会在成本账留下 qty>0/avg_cost=0 的行（巡检 C3），系统拒收` |
| `≥ 1e11` | 400 / 10001 | `unit_cost 超出可记账范围（须 < 100000000000）：100000000000` |

- 另外两个容易踩的入参：
  - `count` 只能是 **1..=1000**（`0` 或超上限 → 400 + 10001）；
  - `location_id`（历史口径：库位**账行** id）与 `location_dict_id`（库位**字典** id）
    **只能给一个**，同时给 → 400；两个都不给 ⇒ 按该仓库默认库位记账
    （仓库没有默认库位才落「未分配」桶，与收货口径相同）。
  - `source_type` 只收 `PURCHASE / TRANSFER / PRODUCE / RETURN`。
- 出参：`count / period_key / serial_nos / list / unit_cost / amount / inventory`；
  `unit_cost`（6 位）、`amount`（4 位，**取自 `apply_cost` 的 `RETURNING amount` 真实增量，
  不是前端算的 N × 单价**）、`inventory.{qty,amount,avg_cost}` 都是 **字符串** Decimal。
- 出处：`backend/src/business/erp/serial_dto.rs::BatchGenerateReq::parse_unit_cost`
  （报文逐字）、`serial_handler.rs::serial_batch_generate`（路由 + 权限码）；
  前端对应实现 `frontend/src/views/business/erp/serial/serial-logic.ts`（同一份报文透出）。

## 附录 A：无源入库单的 `in_type` = `SOURCELESS`（FIX-7 已修）

```text
无源入库（is_sourceless = true）  → in_type = SOURCELESS
采购入库                          → in_type = PURCHASE
```

唯一入口 `backend/src/business/erp/stockin_base.rs::in_type_of`（建单 / 改单都走它，`in_type_vocabulary_matches_doc_kind` 用例钉住）。按 `in_type` 做统计时，别再把「没走采购的入库」混进采购口径。

## 附录 B：资产折旧凭证「同部门多台汇总一行」的口径

折旧凭证按 **部门 × 费用科目** 汇总出一行分录，不是「一台一行」。所以：

- 想知道**单台**金额 ⇒ 查 `biz_depreciation`（按 `asset_id` + 期间）；
- 凭证分录行只能回答「这个部门这期一共提了多少」；
- 这是设计口径（分录粒度到辅助核算维度为止），**不是缺陷**，单台明细的权威来源永远在 `biz_depreciation`。

## 附录 C：审批 / 审计口径

会签与或签、审批意见回写、状态日志操作人姓名快照：见 **`docs/ERP-审批与审计口径.md`**。

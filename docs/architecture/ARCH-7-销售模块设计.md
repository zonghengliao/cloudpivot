# 云枢 · 销售模块设计

> 文档编号：ARCH-7
> 版本：v1.0 · 2026-11-06
> 定位：**销售模块的唯一权威文档**。此前散落在 `ARCH-3 §3.8`、`ARCH-4`、`ERP-批次1-产品需求PRD` 的销售相关内容，已全部收口到本文。其他文档提及销售时以本文为准。
> 前置状态：出库单（DC-13）**已完成**（`stockout` 模块 5238 行，支持 ISSUE/SCRAP/LOSS，成本结转凭证 借 6401/贷 1405 已实现）

---

## 0. 一句话结论

云枢现在是「采购 + 库存 + 付款」的半个 ERP —— **只会花钱不会赚钱**。本模块补齐另一半：**客户 → 销售订单 → 发货 → 出库 → 应收 → 收款 → 发票 → 收入凭证**。

---

## 1. 模块定位与边界

### 1.1 做什么

| 域 | 内容 |
|---|---|
| **客户主数据** | 客户档案、联系人、信用额度、冻结状态 |
| **销售订单** | 建单、金额分级审批、部分/全部发货跟踪、关闭/作废 |
| **发货** | 发货单、过账触发销售出库、超发校验 |
| **销售出库** | 复用 `biz_stock_out`，新增 `out_type=SALES` |
| **应收账款** | 挂账、账龄、部分核销、坏账 |
| **收款** | 收款单、多单核销、银行流水对账（预留） |
| **销售发票** | 开票、作废、与应收勾稽 |
| **收入凭证** | 借 1122 应收账款 / 贷 6001 主营业务收入 + 2221.02 销项税 |

### 1.2 不做什么（明确排除）

| 不做 | 原因 |
|---|---|
| 销售合同（`contract_type=SALES`） | 合同模块 `SUPPORTED_TYPES=["PURCHASE"]` 只放行采购，放开需单独决策。本模块**只读引用**合同号，不建销售合同单据 |
| 客户门户 / 电商对接 | 属集成层，等 IM 集成阶段 |
| 多币种销售 | 现有 `biz_payable` 有 `currency_code` 字段但全库只有 CNY，销售沿用单币种 |
| 佣金 / 销售提成 | 属 HR 薪酬域，不在本模块 |
| 退货 / 红字发票的完整流程 | 首期只做「销售出库作废 → 红字凭证」，退货单另立 |

---

## 2. 业务链路

```
┌─────────┐   ┌──────────────┐   ┌─────────┐   ┌──────────┐
│ 客户档案 │──▶│  销售订单     │──▶│  发货单  │──▶│ 销售出库  │
│ customer│   │ sales_order  │   │shipment │   │stock_out │
└─────────┘   └──────┬───────┘   └────┬────┘   └────┬─────┘
                     │                │              │
                     │ 审批(金额分级)  │              │ 库存扣减
                     │                │              │ + 成本结转凭证
                     │                │              │ (借6401/贷1405)
                     │                ▼              ▼
                     │         ┌────────────┐  ┌──────────────┐
                     │         │  应收账款   │◀─│ 出库过账触发  │
                     │         │ receivable │  │ 应收挂账      │
                     │         └─────┬──────┘  └──────────────┘
                     │               │
                     │               │ 核销
                     │               ▼
                     │         ┌─────────┐   ┌─────────┐
                     │         │  收款单  │   │ 销售发票 │
                     │         │ receipt │   │ invoice │
                     │         └────┬────┘   └────┬────┘
                     │              │             │
                     │              └──────┬──────┘
                     │                     ▼
                     │            ┌────────────────┐
                     └───────────▶│  收入确认凭证   │
                                  │ 借1122 贷6001  │
                                  │  +2221.02销项税│
                                  └────────────────┘
```

---

## 3. 数据模型

### 3.1 新建表（7 张）

#### 3.1.1 `biz_customer` — 客户档案

```sql
id                uuid PK
code              varchar(32)  唯一   -- CUS-000001
name              varchar(200) 必填
short_name        varchar(64)
tax_no            varchar(32)         -- 纳税人识别号
contact_name      varchar(64)
contact_phone     varchar(32)
contact_email     varchar(128)
address           varchar(500)
credit_limit      numeric(18,2) 默认 0   -- 信用额度，0=不限制
credit_used       numeric(18,2) 默认 0   -- 已用额度（销售订单占用）
status            varchar(16)  默认 'ACTIVE'  -- ACTIVE / FROZEN
settlement_method varchar(32)         -- 结算方式：月结30/货到付款/预付
remark            text
-- 审计: created_at, updated_at, create_by, update_by, deleted_at
唯一索引: uk_biz_customer_code (code)
检查: ck_biz_customer_credit (credit_limit >= 0 AND credit_used >= 0)
```

#### 3.1.2 `biz_customer_contact` — 客户联系人

```sql
id            uuid PK
customer_id   uuid FK → biz_customer(id)
name          varchar(64)  必填
phone         varchar(32)
email         varchar(128)
position      varchar(64)
is_primary    boolean 默认 false
remark        text
-- 审计字段
索引: idx_biz_customer_contact_customer (customer_id)
```

#### 3.1.3 `biz_sales_order` — 销售订单头

```sql
id                uuid PK
so_no             varchar(32)  唯一   -- SO-20261106-000001
customer_id       uuid FK → biz_customer(id)
order_date        date 必填
delivery_date     date
status            varchar(24) 默认 'DRAFT'
                  -- DRAFT / SUBMITTED / APPROVED / PARTIAL_SHIPPED / SHIPPED / CLOSED / CANCELLED
total_amount      numeric(18,2) 默认 0    -- 不含税
total_tax         numeric(18,2) 默认 0
total_with_tax    numeric(18,2) 默认 0    -- 价税合计
warehouse_id      uuid FK → biz_warehouse(id)  -- 默认发货仓
contract_no       varchar(64)          -- 只读引用采购/销售合同号（不建合同单）
remark            text
approval_instance_id uuid                -- 审批流程实例
-- 审计字段
唯一索引: uk_biz_sales_order_no (so_no)
检查: ck_biz_so_amount (total_with_tax = total_amount + total_tax)
索引: idx_biz_so_customer (customer_id), idx_biz_so_status (status), idx_biz_so_date (order_date)
```

#### 3.1.4 `biz_sales_order_item` — 销售订单明细

```sql
id            uuid PK
so_id         uuid FK → biz_sales_order(id) ON DELETE CASCADE
line_no       int 必填
material_id   uuid FK → biz_material(id)
qty           numeric(18,4) 必填
price         numeric(18,4) 必填      -- 含税单价
amount        numeric(18,2)           -- 不含税金额
tax_rate      numeric(6,4) 默认 0.13
tax_amount    numeric(18,2)
delivered_qty numeric(18,4) 默认 0    -- 已发货量（用于部分发货）
remark        varchar(500)
索引: idx_biz_so_item_so (so_id), idx_biz_so_item_material (material_id)
检查: ck_biz_so_item_qty (qty > 0), ck_biz_so_item_delivered (delivered_qty >= 0 AND delivered_qty <= qty)
```

#### 3.1.5 `biz_shipment` + `biz_shipment_item` — 发货单

```sql
-- 头
id            uuid PK
ship_no       varchar(32) 唯一     -- SH-20261106-000001
so_id         uuid FK → biz_sales_order(id)
customer_id   uuid FK → biz_customer(id)
warehouse_id  uuid FK → biz_warehouse(id)
ship_date     date 必填
status        varchar(16) 默认 'DRAFT'  -- DRAFT / PENDING_POST / POSTED / CANCELLED
total_qty     numeric(18,4) 默认 0
stock_out_id  uuid                 -- 过账后生成的出库单
remark        text
-- 审计字段

-- 明细
id            uuid PK
shipment_id   uuid FK → biz_shipment(id) ON DELETE CASCADE
so_item_id    uuid FK → biz_sales_order_item(id)
material_id   uuid FK → biz_material(id)
qty           numeric(18,4) 必填
location_id   uuid FK → biz_location(id)
serial_nos    text[]               -- 序列号（序列号物料才填）
remark        varchar(500)
检查: ck_biz_ship_item_qty (qty > 0)
```

#### 3.1.6 `biz_receivable` — 应收账款

**镜像 `biz_payable` 的字段设计**（现有 payable 有 24 个字段，应收对齐）：

```sql
id                  uuid PK
ar_no               varchar(32) 唯一     -- AR-20261106-000001
customer_id         uuid FK → biz_customer(id)
source_type         varchar(16)          -- SALES_OUT（销售出库）/ SHIPMENT / MANUAL
source_id           uuid
source_no           varchar(32)
invoice_no          varchar(64)          -- 关联发票号（开票后回填）
invoice_date        date
currency_code       varchar(8) 默认 'CNY'
amount              numeric(18,2) 必填   -- 应收金额
received_amount     numeric(18,2) 默认 0 -- 已收
unsettled_amount    numeric(18,2) 默认 0 -- 未收 = amount - received_amount
credit_days         int 默认 30
due_date            date                 -- 到期日 = 挂账日 + credit_days
period_code         varchar(8)           -- 归属期间 yyyy-MM
status              varchar(16) 默认 'OPEN'
                    -- OPEN / PARTIAL / SETTLED / WRITTEN_OFF
settled_at          timestamp
voucher_id          uuid                 -- 收入确认凭证
remark              text
-- 审计字段
唯一索引: uk_biz_ar_no (ar_no)
索引: idx_biz_ar_customer (customer_id), idx_biz_ar_status (status), idx_biz_ar_due (due_date)
检查: ck_biz_ar_amount (amount > 0 AND received_amount >= 0 AND received_amount <= amount)
```

#### 3.1.7 `biz_receipt` + `biz_receipt_allocation` — 收款单与核销

```sql
-- 收款单头
id              uuid PK
receipt_no      varchar(32) 唯一     -- RC-20261106-000001
customer_id     uuid FK → biz_customer(id)
receipt_date    date 必填
amount          numeric(18,2) 必填
allocated_amount numeric(18,2) 默认 0   -- 已核销金额
method          varchar(32)            -- BANK_TRANSFER / CASH / CHECK / OTHER
bank_account    varchar(64)
status          varchar(16) 默认 'DRAFT'  -- DRAFT / CONFIRMED / POSTED / CANCELLED
voucher_id      uuid
remark          text
-- 审计字段
检查: ck_biz_receipt_amount (amount > 0 AND allocated_amount >= 0 AND allocated_amount <= amount)

-- 核销明细
id              uuid PK
receipt_id      uuid FK → biz_receipt(id) ON DELETE CASCADE
receivable_id   uuid FK → biz_receivable(id)
amount          numeric(18,2) 必填
remark          varchar(500)
检查: ck_biz_rcpt_alloc_amount (amount > 0)
```

#### 3.1.8 `biz_invoice` — 销售发票

```sql
id                uuid PK
invoice_no        varchar(64) 唯一
invoice_type      varchar(16) 默认 'OUT'   -- OUT（销项）
invoice_code      varchar(32)              -- 发票代码
customer_id       uuid FK → biz_customer(id)
invoice_date      date 必填
amount            numeric(18,2)            -- 不含税
tax_amount        numeric(18,2)
total_with_tax    numeric(18,2)
tax_rate          numeric(6,4) 默认 0.13
status            varchar(16) 默认 'DRAFT'  -- DRAFT / ISSUED / POSTED / VOID
receivable_id     uuid FK → biz_receivable(id)  -- 关联应收
source_type       varchar(16)               -- SO / SHIPMENT
source_no         varchar(32)
voucher_id        uuid
remark            text
-- 审计字段
检查: ck_biz_invoice_amount (total_with_tax = amount + tax_amount)
```

### 3.2 改造现有表

| 表 | 改动 | 理由 |
|---|---|---|
| `biz_stock_out` | `out_type` 值域增加 `SALES` | 销售出库复用通用出库，`stockout` 现有 ISSUE/SCRAP/LOSS |
| `biz_contract` 相关 DTO | `SUPPORTED_TYPES` 暂不改 | 销售合同不在本期范围，只读引用 `contract_no` |

### 3.3 表数量汇总

```
新建 7 张：biz_customer, biz_customer_contact, biz_sales_order, biz_sales_order_item,
          biz_shipment(+item 算 1 张), biz_receivable, biz_receipt(+allocation 算 1 张),
          biz_invoice
实际表数：9 张（含 3 组主子表的明细表）
```

---

## 4. 状态机（必须登记 `LEGAL_EDGES`）

### 4.1 销售订单

```
DRAFT ──提交──▶ SUBMITTED ──审批通过──▶ APPROVED
  │                │                      │
  │                │驳回                  ├──发货(部分)──▶ PARTIAL_SHIPPED
  │                ▼                      │                  │
  │              DRAFT                    │                  ├──发货(全)──▶ SHIPPED ──▶ CLOSED
  │                                       │                  │
  └────────作废────▶ CANCELLED ◀──作废────┘◀─────作废────────┘
```

合法边（9 条）：
```
DRAFT            → SUBMITTED, CANCELLED
SUBMITTED        → APPROVED, DRAFT, CANCELLED
APPROVED         → PARTIAL_SHIPPED, SHIPPED, CANCELLED
PARTIAL_SHIPPED  → SHIPPED, CLOSED
SHIPPED          → CLOSED
CLOSED           → （终态）
CANCELLED        → （终态）
```

### 4.2 发货单

```
DRAFT → PENDING_POST → POSTED
  │         │
  └──作废───┴──▶ CANCELLED
```

### 4.3 应收账款

```
OPEN → PARTIAL → SETTLED
  │       │
  └───────┴──▶ WRITTEN_OFF（坏账）
```

### 4.4 收款单

```
DRAFT → CONFIRMED → POSTED
  │
  └──作废──▶ CANCELLED
```

### 4.5 发票

```
DRAFT → ISSUED → POSTED
  │        │
  └──作废──┴──▶ VOID
```

---

## 5. 接口清单

### 5.1 客户（5 个）

| 方法 | 路径 | 说明 |
|---|---|---|
| POST | `/api/business/erp/customer` | 创建 |
| PUT | `/api/business/erp/customer/{id}` | 更新 |
| DELETE | `/api/business/erp/customer/{id}` | 软删 |
| GET | `/api/business/erp/customer/{id}` | 详情 |
| GET | `/api/business/erp/customer/page` | 分页（code/name/status 过滤） |

### 5.2 销售订单（6 个）

| 方法 | 路径 | 说明 |
|---|---|---|
| POST | `/api/business/erp/sales-order` | 创建（DRAFT） |
| PUT | `/api/business/erp/sales-order/{id}` | 更新 |
| GET | `/api/business/erp/sales-order/{id}` | 详情 |
| GET | `/api/business/erp/sales-order/page` | 分页 |
| PATCH | `/api/business/erp/sales-order/{id}/status` | 状态迁移 |
| GET | `/api/business/erp/sales-order/{id}/shippable-items` | 可发货明细（未发完的行） |

### 5.3 发货（4 个）

| 方法 | 路径 | 说明 |
|---|---|---|
| POST | `/api/business/erp/shipment` | 创建（从订单带明细） |
| GET | `/api/business/erp/shipment/{id}` | 详情 |
| GET | `/api/business/erp/shipment/page` | 分页 |
| PATCH | `/api/business/erp/shipment/{id}/status` | 状态迁移（过账触发出库） |

### 5.4 应收（5 个）

| 方法 | 路径 | 说明 |
|---|---|---|
| GET | `/api/business/erp/receivable/{id}` | 详情 |
| GET | `/api/business/erp/receivable/page` | 分页 |
| GET | `/api/business/erp/receivable/aging` | 账龄分析（参照 `payable_aging.rs`） |
| POST | `/api/business/erp/receivable` | 手工挂账 |
| PATCH | `/api/business/erp/receivable/{id}/write-off` | 坏账核销 |

### 5.5 收款（5 个）

| 方法 | 路径 | 说明 |
|---|---|---|
| POST | `/api/business/erp/receipt` | 创建 |
| GET | `/api/business/erp/receipt/{id}` | 详情 |
| GET | `/api/business/erp/receipt/page` | 分页 |
| POST | `/api/business/erp/receipt/{id}/allocate` | 核销到应收 |
| PATCH | `/api/business/erp/receipt/{id}/status` | 状态迁移 |

### 5.6 发票（4 个）

| 方法 | 路径 | 说明 |
|---|---|---|
| POST | `/api/business/erp/invoice` | 创建（从应收开票） |
| GET | `/api/business/erp/invoice/{id}` | 详情 |
| GET | `/api/business/erp/invoice/page` | 分页 |
| PATCH | `/api/business/erp/invoice/{id}/status` | 状态迁移（ISSUED/VOID） |

**合计 29 个接口**。

---

## 6. 前端页面（6 个）

| 页面 | 路径 | 说明 |
|---|---|---|
| 客户档案 | `views/business/erp/customer/` | 列表 + 新建/编辑 + 状态切换 |
| 销售订单 | `views/business/erp/sales-order/` | 列表 + 建单（明细行）+ 提交/审批进度 + 作废 |
| 发货单 | `views/business/erp/shipment/` | 列表 + 从订单生成 + 过账 |
| 应收台账 | `views/business/erp/receivable/` | 列表 + 账龄看板 + 核销记录 |
| 收款单 | `views/business/erp/receipt/` | 列表 + 收款登记 + 核销 |
| 销售发票 | `views/business/erp/invoice/` | 列表 + 开票 + 作废 |

**必须复用**：
- `FlowProgress.vue` — 审批进度时间线
- 现有表格/表单/弹窗组件（参考 `purchase/index.vue`）
- `BatchActionBar` — 批量操作
- 状态标签颜色规范

---

## 7. 与现有模块的集成点

### 7.1 库存（关键）

```rust
// 发货过账时，一个事务内：
1. 生成 biz_stock_out（out_type='SALES', source_no=ship_no）
2. 调 inventory_writer.rs 扣减库存（唯一写入口，禁直写三本账）
3. 写 biz_inventory_txn（流水）
4. 成本结转凭证（借 6401 主营业务成本 / 贷 1405 库存商品）
5. 回写 so_item.delivered_qty
6. 订单状态转 PARTIAL_SHIPPED / SHIPPED
7. 生成应收挂账（source_type='SALES_OUT'）
```

**必须原子**：库存 + 库位账 + 流水 + 凭证 + 应收 一起成功或一起失败。

### 7.2 凭证（收入与成本两步）

```
① 出库过账 → 成本结转凭证（复用 voucher::create_for_stock_out）
   借：6401 主营业务成本
   贷：1405 库存商品

② 开票/应收确认 → 收入凭证（新建 voucher::create_for_sales_revenue）
   借：1122 应收账款
   贷：6001 主营业务收入
       2221.02 应交税费-销项税
```

### 7.3 应收 ↔ 应付的镜像关系

应收模块**完全镜像**应付模块的设计：

| 应付（已有） | 应收（新建） | 镜像点 |
|---|---|---|
| `payable.rs` 463 行 | `receivable.rs` | 基础 CRUD |
| `payable_aging.rs` 1265 行 | `receivable_aging.rs` | 账龄分析 |
| `payable_payment.rs` 4125 行 | `receipt.rs` + 核销 | 付款/收款 |
| `payable_bridge.rs` 2472 行 | `receivable_bridge.rs` | 联动挂账 |
| `payable_query.rs` 1112 行 | `receivable_query.rs` | 查询 |
| `biz_payable` 24 字段 | `biz_receivable` 24 字段 | 表结构 |

**复用而非重写**：账龄算法、核销逻辑、状态机、凭证生成全部照搬，只把 supplier 换成 customer、paid 换成 received。

### 7.4 流程引擎

销售订单审批复用 `biz_flow_instance`：
- `flow_code = SO_APPROVE`
- `flow_type = APPROVAL`
- 金额分级走 `biz_flow_edge` 的可配置边
- 内控 `separation.rs`：制单人与审批人不能同一人

### 7.5 主数据

- 客户信用额度：下单时校验 `credit_used + 本单金额 <= credit_limit`（额度为 0 表示不限）
- 物料：销售订单明细引用 `biz_material`
- 仓库/库位：发货单指定仓库，明细指定库位

---

## 8. 会计处理

### 8.1 科目映射

| 业务动作 | 借方 | 贷方 |
|---|---|---|
| 销售出库（成本结转） | 6401 主营业务成本 | 1405 库存商品 |
| 应收挂账 / 开票（收入确认） | 1122 应收账款 | 6001 主营业务收入 + 2221.02 应交税费-销项税 |
| 收到货款 | 1002 银行存款 | 1122 应收账款 |
| 坏账核销 | 6702 资产减值损失 | 1122 应收账款 |
| 预收货款（预留） | 1002 银行存款 | 2203 预收账款 |

### 8.2 期间与结账

- 应收挂账归属 `period_code`（yyyy-MM），受会计期间状态约束
- 已结账期间禁止新增应收、收款、发票
- 收入确认必须与出库期间一致（跨期需走调整）

---

## 9. 内控与校验

### 9.1 内控规则

| 规则 | 实现 |
|---|---|
| 制单与审批分离 | 复用 `separation.rs`，`erp.separation_of_duties.approve` |
| 信用额度控制 | 超限时：拒绝 或 提级审批（阈值可配） |
| 客户冻结 | FROZEN 状态的客户不能新建订单 |
| 超发控制 | 发货量 ≤ 订单量 - 已发量 |
| 库存不能负 | 出库时校验可用库存（含 `locked_qty`） |
| 收款不能超收 | 核销总额 ≤ 应收未收额 |

### 9.2 不变量（扩展 `erp_invariants.py`）

新增检查项：
```
S1  销售订单金额: total_with_tax = amount + tax
S2  订单发货一致性: sum(delivered_qty) <= qty
S3  发票金额: total_with_tax = amount + tax_amount
S4  应收核销: received_amount <= amount
S5  收款核销: allocated_amount <= amount，且 sum(allocation) = allocated_amount
S6  出库↔应收勾稽: 每张 SALES 出库单必有对应应收单
S7  凭证借贷平衡: 所有销售相关凭证 debit = credit
S8  三账一致: 库存表 = 库位账 = 流水（销售出库后）
```

---

## 10. 权限与菜单

### 10.1 权限码

```
business:erp:customer:list / create / update / delete / detail
business:erp:sales-order:list / create / update / delete / status / approve
business:erp:shipment:list / create / update / delete / status
business:erp:receivable:list / detail / write-off / aging
business:erp:receipt:list / create / update / delete / allocate / status
business:erp:invoice:list / create / update / delete / status
```

### 10.2 角色授权

| 角色 | 客户 | 销售订单 | 发货 | 应收 | 收款 | 发票 |
|---|---|---|---|---|---|---|
| SALES_MANAGER（销售经理） | 读写 | 读写+提交 | 只读 | 只读 | — | — |
| SALES_CLERK（销售员） | 只读 | 建单+提交 | — | — | — | — |
| WAREHOUSE_MANAGER | — | 只读 | 读写+过账 | — | — | — |
| FINANCE_MANAGER | 只读 | 审批 | 只读 | 读写 | 读写 | 读写 |
| GENERAL_MANAGER | 只读 | 审批（大额） | 只读 | 只读 | 只读 | 只读 |

### 10.3 菜单树

```
销售管理
├── 客户档案
├── 销售订单
├── 发货单
├── 应收账款
│   └── 账龄分析
├── 收款单
└── 销售发票
```

---

## 11. 实施计划（25 张卡，严格串行）

| # | 卡 | 类型 | 预计 |
|---|---|---|---|
| SO-01 | 客户档案表 + 迁移 | 后端 | 1h |
| SO-02 | 客户档案后端 API | 后端 | 1.5h |
| SO-03 | 客户档案前端页面 | 前端 | 1.5h |
| SO-04 | 销售订单表 + 迁移 | 后端 | 1h |
| SO-05 | 销售订单后端 API + 状态机 | 后端 | 1.5h |
| SO-06 | 销售订单审批流 | 后端 | 1h |
| SO-07 | 销售订单前端页面 | 前端 | 1.5h |
| SO-08 | 发货单表 + 迁移 | 后端 | 1h |
| SO-09 | 发货单后端 API | 后端 | 1h |
| SO-10 | **发货过账 → 出库 → 库存扣减**（关键） | 后端 | 1.5h |
| SO-11 | 发货单前端页面 | 前端 | 1.5h |
| SO-12 | 应收账款表 + 迁移 | 后端 | 1h |
| SO-13 | 应收后端 API + 账龄 | 后端 | 1.5h |
| SO-14 | 出库 → 应收挂账联动 | 后端 | 1h |
| SO-15 | 应收台账前端页面 | 前端 | 1.5h |
| SO-16 | 收款单表 + 迁移 | 后端 | 1h |
| SO-17 | 收款后端 API + 核销 | 后端 | 1.5h |
| SO-18 | 收款单前端页面 | 前端 | 1.5h |
| SO-19 | 发票表 + 迁移 | 后端 | 1h |
| SO-20 | 发票后端 API + 开票 | 后端 | 1h |
| SO-21 | 发票前端页面 | 前端 | 1h |
| SO-22 | **收入确认凭证**（借1122/贷6001+2221.02） | 后端 | 1h |
| SO-23 | 销售链不变量（8 条） | 验证 | 1h |
| SO-24 | 端到端链路测试 | 验证 | 1.5h |
| SO-25 | 菜单种子 + 角色授权 | 前端+迁移 | 1h |

**总工时**：约 30 小时。串行执行 ≈ 30 小时；2 并发 ≈ 15 小时。

---

## 12. 验收标准

### 12.1 单卡验收（每张卡必做）

```
1. git commit（提交信息带卡号）
2. 卡内汇报：改了哪些文件/函数、实测输出、连跑两轮结果
3. 后端：cargo test 全绿
4. 前端：pnpm typecheck && pnpm build 通过
5. API：真实 token curl 留证
6. 迁移：APPLY_OK + 回滚可执行
```

### 12.2 模块验收（SO-24 端到端）

完整走一遍：

```
① 建客户「测试客户A」，信用额度 10 万
② 建销售订单 SO-xxx，2 个物料，合计 5 万，提交 → 审批通过
③ 建发货单 SH-xxx，发 1 个物料（部分发货）
④ 过账 → 验证：
   · 生成销售出库单（out_type=SALES）
   · 库存减少
   · 三账一致（库存表 = 库位账 = 流水）
   · 成本结转凭证（借 6401 / 贷 1405）
   · 生成应收 AR-xxx（5 万）
   · 订单状态 PARTIAL_SHIPPED，delivered_qty 正确
⑤ 第二张发货单发剩余，过账 → 订单 SHIPPED
⑥ 开票 IN-xxx（5 万，税率 13%）
⑦ 验证收入凭证（借 1122 5.65万 / 贷 6001 5万 + 2221.02 0.65万）
⑧ 收款 RC-xxx 5.65 万，核销到 AR
⑨ 验证：AR 状态 SETTLED，客户 credit_used 归零
⑩ 跑 erp_invariants.py，8 条销售不变量全绿
```

### 12.3 反向验收（ERP 铁律：正向必配反向）

```
① 销售订单作废 → 状态 CANCELLED，不能再发货
② 发货单作废 → 状态 CANCELLED，库存不扣（或已扣则红字冲回）
③ 出库单作废 → 红字凭证，库存回滚，三账回基线
④ 收款单作废 → AR 回到 OPEN
⑤ 发票作废 → 状态 VOID，红字凭证
```

---

## 13. 已知约束与风险

| # | 约束/风险 | 影响 | 应对 |
|---|---|---|---|
| 1 | 销售合同 `SUPPORTED_TYPES=["PURCHASE"]` 只放行采购 | 销售订单只能引用 `contract_no` 文本，不能建销售合同单 | 本期不放开；后续单独决策 |
| 2 | 现有 `biz_stock_out.out_type` 只有 ISSUE/SCRAP/LOSS | 需加 `SALES` | 改值域（前端下拉 + 后端校验白名单） |
| 3 | 出库成本结转依赖 FIFO 成本 | 若物料无 `costing_method` 会出错 | 销售出库前校验物料成本方法已设置 |
| 4 | 应收账龄算法要照搬应付 | 应付 1265 行，照搬有工作量 | 直接复用 `payable_aging.rs` 的函数，改成泛型或复制改造 |
| 5 | 多币种 | `currency_code` 字段存在但全库 CNY | 销售沿用单币种，不做汇率 |
| 6 | 发票与税务 | 只做记录，不接金税系统 | 预留 `invoice_code` 字段 |
| 7 | 退货流程 | 首期无退货单 | 只支持「出库作废 + 红字」，完整退货另立 |

---

## 14. 与其他文档的关系

| 文档 | 关系 |
|---|---|
| `ARCH-3-模块勾稽关系审计.md §3.8` | 销售缺口分析来源。本文是其销售部分的**展开与收口** |
| `ARCH-4-通用MES与委外架构设计.md` | 第 232 行声明「客户/销售/应收/收款 = ARCH-4 做」，本文接替该职责，ARCH-4 专注 MES |
| `ARCH-6-财务扩展设计.md` | 预算/固资/报销，与销售无重叠 |
| `ERP-批次1-产品需求PRD.md` | 第 2070 行「第 2 批做销售与应收」，本文是该批的实施设计 |
| `采购入库勾稽重构-设计.md` | 采购侧勾稽，销售侧镜像它 |
| `通用勾稽规则与业务链映射表.md` | 补销售链的勾稽映射 |

---

**本文是销售模块唯一权威文档。任何销售相关实现以本文为准，其他文档提及销售处一律引用本文。**

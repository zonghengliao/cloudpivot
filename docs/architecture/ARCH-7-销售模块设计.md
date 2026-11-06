# 云枢 · 销售模块设计

> 文档编号：ARCH-7
> 版本：v1.1 · 2026-11-06（v1.1 补 §15 扩展功能设计，共 44 张卡）
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

> **v1.1 修订**（2026-11-06）：v1.0 曾把销售合同、多币种、佣金、退货列为「不做」，**该判断有误**——用户明确要求「范围先不考虑，先把完整设计做出来」。现已全部纳入 **§15 扩展功能设计**。
>
> 本表仅保留**真正不做**的项。

| 不做 | 原因 | 决策人 / 日期 |
|---|---|---|
| **客户门户 / 电商对接** | 产品定位问题，暂不做 | 用户 · 2026-11-06 |

**已从「不做」移入 §15 的项**：
- 销售合同（`contract_type=SALES`）→ §15.1
- 多币种销售 → §15.6
- 佣金 / 销售提成 → §15.4
- 退货 / 红字发票 → §15.2

**§15 新增的项**：价格管理（§15.3）、信用额度释放（§15.5）、客户对账单（§15.7）、销售预测与目标（§15.8）

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

---

## 15. 扩展功能设计（第二批）

> 本章是 v1.1 新增。v1.0 曾以「最小可用」为由排除销售合同、退货、多币种等，**该判断有误**——用户明确要求「范围先不考虑，先把完整设计做出来」。本章补齐全部缺口。
>
> **唯一明确不做**：客户门户/电商对接（用户 2026-11-06 决定暂不做）。

### 15.1 销售合同

#### 15.1.1 现状核实（技术成本极低）

```rust
// contract_dto.rs:69-72
pub const CONTRACT_TYPES: [&str; 4] = ["PURCHASE", "SALES", "SERVICE", "LEASE"];  // 值域已含 SALES
pub const SUPPORTED_TYPES: [&str; 1] = ["PURCHASE"];                              // 只放行采购
```

**合同表字段已经是泛化设计**：`party_a_id / party_a_name / party_b_type / party_b_id / party_b_name`（甲乙方），不绑死 `supplier_id`。

**放开 SALES 的成本**：改 1 个常量 + 校验点 `contract_handler.rs:388` 一处 + 前端类型下拉放开。

#### 15.1.2 设计

```
① SUPPORTED_TYPES = ["PURCHASE", "SALES"]
② party_b_type = "CUSTOMER" 时，party_b_id 指向 biz_customer
   （contract_dto.rs:482 已有 SALES → CUSTOMER 的推导逻辑，只需接上客户主数据）
③ 销售订单增加 contract_id 字段（可空），关联销售合同
④ 合同金额控制：订单累计金额不能超过合同金额（超出需审批）
⑤ 合同状态：DRAFT → SIGNED → ACTIVE → COMPLETED / TERMINATED
   与采购合同同一套状态机
⑥ 前端合同页类型下拉放开 SALES，乙方选择器在 SALES 时改为客户选择器
```

#### 15.1.3 表改动

| 表 | 改动 |
|---|---|
| `biz_contract` | 无表结构改动（party_a/party_b 已泛化） |
| `biz_sales_order` | 新增 `contract_id uuid`（可空，FK → biz_contract） |

---

### 15.2 退货管理

#### 15.2.1 为什么必须有

**作废 ≠ 退货**：
- 作废 = 单据录错，整单撤销，库存回滚，红字凭证
- 退货 = 客户买了又退回来，是**正常业务**：库存回库、应收冲减、可能开红字发票

现有 `biz_stock_out` 只有作废时的红字冲回，**没有退货单据**。

#### 15.2.2 表设计

```sql
-- 销售退货单
biz_sales_return
  id              uuid PK
  return_no       varchar(32) 唯一    -- SR-20261106-000001
  customer_id     uuid FK → biz_customer(id)
  so_id           uuid FK → biz_sales_order(id)      -- 原销售订单（可空）
  shipment_id     uuid FK → biz_shipment(id)         -- 原发货单（可空）
  return_date     date 必填
  return_type     varchar(16)          -- QUALITY(质量) / WRONG_ITEM(错发) / OTHER(其他)
  warehouse_id    uuid FK → biz_warehouse(id)
  status          varchar(16) 默认 'DRAFT'
                  -- DRAFT / SUBMITTED / APPROVED / PENDING_POST / POSTED / CANCELLED
  total_amount    numeric(18,2)        -- 不含税
  total_tax       numeric(18,2)
  total_with_tax  numeric(18,2)
  stock_in_id     uuid                 -- 过账后生成的入库单
  receivable_id   uuid                 -- 冲减的应收单
  red_invoice_id  uuid                 -- 红字发票
  approval_instance_id uuid
  remark          text
  -- 审计字段

-- 退货明细
biz_sales_return_item
  id            uuid PK
  return_id     uuid FK → biz_sales_return(id) ON DELETE CASCADE
  so_item_id    uuid FK → biz_sales_order_item(id)
  material_id   uuid FK → biz_material(id)
  qty           numeric(18,4) 必填
  price         numeric(18,4)
  amount        numeric(18,2)
  tax_rate      numeric(6,4)
  tax_amount    numeric(18,2)
  location_id   uuid FK → biz_location(id)
  serial_nos    text[]
  remark        varchar(500)
  检查: ck_biz_sreturn_item_qty (qty > 0)
```

#### 15.2.3 流转逻辑

```
退货单 POSTED 时，一个事务内：
1. 生成采购式入库单（复用 biz_stock_in，source_type=SALES_RETURN）
   → 库存回库（inventory_writer）
2. 生成红字入库凭证（借 1405 / 贷 6401，冲减成本）
3. 冲减应收：生成负向 receivable 或调整原应收 unsettled_amount
4. 若已开票 → 生成红字发票（biz_invoice，amount 为负，type=OUT_RED）
5. 回写原销售订单 delivered_qty（减少）
6. 更新客户信用额度占用

退货数量约束：sum(已退) + 本次 <= 已发货量
```

#### 15.2.4 状态机

```
DRAFT → SUBMITTED → APPROVED → PENDING_POST → POSTED
                       │            │
                       └──驳回──────┴──作废──▶ CANCELLED
```

---

### 15.3 价格管理

#### 15.3.1 为什么需要

现在销售订单的 `price` 字段只能**手工填**——同一个物料对不同客户价格不同，销售员每次都得查价、记错就亏。

#### 15.3.2 表设计

```sql
-- 客户协议价（优先级最高）
biz_customer_price
  id            uuid PK
  customer_id   uuid FK → biz_customer(id)
  material_id   uuid FK → biz_material(id)
  price         numeric(18,4) 必填     -- 含税单价
  min_qty       numeric(18,4) 默认 0   -- 起订量
  effective_from date 必填
  effective_to  date                   -- 空=长期有效
  status        varchar(16) 默认 'ACTIVE'
  remark        text
  -- 审计字段
  唯一索引: uk_biz_cust_price (customer_id, material_id, min_qty, effective_from)

-- 阶梯价 / 促销价（通用）
biz_price_tier
  id            uuid PK
  material_id   uuid FK → biz_material(id)
  customer_id   uuid                   -- 空=适用于所有客户
  tier_type     varchar(16)            -- TIER(阶梯) / PROMO(促销)
  min_qty       numeric(18,4) 必填
  max_qty       numeric(18,4)          -- 空=无上限
  price         numeric(18,4) 必填
  effective_from date
  effective_to   date
  status        varchar(16) 默认 'ACTIVE'
  -- 审计字段
```

#### 15.3.3 取价规则（**必须确定性**，不能有歧义）

```
优先级从高到低：
① 客户协议价（biz_customer_price）
   条件：customer_id + material_id 匹配，min_qty <= 订购量，
        effective_from <= 订单日期 <= effective_to(或空)
   多条命中 → 取 min_qty 最大（最匹配）的那条

② 客户专属阶梯价（biz_price_tier where customer_id is not null）
   条件：material_id 匹配 + min_qty <= 订购量 < max_qty + 有效期

③ 通用阶梯价（biz_price_tier where customer_id is null）

④ 物料标准售价（biz_material.standard_price，需新增字段）

⑤ 都没有 → 返回空，让销售员手填（并提示"未找到协议价"）

**接口**: GET /api/business/erp/price/resolve?customer_id=&material_id=&qty=&date=
返回: { price, source: 'AGREED'|'TIER_CUSTOMER'|'TIER_COMMON'|'STANDARD'|'NONE', source_id }
```

#### 15.3.4 表改动

| 表 | 改动 |
|---|---|
| `biz_material` | 新增 `standard_price numeric(18,4)`（标准售价） |

---

### 15.4 销售员与佣金

#### 15.4.1 现状

```
sys_user 无 salesperson 字段
sys_role 无 SALES 类角色
销售订单无销售员字段
```

#### 15.4.2 设计

```sql
-- ① 销售订单记录销售员
ALTER TABLE biz_sales_order ADD COLUMN salesperson_id uuid;  -- FK → sys_user(id)

-- ② 佣金规则（按销售员 / 按物料类别 / 按金额区间）
biz_commission_rule
  id            uuid PK
  rule_name     varchar(100)
  scope_type    varchar(16)            -- SALESPERSON / MATERIAL_CATEGORY / ALL
  scope_id      uuid                   -- salesperson_id 或 material_category_id
  calc_type     varchar(16)            -- PERCENT(百分比) / FIXED(固定额)
  rate          numeric(8,4)           -- 百分比 或 固定金额
  min_amount    numeric(18,2) 默认 0   -- 起算金额
  max_amount    numeric(18,2)          -- 封顶
  effective_from date
  effective_to   date
  status        varchar(16) 默认 'ACTIVE'
  -- 审计字段

-- ③ 佣金结算单（按期间汇总）
biz_commission_settlement
  id            uuid PK
  settlement_no varchar(32) 唯一       -- CM-202611-000001
  salesperson_id uuid FK → sys_user(id)
  period_code   varchar(8)             -- yyyy-MM
  total_sales   numeric(18,2)          -- 期间销售额（已收款部分）
  total_commission numeric(18,2)
  status        varchar(16) 默认 'DRAFT'  -- DRAFT / APPROVED / PAID
  paid_at       timestamp
  voucher_id    uuid                   -- 佣金计提凭证
  remark        text
  -- 审计字段

-- ④ 佣金明细（哪笔订单产生多少佣金）
biz_commission_item
  id              uuid PK
  settlement_id   uuid FK → biz_commission_settlement(id) ON DELETE CASCADE
  so_id           uuid FK → biz_sales_order(id)
  receivable_id   uuid FK → biz_receivable(id)
  base_amount     numeric(18,2)        -- 佣金基数（已收金额）
  rule_id         uuid FK → biz_commission_rule(id)
  rate            numeric(8,4)
  commission      numeric(18,2)
```

#### 15.4.3 计提规则（关键：**按收款计佣，不按开单计佣**）

```
为什么按收款：开单不等于收到钱，坏账还要扣回。按收款计佣是销售管理的通行做法。

规则：
① 佣金基数 = 该销售员负责的订单对应的应收，**已核销（收到钱）的部分**
② 按 biz_commission_rule 匹配规则（销售员 > 物料类别 > 全局）
③ 结算周期：月度（period_code）
④ 佣金计提凭证：借 6601 销售费用-佣金 / 贷 2241 其他应付款-佣金
⑤ 实际支付：借 2241 / 贷 1002 银行存款
⑥ 退货冲减：退货涉及的订单，佣金按比例扣回
```

#### 15.4.4 表改动

| 表 | 改动 |
|---|---|
| `biz_sales_order` | 新增 `salesperson_id uuid` |
| `sys_role` | 新增 SALES_MANAGER / SALES_CLERK 两个角色（种子） |

---

### 15.5 信用额度占用与释放

#### 15.5.1 v1.0 的漏洞

v1.0 只写了「下单时校验 `credit_used + 本单金额 <= credit_limit`」，**但没写什么时候释放**。如果只占用不释放，客户额度会被永久占死。

#### 15.5.2 完整设计

```
额度占用时机（唯一）：
  销售订单 APPROVED 时 → credit_used += 订单含税金额

额度释放时机（三个出口，任一触发）：
  ① 订单作废/关闭且未发货 → credit_used -= 订单金额（全额释放）
  ② 应收核销完成（SETTLED） → credit_used -= 该应收金额
     ^^^ 这是主要出口：钱收到了，额度自然释放
  ③ 退货过账 → credit_used -= 退货金额

额度重算（兜底，防止漂移）：
  定时任务每日重算：credit_used = sum(
    APPROVED/PARTIAL_SHIPPED/SHIPPED 状态订单的含税金额
  ) - sum(
    已核销的应收金额
  )
  → 与 credit_used 比对，不一致则告警并修正（记 changelog）
```

#### 15.5.3 表改动

| 表 | 改动 |
|---|---|
| `biz_customer` | `credit_used` 字段已有，需补索引 |

#### 15.5.4 不变量（追加到 S 系列）

```
S9  信用额度一致性: customer.credit_used =
    sum(有效订单含税金额) - sum(已核销应收金额)
    （容忍误差 0.01，超出即 BLOCKER）
```

---

### 15.6 多币种

#### 15.6.1 现状

```
有 currency_code 的表: biz_contract / biz_payable / biz_purchase_order / biz_stock_in
实际值:                全部 CNY
汇率表:                不存在
金额精度:              numeric(18,4)
```

#### 15.6.2 设计原则（**双币记账，本位币入账**）

```
每一笔业务金额都存两个值：
  · 原币金额（currency_amount）+ 币种（currency_code）
  · 本位币金额（base_amount）+ 汇率（exchange_rate）

凭证一律按**本位币**入账，原币仅作展示与对账。
```

#### 15.6.3 表设计

```sql
-- ① 币种主数据
biz_currency
  code          varchar(8) PK          -- CNY / USD / EUR / HKD
  name          varchar(64)
  symbol        varchar(8)
  decimal_places int 默认 2
  is_base       boolean 默认 false     -- 本位币（唯一）
  status        varchar(16) 默认 'ACTIVE'

-- ② 汇率表（按月维护，月末汇率结账用）
biz_exchange_rate
  id            uuid PK
  from_currency varchar(8) FK → biz_currency(code)
  to_currency   varchar(8)              -- 本位币
  rate          numeric(18,8) 必填
  rate_type     varchar(16)            -- SPOT(即时) / MONTH_END(月末)
  effective_date date 必填
  -- 审计字段
  唯一索引: uk_biz_exchange_rate (from_currency, to_currency, rate_type, effective_date)

-- ③ 已有表加字段（以 biz_sales_order 为例，应收/收款/发票同理）
ALTER TABLE biz_sales_order
  ADD COLUMN currency_code varchar(8) DEFAULT 'CNY',
  ADD COLUMN exchange_rate numeric(18,8) DEFAULT 1,
  ADD COLUMN base_total_amount numeric(18,2),      -- 本位币金额
  ADD COLUMN base_total_tax numeric(18,2),
  ADD COLUMN base_total_with_tax numeric(18,2);
```

#### 15.6.4 关键约束

```
① 汇率固定原则：单据创建时锁定汇率，后续汇率变动不影响已建单据
   （否则已开的发票金额会漂移，审计通不过）

② 汇兑损益：月末结账时，未核销的外币应收按月末汇率重估
   借/贷：6603 财务费用-汇兑损益 / 1122 应收账款

③ 本位币唯一：biz_currency 中 is_base=true 只能有一行（部分唯一索引）

④ 录入校验：currency_code != 本位币 时，exchange_rate 必须 > 0 且必填
```

#### 15.6.5 表改动汇总

| 表 | 新增字段 |
|---|---|
| `biz_sales_order` | currency_code, exchange_rate, base_total_amount, base_total_tax, base_total_with_tax |
| `biz_receivable` | 同上 5 个 |
| `biz_receipt` | 同上 5 个 |
| `biz_invoice` | 同上 5 个 |

**新增表**：`biz_currency`、`biz_exchange_rate`

---

### 15.7 客户对账单

#### 15.7.1 为什么需要

应付侧有对账（`payable_aging.rs` + 对账单），**应收没有对账单**。客户财务要求「给我一份对账单，我对一下账」时，现在只能手工拉 Excel。

#### 15.7.2 设计

```
不做新表，用查询生成对账单（数据来自 biz_receivable + biz_receipt_allocation）

接口:
GET /api/business/erp/receivable/statement?customer_id=&period_from=&period_to=
返回:
{
  customer: { code, name, tax_no },
  period: { from, to },
  opening_balance: 期初余额,
  items: [
    { date, type: 'INVOICE'|'RECEIPT'|'RETURN', doc_no, amount, balance }
  ],
  closing_balance: 期末余额,
  detail: { total_invoiced, total_received, total_returned }
}

导出: GET .../statement/export?format=xlsx  → 生成 Excel
```

#### 15.7.3 客户对账确认

```sql
biz_statement_confirm
  id            uuid PK
  customer_id   uuid FK → biz_customer(id)
  period_from   date
  period_to     date
  total_amount  numeric(18,2)          -- 对账总额
  status        varchar(16) 默认 'SENT'  -- SENT / CONFIRMED / DISPUTED
  confirmed_at  timestamp
  confirmed_by  varchar(64)            -- 客户方确认人
  dispute_reason text
  attachment_id uuid                   -- 客户盖章回传的对账单
  -- 审计字段
```

---

### 15.8 销售预测与目标

#### 15.8.1 设计

```sql
-- ① 销售目标（按销售员/部门/产品线，按期间）
biz_sales_target
  id            uuid PK
  target_no     varchar(32)
  scope_type    varchar(16)            -- SALESPERSON / DEPT / MATERIAL_CATEGORY / ALL
  scope_id      uuid
  period_code   varchar(8)             -- yyyy-MM 或 yyyy-QN
  target_amount numeric(18,2)          -- 目标销售额
  target_qty    numeric(18,4)          -- 目标数量（可选）
  status        varchar(16) 默认 'ACTIVE'
  -- 审计字段

-- ② 销售预测（滚动预测，逐月更新）
biz_sales_forecast
  id            uuid PK
  forecast_no   varchar(32)
  customer_id   uuid                   -- 可空（按客户预测）
  material_id   uuid                   -- 可空（按物料预测）
  period_code   varchar(8) 必填
  forecast_amount numeric(18,2)
  confidence    varchar(16)            -- HIGH / MEDIUM / LOW
  version_no    int 默认 1
  -- 审计字段
```

#### 15.8.2 达成率看板

```
接口: GET /api/business/erp/sales-target/achievement?period_code=&scope_type=
返回: [
  { scope, target_amount, actual_amount, achievement_rate, gap }
]

actual_amount 取数口径：
  · 按订单金额（APPROVED 及之后状态）或
  · 按已收款金额（财务口径）
  两个口径都要给，前端可切换
```

---

### 15.9 明确不做

| 项 | 原因 | 决策人 | 决策日期 |
|---|---|---|---|
| **客户门户 / 电商对接** | 产品定位问题，暂不做 | 用户 | 2026-11-06 |

**其余 v1.0 排除项全部纳入本章**：销售合同（§15.1）、退货（§15.2）、价格管理（§15.3）、销售员佣金（§15.4）、信用额度释放（§15.5）、多币种（§15.6）、客户对账单（§15.7）、销售预测（§15.8）。

---

### 15.10 扩展功能实施计划（19 张卡）

| # | 卡 | 类型 | 预计 |
|---|---|---|---|
| **销售合同（2）** | | | |
| SO-26 | 合同放开 SALES + 客户主数据关联 | 后端 | 1h |
| SO-27 | 销售订单关联合同 + 金额控制 + 前端合同类型放开 | 前后端 | 1.5h |
| **退货（4）** | | | |
| SO-28 | 退货单表 + 迁移 | 后端 | 1h |
| SO-29 | 退货后端 API + 状态机 | 后端 | 1.5h |
| SO-30 | 退货过账（回库+冲应收+红字凭证） | 后端 | 1.5h |
| SO-31 | 退货单前端页面 | 前端 | 1.5h |
| **价格管理（3）** | | | |
| SO-32 | 价格表 + 迁移（客户协议价 + 阶梯价） | 后端 | 1h |
| SO-33 | 价格解析接口 + 取价规则 | 后端 | 1.5h |
| SO-34 | 价格管理前端（协议价维护 + 订单自动带价） | 前端 | 1.5h |
| **销售员佣金（3）** | | | |
| SO-35 | 销售员字段 + 佣金规则表 + 迁移 | 后端 | 1h |
| SO-36 | 佣金计提（按收款计佣）+ 结算单 | 后端 | 1.5h |
| SO-37 | 佣金前端（规则配置 + 结算查询） | 前端 | 1.5h |
| **信用额度（1）** | | | |
| SO-38 | 额度占用/释放/重算 + 不变量 S9 | 后端 | 1.5h |
| **多币种（4）** | | | |
| SO-39 | 币种 + 汇率表 + 迁移 | 后端 | 1h |
| SO-40 | 销售单据双币字段 + 汇率锁定 | 后端 | 1.5h |
| SO-41 | 凭证本位币入账 + 汇兑损益重估 | 后端 | 1.5h |
| SO-42 | 币种汇率前端维护页 | 前端 | 1h |
| **对账单 + 预测（2）** | | | |
| SO-43 | 客户对账单（查询 + Excel 导出 + 确认） | 前后端 | 1.5h |
| SO-44 | 销售目标与预测（表 + 达成率看板） | 前后端 | 1.5h |

**扩展工时**：约 27 小时。
**加上第一批 25 张（30 小时）**：**总计 44 张卡，约 55 小时**（串行）。

---

### 15.11 扩展功能的验收补充

在 §12.2 的 10 步端到端基础上，追加：

```
⑪ 销售合同：签合同 → 建订单关联 → 验证合同金额控制
⑫ 退货：发货后建退货单 → 过账 → 验证库存回库、应收冲减、红字凭证
⑬ 价格：配客户协议价 → 建订单 → 验证自动带出协议价（不是标准价）
⑭ 佣金：收款后跑佣金计提 → 验证按已收金额计算、结算单正确
⑮ 信用额度：下单占用 → 收款释放 → 验证 credit_used 归零
⑯ 多币种：USD 建单（汇率 7.2）→ 验证本位币金额 = 原币 × 汇率、凭证按本位币
⑰ 对账单：生成对账单 → 验证期初+发生=期末、Excel 导出正确
⑱ 销售目标：配目标 → 验证达成率计算
```

---

**本文是销售模块唯一权威文档。任何销售相关实现以本文为准，其他文档提及销售处一律引用本文。**

# 云枢（CloudPivot）ERP 第 2 批 —— 产品需求文档（PRD）

| 项 | 内容 |
|---|---|
| 文档版本 | v1.0（评审稿） |
| 撰写人 | product-manager |
| 日期 | 2026-10-31 |
| 产品 | 云枢后台 · ERP 模块**第 2 批：销售侧闭环（进销两头通）** |
| 技术底座 | Rust（nik 框架）+ Vue 3 + TDesign Vue Next + Pinia + Vite + PostgreSQL |
| 本批范围 | 客户档案与信用 → 销售订单 → 销售出库（移动加权成本结转）→ 应收 → 收款与核销 → 记账凭证 → 总账/科目余额表（复用批次 1）→ 销售与应收报表 |
| **不含范围** | 发票管理、销售退货、生产/BOM、期末结账与三大报表、多币种、客户价格协议、坏账计提、物流发货 |
| 上游前置 | 批次 1（采购 → 收货 → 应付 → 凭证 → 总账，53/53 已交付）；批次 D 补丁（调拨 / 序列号 / 库存盘点 / 菜单重组） |
| 交付物 | `docs/ERP-批次2-产品需求PRD.md` |
| 下游消费对象 | architect（P3-D2 表结构/接口/事务边界）、backend-developer（出库成本结转、核销引擎、凭证规则扩展）、frontend-developer（6 个新页面）、test-enginner（第八章 6 个验收场景可直接转用例） |
| 行数 | 见附录 C.2 自检（目标 ≥1500 行；批次 1 PRD 为 2186 行） |

## 阅读指引

- **开发**：先读第 0 章（真实库/真实代码核查出来的 34 条事实 + 7 条文档偏差），再读第三章（14 个决策，每条都有候选对比表与结论），然后读第四章（13 条业务规则，每条都有公式 + 算例 + 边界）。这三章是硬约束，其余章节是展开。
- **测试**：直接读第八章，6 个场景每一步都有明确数字，可逐条转用例；错误文案总表在 5.6。
- **评审 / 老大**：读第一章（谁用、怎么用）+ 第二章（做什么、哪些先做）+ 第三章 3.0 决策速览表 + 附录 B（需要你拍板的 5 条）。
- **架构 / 后端**：附录 A 是 11 张新表的字段口径（**字段表，不是 DDL** —— DDL 由 P3-D2 技术架构给），第十章是与批次 1 的衔接边界。

---

# 第 0 章 前置事实核查（本 PRD 的落地依据）

> 本章的每一条都是**实测**，不是推测。核查手段三类：
> ① **实库**：`172.18.10.144` 上的 `cloudpivot` 库（批次 1 + 批次 D 全部迁移已应用），用 `information_schema.columns` / `pg_constraint` / `pg_indexes` / `schema_migrations` 逐表导出；
> ② **实码**：`/opt/cloudpivot` 仓库 `main`（HEAD `fe84821`）的 `backend/src/business/erp/*.rs` 与 `backend/migrations/business/V0145–V0155`；
> ③ **文档**：`docs/ERP-批次1-产品需求PRD.md`（2186 行）、`docs/ERP-批次1-技术架构.md`（2895 行）。
>
> **凡本章结论与批次 1 文档冲突，一律以本章为准**（偏差逐条列在 0.4，并且每条都说明了为什么不许改实库）。

## 0.1 实库表结构事实（F1–F18）

| # | 核查结论 | 证据（可复现的查询） | 对本批的约束 |
|---|---|---|---|
| **F1** | `public` schema 共 **42 张 `biz_` 表**。ERP 域 **23 张**：`biz_account` `biz_accounting_period` `biz_inventory` `biz_inventory_location` `biz_inventory_txn` `biz_location` `biz_material` `biz_material_serial` `biz_payable` `biz_purchase_order` `biz_purchase_order_item` `biz_seq` `biz_stock_in` `biz_stock_in_item` `biz_stock_transfer` `biz_stock_transfer_item` `biz_stock_transfer_item_serial` `biz_supplier` `biz_unit` `biz_voucher` `biz_voucher_entry` `biz_voucher_rule` `biz_warehouse`；其余 19 张属审批域 `biz_flow*`（9）、任务域 `biz_task*`（8）、项目域 `biz_project*`（2） | `select count(*) from information_schema.tables where table_schema='public' and table_name like 'biz\_%'` → **42** | 本批新增表**一律 `biz_` 前缀**、只落 ERP 域，**不碰**任务/审批/项目域；**没有 `biz_customer`**，客户是全新主数据 |
| **F2** | `biz_purchase_order`（订单主表）列：`id` `order_no` `supplier_id` `order_date` `expect_date` `warehouse_id` `status` `currency_code`(char3,默认 CNY) `total_qty` `total_amount` `total_tax` `total_with_tax` `credit_days` `remark` `submitted_at/by` `approved_at/by` `closed_at/by` + 审计五件套；`price_mode`(varchar8, 默认 `EXCL`)。CHECK：`ck_biz_po_status`（`DRAFT`/`SUBMITTED`/`APPROVED`/`PARTIAL_RECEIVED`/`RECEIVED`/`CLOSED`/`CANCELLED`）、`ck_biz_po_tax_sum`（`total_with_tax = total_amount + total_tax`）、`ck_biz_po_price_mode`（`EXCL`/`INCL`）、`ck_biz_po_amount`、`ck_biz_po_qty`、`ck_biz_po_credit` | `information_schema.columns` + `pg_constraint where conrelid='biz_purchase_order'::regclass` | **销售订单必须逐字镜像这套结构**：`supplier_id`→`customer_id`，状态值换成 `PARTIAL_OUT`/`OUTBOUND`，**三个金额列名与恒等式一个字不改** |
| **F3** | `biz_purchase_order` 索引：`uk_biz_po_no`（**部分唯一** `WHERE deleted_at IS NULL`）+ `idx_biz_po_date/status/supplier/warehouse` | `pg_indexes` | 销售订单同样建 `uk_biz_so_no`（部分唯一）与 4 条同类索引；**单号唯一必须带 `deleted_at IS NULL` 条件**，否则软删后重号报错 |
| **F4** | `biz_purchase_order_item`：`line_no` `material_id` `unit_id` `qty`(18,4) `price`(18,6) `price_incl`(18,6) `tax_rate`(7,4) `amount`(18,4) `tax_amount`(18,4) `with_tax_amount`(18,4) `received_qty`(18,4) `remark`；CHECK `ck_biz_po_item_qty`（`qty>0`）、`ck_biz_po_item_tax`（`with_tax_amount = amount + tax_amount`）、`ck_biz_po_item_tax_rate`（`0 ≤ tax_rate ≤ 100`，**百分数口径**）、`ck_biz_po_item_recv`（`0 ≤ received_qty ≤ qty`）；`uk_biz_po_item_line`（`order_id, line_no`）；外键 `order_id → 主表 ON DELETE CASCADE` | 同上 | 销售明细镜像为 `shipped_qty`，CHECK 写 `0 ≤ shipped_qty ≤ qty`（**部分出库的合法上限**）；`tax_rate` 在单据行是**百分数**（13 表示 13%） |
| **F5** | `biz_stock_in`（收货单）：`in_no` `in_type` `source_order_id` `supplier_id` `warehouse_id`（**仓库在表头**）`in_date` `status` `total_qty/amount/tax/with_tax` `invoice_received` `voucher_id` `payable_id` + `posted_at/by` `cancelled_at/by` `cancel_reason` + 审计。CHECK：`ck_biz_si_status`（`DRAFT`/`POSTED`/`CANCELLED`）、`ck_biz_si_type`（`PURCHASE`/`RETURN`/`OTHER`）、`ck_biz_si_supplier`、`ck_biz_si_tax`、`ck_biz_si_qty` | `information_schema.columns` + `pg_constraint` | 出库单镜像：`out_no` `out_type` `source_order_id` `customer_id` `warehouse_id` `out_date` `status` `total_*` + **`voucher_id`（收入凭证）与 `voucher_cost_id`（成本凭证）两个指针** + `receivable_id`；状态只用 `DRAFT`/`POSTED`/`CANCELLED` 三值（**不引入审批态**，理由见决策 ⑧） |
| **F6** | `biz_stock_in_item`：`source_item_id` `material_id` `unit_id` `qty` `price` `price_incl` `tax_rate` `amount` `tax_amount` `with_tax_amount` `cost_price` `location_id` `lot_no` `remark` | `information_schema.columns` | 出库明细除同名字段外，**必须带 `cost_price`(18,6) 与 `cost_amount`(18,4)** —— 出库成本一旦生成就要能追溯，不能只留在流水里（决策 ②/规则 4.5） |
| **F7** | `biz_payable`（应付）：`payable_no` `supplier_id` `source_type`（`STOCK_IN`）`source_id` `source_no` `invoice_no/date` `currency_code` `amount` `paid_amount` `unpaid_amount` `credit_days` `due_date` `period_code` `status` `settled_at` `voucher_id`。CHECK：`ck_biz_payable_unpaid`（`unpaid_amount = amount − paid_amount`）、`ck_biz_payable_paid`（`0 ≤ paid_amount ≤ amount`）、`ck_biz_payable_settled`（`SETTLED → unpaid=0`）、`ck_biz_payable_status`（`OPEN`/`PARTIAL_PAID`/`SETTLED`/`CANCELLED`）、`ck_biz_payable_amount`、`ck_biz_payable_credit`。索引 `uk_biz_payable_no`、**`uk_biz_payable_source`（`source_type, source_id` 部分唯一）** | 同上 | 应收单 `biz_receivable` **逐字镜像**：`paid_amount`→**`received_amount`**、`supplier_id`→`customer_id`；同样建 `uk_biz_receivable_source` 保证「**一张出库单只能有一张应收单**」（数据库层面兜住重复生成） |
| **F8** | `biz_voucher`（凭证）：`voucher_word` `voucher_no` `period` `voucher_date` `voucher_type`(`AUTO`/`MANUAL`) `source_type` `source_id` `source_no` `summary` `total_debit` `total_credit` `entry_count` `status`(`POSTED`/`DRAFT`/`AUDITED`/`CANCELLED`) `is_red`(smallint) `reverse_of` `reversed_by` `posted_at/by`。CHECK 共 8 条：`ck_biz_voucher_balance`（`total_debit = total_credit`）、`ck_biz_voucher_entries`（`entry_count ≥ 2`）、`ck_biz_voucher_positive`、`ck_biz_voucher_red`（`is_red ∈ {0,1}`）、**`ck_biz_voucher_red_ref`（`is_red=1 → reverse_of IS NOT NULL`，反之必须为 NULL）**、`ck_biz_voucher_self`、`ck_biz_voucher_status`、`ck_biz_voucher_type` | `pg_constraint` on `biz_voucher` | ①收入凭证与成本凭证**共用同一套表**，靠 `source_type` 区分（决策 ⑦）；②红字必须挂 `reverse_of`，且原凭证要回写 `reversed_by`——**两张都要写，不能只写一头** |
| **F9** | `biz_voucher_entry`（分录）：`line_no` `summary` `account_id` `account_code` `account_name`（**编码+名称快照**）`debit_amount`/`credit_amount` 均为 **`NUMERIC(20,4)`** `aux_supplier_id` `aux_material_id` `aux_warehouse_id` `aux_dept_id` `aux_qty`(18,4) `aux_extra`(jsonb)。CHECK：`ck_biz_voucher_entry_side`（**单边**：一列 >0 另一列 =0）、`ck_biz_voucher_entry_positive`；唯一 `uk_biz_voucher_entry_line`（`voucher_id, line_no`） | 同上 | **没有 `aux_customer_id` 列** → 本批必须 `ALTER TABLE biz_voucher_entry ADD COLUMN aux_customer_id UUID`（可空、无默认值，PG 秒级）；批次 1 技术架构 §6.2 已预判这件事 |
| **F10** | `biz_voucher_rule` 是**扁平三科目**结构：`rule_code` `biz_type` `debit_account_id` `credit_account_id` `tax_account_id` `is_active`(smallint)。**没有** `rule_entry` 子表、**没有** `event_type`/`status`/`priority`、**没有**税额方向字段 | `information_schema.columns` + `select * from biz_voucher_rule`（实库**只有 1 行**：`PURCHASE_IN_DEFAULT` / `PURCHASE_IN` / 借 1405 / 贷 2202 / 税 2221.01） | 批次 1 的分录形状是「借 借方科目 + **借** 税科目 + 贷 贷方科目」。销售是「借 应收 + 贷 收入 + **贷** 销项税」，**税额方向相反** → 规则表必须补 `tax_side`（决策 ⑦ / 迁移 V0162）；**一个 `biz_type` 只能生成一张固定形状的凭证** → 收入与成本必须两个 `biz_type` |
| **F11** | `biz_account`（科目）：`account_code` `account_name` `parent_id` `level` `account_category` `balance_direction` `is_leaf` `aux_dimensions` `status` … 实库**只有 4 行**：`1405 库存商品`(ASSET/DEBIT/level1/leaf)、`2202 应付账款`(LIABILITY/CREDIT/level1/leaf)、`2221 应交税费`(LIABILITY/CREDIT/**level1/非末级**)、`2221.01 应交税费-进项税`(LIABILITY/CREDIT/level2/leaf)。科目编码体系 = **一级 4 位 + `.` + 2 位**，`2221.01` 已是末级 | `select account_code,account_name,account_category,balance_direction,is_leaf,level,id from biz_account` | ①**销项税编码是 `2221.02`**（不是 `2221.01.05`，也没有三级科目）；②新增科目必须遵守「父级存在 + 父级置非末级」；③科目种子用固定 UUID `d1370004-0000-4000-8000-0000000000NN`，存量已占 `01`–`04` → 本批顺延 `05` 起 |
| **F12** | `biz_seq`（取号）：`seq_type` `seq_name` `prefix` `date_pattern` `period_key` `current_no` `step` `separator` `status`。实库 **6 行**：`AP`(应付) / `IN`(收货入库) / `PO`(采购订单) / `PZ`(凭证) / `SN`(序列号) / `TK`(调拨)。`date_pattern='yyyyMMdd'`、`separator='-'`、`step=1`、`status=1`；单号实际形如 `IN-20261031-000004`（**日号 + 6 位**，按日重置） | `select * from biz_seq`；`select in_no from biz_stock_in` | **本批新增 4 条**：`SO`（销售订单）/`OUT`（销售出库）/`AR`（应收）/`RC`（收款），格式 `{前缀}-yyyyMMdd-000001`；**不许改成月号**（偏差 X1） |
| **F13** | `biz_inventory`：`material_id` `warehouse_id` `location_id` `qty`(18,4) `locked_qty`(18,4) `avg_cost`(18,**6**) `amount`(18,4) `last_txn_at`。约束**只有** `ck_biz_inv_locked`（`locked_qty ≥ 0`）→ **`qty` 没有非负约束，可以为负**。唯一键 `uk_biz_inv_mat_wh_loc (material_id, warehouse_id, COALESCE(location_id, '00000000-…-000000000000'))` | `pg_constraint` + `pg_indexes` on `biz_inventory` | ①出库扣减的定位键就是这个**三元唯一键**（NULL 库位用全零 UUID 兜住）；②`qty` 可为负 = 「负库存」在数据库层是**合法状态**，必须在应用层用 `allow_negative` 拦（决策 ②） |
| **F14** | `biz_inventory_txn`（库存流水）：`txn_type`(`IN`/`OUT`) `biz_type` `doc_type` `doc_id` `doc_no` `line_no` `qty` `unit_cost`(18,6) `amount`(18,4) + **before/after 三量快照**（`before_qty/amount/cost`、`after_qty/amount/cost`）`period_code` `operator_id` `remark`。CHECK：`ck_biz_inv_txn_qty`（**`qty > 0`，禁负数和 0**）、`ck_biz_inv_txn_amt`（`amount ≥ 0`）、`ck_biz_inv_txn_type`；`biz_type` **没有 CHECK 枚举**（加值不用迁移） | 同上 | ①出库写 `txn_type='OUT'` + `qty` 正数，方向由 `txn_type` 表达；②作废红冲写 **`txn_type='IN'`**（不是负数）；③`biz_type` 直接写 `SALE_OUT`（技术架构 §6.2 已预留），成本红冲写 `SALE_OUT_CANCEL` |
| **F15** | `biz_material_serial`（一物一码）：`serial_no` `material_id` `warehouse_id` `location_id` `batch_no` `status`(`IN_STOCK`/`OUTBOUND`/`SCRAPPED`) `inbound_at` `outbound_at` `source_type` `source_id` + 审计（**有 `deleted_at`**）。索引 `uk_biz_material_serial_no`（**部分唯一** `WHERE deleted_at IS NULL`）、`idx_…_material/wh_loc/status`（均带软删条件） | `pg_indexes` / `pg_constraint` | 出库时改 `status='OUTBOUND'` + `outbound_at` + `source_type='SALE_OUT'` + `source_id=出库单 id`；**`warehouse_id` 不清空**（保留最后位置，售后要能反查）；序列号**不可软删再重用同号**（唯一键带软删条件，等价于「号可以复用但历史行还在」——产品上按**号永不重用**执行） |
| **F16** | `biz_warehouse`：`warehouse_code` `warehouse_name` `warehouse_type`(`NORMAL`/`RAW`/`FINISHED`/`SCRAP`/`IN_TRANSIT`) `manager` **`allow_negative`(smallint 0/1)** `parent_id` `level`(1–3) `warehouse_path` `is_leaf` `status`。CHECK：`ck_biz_warehouse_neg`（`allow_negative ∈ {0,1}`）、`ck_biz_warehouse_level`、`ck_biz_warehouse_root`、`ck_biz_warehouse_no_self`、`ck_biz_warehouse_leaf`、`ck_biz_warehouse_type` | `pg_constraint` on `biz_warehouse` | **负库存开关已经存在于实库**（不用新增字段，决策 ② 直接用）；仓库是**三层树**，出库明细可指定库位但**只有 `is_leaf=1` 的仓可收发**（批次 1 已定） |
| **F17** | `biz_material`：`material_code` `material_name` `spec` `unit_id` `material_type`(`RAW`/`SEMI`/`FINISHED`/`SERVICE`) **`costing_method`**(`MOVING_AVG`/`FIFO`/`STANDARD`) `default_warehouse_id` `safety_stock` `inventory_account_id` **`tax_rate` `NUMERIC(7,4)` 且 `CHECK 0 ≤ tax_rate ≤ 1`（比例口径！）** `status` **`serial_required`(boolean)** `external_sn` | `pg_constraint` on `biz_material`；实例：`M-W1A-004 成品D` `serial_required=t` `tax_rate=0.1300` | ⚠️ **税率两套单位**：`biz_material.tax_rate` 是**比例**（0.13），订单/入库明细 `tax_rate` 是**百分数**（13）。本批做「物料带出税率」时**必须 ×100**，否则算出 0.13% 的税（这是最容易犯的错，见 4.1.6 边界） |
| **F18** | `biz_supplier`：`supplier_code` `supplier_name` `short_name` `contact_person` `contact_phone` `email` `tax_no` `address` `credit_days`(int) **`credit_limit`(18,4)** **`payable_account_id`** `invoice_title` `invoice_bank_name` `invoice_bank_account` `status` | 同上 | **客户表就是供应商表的镜像**：`credit_limit`/`credit_days`/`tax_no` 三件套直接照抄结构，把 `payable_account_id` 换成 `receivable_account_id`；**`credit_limit` 就是信用额的权威存储（不加第二张额度表，理由见决策 ⑧）** |
| **F19** | `biz_accounting_period`：`period_code` `period_year` `period_month` `start_date` `end_date` `status`(`NOT_OPENED`/`OPEN`/`CLOSED`) `closed_at/by`；实库已有 `2026-10` 且 `status=OPEN` | `select * from biz_accounting_period` | 期间三态实库已定，本批**只读校验**：`status='CLOSED'` 拒绝写凭证；`NOT_OPENED` 也要拒绝（批次 1 文档只写了已结账，这里补严） |

## 0.2 实库数据基线（F20–F23，可直接当验收的「起跑线」）

| # | 结论 | 证据 |
|---|---|---|
| **F20** | `cloudpivot` 库里已有可用的**主数据**：仓库 `WH01 总仓`(level1,非末级)、`WH02 分仓`(level2,末级)、`WH-TRANSIT 在途仓`；物料 `M-W1A-001 原料A`…`M-W1A-005 成品E`（`M-W1A-004 成品D` 是 `serial_required=t` 的一物一码物料，税率 0.13）；计量单位 `PCS/BOX/KG`；库位 `A-01-01`…`RECV-01`（均在 `WH02`） | `select warehouse_code,warehouse_name,allow_negative,level,is_leaf from biz_warehouse` / `select material_code,serial_required,tax_rate from biz_material` |
| **F21** | 已有一笔**完整采购闭环**可作对照：`IN-20261031-000003` 与 `000004` 状态 `POSTED`（价税合计各 226.00），对应应付 `AP-20261031-000001`（226.00 / 已付 0 / `OPEN` / 到期 `2026-11-30`）与凭证 `PZ-20261031-000001`（3 行分录、`STOCK_IN`、`POSTED`）；库存 3 行（`M-W1A-001`@WH02 200 件 / 400.00 / `avg_cost=2.000000` 等） | `select in_no,status,total_with_tax from biz_stock_in`；`select payable_no,amount,unpaid_amount,status,due_date from biz_payable`；`select voucher_no,entry_count,source_type from biz_voucher`；`select material_id,warehouse_id,qty,amount,avg_cost from biz_inventory` |
| **F22** | 序列号池：`IN_STOCK` 8 件、`OUTBOUND` 2 件、`SCRAPPED` 1 件 —— 出库验收**不要自己造数据**，直接用这 8 件在库的；`biz_inventory_location` 目前 **0 行**（库位库存未启用，出库一律落「整仓汇总行」`location_id IS NULL`） | `select status,count(*) from biz_material_serial group by 1`；`select count(*) from biz_inventory_location` |
| **F23** | 角色只有 5 个：`SUPER_ADMIN` / `MUTEX_A`(制单员) / `MUTEX_B`(审核员) / `QA_LIMITED` / `QA_VIEWER`。**批次 1 的迁移里写的 `FINANCE`/`ACCOUNTANT`/`ADMIN` 三个角色在本库不存在** → 那些授权语句是**空操作**（0 行），实际只有超管能看到应付 | `select role_code from sys_role where deleted_at is null`；对照 `V0151__erp_payable_permissions.up.sql` 的 `role_code IN (...)` 列表 | 本批菜单种子的授权段**必须**同时写「**真实存在**的角色 + 期望角色（空操作不报错）」，并在附录里点明「上线前要么建角色，要么只给超管」 |

## 0.3 实库与实码事实（F24–F32）—— 本批必须复用、不许重写的部分

| # | 结论 | 证据（文件:行） | 对本批的约束 |
|---|---|---|---|
| **F24** | 凭证引擎按 **`biz_type`** 取规则：`SELECT debit_account_id, credit_account_id, tax_account_id FROM biz_voucher_rule WHERE biz_type=? AND is_active=1 AND deleted_at IS NULL ORDER BY created_at ASC LIMIT 1`。采购入库的分录形状固定为「借 debit（价款）→ 借 tax（税额，为 0 不出行）→ 贷 credit（价税合计）」 | `backend/src/business/erp/voucher.rs:330-361`（`load_rule`）、`voucher.rs:80-105`（常量：`BIZ_TYPE_PURCHASE_IN`/`SOURCE_STOCK_IN`/`WORD_DEFAULT='记'`/`STATUS_POSTED`/`DEFAULT_INVENTORY_CODE='1405'`/`DEFAULT_PAYABLE_CODE='2202'`/`DEFAULT_TAX_CODE='2221.01'`） | ①「一个 `biz_type` = 一张固定形状的凭证」是硬约束 → **收入凭证与成本凭证必须是两个 `biz_type`**；②税额行方向必须参数化（`tax_side`），否则销项税会被记到借方，试算立刻不平 |
| **F25** | 红冲（作废）的实码是**借贷互换 + 行号重排 + 摘要前缀「红冲 」**，不是「金额取负」——因为 `ck_biz_voucher_entry_positive` 与 `ck_biz_voucher_entry_side` 不允许负数 | `voucher.rs:44-51`（文件头注释写明「卡面要求金额取负，但同卡面 CHECK 金额≥0，因此改为互换」） | 本批所有红字（出库作废、收款作废）**一律借贷互换**，且 `is_red=1` + `reverse_of` + 原凭证 `reversed_by` 三处都要写（偏差 X3） |
| **F26** | 应付的实码生成口径：`payable_no` 取号 → `credit_days` **从供应商主数据取**（`biz_supplier.credit_days`）→ `due_date = 单据日期 + credit_days` → `period_code` 由 `voucher::period_of(过账日期)` 解析 → `status='OPEN'`、`paid_amount=0`、`unpaid_amount=amount`、`amount` 先 `round_dp(4)` | `backend/src/business/erp/payable.rs:220-310`（`insert` / `create_for_stock_in` / `credit_days_of`） | 应收**逐条照抄这套口径**：账期取 `biz_customer.credit_days`，到期日 = 出库日期 + 账期，金额 = 出库单价税合计，状态 `OPEN`。**不许自己发明"到期日取月末"之类的规则** |
| **F27** | 出库扣减的实码已有（调拨发货在用）：`UPDATE biz_inventory SET qty = qty − n, amount = amount − ROUND(n × avg_cost, 4) WHERE id = ? AND qty >= n` —— **条件 UPDATE，靠受影响行数挡并发超卖**；金额在 **SQL 侧 `ROUND(…, 4)`** 计算（不是 2 位） | `backend/src/business/erp/transfer_model.rs:1847-1893`（`deduct_inventory_row`，文件头解释「Rust 的 Decimal 舍入与 PG numeric 不同，各算一遍会在半分位差 1 分」） | ①本批出库**复用这条路径**，不新写成本引擎；②**注意：调拨这条路径硬拦负库存，没有 `allow_negative` 分支** —— 销售出库要按决策 ② 补上，但**不许改调拨的行为**（回归必须过） |
| **F28** | 一物一码物料的**账面数量是派生值**：`recompute_quantity` 把 `biz_inventory.qty` 直接置为 `COUNT(serial where status='IN_STOCK')`，`amount` 置为 `qty × avg_cost`；序列号状态流转（`IN_STOCK→OUTBOUND`）时会自己写一条 `OUT` 流水（`biz_type='SERIAL_OUT'`、`doc_type='MATERIAL_SERIAL'`） | `backend/src/business/erp/serial_model.rs:592-629`（`recompute_quantity`）、`715-735`（`link_inventory`）、`658-707`（`post_serial_txn`，流水金额 = `qty × avg_cost`） | ⚠️ **本批最大的坑**：一物一码物料**不能再走 `qty = qty − n`**（会被下一次 `recompute_quantity` 覆盖，且会和序列号件数打架）→ 出库必须**按物料类型分流两条扣减路径**（决策 ⑬） |
| **F29** | 序列号状态机**单向不可逆**：只允许 `IN_STOCK→OUTBOUND`、`IN_STOCK→SCRAPPED`、`OUTBOUND→SCRAPPED`；`OUTBOUND→IN_STOCK` **明确禁止**，并有单测 `assert!(!can_transition("OUTBOUND","IN_STOCK"), "出库不能回库")` | `serial_model.rs:347-375`（`can_transition`）、`serial_model.rs:1243-1265`（单测） | **销售退货若要做，就必须打破这条已上线且有单测的规则** → 这是决策 ⑥「本批不做退货」的决定性理由 |
| **F30** | 调拨发货的同事务不变式（可直接照抄给销售出库）：①动过的库存行 `qty ≥ 0`；②`serial_required` 物料「账面数量 == 在库序列号件数」；③成本守恒 `SUM(OUT amount) == SUM(IN amount)` | `transfer_model.rs:2129-2200`（`assert_settle_invariants`） | 销售出库的等价不变式：①`qty ≥ 0`（或仓库允许负库存时显式放行）；②一物一码 `qty == COUNT(IN_STOCK)`；③**成本凭证 6401 合计 == Σ 出库流水 amount（按 2 位对齐规则，见 4.5）** |
| **F31** | **菜单结构（今日刚变过，必须按最新）**：`commit 8fa2d1e「feat(menu): 去掉 ERP 顶级目录，4 个业务目录提到根级」` 之后，**已不存在 ERP 顶级目录 0940**。现在 4 个 ERP 业务目录都挂在**根级**（`parent_id IS NULL`）：`0946 采购管理`(sort=62, auth_code=`erp-purchase`) / `0e01 仓库管理`(66, `erp-warehouse`) / `0e02 财务管理`(72, `erp-finance`) / `0e03 基础数据`(76, `erp-master-data`)。权限码唯一来源是 `sys_menu.auth_code`（**没有 `sys_permission` 表**），共 **61 条** `business:erp:%`，格式 `business:erp:{资源}:{动作}`，动作实测 `list/detail/create/update/delete/status/audit/view/generate/trial-balance`；`sys_menu` id 段 `1000–1010` 已用（1006 空号），1010 = `payable:detail` | `select right(id::text,4),parent_id,menu_type,auth_code,path,sort_order,title from sys_menu`；`select count(*) from sys_menu where auth_code like 'business:erp:%'` → 61；`git log --oneline -2`（`8fa2d1e`） | 本批新增的「销售管理」必须是**根级目录**（`parent_id=NULL`，sort 插在 62 与 66 之间取 64），**不许再挂到 0940 下**（该行已不存在，挂上去会变成孤儿菜单）；id 从 `1011` 起；授权走 `sys_role_menu` |
| **F32** | 前端页面目录是 `frontend/src/views/business/erp/{ledger,material,payable,purchase,serial,shared,stockin,transfer,voucher,warehouse}`（**小写、无连字符**），api 层在 `frontend/src/api/business/erp/`；`sys_menu.component` 存的就是 `business/erp/{page}/index` 形式 | `ls frontend/src/views/business/erp/` | 本批 6 个新页面按同风格命名：`customer` `saleorder` `stockout` `receivable` `receipt` `report`；`component` 写 `business/erp/saleorder/index`（**不是** `sale-order`）—— 若按 URL 里的连字符命名，前端路由会找不到文件 |

## 0.4 与批次 1 文档的 7 处已知偏差（**必须按实库/实码执行**）

| # | 批次 1 文档怎么写 | 实库 / 实码是什么 | 本批采用 | 影响面 |
|---|---|---|---|---|
| **X1** | 单号 `PO-202610-0001`（月号 `YYYYMM`，`PO/GR/AP` 前缀） | `IN-20261031-000004`（**日号 `YYYYMMDD` + 6 位**，`biz_seq.date_pattern='yyyyMMdd'`；收货单前缀是 `IN` 不是 `GR`） | **日号 + 6 位**；销售侧 `SO/OUT/AR/RC` | 所有单号示例、验收场景、测试断言 |
| **X2** | 销项税 `2221.01.05`、进项税 `2221.01.01`（三级科目） | 科目只有两级，`2221.01` 即末级 | **进项 `2221.01` / 销项 `2221.02`** | 科目种子、凭证规则、验收 |
| **X3** | 红字凭证「金额取负」 | 借贷互换（因 CHECK 禁负数） | **借贷互换** | 作废/红冲全部 |
| **X4** | 出库金额 `round(qty × avg_price, 2)`（2 位） | `biz_inventory.amount` 是 `NUMERIC(18,4)`，实码 `ROUND(n × avg_cost, 4)` | **库存金额 4 位；单据/凭证金额 2 位**，两者之间用「末行倒挤」对齐（4.5.3 给算例） | 出库成本、成本凭证 |
| **X5** | 表名 `erp_item` / `erp_supplier` / `erp_receivable` / `erp_stock_flow` | `biz_material` / `biz_supplier` / （无）/ `biz_inventory_txn` | **`biz_` 前缀** | 全文 |
| **X6** | 结存单价列 `avg_price` | `biz_inventory.avg_cost`（`NUMERIC(18,6)`） | **`avg_cost`** | 全文 |
| **X7** | `locked_qty`（锁定量）「本批恒 0，为第 2 批销售占用预留」 | 列存在，`ck_biz_inv_locked` 只要求 `≥ 0`；调拨与序列号逻辑**从不写它** | **本批不启用 `locked_qty`**（决策 ⑫ 给了 3 条理由） | 可用量口径、信用/库存查询 |

> 这 7 条不是「改设计」，是**文档追平实现**。开发的硬红线是：与实库 DDL / 实码一致；同时与批次 1 的**金额口径、成本法（移动加权）、尾差规则**保持一致 —— 这三件事本批**一个字不改**。

---
# 一、用户角色与真实场景

## 1.1 角色总览

批次 1 解决的是「钱怎么出去」，本批解决「钱怎么回来」。
买货的人和卖货的人，在系统里是两个完全不同的岗：采购关心「别缺货」，销售关心「别超信」。
所以本批的角色不是批次 1 的复刻，而是**对着销售的日常**写的。

| 角色 | 岗位 | 在本批做什么 | 频率 | 最痛的一点（决定功能优先级） |
|---|---|---|---|---|
| **销售内勤 小刘** | 销售部内勤 / 跟单 | 客户建档、录销售订单、盯发货进度、催财务开票 | 每天 20–40 单 | 客户要货急，单子录完不知道仓库发没发；电话打三遍 |
| **仓管员 老陈** | 成品仓管 | 接单拣货、扫码出库、打印出库单随货走 | 每天 5–15 张出库单 | 一物一码的货，扫错一个号整单重来；纸质单据丢了没法反查 |
| **财务会计 张会计** | 往来会计 | 看应收、核销收款、对账、检查凭证与总账 | 每天核销 3–10 笔 | 客户打款不写单号，一笔钱要手工拆到 4 张单上 |
| **财务主管 陈主管** | 财务负责人 | 定信用额度、批超额放行、看账龄与毛利 | 每周 | 销售冲业绩，客户欠款没人管；账龄要靠 Excel 现拼 |
| **老板 王总** | 总经理（只读） | 看当月销售、毛利、应收余额 | 每周/每月 | 只想看三个数：卖了多少、赚了多少、还有多少钱没收回来 |
| **系统管理员** | IT / 实施 | 配科目、配凭证规则、配权限与菜单 | 上线时一次 | 规则配错导致过账全失败，且看不懂错在哪 |

> 角色设计的一条原则：**本批不新增"审批人"角色**。批次 D 已有可编辑工作流（`biz_flow_*` 9 张表），
> 但销售侧本批只保留两个审批动作（信用超额放行、调额），用「权限码 + 直接通过」实现，
> 不接工作流引擎（理由见决策 ⑧ 与第十章「不做清单」）。

## 1.2 端到端故事

### A. 销售内勤 小刘：一张订单从电话到发货（本批的主干链路）

周一上午 9:40，华东电子采购员打电话来：「上次那种成品 D 要 60 台，老价格，月底前送到。」

小刘打开 `销售管理 → 销售订单 → 新建`：

1. 客户一栏敲「华东」，下拉出现 `CUS-000001 华东电子科技有限公司`，
   右侧自动带出：信用额度 `100,000.00`、可用额度 `99,774.00`、信用期 `30 天`、结算方式 `月结`。
   —— 可用额度是实时算的：额度 − 未结应收（`Σ unpaid_amount`），见规则 4.7。
2. 表头带出口径徽标 `[不含税]`（默认，与批次 1 一致），仓库 `WH02 分仓`，订单日期 2026-10-31。
3. 加一行：物料 `M-W1A-004 成品D`，数量 60，单价 2,000.00（不含税），税率自动带出 `13`。
   —— 系统在单价框下面提示一行灰字：「近 90 天该客户成交价 2,000.00 / 2,050.00」，本批只提示不拦（决策 ⑨）。
4. 行金额自动算：价款 `120,000.00`、税额 `15,600.00`、价税合计 `135,600.00`。
5. 点「保存草稿」→ 单号立刻生成 `SO-20261031-000001`（**保存即取号**，与批次 1 同口径）。
6. 点「提交」→ 系统做**软性信用校验**：
   `135,600.00 > 99,774.00`，弹窗：

   ```
   ⚠ 客户 华东电子 超额 35,826.00（可用 99,774.00，本单 135,600.00）
     处理方式：
     ( ) 取消
     (•) 申请超额放行（需「信用超额审批」权限，放行后额度占用 135,600.00）
   ```
   小刘没有审批权限，点「申请超额放行」→ 单据进入 `SUBMITTED`，状态旁挂红标 `超额待批`。
   —— **下单只是软拦**（可提交、待批），真正的硬拦在出库（决策 ①）。

7. 10:05 陈主管收到待办（本批用「待办列表 + 消息」提示，不接工作流），看一眼客户历史回款记录，
   点「通过」→ 生成本单的信用例外（`biz_customer_credit_exception`，有效期到 2026-11-30，
   只对这一单生效）→ 订单转 `APPROVED`。

到这里，小刘做的事结束。**她不需要知道成本、不需要知道凭证**。

### B. 仓管员 老陈：60 台一物一码的货怎么扫出去

下午 14:20，老陈在 `销售管理 → 销售出库 → 从订单拉单`，选 `SO-20261031-000001`：

1. 系统把订单未发数量带进来（60），仓库 `WH02`，出库日期默认今天。
2. 第 1 行是 `M-W1A-004 成品D`，`serial_required=true` → 该行的「序列号」列**变成必填**，
   右侧出现扫码框和一行提示：

   ```
   已扫 0 / 需 60        可用在库 8 件（WH02）
   ```
   —— 「可用在库 8」是实时查的 `COUNT(biz_material_serial where status='IN_STOCK' and warehouse_id='WH02')`。
   只有 8 件，老陈先发 8 台（**部分出库**是本批的 P0 能力，规则 4.13）。

3. 老陈把扫码枪对准箱子，逐个扫：每扫一个，列表追加一行 `SN-20261031-00000X`，
   计数变成 `已扫 3 / 需 8`。重复扫同一个号，立刻红字：
   `该序列号已在本次出库中（第 1 行）`。
4. 扫满 8 个，过账按钮亮起。点「过账」→ 弹确认框，列三件事：

   ```
   本次出库：8 件 / 价款 16,000.00 / 税额 2,080.00 / 价税合计 18,080.00
   出库成本：8 × 2.000000 = 16,000.00（移动加权，当前均价 2.000000）
   过账后：生成应收 AR-20261031-000001（18,080.00，到期 2026-11-30）
           生成凭证 2 张（收入 3 行 / 成本 2 行）
   ```

5. 过账成功 → 出库单 `OUT-20261031-000001` 状态 `POSTED`；8 个序列号状态 `IN_STOCK → OUTBOUND`；
   库存 `WH02/成品D` 从 8 变 0；订单行 `shipped_qty` 从 0 变 8，订单状态 `PARTIAL_OUT`。
6. 老陈点「打印出库单」，A4 三联（存根/财务/随货），随货那联带二维码，客户签收后回传。

### C. 财务会计 张会计：一笔 18,080 的到账，拆到 3 张应收上

11 月 28 日，华东电子打款 18,080.00，银行流水备注只写了「货款」。

1. 张会计打开 `收款管理 → 新建收款单`：客户 `华东电子`，收款日期 2026-11-28，
   收款方式 `银行转账`，金额 18,080.00，收款账户 `1002 银行存款-工行`。
2. 点「按 FIFO 自动核销」→ 系统按**到期日早→晚**列出该客户全部未结应收，自动填核销金额：

   | 应收单号 | 到期日 | 应收 | 已收 | 本次核销 | 核销后 |
   |---|---|---|---|---|---|
   | `AR-20261015-000002` | 2026-11-14 | 565.00 | 0.00 | **565.00** | 0.00（`SETTLED`） |
   | `AR-20261020-000003` | 2026-11-19 | 226.00 | 0.00 | **226.00** | 0.00（`SETTLED`） |
   | `AR-20261031-000001` | 2026-11-30 | 18,080.00 | 0.00 | **17,289.00** | 791.00（`PARTIAL_PAID`） |
   | 合计 | | 18,871.00 | | **18,080.00** | 791.00 |

3. 张会计手工把第 3 行改回 0，把第 1、2 行也清零，全部 18,080 挂到 `AR-20261031-000001` 上
   —— **手工指定优先于 FIFO**（决策 ④）：客户明确说这笔是 10 月那批货的钱。
4. 点「审核」→ 系统在一个事务里：收款单 `POSTED`、3 张（实际 1 张）应收 `received_amount` 增、
   `unpaid_amount` 减、状态流转 `OPEN → PARTIAL_PAID`，写核销明细 `biz_receipt_item`，
   生成凭证 `借 1002 银行存款 18,080.00 / 贷 1122 应收账款 18,080.00`。
5. 张会计看「应收账款账龄表」：这笔 791.00 落在 `1–30 天` 桶里。

### D. 财务主管 陈主管：月末三个数

11 月 30 日晚上，陈主管打开 `销售报表`：

1. **销售明细表**（按客户/物料/期间）：11 月卖了 8 件成品 D，价款 16,000.00，税额 2,080.00。
2. **毛利分析**：价款 16,000.00 − 成本结转 16,000.00 = 毛利 0.00，毛利率 0.00%。
   —— 陈主管立刻发现「这批货是平价出的」，去问销售为什么。这就是毛利分析存在的唯一理由：
   **把"卖了多少"和"赚了多少"放在同一张表上，让人一眼看出异常**。
3. **应收账龄**：未结 791.00（1–30 天桶），逾期 0.00，客户信用占用 791.00 / 100,000.00。

### E. 老板 王总：只看三个数

王总在首页看板看到三张卡片（本批只做数据，不做图形美化）：

```
本月销售（价税合计）  18,080.00
本月毛利              0.00   毛利率 0.00%
应收余额              791.00  其中逾期 0.00
```

点「应收余额」下钻到账龄表。**他不需要点进任何一张单据**，这是本批报表的设计目标。

---
# 二、功能清单（P0 / P1）

## 2.0 优先级定义与总量

| 优先级 | 定义 | 本批要求 |
|---|---|---|
| **P0** | 缺了它，业务链断掉或账不平 | 必须本批上线，验收必须覆盖 |
| **P1** | 缺了它，用户能凑合，但要手工补 | 本批排进去，若工期紧可顺延到批次 2.5，**但表结构与接口要预留** |
| P2 | 明确的下一代能力 | 本批**不做**，也不预留字段（防猜测式设计） |

本批共 **78 项功能**：P0 61 项、P1 17 项。

| 模块 | 名称 | P0 | P1 | 小计 |
|---|---|---|---|---|
| S1 | 客户与信用 | 11 | 3 | 14 |
| S2 | 销售订单 | 13 | 3 | 16 |
| S3 | 销售出库 | 15 | 3 | 18 |
| S4 | 应收与收款 | 17 | 5 | 22 |
| S5 | 销售与应收报表 | 5 | 3 | 8 |
| | **合计** | **61** | **17** | **78** |

> 与卡面要求的「S1 约 12 / S2 约 14 / S3 约 16 / S4 约 20 / S5 约 8」相比，
> 总量 78 ≈ 卡面 70，差异是：S1 多 2 项（额度例外、对账单口径）、S3 多 2 项（两条扣减路径、出库单打印）、
> S4 多 2 项（预收挂账、核销反核销）。**多出来的每一项都在第四章有对应规则条款。**

## 2.1 客户与信用（模块 S1，共 14 项）

| 编号 | 功能 | 说明 | 优先级 |
|---|---|---|---|
| S1-01 | 客户档案列表 | 分页 + 按编码/名称/状态/信用状态筛选；列：编码、名称、税号、信用额度、已用额度、可用额度、信用期、状态 | P0 |
| S1-02 | 新建客户 | 必填：编码（建议值可改）、名称、结算方式、信用期（天）、信用额度（默认 0）、应收科目；税率默认 13 | P0 |
| S1-03 | 编辑客户 | 编码保存后不可改（业务引用稳定性，同批次 1 §3.5.4）；名称/联系人/地址/税号/开票信息可改 | P0 |
| S1-04 | 停用/启用客户 | 停用后不可再开单（订单、出库、收款都拦），历史单据正常显示 | P0 |
| S1-05 | 客户详情（含额度与往来） | 一屏看全：基础信息 + 额度三件套 + 未结应收笔数/金额 + 最近 5 笔收付款 | P0 |
| S1-06 | 信用额度设置与调整 | 直接改 `credit_limit`＋写一条额度流水（`biz_customer_credit_log`）；**调额需单独权限**（决策 ⑧） | P0 |
| S1-07 | 额度占用与释放 | 由出库/收款自动驱动，**不提供手工占用按钮**；详情页可见占用明细（哪几张应收占了多少） | P0 |
| S1-08 | 客户分类 / 等级 | 字典驱动（`dict` 复用底座），用于报表分组与折扣策略预留；本批**不影响价格** | P1 |
| S1-09 | 信用例外（逐单放行） | 超额订单经审批后生成一条例外：绑定订单、放行金额、有效期；过期自动失效 | P0 |
| S1-10 | 信用状态视图 | 按「正常 / 接近超额（≥80%）/ 已超额 / 已冻结」四态筛选；冻结=手工置「禁止开单」 | P0 |
| S1-11 | 客户对账单 | 给客户的对账文件：期初、本期应收（按出库日）、本期收款、期末；可导出 Excel / 打印 PDF | P1 |
| S1-12 | 客户合并/去重提示 | 新建时按「税号相同」或「名称相似度>80%」提示疑似重复，**只提示不阻止** | P1 |
| S1-13 | 客户导入 | 复用底座通用导入（批次 A 已交付），模板含 12 列 | P1 |
| S1-14 | 冻结/解冻（禁止开单） | 与「停用」不同：停用是主数据维度（不可见），冻结是信用维度（可见但不可开单） | P0 |

> **为什么 S1-14 和 S1-04 要分开**：停用是「这个客户不合作了」（主数据动作，影响所有报表口径）；
> 冻结是「先别给他发货」（信用动作，随时可解）。合并成一个状态会让财务和销售的权限打架。

## 2.2 销售订单（模块 S2，共 16 项）

| 编号 | 功能 | 说明 | 优先级 |
|---|---|---|---|
| S2-01 | 订单列表 | 筛选：单号/客户/日期区间/状态/口径；列含「已发/未发」进度条 | P0 |
| S2-02 | 新建订单（草稿） | 表头 + 多明细行；行内单价、税率、金额实时算（口径见 4.1） | P0 |
| S2-03 | 两种金额口径切换 | 表头口径徽标 `[不含税]/[含税]`，切换弹确认框后整单按各行税率重算（与批次 1 完全同款） | P0 |
| S2-04 | 编辑订单 | **仅 `DRAFT`/`SUBMITTED` 可改**；`APPROVED` 后改数量需先撤回提交 | P0 |
| S2-05 | 提交订单 | `DRAFT → SUBMITTED`；提交时做**软性信用校验**（超额度则不拦，转「超额待批」标记） | P0 |
| S2-06 | 订单审批 / 驳回 | 仅针对「超额待批」与「大额订单（阈值参数）」；有 `sale-order:audit` 权限者可批 | P0 |
| S2-07 | 关闭订单 | `APPROVED`/`PARTIAL_OUT` → `CLOSED`：剩余未发数量作废不再发；已发部分不受影响 | P0 |
| S2-08 | 取消订单 | 仅 `DRAFT`/`SUBMITTED`（未出库）可取消；`PARTIAL_OUT` 只能关闭不能取消 | P0 |
| S2-09 | 删除订单 | 仅 `DRAFT` 可删（软删）；软删后单号可被新单复用（`uk` 带 `deleted_at IS NULL`） | P1 |
| S2-10 | 从订单拉出库单 | 带出未发数量；支持**部分出库**（一次拉一部分）；可拆成多张出库单 | P0 |
| S2-11 | 订单执行进度 | 行级 `shipped_qty` / `qty`、单级「已发金额/订单金额」；列表页进度条 + 详情页进度表 | P0 |
| S2-12 | 订单变更留痕 | 单据轨迹（复用 `doc_log`）：谁在何时改了哪个字段的什么值（数量/单价/交期三类必留） | P0 |
| S2-13 | 订单打印/导出 | 销售确认单（给客户签字）；导出 Excel（复用底座导出） | P1 |
| S2-14 | 未清订单看板 | 只看「已批未发完」的单，按交期升序，逾期高亮 | P1 |
| S2-15 | 客户协议价提示 | 单价框下方灰字提示「近 90 天该客户该物料成交价区间」；**不做价格表、不做价控**（决策 ⑨） | P1 |
| S2-16 | 订单复制 | 复制现有单据生成新草稿（改数量/交期最常用） | P1 |

## 2.3 销售出库（模块 S3，共 18 项）

| 编号 | 功能 | 说明 | 优先级 |
|---|---|---|---|
| S3-01 | 出库单列表 | 筛选：单号/客户/仓库/日期/状态/是否已生成应收 | P0 |
| S3-02 | 新建出库单（手工） | 不挂订单也能出库（样品、补发），`out_type='OTHER'`；挂订单则 `out_type='SALE'` | P0 |
| S3-03 | 从订单拉单生成 | 带出未发数量与订单价，**不允许改价**（改价请改订单）；行与订单行一一对应（`source_item_id`） | P0 |
| S3-04 | 编辑出库单 | 仅 `DRAFT` 可改；过账后只能作废红冲（4.11） | P0 |
| S3-05 | 过账（核心） | 一个事务完成 11 步：校验 → 扣库存 → 写流水 → 序列号流转 → 回写订单 → 生成应收 → 生成两张凭证 → 回写指针（4.10） | P0 |
| S3-06 | 库存扣减（非一物一码） | 条件 UPDATE `qty = qty − n WHERE qty >= n`（复用 F27 路径），金额 `ROUND(n × avg_cost, 4)` | P0 |
| S3-07 | 库存扣减（一物一码） | **不走扣减**，走「序列号状态流转 + 派生数量重算」（决策 ⑬ / 4.8） | P0 |
| S3-08 | 负库存控制 | 仓库级 `allow_negative`：0 = 硬拦（默认），1 = 允许且成本取**最后一次入库单价**并打标（决策 ②） | P0 |
| S3-09 | 序列号出库（扫码） | 扫码框 + 已扫/需扫计数 + 重复/不存在/非本仓/非本物料/已出库 五类实时校验（4.8.4） | P0 |
| S3-10 | 出库即生成应收 | 一张出库单 → 一张应收（`uk_biz_receivable_source` 兜住重复），金额 = 出库价税合计 | P0 |
| S3-11 | 出库即生成凭证（收入 3 行） | 借 1122 / 贷 6001 / 贷 2221.02（多税率按税率拆行，4.4.4） | P0 |
| S3-12 | 出库即生成凭证（成本 2 行） | 借 6401 / 贷 1405，金额按 4.5 与流水对齐 | P0 |
| S3-13 | 作废出库单（红冲） | 已过账出库单作废 = 库存回补 + 序列号回库？**不,序列号不可逆**（见 4.11.2 的处置）+ 红字两凭证 + 应收冲销 | P0 |
| S3-14 | 出库单打印 | A4 三联（存根/财务/随货），随货联带二维码（内容 = 单号） | P0 |
| S3-15 | 出库单导出 | Excel，含明细行与序列号清单 | P1 |
| S3-16 | 批次/库位选择 | 明细可指定库位（仅 `is_leaf=1` 的仓的库位）；不指定则落整仓汇总行 | P1 |
| S3-17 | 出库单复制/补发 | 从历史单据复制（补发、换货场景） | P1 |
| S3-18 | 出库汇总看板 | 按仓/按物料/按日的出库量与金额 | P1 |

## 2.4 应收与收款（模块 S4，共 22 项）

| 编号 | 功能 | 说明 | 优先级 |
|---|---|---|---|
| S4-01 | 应收单自动生成 | 出库过账时生成，**不提供手工新建应收**（防止账外挂账）；单号 `AR-yyyyMMdd-000001` | P0 |
| S4-02 | 应收列表 | 筛选：客户/到期日/状态/是否逾期/账龄桶；列含应收、已收、未收、逾期天数 | P0 |
| S4-03 | 应收详情 | 基础信息 + 核销明细（哪几笔收款核销了它）+ 关联出库单 + 关联凭证 | P0 |
| S4-04 | 账龄分析 | 五桶：未到期 / 1–30 / 31–60 / 61–90 / 90+；按客户汇总与按单据明细两种视图 | P0 |
| S4-05 | 收款单列表 | 筛选：单号/客户/日期/方式/状态/是否已核销完 | P0 |
| S4-06 | 新建收款单 | 客户、收款日期、方式（转账/现金/承兑/其他）、金额、收款账户（银行存款科目）、备注、附件（回单） | P0 |
| S4-07 | 编辑/删除收款单 | 仅 `DRAFT` 可改可删；`POSTED` 只能作废（反核销 + 红冲） | P0 |
| S4-08 | 收款单审核（过账） | `DRAFT → POSTED`：核销生效 + 生成凭证；需 `receipt:audit` 权限 | P0 |
| S4-09 | 自动 FIFO 核销 | 按到期日升序、同日按单号升序，自动填核销金额至收款金额用尽（4.6.3） | P0 |
| S4-10 | 手工指定核销 | 逐行填核销金额；可跨多张应收；可只核一部分 | P0 |
| S4-11 | 部分收款 / 多次收款 | 一张应收可被多笔收款核销；状态 `OPEN → PARTIAL_PAID → SETTLED` | P0 |
| S4-12 | 核销校验 | `Σ核销 ≤ 收款金额`、`每张应收核销 ≤ 该单未收`、金额必须 > 0、必须同客户（4.6.4） | P0 |
| S4-13 | 反核销（作废收款单） | 作废已过账收款单 = 逐条反核销 + 应收状态回退 + 红字凭证；**已跨期时禁止**（4.11.3） | P0 |
| S4-14 | 未核销余额（预收） | 收款金额大于核销合计时，差额挂 `2203 预收账款`，列表可见「预收余额」，后续可再核销 | P0 |
| S4-15 | 应收余额表 | 按客户汇总：应收合计、已收、未收、预收、净敞口 | P0 |
| S4-16 | 逾期预警列表 | 到期日已过且未结清；按逾期天数排序，可一键导出催款清单 | P0 |
| S4-17 | 收款凭证（借银行 / 贷应收） | 收款单审核生成；预收部分单独一行挂 2203（4.4.5） | P0 |
| S4-18 | 应收手工调账 | 仅超管可用，生成「其他应收/坏账」调整（本批**只做整单作废，不做部分调账**，见第十章） | P1 |
| S4-19 | 对账单批量生成 | 按客户 + 期间批量生成 PDF，支持邮件发送（复用底座通知） | P1 |
| S4-20 | 收款认领（无主款） | 银行流水导入后未匹配客户的款，先进「待认领池」 | P1 |
| S4-21 | 核销历史查询 | 按应收查全部核销记录、按收款查全部核销去向（双向可查） | P1 |
| S4-22 | 客户信用与应收联动 | 应收 `SETTLED` 时释放额度占用；列表页显示「额度占用」列（4.7.4） | P1 |

## 2.5 销售与应收报表（模块 S5，共 8 项）

| 编号 | 功能 | 说明 | 优先级 |
|---|---|---|---|
| S5-01 | 销售明细表 | 期间 + 客户 + 物料 + 仓库维度；列：数量、价款、税额、价税合计、成本、毛利、毛利率 | P0 |
| S5-02 | 客户销售汇总 | 按客户汇总上述金额 + 应收余额 + 逾期金额 | P0 |
| S5-03 | 毛利分析 | 收入（价款）− 成本结转；支持按物料/客户/期间下钻；毛利率 < 0 高亮 | P0 |
| S5-04 | 应收账款账龄表 | 五桶账龄 + 逾期明细；可切「按单据 / 按客户」 | P0 |
| S5-05 | 销售与应收勾稽表 | 校验：Σ 出库价税合计 = Σ 应收 amount；Σ 收款核销 = Σ 应收 received_amount（月结前必看） | P0 |
| S5-06 | 出库成本明细表 | 逐行：数量、出库时 avg_cost、成本、流水 before/after（给财务抽查用） | P1 |
| S5-07 | 销售员业绩 | 按业务员（订单 `create_by` 或客户归属人）汇总；本批**只按创建人**统计 | P1 |
| S5-08 | 报表导出与订阅 | 全部报表支持 Excel 导出；周/月自动推送（复用底座定时任务） | P1 |

> 报表的一条硬约束：**报表明细必须能与凭证对账**。
> 因此 S5-01/S5-03 的成本列**只能**取 `biz_stock_out_item.cost_amount`（出库时快照），
> 不许用「当前库存均价 × 数量」重算 —— 那会导致上月报表明天再看就变了（4.5.4 给反例算例）。

---
# 三、产品决策（14 条，逐条给候选对比表 + 结论 + 理由）

## 3.0 决策速览表

| # | 决策 | 结论 | 是否可配置 | 落在哪 |
|---|---|---|---|---|
| ① | 信用额度控制在哪一步拦 | **C 两者都拦**：下单**软拦**（可提交待批）、出库**硬拦**（不可过账） | ✅ 系统参数可切 A/B/C | 4.7 |
| ② | 出库成本结转口径 + 负库存 | 沿用**移动加权**；负库存**默认禁止**（`allow_negative=0`），允许时成本取**最后一次入库单价** | ✅ 仓库级 `allow_negative`（已存在） | 4.2 |
| ③ | 销售收入确认时点 | **A 出库即确认**（与批次 1「入库即生成」对称）；未开票的收入已确认是**可接受**的（理由见 3.3.3） | ❌ 不可配 | 4.4 |
| ④ | 收款核销规则 | 一对多 ✓、多对一 ✓；默认 **FIFO（按到期日）**，可**手工指定覆盖** | ✅ 核销页默认策略 | 4.6 |
| ⑤ | 出库与序列号 | `serial_required` 物料**必须逐件指定**；扫码出库；状态 `IN_STOCK → OUTBOUND` | ❌ 不可配（物料属性决定） | 4.8 |
| ⑥ | 销售退货 | **本批不做**，明确排到批次 3（序列号不可逆是硬阻塞） | — | 3.6 / 第十章 |
| ⑦ | 收入凭证与成本凭证：一张还是两张 | **两张**：`SALE_OUT`（收入 3 行）+ `SALE_OUT_COST`（成本 2 行） | ❌ 不可配 | 4.4.4 / 4.5 |
| ⑧ | 信用额度调整要不要审批 | **不走工作流**；改为「调额权限 + 强制留痕 + 调额上限参数 + 降额即时生效」 | ✅ 上限参数 | 3.8 |
| ⑨ | 客户协议价本批做不做 | **不做价格表**；只做「近 90 天成交价提示」 | — | 3.9 |
| ⑩ | 坏账处理本批做不做 | **不做计提**；只保留「整单作废 + 逾期预警」，坏账在批次 4 | — | 3.10 |
| ⑪ | 收款凭证什么时候生成、未核销的钱记哪里 | 收款单**审核即生成**；未核销部分挂 `2203 预收账款` | ❌ 不可配 | 4.4.5 |
| ⑫ | 出库单要不要锁库存（`locked_qty`） | **不锁**：`locked_qty` 本批不启用，可用量用「账面 − 已发」表达 | ❌ | 3.12 |
| ⑬ | 出库扣减的唯一写入口（一物一码 vs 普通） | **两条路径分流**，按 `material.serial_required` 决定，互斥 | ❌ 不可配 | 4.8.2 |
| ⑭ | 部分出库的应收/凭证粒度 | **逐张出库单**生成应收与凭证，**不按订单合并** | ❌ | 4.13 |

---

## 3.1 决策 ①：信用额度控制在哪一步拦？

### 3.1.1 候选方案对比

| 方案 | 做法 | 好处 | 坏处 | 结论 |
|---|---|---|---|---|
| **A 下单时拦（硬拒）** | 订单金额 > 可用额度 → 提交被拒 | 最省钱（不发货就没风险）；实现最简单 | ①**销售最反感**：超额是常态（账期客户月末集中下单），硬拒会让销售绕过系统线下开单；②订单是"意向"，真正产生应收的是出库，拦在意向层等于**用错的时点控风险**；③额度随时在变（客户可能 5 分钟前刚回款），下单被拒的电话会打到财务 | ❌ |
| **B 出库时拦（硬拦）** | 发货前校验，超了不给出库 | 控在**真正产生债权**的那一步，口径最准（应收金额已定）；销售可以先把单录下来 | ①下单阶段无感知，销售可能录了 10 张单，到发货才发现全超；②仓管在现场被拦，客户车已在门口，**冲突发生在最不该发生的时点** | 🟡 单独用不够 |
| **C 两者都拦（下单软拦 + 出库硬拦）** ★ | 下单：超额 → 允许提交但标「超额待批」，需审批放行；出库：超额 → **硬拦**，除非本单有有效例外 | ①把"预警"放在销售能处理的时候（录单时就知道）；②把"硬控"放在真正出风险的时候（发货时）；③审批人只需要看一眼，不用往返 | 需要一套「例外」机制（`biz_customer_credit_exception`），多一张表 | ✅ **采用** |

### 3.1.2 结论

```
默认策略 = C。
  下单（提交）  → 软拦：超额不阻止提交，单据置 credit_flag='OVER'，需 sale-order:audit 放行
  出库（过账）  → 硬拦：超额且无有效例外 → 拒绝过账（错误码 66012）
配置：
  系统参数 erp.sale.credit_mode = SOFT_AT_ORDER_HARD_AT_OUT（默认）
                                 | HARD_AT_ORDER（方案 A）
                                 | HARD_AT_OUT（方案 B）
                                 | OFF（不控，仅记录）
超额处置：
  放行 = 生成一条信用例外（绑定本单、放行金额 = 超出的那部分、有效期默认 30 天）
  放行需 business:erp:customer-credit:approve 权限（默认只给财务主管/超管）
```

### 3.1.3 理由（为什么不是 A，也不是 B）

1. **控风险的时点必须与债权发生时点对齐**。会计上债权在出库（收入确认）那一刻产生，
   所以**唯一必须硬拦的点是出库**。这是本决策的地基。
2. **下单软拦是给销售的"早知道"**，不是控制手段。它的价值在于：销售录单当场知道超了，
   能立刻找财务调额或让客户回款，而不是等仓库拒发。**把控制手段和预警手段分开**，是这一决策的核心思路。
3. **硬拒下单会制造线下流程**。这是 ERP 项目最常见的失败模式：系统越严，数据越假。
4. **可配置**：不同客户群管控强度不同（大客户可以放宽到只警示）。
   参数化后不需要改代码，也不需要为不同客户写分支。

### 3.1.4 边界与算例

| 场景 | 处理 |
|---|---|
| 可用额度 100,000，订单 80,000，已提交未出库 | 下单：不超（80,000 ≤ 100,000）→ 通过；此时**额度是否被"占用"？** 见下条 |
| 同客户再来一张 80,000 的单 | 两张单合计 160,000 > 100,000 → 第 2 张提交时软拦（因为按规则 4.7.3，**已提交未出库的订单计入占用**） |
| 客户信用额度 = 0（新客户未评级） | 视为「**禁止赊销**」，任何金额下单都软拦、出库硬拦；允许现结（收款后才出库，见 4.7.6 例外） |
| 客户已冻结 | 下单直接拒（不是软拦），文案 M-05；出库也拒 |
| 出库时超额，但订单有有效例外 | 放行，且**例外额度按本单金额扣减**（一张例外只服务一张订单，不可跨单复用） |
| 例外过期（默认 30 天） | 自动失效，出库恢复硬拦；过期不影响已放行的历史单据 |
| 客户回款后额度恢复 | 收款单审核时实时释放（`unpaid_amount` 减少 → 占用减少），无需人工 |

**算例：软拦 → 硬拦 → 放行 → 释放 全链路**

```
设定：客户 华东电子 credit_limit = 100,000.00，当前 Σ未结应收 = 0

① 10-20 出库 8 件，价税合计 18,080.00 → 应收 AR-001 未结 18,080.00
   占用 = 18,080.00，可用 = 81,920.00
② 10-31 订单 SO-001 135,600.00 提交
   占用口径（4.7.3）= 未结应收 18,080.00 + 已提交未出库订单 135,600.00 = 153,680.00
   153,680.00 > 100,000.00 → 超额 53,680.00 → 软拦，单据 credit_flag='OVER'
③ 陈主管放行 → 例外 EX-001：订单 SO-001，放行金额 53,680.00，有效期至 11-30
④ 11-02 出库 8 件（18,080.00）
   校验：本单有例外，且 18,080.00 ≤ 例外可用额 53,680.00 → 放行过账
   例外已用 18,080.00，剩余 35,600.00
⑤ 11-28 收款 18,080.00 核销 AR-001 → 未结清零，占用释放 18,080.00
```

---

## 3.2 决策 ②：出库成本结转的口径与负库存

### 3.2.1 候选方案对比（成本口径）

| 方案 | 做法 | 好处 | 坏处 | 结论 |
|---|---|---|---|---|
| **A 移动加权（沿用批次 1）** ★ | 成本 = 出库数量 × 出库当时 `avg_cost`；结存金额减法；出库不改均价 | 与批次 1 入库完全同构；库存金额永远等于总账 1405；实码 `deduct_inventory_row` 已实现同样公式（F27） | 单价除不尽时有微小尾差（用清仓倒挤消除） | ✅ **采用** |
| B 先进先出（FIFO 批次） | 按入库批次逐批结转 | 更贴近某些行业习惯（食品、电子） | ①批次维度本批没建（`lot_no` 字段虽有但未启用）；②`biz_material.costing_method` 虽允许 `FIFO`，但批次 1 PRD §3.2.1 明确「本批只允许选 MOVING_AVG」；③实现要维护批次队列，与"随时可查单价"冲突 | ❌ 批次 4+ |
| C 标准成本 | 用物料标准价结转，差异单列 | 便于考核 | 需要标准价维护与差异分摊科目，属于成本会计模块 | ❌ 不做 |

### 3.2.2 候选方案对比（负库存）

| 方案 | 做法 | 好处 | 坏处 | 结论 |
|---|---|---|---|---|
| A 一律禁止 | `qty - n < 0` → 拒绝 | 账实最干净 | 实际业务里"货已发出、单还没入"很常见（先销后采），一律禁会逼用户造入库单 | ❌ 太硬 |
| **B 仓库级开关（复用已存在的 `allow_negative`）** ★ | `allow_negative=0`（默认）硬拦；`=1` 允许，成本取**最后一次入库单价** | ①开关**实库已有**（F16），不新增字段；②批次 1 技术架构 §3.2.4 已预先定了负库存成本口径（F32），本批只是落地；③按仓区分符合实际（成品仓不允许、备件仓允许） | 负库存下 `amount/avg_cost` 失去意义，后续入库的移动加权会被污染 → 必须强提示 | ✅ **采用** |
| C 允许但成本记 0 | 负库存出库成本 = 0 | 简单 | 成本为 0 → 毛利虚高 → 报表失真，财务一定不接受 | ❌ |

### 3.2.3 结论

```
成本口径：移动加权平均，核算维度 = 物料 × 仓库（× 库位汇总桶），与批次 1 一字不改。

出库成本（普通）：
  cost_amount = ROUND(out_qty × avg_cost, 4)      ← 库存侧精度 4 位（X4）
  单据/凭证侧：cost_amount_2dp = round(cost_amount, 2)

结存更新：
  new_qty    = old_qty − out_qty
  new_amount = old_amount − cost_amount
  avg_cost   = 不变（移动加权出库不改均价）
  当 new_qty = 0 → new_amount 强制归零（清仓倒挤，尾差一次带走）

负库存：
  仓库 allow_negative = 0（默认）→ 出库数量 > qty → 拒绝过账（66010）
  仓库 allow_negative = 1        → 允许，成本 unit_cost 取「该物料该仓库最近一条 txn_type='IN' 流水的 unit_cost」
                                    找不到任何入库记录 → 仍然拒绝（66011）
                                    流水 remark 写「负库存出库」，单据与列表页挂黄标
一物一码物料：
  allow_negative 天然无效 —— 必须扫到实物序列号才能出库，扫不到就是没有（4.8.5）
```

### 3.2.4 算例（卡面要求的那个算例 + 三个补充算例）

**算例 1（卡面指定）：两批不同价入库后出库 50 件**

| 步骤 | 业务 | 数量变动 | 金额变动 | 结存数量 | 结存金额 | avg_cost |
|---|---|---|---|---|---|---|
| 0 | 初始 | — | — | 0 | 0.00 | — |
| 1 | 入库 100 件 @2.00（不含税） | +100 | +200.00 | 100 | 200.00 | 2.000000 |
| 2 | 入库 100 件 @3.00（不含税） | +100 | +300.00 | 200 | 500.00 | 500/200 = **2.500000** |
| 3 | **出库 50 件** | −50 | **−125.00** | **150** | **375.00** | **2.500000（不变）** |

验算：`50 × 2.500000 = 125.00`；`500.00 − 125.00 = 375.00`；`375.00 / 150 = 2.500000` ✅
成本凭证：`借 6401 主营业务成本 125.00 / 贷 1405 库存商品 125.00`。

**算例 2：清仓倒挤（尾差必须一次带走）**

| 步骤 | 业务 | 结存数量 | 结存金额 | avg_cost | 出库成本 |
|---|---|---|---|---|---|
| 1 | 入库 10 件 @1.11 | 10 | 11.10 | 1.110000 | — |
| 2 | 入库 3 件 @3.33 | 13 | 21.09 | 1.622308（21.09/13） | — |
| 3 | 出库 7 件 | 6 | 21.09 − 7×1.622308 = **9.733844 → 9.7338** | 1.622308 | 7 × 1.622308 = 11.356156 → **11.3562** |
| 4 | 出库 6 件（清仓） | **0** | **0.00（倒挤）** | — | **= 结存 9.7338**（不是 6×1.622308 = 9.7338，恰好相等；若不等则取结存值） |

> 关键：第 4 步**不能**再乘一次均价。只要 `new_qty = 0`，`cost_amount` 一律取「出库前的 `amount`」。
> 这样 `qty=0 → amount=0` 恒成立，总账 1405 永远不会留下 1 分钱残值。
> 若按乘法算，`6 × 1.622308 = 9.733848` 与结存 `9.7338` 差 `0.000048`，长期累积会让库存金额与总账差几分钱。

**算例 3：负库存出库（allow_negative = 1）**

```
设定：物料 M-W1A-005，仓库 WH-B（allow_negative=1）
  2026-10-01 入库 10 件 @2.00 → qty=10 amount=20.00 avg_cost=2.000000
  2026-10-05 出库 10 件 → qty=0 amount=0.00（清仓倒挤），avg_cost 仍 2.000000
  2026-10-08 出库 5 件 → qty = −5
     成本 = 5 × 最近入库单价 2.000000 = 10.00
     amount = 0.00 − 10.00 = **−10.00**（允许为负，因为 qty 为负）
     avg_cost = 不变（2.000000）
     流水：txn_type='OUT' qty=5 unit_cost=2.000000 amount=10.00 remark='负库存出库'
  2026-10-12 入库 8 件 @3.00 → qty = 3, amount = −10.00 + 24.00 = 14.00
     avg_cost = 14.00 / 3 = 4.666667  ← 单价被"污染"（负库存的必然代价）
```

> 这就是为什么默认**必须** `allow_negative=0`。允许负库存的仓库，实际单价会偏离真实成本，
> 业务上只能用于「货到单未到、临时放行」的少数仓库，且必须在月结前清零（批次 4 报表单独披露）。

**算例 4：一物一码物料出库（走派生路径，不算乘法）**

```
设定：M-W1A-004 成品D（serial_required=t），WH02 在库 8 件，avg_cost = 2.000000，amount = 16.00

出库 8 件并扫 8 个序列号：
  ① 8 个序列号 IN_STOCK → OUTBOUND（逐个，事务内）
  ② recompute_quantity：qty = COUNT(IN_STOCK) = 0，amount = 0 × 2.000000 = 0.00
  ③ 每个序列号写一条 OUT 流水：amount = 1 × 2.000000 = 2.00（8 条，合计 16.00）
  ④ 出库单成本 = Σ 流水 amount = 16.00
```

✅ 与「8 × 2.000000 = 16.00」一致，但**实现路径不同**：一物一码的成本是**流水求和**，不是乘法。
差别在多仓/多库位时才会体现：派生的 `amount` 是按 `qty × avg_cost` 重算的，
而流水求和是**历史快照**。本批规定：**出库单成本一律取 Σ 流水 amount**（可追溯、可审计），
`biz_inventory.amount` 取派生值（保证 `qty=0 → amount=0`）。两者差 ≤ 0.0001/单，月末勾稽表（S5-05）兜住。

---

## 3.3 决策 ③：销售收入确认时点

### 3.3.1 候选方案对比

| 方案 | 做法 | 好处 | 坏处 | 结论 |
|---|---|---|---|---|
| **A 出库即确认（票货同行）** ★ | 出库过账 → 立刻做「借应收 / 贷收入 + 贷销项税」 | ①与批次 1「入库即生成」完全对称（F24/F25 同一套引擎）；②库存与总账永远同步，随时可查都平；③财务零手工；④本批就能闭环验收 | 若实际未开票，销项税提前确认（增值税纳税义务时点可能未到） | ✅ **P0 采用** |
| B 开票才确认收入 | 出库只动库存与成本，等发票模块再确认收入 | 严格贴合「开票确认」的税务口径 | ①**本批没有发票模块**（批次 3）→ 收入侧零凭证，闭环断链，无法验收；②库存已减、收入未记，月末账实不符 | ❌ 批次 3 |
| C 收款才确认 | 收到钱才记收入 | 现金流视角直观 | 违反权责发生制（《企业会计准则第 14 号——收入》）；预收款会大量挂账；账龄分析失去意义 | ❌ |

### 3.3.2 结论

```
收入确认时点 = 出库过账（POSTED）那一刻，与批次 1 的「入库即生成凭证」完全对称。
凭证状态 = POSTED（直接进总账，不等人工审核），与批次 1 §3.3.3 同口径。
```

### 3.3.3 「出库了但没开票，收入已确认、销项税已计」—— 这可以接受吗？

**结论：可以接受，但必须显式告知用户，并把发票核对留到批次 3。**

理由三条：

1. **会计准则站在 A 这边**。收入确认看「控制权转移」，不看开票。
   货已发出、客户已签收（随货联签字），控制权已转移，**收入就该确认**。
   如果等到开票才确认，反而是错的（开票可能拖两个月）。
2. **增值税纳税义务时点**：现行规定是「先开具发票的，为开具发票的当天；未开具发票的，
   为货物发出的当天（赊销按合同约定收款日期）」。所以**发货即产生纳税义务**，
   A 方案记销项税在发货时点，与税法口径一致，不是提前。
3. **不一致的风险在"票"，不在"账"**。真正的风险是：账上销项税与申报表/发票台账的销项税对不上。
   本批的处置是**把差异显性化**，不是回避：
   - 出库单有 `invoice_status` 字段（`NOT_INVOICED` / `INVOICED`），本批恒为 `NOT_INVOICED`，
     由批次 3 的发票模块回填；
   - 报表 S5-05「销售与应收勾稽表」固定一行：**「已确认收入但未开票金额」**，财务每月末看一眼；
   - 过账确认弹窗里明写一行提示：
     `本次出库将确认收入 16,000.00、销项税 2,080.00（发票状态：未开票）。`
   - 附录 B 第 3 条请老大确认：假如公司要求"开票才确认"，本批就切 B 方案（表结构不用改，只改凭证触发点）。

---

## 3.4 决策 ④：收款核销规则

### 3.4.1 候选方案对比（核销方向）

| 方案 | 一对多（1 收款 → N 应收） | 多对一（N 收款 → 1 应收） | 结论 |
|---|---|---|---|
| A 一收款只能核一应收 | ✅ 简单 | ❌ 不支持 | ❌ 实际业务里客户一笔打款覆盖多张单是**常态**，不支持等于逼财务造"合并应收单" |
| B 一应收只能收一次 | ❌ | ✅ | ❌ 部分付款极常见（先付 70%，尾款月结） |
| **C 双向多对多（核销明细表）** ★ | ✅ | ✅ | ✅ **采用**：`biz_receipt_item` 记录「哪笔收款核销了哪张应收的多少钱」，天然支持双向多次 |

### 3.4.2 候选方案对比（核销顺序）

| 方案 | 做法 | 好处 | 坏处 | 结论 |
|---|---|---|---|---|
| A 只能手工 | 财务逐行填 | 完全符合意图 | 客户打款 10 笔时要填 10 行，日工作量爆炸 | ❌ 单独用不做 |
| **B 默认 FIFO（按到期日）+ 允许手工覆盖** ★ | 打开核销页自动按到期日升序填满；财务可逐行改数 | ①90% 场景零手工；②保留例外能力（客户指定某笔） | 需要清晰的「自动/手工」视觉区分（改过的行打「手工」标签） | ✅ **采用** |
| C 按单号匹配（备注解析） | 从银行流水备注里提取单号 | 精准 | 依赖客户填备注，实际不到 20% 会填 | 🟡 作为 FIFO 的前置尝试（P1，S4-20） |

### 3.4.3 结论

```
核销维度：一张收款单 ↔ N 张应收（biz_receipt_item 明细），一张应收 ↔ N 笔收款核销。
默认顺序：FIFO —— 到期日升序；同日按应收单号升序。
覆盖方式：手工指定优先，不限制顺序（可以先核销新单、留下老单不核）。
状态机：
  OPEN          ──收到部分款──▶ PARTIAL_PAID ──收满──▶ SETTLED
     │                              │
     └──────────出库单作废──────────┴──▶ CANCELLED
  规则：unpaid_amount = amount − received_amount
        OPEN: received = 0；PARTIAL_PAID: 0 < received < amount；SETTLED: received = amount
```

### 3.4.4 算例：一笔 5,000 元核销 3 张应收（FIFO）

```
客户 江南贸易，4 张未结应收：
  AR-001 到期 10-20  1,130.00  未收 1,130.00
  AR-002 到期 11-09    565.00  未收   565.00
  AR-003 到期 11-19    226.00  未收   226.00
  AR-004 到期 11-30  5,650.00  未收 5,650.00
收款 5,000.00，FIFO 自动填：
  AR-001: 1,130.00 → SETTLED      （余额 3,870.00）
  AR-002:   565.00 → SETTLED      （余额 3,305.00）
  AR-003:   226.00 → SETTLED      （余额 3,079.00）
  AR-004: 3,079.00 → PARTIAL_PAID （未收 2,571.00，余额 0.00）
校验：1,130 + 565 + 226 + 3,079 = 5,000.00 ✅ = 收款金额
凭证：借 1002 银行存款 5,000.00 / 贷 1122 应收账款 5,000.00（3 行贷方按客户辅助核算合并为 1 行）
```

---

## 3.5 决策 ⑤：出库与序列号（一物一码）

### 3.5.1 候选方案对比

| 方案 | 做法 | 好处 | 坏处 | 结论 |
|---|---|---|---|---|
| A 不指定序列号（只填数量） | 出库只看数量，序列号由系统随便挑 n 个置 OUTBOUND | 操作最快 | ①**售后无法追溯**（客户拿坏件回来，查不到是哪批）；②一物一码商品的意义全失；③客户签收单上要写序列号（3C/设备类强制） | ❌ |
| **B 逐件指定（扫码）** ★ | `serial_required` 物料的出库数量 = 已扫序列号数，不等不许过账 | ①售后可追溯；②客户签收单可打印序列号清单；③软件防错（重复扫/错仓/错料实时拦） | 操作慢一点（扫码枪 1–2 秒/件） | ✅ **采用** |
| C 半自动（系统预填 + 人工删） | 系统按先进先出预选 n 件，人工可删改 | 快 | 预选逻辑要按 `inbound_at` 排序，容易把客户指定的"新货"发掉；且与 B 的成本一致但可追溯性下降 | 🟡 P1 备选 |

### 3.5.2 结论

```
适用范围：仅 material.serial_required = true 的物料（实库有 2 个：M-W1A-004 成品D、E2E 测试物料）。
强制程度：出库单该行「数量」由已扫序列号数决定 —— 数量 ≠ 序列号数 → 不许过账（66014）。
状态流转：IN_STOCK → OUTBOUND（不可逆，复用实码 F29）；warehouse_id 保留最后位置不清空。
扫码交互：文本框常驻焦点 + 扫码枪回车提交 + 也支持手工输入 + 支持批量粘贴（一行一个）。
不看序列号的物料：不出现扫码区，行级只有数量。
```

### 3.5.3 理由

1. **一物一码的价值只在出库那一端兑现**。入库录序列号（批次 1 已做）只是把号存起来；
   只有出库时**把号绑到客户和单据上**，售后才能回答「这台机器卖给谁了、什么时候、哪一单」。
2. **实码已经支持单向流转，且不允许回库（F29）**。本决策不需要改状态机，只是把它接入销售出库。
3. **一物一码物料的账面数量是派生值（F28）**，逐件指定是**唯一**与派生机制自洽的方式（决策 ⑬）。

### 3.5.4 边界

| 场景 | 处理 |
|---|---|
| 客户只要 3 台，在库 8 台 | 扫 3 个号即可（部分出库），剩余 5 台仍在库 |
| 扫了一个不在本仓的号 | 拒绝，「序列号 SN-… 当前在 WH-02，与本单仓库 WH-01 不一致」（M-13） |
| 扫了一个已出库的号 | 拒绝，「序列号 … 已于 2026-10-20 出库（单据 OUT-20261020-0003）」（M-12） |
| 扫了别的物料的号 | 拒绝，「序列号 … 属于物料 M-W1A-005，与本行物料 M-W1A-004 不一致」（M-14） |
| 扫了已报废的号 | 拒绝，「序列号 … 已报废，不能出库」（M-15） |
| 一个号扫两次 | 拒绝，「该序列号已在本次出库中（第 1 行）」（M-16） |
| 手工输入不存在的号 | 拒绝，「序列号 SN-… 不存在，请核对后重新扫码」（M-11） |
| 客户退货（本批不做） | 见决策 ⑥：序列号不可逆，退货必须引入新状态，排批次 3 |

---

## 3.6 决策 ⑥：销售退货，本批做不做？

### 3.6.1 候选方案对比

| 方案 | 做法 | 好处 | 坏处 | 结论 |
|---|---|---|---|---|
| A 本批做完整退货 | 退货单 → 库存增加 → 冲减应收 → 红字凭证；序列号 `OUTBOUND → IN_STOCK` | 业务完整 | ①**与已上线的序列号状态机冲突**：`OUTBOUND→IN_STOCK` 被 `can_transition` 明确禁止，且有单测锁死（F29），要改就得回改批次 1 的代码与测试；②退货涉及"退哪张单、退几件、按什么价退、成本用原成本还是当前均价"四个新决策，每个都要算例与验收；③退货率高的行业还需要质量问题单、让步接收等分支 | ❌ 本批不做 |
| B 本批只做「整单红冲」（作废出库单） | 出库单作废 → 库存回补 + 红字两凭证 + 应收冲销 | 覆盖"单据录错/客户拒收整单"这类场景，实现成本低 | ①只能整单，不能退其中 2 件；②**序列号仍然回不来**（同样撞 F29），只能留在 `OUTBOUND` 并写"作废原因" | ✅ **本批做（S3-13）** |
| C 写"批次 3"明确外延 | 本批只在文档与 UI 上说明不支持退货 | 零成本 | 客户会问，销售要有话术 | ✅ 作为补充 |

### 3.6.2 结论

```
本批：做 B（整单红冲 / 出库单作废），不做 A（部分退货）。
  作废的红冲口径见 4.11.2；序列号处置：保持 OUTBOUND + 写 remark「单据作废回补，待批次3退货流程处理」，
  账面数量按 recompute_quantity 重算（会因序列号仍在 OUTBOUND 而不回补）→ 因此本批的作废只允许在
  「该行未使用序列号（非一物一码物料）」时执行；一物一码物料的出库单**不允许作废**，
  只能走"反向出库单"（另开一张 out_type='RETURN' 的入库方向单据，本批不做）→ 见 4.11.2 的明确限制。
  这是本批唯一一处「功能被有意收窄」，必须在 UI 上置灰并给文案（M-19）。
批次 3：完整退货（含序列号新状态 RETURNED、退货原因、退货成本口径、退货发票红冲）。
```

### 3.6.3 理由（为什么宁可不做，也不做半成品）

1. **序列号不可逆是硬阻塞**。要么改已上线的状态机（破坏批次 1+D 的 11 条单测与不变式），
   要么新增 `RETURNED` 状态（要同时改 `ALL_STATUSES`、`can_transition`、`recompute_quantity` 的 `IN_STOCK` 计数、
   库存审计的件数不变式）—— 这两件事都不是"加个页面"的量级。
2. **退货的成本口径必须单独决策**：退回来的货按**原出库成本**入账（能对上）还是按**当前均价**（会改变结存均价）？
   本批没有足够时间把这个决策做扎实，做错会让毛利与库存双向失真。
3. **本批的目标是"进销两头通"**，不是"业务全闭环"。退货属于售后闭环，批次 3 做更合适。

---

## 3.7 决策 ⑦：收入凭证与成本凭证 —— 一张还是两张？

### 3.7.1 候选方案对比

| 方案 | 做法 | 好处 | 坏处 | 结论 |
|---|---|---|---|---|
| A 一张凭证 5 行 | 借 1122 / 贷 6001 / 贷 2221.02 / 借 6401 / 贷 1405 | 一次看全，单据↔凭证一对一，`voucher_id` 只需一列 | ①**与实码的规则表结构冲突**：`biz_voucher_rule` 是扁平三科目（F10），一个 `biz_type` 只能出「借 debit + 借/贷 tax + 贷 credit」三行；要多两行必须改表加子表（改批次 1 已上线结构）；②红冲时要一起冲，但成本与收入的作废时点可能不同（跨期时财务会想把成本冲回上期、收入留在本期）；③批次 4 若要把成本改为月结一次结转，一张凭证里拆不出来 | ❌ |
| **B 两张凭证（收入 3 行 + 成本 2 行）** ★ | `SALE_OUT`：借 1122 / 贷 6001 / 贷 2221.02（税 0 时 2 行）；`SALE_OUT_COST`：借 6401 / 贷 1405 | ①**完全复用现有规则表与引擎**（只需给规则表加 `tax_side`）；②红冲独立，可按需分别冲；③成本凭证金额 = 库存流水金额，可与库存勾稽；④批次 4 改成本结转策略不用动收入凭证 | 出库单要存两个 `voucher_id`（`voucher_id` + `voucher_cost_id`），列表页要显示两个号 | ✅ **采用** |
| C 一张凭证 + 成本行用 `aux_extra` 塞 | 塞进摘要或 JSON | 不改表 | 会计上无法按科目取数，总账取不到 6401/1405 发生额 | ❌ 错账 |

### 3.7.2 结论

```
两张凭证，同一事务内生成，同一日期、同一期间、同一 `source_id`（出库单 id）：
  凭证 1：source_type='SALE_OUT'       biz_type='SALE_OUT'       收入凭证（3 行）
  凭证 2：source_type='SALE_OUT_COST'  biz_type='SALE_OUT_COST'  成本凭证（2 行）
出库单表头回写：voucher_id = 收入凭证 id，voucher_cost_id = 成本凭证 id
两张凭证的状态都是 POSTED（自动凭证直接进总账，同批次 1 §3.3.3）
```

### 3.7.3 规则表怎么扩（最小改动）

```sql
-- V0162：给规则表补税额方向，并种 3 条销售侧规则
ALTER TABLE biz_voucher_rule
  ADD COLUMN tax_side VARCHAR(6) NOT NULL DEFAULT 'DEBIT'
      CHECK (tax_side IN ('DEBIT','CREDIT')),
  ADD COLUMN rule_nature VARCHAR(16) NOT NULL DEFAULT 'GENERAL'
      CHECK (rule_nature IN ('GENERAL','REVENUE','COST','PAYMENT'));

-- 存量规则 PURCHASE_IN_DEFAULT 取默认值 DEBIT，分录形状完全不变（回归必须验：200/26/226）
INSERT INTO biz_voucher_rule (rule_code, biz_type, debit_account_id, credit_account_id, tax_account_id, tax_side, rule_nature, is_active)
VALUES
  ('SALE_OUT_DEFAULT', 'SALE_OUT',      <1122>, <6001>, <2221.02>, 'CREDIT', 'REVENUE', 1),
  ('SALE_OUT_COST',    'SALE_OUT_COST', <6401>, <1405>, NULL,      'DEBIT',  'COST',    1),
  ('RECEIPT_DEFAULT',  'RECEIPT',       <1002>, <1122>, <2203>,    'DEBIT',  'PAYMENT', 1);
```

> `RECEIPT_DEFAULT` 的 `tax_account_id` 一列在这里被复用为「预收科目（2203）」——
> 因为收款凭证需要两个贷方（应收 + 预收）。**这是本批唯一一处语义复用，必须在迁移注释里写清**，
> 不要让后面的开发者误以为是税额科目。若架构评审认为不可接受，替代方案是给规则表再加一列
> `secondary_credit_account_id`（更干净，但要改 `load_rule` 的 SELECT）。**本 PRD 建议加列**（附录 B 第 4 条请老大拍）。

---

## 3.8 决策 ⑧：信用额度调整要不要审批？

### 3.8.1 候选方案对比

| 方案 | 做法 | 好处 | 坏处 | 结论 |
|---|---|---|---|---|
| A 接工作流引擎审批 | 调额 → 生成 `biz_flow_instance` → 审批 → 生效 | 有正式审批链，可追溯 | ①批次 D 的工作流是"可编辑流程"，调额这种**单节点审批**用它是杀鸡用牛刀，且要维护流程定义；②调额是低频动作（一周几次），流程带来的延迟没有业务价值 | ❌ |
| **B 权限 + 留痕 + 上限参数** ★ | 调额需要 `customer-credit:adjust` 权限（默认只给财务主管/超管）；每次调额写一条 `biz_customer_credit_log`（原值/新值/原因/操作人/时间）；单次上调超过 `erp.sale.credit_adjust_max`（默认 50,000）时**必须二次确认 + 必填原因 ≥10 字** | ①低频动作不需要流程引擎；②审计靠日志就够（谁改的、改多少、为什么，一条不落）；③上限参数挡住"手滑多打一个 0" | 没有"两个人签字"的硬约束 | ✅ **采用** |
| C 谁都不许改，只能新建客户 | 额度写死 | 最安全 | 业务不可用（额度随客户成长变化是常态） | ❌ |

### 3.8.2 结论与规则

```
调额权限：business:erp:customer-credit:adjust（默认 SUPER_ADMIN + FINANCE）
留痕：每次变动写 biz_customer_credit_log，
      字段：customer_id / change_type(ADJUST|FREEZE|UNFREEZE|EXCEPTION) / before_limit / after_limit /
            delta / reason(必填≥5字，超上限时≥10字) / operator_id / created_at
降额：立即生效，且**不阻止已有未结应收**（降额只影响新单；已有单据照常按原额度结算）
上调：立即生效，可立刻用于新单
冻结：requires customer-credit:freeze 权限；冻结后下单直接拒（不是软拦）
```

### 3.8.3 算例

```
客户 江南贸易 原额度 50,000.00
陈主管调至 120,000.00（delta = +70,000.00 > 上限参数 50,000.00）
  → 弹二次确认：「本次上调 70,000.00 超过单次上限 50,000.00，请填写原因（≥10 字）」
  → 填「2026 年度战略客户，经总经理批准提额」→ 提交
  → 写日志：before=50000, after=120000, delta=70000, reason=..., operator=陈主管
  → 生效，可用额度立刻重算
```

---
## 3.9 决策 ⑨：客户协议价本批做不做？

### 3.9.1 候选方案对比

| 方案 | 做法 | 好处 | 坏处 | 结论 |
|---|---|---|---|---|
| A 完整价格表（客户×物料×生效期+折扣阶梯） | 新建 `biz_price_agreement` + 明细 + 审批 | 销售体验最好 | ①8–12 张新表里没有它的位置，会挤掉 P0；②价格表一旦存在，就必须解决"下单价与协议价不符时拦还是放"的价控问题（又是一个决策）；③客户价格在中小企业普遍用 Excel/口头维护，强行上系统会被绕过 | ❌ 批次 3 |
| **B 只做「成交价提示」** ★ | 录单时按「客户 + 物料」查近 90 天最近 3 笔成交价，灰字提示区间，**不拦不填** | 零新表（直接查出库明细）；解决 80% 的痛点（销售凭记忆报价）；不会产生价控争议 | 不能自动带出协议价 | ✅ **采用（P1）** |
| C 完全不做 | — | 最省 | 销售连参考都没有 | 🟡 备选 |

### 3.9.2 结论

```
本批不做价格表、不做价控（不拦低价、不拦高于协议价）。
P1 做「最近成交价提示」：SELECT price, price_incl, out_date FROM biz_stock_out_item 联合主表
                          WHERE customer_id=? AND material_id=? AND status='POSTED'
                          ORDER BY out_date DESC LIMIT 3
提示文案：「近 90 天该客户成交价：2,000.00 / 2,050.00 / 2,000.00（不含税）」
```

---

## 3.10 决策 ⑩：坏账处理本批做不做？

### 3.10.1 候选方案对比

| 方案 | 做法 | 好处 | 坏处 | 结论 |
|---|---|---|---|---|
| A 做坏账计提（账龄百分比法） | 按月按账龄桶计提「坏账准备」，生成「借 6702 信用减值损失 / 贷 1231 坏账准备」 | 报表更真实 | ①计提比例是会计政策（不是产品逻辑），要可配置且每家不同；②需要期末结账配合（本批无结账）；③科目（6702/1231）本批没种 | ❌ 批次 4 |
| B 做坏账核销（直接转销） | 确认收不回来 → 作废应收 + 红字凭证 | 简单 | 直接转销不符合准则（应先用备抵法）；且核销后客户还钱了怎么处理没定 | ❌ 批次 4 |
| **C 只做「逾期预警 + 冻结」** ★ | 逾期 90+ 的应收进「高风险清单」，可一键冻结客户；不做任何账务处理 | 零新表零科目；把"该催的催、该停的停"落地；账务上不动（合规：无证据证明收不回前不提） | 报表上看不到坏账影响 | ✅ **采用（P0=S4-16，P1=冻结联动）** |

### 3.10.2 结论

```
本批不做坏账计提与核销。
做：逾期清单（S4-16）、逾期 90+ 自动打「高风险」标、一键冻结客户（S1-14）、催款清单导出。
批次 4：坏账准备（科目 6702/1231）+ 计提政策配置 + 期末结账联动。
```

---

## 3.11 决策 ⑪：收款凭证什么时候生成、未核销的钱记哪里？

### 3.11.1 候选方案对比

| 方案 | 做法 | 好处 | 坏处 | 结论 |
|---|---|---|---|---|
| A 收款单保存就生成 | `DRAFT` 即生成凭证 | 少一步 | 草稿是"还没确认到账"的状态，生成凭证会让总账出现未确认的银行存款 | ❌ |
| **B 审核（过账）时生成** ★ | `DRAFT → POSTED` 时生成凭证 | 与批次 1「过账即生成」同口径；审核人即责任人 | 多一步操作（但财务本来就要审） | ✅ **采用** |
| C 只登记不发凭证，月末汇总一次 | 月末做一张汇总凭证 | 分录少 | 无法逐笔对账；红冲粒度变粗 | ❌ |

### 3.11.2 未核销的钱记哪里（三个候选）

| 方案 | 做法 | 结论 |
|---|---|---|
| A 留在 1122 应收（负数） | 贷 1122 超过该客户未结应收 → 1122 出现贷方余额 | ❌ 应收账款贷方余额在报表上会被误读为"客户欠我们负的钱"，且账龄表算不出来 |
| **B 挂 2203 预收账款** ★ | 未核销部分：借 1002 / 贷 2203；后续核销时：借 2203 / 贷 1122 | ✅ **采用**（标准做法，报表可单列"预收账款"） |
| C 挂"其他应付款" | 借 1002 / 贷 2241 | ❌ 性质不对（预收是货款） |

### 3.11.3 结论与凭证模板

```
收款单审核（POSTED）时生成一张凭证，biz_type='RECEIPT'，source_type='RECEIPT'：

情形 1：全部核销（核销合计 = 收款金额）
  借 1002 银行存款                 5,000.00
  贷 1122 应收账款（客户辅助）      5,000.00

情形 2：部分核销（核销合计 < 收款金额，差额为预收）
  借 1002 银行存款                 6,000.00
  贷 1122 应收账款（客户辅助）      5,000.00
  贷 2203 预收账款（客户辅助）      1,000.00

情形 3：纯预收（核销合计 = 0）
  借 1002 银行存款                 1,000.00
  贷 2203 预收账款（客户辅助）      1,000.00

情形 4：后续用预收核销（第二次核销，收款单不再生成凭证，只做核销动作）
  —— 本批简化：预收余额的后续核销**不单独生成凭证**，而是在下一次"用预收核销"时
     生成一张转账凭证：借 2203 预收账款 / 贷 1122 应收账款。biz_type='RECEIPT_APPLY'。
```

> 为什么情形 4 要单独一个 `biz_type`：因为它是"科目之间的转账"（预收 → 应收），
> 不涉及银行科目，形状与收款凭证不同。**不做的话预收余额会永远挂在 2203 上**，账永远不平（预收虚高）。

---

## 3.12 决策 ⑫：出库单要不要锁库存（`locked_qty`）？

批次 1 PRD §3.2.2 留了一句话：「`locked_qty` 本批恒 0，**为第 2 批销售占用预留**」。
本决策就是回答这句话到底怎么兑现。

### 3.12.1 候选方案对比

| 方案 | 做法 | 好处 | 坏处 | 结论 |
|---|---|---|---|---|
| A 订单提交即锁 `locked_qty` | `locked_qty += 订单数量`，出库时释放 | 防超卖（两个销售不能同时把最后 10 件都许给客户） | ①**与一物一码的派生数量冲突**：`recompute_quantity` 会把 `qty` 直接置成件数，`locked_qty` 与 `qty` 的关系没人维护；②锁了要有人放（订单取消/关闭/超期），漏放就永久占用；③批次 1 的调拨与序列号代码**从不写这一列**，本批引入等于在两套机制间制造不一致 | ❌ |
| B 出库单过账时才动（=不锁） | 不做预留，出库那一刻条件 UPDATE 挡并发 | 与实码完全一致（F27：`WHERE qty >= n` 就是并发防线）；零维护成本 | 销售看不到"已经被别人订走多少" → 用「订单占用量」视图补 | ✅ **采用** |
| C 只读不写（只增加"订走"视图） | 可用量 = 账面 − Σ已提交未出库订单数量，作为**视图/查询**，不落列 | 销售能看到真实可承诺量；不引入并发写风险 | 查询要 join 订单明细，稍慢（本批数据量可忽略） | ✅ **采用（与 B 并存）** |

### 3.12.2 结论

```
`locked_qty` 本批继续恒为 0（不启用），批次 1 PRD 的那句"预留"在本批**正式作废**，
理由写进本文（X7）：它防不了并发（真正的防线是条件 UPDATE），却会与派生数量打架。

"可承诺量"用查询表达（S3 页面的物料选择器显示）：
  可承诺量 = biz_inventory.qty − Σ(销售订单明细未发数量 where 状态 in (SUBMITTED, APPROVED, PARTIAL_OUT))
  注意：同一物料多仓时按仓分别算；不跨仓调拨（批次 4）
```

### 3.12.3 算例

```
M-W1A-004 成品D @WH02：
  账面 qty = 8（一物一码，8 个 IN_STOCK 序列号）
  已提交未出库订单数量 = 3（SO-002 要 3 台，未发）
  可承诺量 = 8 − 3 = 5
销售再开单 6 台 → 新建订单页灰字提示「可承诺量仅 5，超出部分需等入库」，但不阻止保存（软提示）
仓管真正出库时，条件 UPDATE / 序列号校验会挡住超额（硬拦）
```

---

## 3.13 决策 ⑬：出库扣减的唯一写入口（一物一码 vs 普通物料）

> 这是本批**技术风险最高**的一条，写得细是因为它踩下去就是"库存数量莫名奇妙变了"。

### 3.13.1 候选方案对比

| 方案 | 做法 | 好处 | 坏处 | 结论 |
|---|---|---|---|---|
| A 一律走 `qty = qty − n` | 出库单直接减账面数量（含一物一码物料） | 代码只有一条路径 | ❌ **必错**：一物一码物料的 `qty` 是派生值（F28），下一次任何序列号动作都会用 `COUNT(IN_STOCK)` 覆盖它 → 出库后数量又"长回来"；且账面对不上件数，库存审计（S5-06 / 批次 D 的 `inventory_audit`）立刻报红 | ❌ |
| B 一律走序列号（普通物料也生成虚拟序列号） | 所有出库都走序列号 | 路径唯一 | ①普通物料（如螺丝）不存在一物一码，强行生成号是数据污染；②实库 `biz_material_serial` 的件数不变式对所有 `serial_required` 物料才成立 | ❌ |
| **C 按 `material.serial_required` 分流两条路径，互斥** ★ | 一物一码：序列号状态流转（由 `recompute_quantity` 派生数量）；普通物料：条件 UPDATE 扣减 | 各自走各自自洽的机制；与实码完全一致 | 出库单过账逻辑要分支（但分支点在"行"级别，不复杂） | ✅ **采用** |

### 3.13.2 结论（开发照此实现）

```
for each 出库明细行:
    material = load(material_id)
    if material.serial_required:
        ① 校验本行序列号数 == 出库数量（否则 66014）
        ② 逐件 UPDATE biz_material_serial SET status='OUTBOUND', outbound_at=now(),
                source_type='SALE_OUT', source_id=出库单id, update_by=操作人
              WHERE id=? AND status='IN_STOCK' AND warehouse_id=本单仓库     ← 条件 UPDATE，挡并发
           受影响行数 != 1 → 拒绝（66015 序列号状态已变，请刷新重试）
        ③ 写 OUT 流水（每件一条，biz_type='SALE_OUT'，doc_type='STOCK_OUT'，qty=1，amount=1×avg_cost）
        ④ recompute_quantity(material, warehouse)   ← 派生数量与金额
        ⑤ 本行成本 = Σ 本行 ③ 的 amount（流水求和，不是乘法）
    else:
        ① 条件 UPDATE biz_inventory
              SET qty = qty − n, amount = amount − ROUND(n × avg_cost, 4), last_txn_at = now()
            WHERE id = 库存行id AND (? OR qty >= n)    ← ? = 仓库 allow_negative
           受影响 0 行 → 66010（库存不足）或 66015（并发）
        ② 写 1 条 OUT 流水（qty=n，unit_cost=avg_cost，amount=ROUND(n×avg_cost,4)）
        ③ 本行成本 = ② 的 amount
```

### 3.13.3 边界

| 场景 | 处理 |
|---|---|
| 同一张出库单里既有普通物料又有一物一码物料 | 允许，逐行分流。凭证仍只有两张（收入按行汇总、成本按成本合计） |
| 一物一码物料 `qty` 与件数不一致（历史脏数据） | 过账前先跑一次件数校验，不一致 → 拒绝并提示「请先在库存巡检页修复（批次 D 已有 inventory:audit）」 |
| 普通物料命中多行库存（同仓不同库位） | 本批**不按库位拆分**：一律扣 `location_id IS NULL` 的整仓汇总行；若该行不存在而库位行存在 → 先把库位行汇总到汇总行（或拒绝并提示，见附录 B 第 5 条） |
| 并发出库同一行库存 | 条件 UPDATE 天然串行；受影响 0 行 → 不重试，直接报「库存已被其他单据占用，请刷新后重试」 |

---

## 3.14 决策 ⑭：部分出库的应收与凭证粒度

### 3.14.1 候选方案对比

| 方案 | 做法 | 好处 | 坏处 | 结论 |
|---|---|---|---|---|
| A 按订单出一张应收，出库时累加金额 | 订单 → 一张应收，多次出库累加 | 客户对账简单 | ①应收金额在出库前就要定，但价格/数量可能变；②一张应收对应多张出库单，作废其中一张时应收怎么改？③**违反实库 `uk_biz_payable_source(source_type, source_id)` 的镜像设计**（一张来源单一张台账） | ❌ |
| **B 按出库单出一张应收 + 两张凭证** ★ | 每次出库（部分或全部）= 一张出库单 = 一张应收 = 两张凭证 | ①与批次 1「一张入库单一张应付一张凭证」完全对称；②作废边界清晰（作废哪张出库单，就冲哪张应收/凭证）；③客户对账能看到"这一车货一张单" | 一个订单分 3 次发 → 客户收到 3 张应收单（但账龄与对账按单正好） | ✅ **采用** |
| C 按订单 + 按次拆两张应收 | 订单级汇总应收 + 每次出库的明细应收 | 兼顾 | 两套台账要勾稽，复杂度翻倍 | ❌ |

### 3.14.2 结论与算例

```
粒度：一张出库单 → 一张应收（uk_biz_receivable_source 兜住）→ 两张凭证（收入 + 成本）。
订单不产生任何应收与凭证（订单没有会计影响，与批次 1 采购订单同口径）。

算例：订单 SO-001 共 60 件，分两次发（60 → 30 + 30，本批部分出库）
  出库 OUT-001：30 件，价税合计 67,800.00 → 应收 AR-001 67,800.00 → 凭证 PZ-…-0011/0012
  出库 OUT-002：30 件，价税合计 67,800.00 → 应收 AR-002 67,800.00 → 凭证 PZ-…-0013/0014
  订单 shipped_qty = 60 → 状态 OUTBOUND（全部发完）
  Σ 应收 = 135,600.00 = 订单价税合计 ✅（S5-05 勾稽表恒等式）
```

---

# 四、关键业务规则（开发依据，逐条给公式 + 算例 + 边界）

## 4.0 规则总览

| # | 规则 | 一句话结论 | 影响面 |
|---|---|---|---|
| 4.1 | 金额口径 | **沿用批次 1 一字不改**：默认不含税录入，价外税，三金额恒等；含税口径用减法倒挤 | 订单/出库明细、应收、凭证 |
| 4.2 | 出库成本结转 | 移动加权；`ROUND(qty × avg_cost, 4)`；清仓倒挤归零；负库存取最后入库价 | 库存、流水、成本凭证 |
| 4.3 | 应收生成 | 一张出库单一张应收；到期日 = 出库日 + 客户账期；金额 = 价税合计 | 应收台账、账龄 |
| 4.4 | 收入凭证 | 借 1122 / 贷 6001 / 贷 2221.02；多税率按税率拆行 | 总账、科目余额表 |
| 4.5 | 成本凭证与流水对齐 | 2 位 vs 4 位的对齐规则，末行倒挤；成本取快照不重算 | 总账 6401/1405 |
| 4.6 | 收款与核销 | 双向多对多；FIFO 默认；四项校验；状态机 | 应收状态、额度释放 |
| 4.7 | 信用额度 | 可用 = 额度 − 未结应收 − 已提交未出库订单；下单软拦出库硬拦 | 订单、出库 |
| 4.8 | 序列号出库 | 逐件、单向、派生数量；五类实时校验 | 出库、库存件数 |
| 4.9 | 取号 | `SO/OUT/AR/RC` 日号 6 位，按日重置，事务内取号不跳号 | 所有单据 |
| 4.10 | 出库过账事务 | 11 步，同事务，任一步失败全回滚 | 全链 |
| 4.11 | 作废与红冲 | 借贷互换；已核销的应收不能作废；序列号不可回退 | 出库/收款 |
| 4.12 | 期间控制 | 只认 `CLOSED`/`NOT_OPENED` 两态拦；四处校验点 | 所有写凭证入口 |
| 4.13 | 部分出库与订单进度 | 行级 `shipped_qty`；单级进度按金额；状态按已发比例流转 | 订单状态机 |

---
## 4.1 金额口径（沿用批次 1 §3.1，一个字不改）

### 4.1.1 事实前提

中国增值税是**价外税**：价格之外另收税。所以一笔销售必然有三个金额，且恒满足：

```
价税合计（客户该付多少） = 不含税金额（价款，即收入） + 税额（销项税，代国家收）
```

**这条是本章的地基**：收入只吃「不含税金额」，应收吃「价税合计」，销项税单独进 `2221.02`。
库存成本只吃「不含税金额」（采购侧同理，批次 1 已定）。

### 4.1.2 字段口径（全系统统一，不得各页自定义）

| 字段（销售侧列名） | 含义 | 实库精度 | 说明 |
|---|---|---|---|
| `qty` | 数量 | `NUMERIC(18,4)` | 展示去掉无意义尾 0（60.0000 → 60） |
| `price` | **不含税单价** | `NUMERIC(18,6)` | 与批次 1 的 `price` 列同名同义（**不叫 price_excl**，F4） |
| `price_incl` | 含税单价 | `NUMERIC(18,6)` | 由 `price × (1+税率)` 派生，不存权威值 |
| `amount` | 不含税金额（价款） | `NUMERIC(18,4)` | 计收入 |
| `tax_amount` | 税额（销项） | `NUMERIC(18,4)` | 计 2221.02 |
| `with_tax_amount` | 价税合计 | `NUMERIC(18,4)` | 计应收 |
| `tax_rate` | 税率（**百分数**） | `NUMERIC(7,4)`，CHECK 0–100 | 13 表示 13% |

> ⚠️ **税率的两个单位陷阱**（F17）：
> `biz_material.tax_rate` 是**比例**（0.13），单据明细 `tax_rate` 是**百分数**（13）。
> 从物料带出税率时必须 `× 100`，否则算出的税额是应有值的 1/100。
> 验收场景 1 专门有一条断言检查这件事。

### 4.1.3 两种录入口径与公式

单据头有一个口径开关 `price_mode`（实库 `VARCHAR(8)`，`EXCL`/`INCL`，默认 `EXCL`，F2）：

**口径 A：不含税单价录入（默认，主路径）**

```
amount         = round(qty × price, 2)                    ← 先定价款
tax_amount     = round(amount × tax_rate / 100, 2)        ← 再按价款计税
with_tax_amount = amount + tax_amount                     ← 加法得出，不再舍入
```

**口径 B：含税单价录入（客户报价是含税价时切过来）**

```
with_tax_amount = round(qty × price_incl, 2)              ← 先定价税合计
amount          = round(with_tax_amount / (1 + tax_rate/100), 2)   ← 倒算价款
tax_amount      = with_tax_amount − amount                ← 减法倒挤（唯一能保证恒等的写法）
```

### 4.1.4 算例 1：同样敲「2,000 元」，两种口径差 260 元/台

以「成品 D，1 台，单价 2,000 元，税率 13%」为例（已用计算器核验）：

| 口径 | 输入 | `amount` 价款 | `tax_amount` 税额 | `with_tax_amount` 价税合计 |
|---|---|---|---|---|
| **A 不含税（默认）** | 单价 2,000.000000 | **2,000.00** | **260.00** | **2,260.00** |
| B 含税 | 单价 2,000.000000 | 1,769.91 | 230.09 | 2,000.00 |

验算（A）：`1 × 2000 = 2000.00`；`2000.00 × 13% = 260.00`；`2000 + 260 = 2260.00` ✅
验算（B）：`1 × 2000 = 2000.00`；`2000 ÷ 1.13 = 1769.911504… → 1769.91`；`2000 − 1769.91 = 230.09` ✅

**同样的"2,000"，收入差 230.09、应收差 260.00。** 所以：
- 单据头必须常驻口径徽标 `[不含税]` / `[含税]`，**不许省略**；
- 切换时弹确认框：「切换为含税录入后，所有行将按各行税率重新计算金额，是否继续？」；
- 列表页金额列显示**价税合计**并在表头注明 `金额(价税合计)`。

### 4.1.5 算例 2：多税率一张单（销项税要按税率拆行）

订单 2 行：
- 行 1：成品 D 8 台 @2,000.00，13% → 价款 16,000.00 / 税 2,080.00 / 合计 18,080.00
- 行 2：运输服务 1 次 @500.00，9% → 价款 500.00 / 税 45.00 / 合计 545.00

单据合计：价款 16,500.00 / 税 2,125.00 / 合计 18,625.00

凭证（收入凭证，销项税按税率拆 2 行）：

| 行 | 科目 | 借方 | 贷方 | 辅助 |
|---|---|---|---|---|
| 1 | 1122 应收账款 | 18,625.00 | | 客户 华东电子 |
| 2 | 6001 主营业务收入 | | 16,000.00 | — |
| 3 | 6001 主营业务收入 | | 500.00 | — |
| 4 | 2221.02 应交税费-销项税（13%） | | 2,080.00 | — |
| 5 | 2221.02 应交税费-销项税（9%） | | 45.00 | — |
| | **合计** | **18,625.00** | **18,625.00** | ✅ 平衡 |

> 行 2、3 科目相同（6001），是否合并？**本批规定：按「收入行按物料汇总、税额行按税率汇总」两条各自合并**，
> 即行 2/3 可合并为一行 16,500.00（若合并，则行数变 4）。为便于按物料查明细，本批**不合并收入行**，
> 一行明细对一行凭证（`aux_material_id` 带上），财务要按物料看收入时能直接取。

### 4.1.6 汇总与尾差

```
单据 amount 合计          = Σ 各行 amount            （先逐行舍入到 2 位，再相加）
单据 tax_amount 合计      = Σ 各行 tax_amount
单据 with_tax_amount 合计 = Σ 各行 with_tax_amount
```

**禁止**「先把各行未舍入金额加总再一次性舍入」——那会让单据合计与凭证行之和差 1 分钱，凭证立刻不平。
凭证一律以**行合计**为准（批次 1 §3.1.5 同款）。

### 4.1.7 边界

| 场景 | 处理 |
|---|---|
| 税率 0（免税/不征税） | `tax_amount = 0`，收入凭证只有 2 行（借应收 / 贷收入），不产生销项税行 |
| 单价为 0（赠品/样品） | 允许，`amount = 0`；整单全 0 → **不生成凭证**，出库单详情提示「本单金额为 0，未生成凭证」（但**仍生成应收吗？不生成**，理由：0 元应收无意义且污染账龄） |
| 负单价 | **禁止**填负数（退货走批次 3，见决策 ⑥） |
| 数量为 0 的行 | 保存草稿允许；提交/过账时拦：「第 3 行数量为 0，请删除或填写数量」 |
| 超过 6 位小数的单价 | 前端限制 6 位，后端按 6 位舍入后计算 |
| 税率不在物料默认值 | 允许改（服务类/不同商品税率不同）；改动留痕（S2-12） |
| 同物料多行 | **不合并**（销售场景客户可能要求分行交付，合并会丢失交期信息） |

---

## 4.2 出库成本结转（移动加权，与批次 1 §3.2 同构）

### 4.2.1 公式（唯一版本，任何页面/接口都按这个算）

**普通物料（非一物一码）**

```
出库成本：
  cost_amount_4dp = ROUND(out_qty × avg_cost, 4)          ← 库存侧精度（实码 F27 同款）
  cost_amount     = round(cost_amount_4dp, 2)             ← 单据/凭证侧精度

结存更新（同一 SQL，原子）：
  new_qty    = old_qty − out_qty
  new_amount = old_amount − cost_amount_4dp
  avg_cost   = 不变（移动加权出库不改均价）
  若 new_qty = 0 → new_amount := 0.00（清仓倒挤，尾差一次带走）
```

**一物一码物料**：成本 = Σ 该行每件的 OUT 流水 `amount`（每件 `amount = 1 × avg_cost`），不走乘法（决策 ⑬）。

### 4.2.2 算例 1（卡面指定）：两批不同价入库后出库 50 件

| 步骤 | 业务 | 数量变动 | 金额变动 | 结存数量 | 结存金额 | avg_cost |
|---|---|---|---|---|---|---|
| 0 | 初始 | — | — | 0 | 0.00 | — |
| 1 | 入库 100 @2.00 | +100 | +200.00 | 100 | 200.00 | 2.000000 |
| 2 | 入库 100 @3.00 | +100 | +300.00 | 200 | 500.00 | **2.500000** |
| 3 | **出库 50** | −50 | **−125.00** | **150** | **375.00** | **2.500000** |

`50 × 2.500000 = 125.00` ✅；`500 − 125 = 375.00` ✅；`375 / 150 = 2.500000` ✅（均价不变）

### 4.2.3 算例 2：除不尽的均价 + 连续出库

| 步骤 | 业务 | 结存数量 | 结存金额 | avg_cost | 本次成本 |
|---|---|---|---|---|---|
| 1 | 入库 10 @1.11 | 10 | 11.10 | 1.110000 | — |
| 2 | 入库 3 @3.33 | 13 | 21.09 | 1.622308 | — |
| 3 | 出库 4 | 9 | 21.09 − 6.489232 = 14.600768 → **14.6008** | 1.622308 | 4 × 1.622308 = 6.489232 → **6.4892** |
| 4 | 出库 5 | 4 | 14.6008 − 8.11154 = 6.48926 → **6.4893** | 1.622308 | 5 × 1.622308 = 8.11154 → **8.1115** |

注意第 3/4 步：`cost` 与 `new_amount` 都在 4 位精度上做减法和乘法，**不允许把中间值先舍到 2 位再参与后续计算**，
否则连续出库会以每笔 1 分的速度漂移。

### 4.2.4 算例 3：清仓倒挤（本批最重要的一处尾差处置）

| 步骤 | 业务 | 结存数量 | 结存金额 | 本次成本 |
|---|---|---|---|---|
| 1 | 入库 10 @1.11 | 10 | 11.10 | — |
| 2 | 入库 3 @3.33 | 13 | 21.09 | — |
| 3 | 出库 13（清仓） | 0 | **0.00** | **21.09**（= 出库前结存金额，不是 13×1.622308 = 21.090004） |

✅ `qty = 0 → amount = 0` 恒成立；成本凭证 6401 = 21.09，与结存完全对齐。
若按乘法算会得到 21.090004，四舍五入到 4 位是 21.0900，与 21.09 恰好相等；但若均价是 `1.6223077`，
乘法结果 `21.0900001` 会留残值 —— 所以**一律倒挤，不做乘法**。

### 4.2.5 算例 4：负库存出库（allow_negative = 1）

```
设定：WH-B allow_negative=1；M-W1A-005
  10-01 入 10 @2.00 → qty=10 amount=20.00 avg_cost=2.000000
  10-05 出 10 → qty=0 amount=0.00（清仓倒挤），avg_cost=2.000000 不变
  10-08 出 5 → qty=−5
     成本 = 5 × 2.000000（最近一次入单价）= 10.00
     amount = 0 − 10 = −10.00
     流水 remark='负库存出库'
  10-12 入 8 @3.00 → qty=3 amount=−10+24=14.00 avg_cost=4.666667（被污染，接受）
```

### 4.2.6 边界

| 场景 | 处理 |
|---|---|
| 一物一码物料 `qty` 与件数不一致 | 过账前校验，不一致拒绝（66016），提示先去库存巡检修复 |
| 同物料在一张出库单多行 | 逐行顺序处理；因都是减法且不改均价，结果与合并一致 |
| 出库数量 0 | 保存草稿允许，过账拒绝 |
| 出库数量为负 | 禁止（`ck_biz_so_item_qty`/`ck_biz_out_item_qty` 均为 `qty > 0`） |
| 并发对同一库存行出库 | 条件 UPDATE（`WHERE qty >= n`），受影响 0 行 → 66015 不重试 |
| 结存金额出现极小负（如 −0.0001） | 只在清仓与负库存并存时可能出现；清仓时强制归零，其余情况记日志并由 4.5.4 的勾稽兜住 |
| `avg_cost = 0` 但有库存（历史脏数据） | 出库成本 = 0，**黄标警告**「该物料均价为 0，出库成本将为 0，请检查入库金额」，不阻止（避免卡死业务） |

---

## 4.3 应收单生成规则

### 4.3.1 公式

```
触发：出库单过账（POSTED）时，在同一事务里生成。
     一张出库单 → 一张应收（uk_biz_receivable_source(source_type, source_id) 兜住重复）

receivable：
  receivable_no  = AR-yyyyMMdd-000001（取号规则 4.9）
  customer_id    = 出库单.customer_id
  source_type    = 'STOCK_OUT'
  source_id      = 出库单.id
  source_no      = 出库单.out_no
  amount         = 出库单.total_with_tax            ← 价税合计（唯一口径）
  received_amount= 0
  unpaid_amount  = amount
  credit_days    = 客户档案.credit_days             ← 从主数据取（同批次 1 F26）
  due_date       = 出库单.out_date + credit_days
  period_code    = 出库单.out_date 所属期间（voucher::period_of）
  status         = 'OPEN'
  voucher_id     = 收入凭证 id
```

### 4.3.2 算例

```
客户 华东电子 credit_days = 30
出库单 OUT-20261031-000001：out_date=2026-10-31，total_with_tax=18,080.00
  → AR-20261031-000001
     amount=18,080.00 received=0 unpaid=18,080.00
     due_date = 2026-10-31 + 30 天 = **2026-11-30**
     period_code = '2026-10'
     status = OPEN
```

### 4.3.3 边界

| 场景 | 处理 |
|---|---|
| 客户 `credit_days = 0`（现结） | `due_date = out_date`，当天到期；账龄从次日起算逾期 |
| 客户账期为负 | 主数据 CHECK 拦（`credit_days >= 0`，仿 F18 的 `ck_biz_supplier_credit_days`） |
| 出库单金额为 0 | **不生成应收**（0 元应收会污染账龄与客户对账） |
| 出库单作废 | 对应应收 → `CANCELLED`（**不可物理删**）；若已被收款核销 → 拒绝作废（4.11.2） |
| 重复过账 / 重试 | `uk_biz_receivable_source` 抛唯一冲突 → 事务回滚，接口返回「该出库单已生成应收单」（66013） |
| 客户被停用 | 不影响已有应收的账龄计算（停用只是不能开新单） |

---

## 4.4 收入凭证（`SALE_OUT`，3 行）

### 4.4.1 分录模板

```
借 1122 应收账款（客户辅助)          with_tax_amount 合计
贷 6001 主营业务收入（物料辅助/行）  amount 合计（按行，不合并）
贷 2221.02 应交税费-销项税（按税率）tax_amount 合计（按税率拆行，税率为 0 时不出行）
```

### 4.4.2 单行算例

出库单 OUT-20261031-000001：成品 D 8 台 @2,000.00（不含税），13%

| 行 | 摘要 | 科目 | 借方 | 贷方 | 辅助 |
|---|---|---|---|---|---|
| 1 | 销售出库 OUT-20261031-000001 华东电子 | 1122 应收账款 | 18,080.00 | | 客户：华东电子 |
| 2 | 销售出库 OUT-20261031-000001 成品D | 6001 主营业务收入 | | 16,000.00 | 物料：M-W1A-004 |
| 3 | 销售出库 OUT-20261031-000001 销项税额(13%) | 2221.02 应交税费-销项税 | | 2,080.00 | — |
| | **合计** | | **18,080.00** | **18,080.00** | ✅ |

### 4.4.3 多税率算例

见 4.1.5（5 行版本）。**关键点**：销项税按税率拆行，收入按行不合并。

### 4.4.4 凭证与实码的对接（给 backend 的落地口径）

```
现有引擎（voucher.rs:330 load_rule）取的是 debit/credit/tax 三科目，且税额行恒为「借」。
本批必须：
  ① 规则表补 tax_side（F10 / 决策 ⑦），load_rule 的 SELECT 增加 tax_side 列；
  ② 生成分录时：tax_side='DEBIT' → 税额行在借方（批次 1 采购）；'CREDIT' → 贷方（本批销售）；
  ③ 一个 biz_type 一张凭证 → SALE_OUT 与 SALE_OUT_COST 两个 biz_type（决策 ⑦）；
  ④ 辅助核算：应收行写 aux_customer_id（本批 ALTER 新增列，F9），收入行写 aux_material_id，
     仓库写 aux_warehouse_id（已有列）；
  ⑤ 凭证状态 POSTED、voucher_type='AUTO'、source_type='SALE_OUT'、source_no=出库单号。
```

### 4.4.5 收款凭证（`RECEIPT`）与预收应用凭证（`RECEIPT_APPLY`）

```
RECEIPT（收款单审核时）：
  借 1002 银行存款（收款账户）        收款金额
  贷 1122 应收账款（客户辅助）        Σ核销金额
  贷 2203 预收账款（客户辅助）        收款金额 − Σ核销金额（差额为 0 时不出行）

RECEIPT_APPLY（用预收余额核销应收时）：
  借 2203 预收账款（客户辅助）        本次用掉的预收
  贷 1122 应收账款（客户辅助）        本次用掉的预收
```

算例（预收 1,000 → 后来核销 600）：

```
① 收款 6,000，核销 5,000 → 凭证：借 1002 6,000 / 贷 1122 5,000 / 贷 2203 1,000
② 两周后新增应收 800，用预收核销 600 →
   应收：received 600，unpaid 200，状态 PARTIAL_PAID
   预收余额：1,000 − 600 = 400
   凭证（RECEIPT_APPLY）：借 2203 600 / 贷 1122 600
```

---

## 4.5 成本凭证与库存流水的对齐规则（本批最容易被忽略的 1 分钱）

### 4.5.1 两套精度为什么会打架

```
库存侧：biz_inventory.amount NUMERIC(18,4)，流水 amount NUMERIC(18,4)   → 4 位
凭证侧：biz_voucher_entry.debit/credit_amount NUMERIC(20,4)            → 4 位
单据侧：biz_stock_out.total_* NUMERIC(18,4)，但业务口径按 2 位舍入      → 2 位
```

`ROUND(7 × 1.622308, 4) = 11.3562`，而按业务口径 2 位是 `11.36`。若成本凭证直接写 `11.3562`，
财务看凭证会觉得"金额为什么有 4 位小数"；若写 `11.36`，则凭证合计与流水合计差 0.0038，勾稽表报红。

### 4.5.2 结论（对齐规则）

```
① 成本凭证每行金额 = round(该行 cost_amount_4dp, 2)；
② 凭证末行（贷 1405 那行）金额 = 借 6401 合计（借贷必须相等，这是 DB CHECK）；
③ 出库单 cost_amount 存 4 位（= 流水 amount 之和，可追溯）；
④ 凭证与单据的差额（Σ round(cost,2) − Σ cost_4dp，绝对值 < 0.005×行数）**不回写单据**，
   由 S5-05 勾稽表以「成本尾差」一行披露；月结时财务可接受（< 1 分/单）；
⑤ 严禁把尾差挂到"成本调整"科目（批次 1 §3.2.3 已明确不做挂账）。
```

### 4.5.3 算例：尾差 0.0038 的完整处置

```
出库 2 行（同一物料，不作合并）：
  行 1：qty=7  avg_cost=1.622308 → cost_4dp = 11.356156 → 11.3562 → 2 位 11.36
  行 2：qty=6  avg_cost=1.622308 → cost_4dp = 9.733848 → 9.7338 → 2 位 9.73

出库单 cost_amount 合计 = 11.3562 + 9.7338 = 21.0900（4 位）
成本凭证：
  借 6401 主营业务成本  11.36
  借 6401 主营业务成本   9.73
  贷 1405 库存商品      21.09        ← 借方合计 21.09，借贷相等 ✅
尾差：21.09（凭证）− 21.0900（流水）= 0.0000 —— 本例恰好为 0
```

再举一个**必然有尾差**的：

```
出库 3 行，每行 cost_4dp 均为 1.1111（合计 3.3333）
凭证：借 6401 1.11 / 1.11 / 1.11 = 3.33；贷 1405 = 3.33
尾差 = 3.33 − 3.3333 = −0.0033 → S5-05「成本尾差」行披露 −0.0033
库存侧 amount 仍按 3.3333 扣减（权威值），总账 1405 与库存金额差 0.0033，
月末由财务决定是否做一笔手工调整凭证（本批不做自动调整，批次 4 结账时统一处理）
```

### 4.5.4 成本来源的唯一性（谁都不许重算）

| 位置 | 成本取自 | 禁止 |
|---|---|---|
| 出库单明细 `cost_price` / `cost_amount` | 过账时快照（`avg_cost` 当时值；一物一码取流水 unit_cost） | ❌ 不许在查询时用当前均价重算 |
| 成本凭证 | 出库单明细 `cost_amount` 的 2 位舍入 | ❌ 不许重新查库存算 |
| 毛利分析（S5-03） | `biz_stock_out_item.cost_amount` | ❌ 不许用当前均价 |
| 出库成本明细表（S5-06） | 明细 + 流水快照 | — |

**反例（说明为什么必须快照）**：
10 月出库 100 件，成本 200.00（均价 2.00）。11 月又入库 100 件 @4.00，均价变 3.00。
如果报表用「当前均价 × 数量」算，10 月的成本会变成 300.00，**10 月的毛利凭空减少 100.00**。
报表不许"今天看和昨天看不一样"。

---

## 4.6 收款与核销规则

### 4.6.1 数据结构

```
biz_receipt（收款单，主表）
  receipt_no, customer_id, receipt_date, receipt_method(TRANSFER/CASH/ACCEPTANCE/OTHER),
  account_code(银行存款科目编码), amount, allocated_amount(已核销合计), 
  unallocated_amount(未核销=预收), status(DRAFT/POSTED/CANCELLED), voucher_id,
  bank_ref(银行流水号), remark, 审计

biz_receipt_item（核销明细）
  receipt_id, receivable_id, allocated_amount, source_no(应收单号快照),
  created_at, create_by
```

### 4.6.2 公式

```
核销校验（审核时全过，任一不过整个收款单不能审核）：
  ① 每行 allocated_amount > 0
  ② Σ allocated_amount ≤ receipt.amount
  ③ 每张应收：该行 allocated_amount ≤ 该应收.unpaid_amount（事务内 SELECT ... FOR UPDATE 读到的值）
  ④ 所有 receivable.customer_id == receipt.customer_id（不允许跨客户核销）
  ⑤ 应收状态必须是 OPEN 或 PARTIAL_PAID（SETTLED/CANCELLED 不能核销）

核销结果：
  receivable.received_amount += allocated
  receivable.unpaid_amount    = amount − received_amount
  receivable.status = CASE
      WHEN received_amount = amount THEN 'SETTLED'
      WHEN received_amount > 0      THEN 'PARTIAL_PAID'
      ELSE 'OPEN' END
  若变成 SETTLED → 写 settled_at = now()

receipt.allocated_amount   = Σ allocated
receipt.unallocated_amount = amount − Σ allocated
```

### 4.6.3 FIFO 自动核销算法（前端填数，后端复核）

```
输入：customer_id、收款金额 A
步骤：
  1. 取该客户 status IN ('OPEN','PARTIAL_PAID') 的应收，按 due_date ASC, receivable_no ASC 排序
  2. 依次分配：alloc = min(A_剩余, 应收.unpaid_amount)
  3. A_剩余 = 0 时停止；剩余未分配额进「预收」
注意：FIFO 只是**填数建议**，最终提交的是明细行；后端不认"FIFO"这个标记，只认明细
```

### 4.6.4 算例 1：一笔收款核销三张应收（部分收款）

```
客户 江南贸易，未结：
  AR-A 到期 11-14  565.00  未收 565.00
  AR-B 到期 11-19  226.00  未收 226.00
  AR-C 到期 11-30  18,080.00 未收 18,080.00
收款 6,000.00，FIFO：
  AR-A 565.00  → SETTLED
  AR-B 226.00  → SETTLED
  AR-C 5,209.00 → PARTIAL_PAID（未收 12,871.00）
  Σ = 6,000.00 ✅
  unallocated = 0.00
```

### 4.6.5 算例 2：一张应收多次收款（多对一）

```
AR-C 18,080.00
  11-10 收 5,000 → PARTIAL_PAID，未收 13,080.00
  11-20 收 8,000 → PARTIAL_PAID，未收 5,080.00
  11-28 收 5,080 → SETTLED，settled_at=11-28
三次收款三张收款单，`biz_receipt_item` 三行指向同一 receivable_id ✅
```

### 4.6.6 算例 3：预收（核销不足）

```
收款 6,000.00，客户说"这是明年的预付款"，财务不核销任何应收：
  Σ allocated = 0 → allocated_amount=0, unallocated_amount=6,000.00
  凭证：借 1002 6,000 / 贷 2203 6,000（情形 3）
  客户详情页：预收余额 6,000.00
  两周后新出库产生 AR-D 800.00 → 用预收核销 600 →
     AR-D: received 600 / unpaid 200 / PARTIAL_PAID
     预收余额 5,400
     凭证 RECEIPT_APPLY：借 2203 600 / 贷 1122 600
```

### 4.6.7 边界

| 场景 | 处理 |
|---|---|
| 核销金额 > 收款金额 | 拒绝（66017）：「核销合计 6,100.00 超过收款金额 6,000.00，请调整」 |
| 核销金额 > 该应收未收 | 拒绝（66018）：「应收 AR-… 未收仅 12,871.00，本次核销 13,000.00 超出」 |
| 跨客户核销 | 拒绝（66019）：「收款单客户为 江南贸易，应收 AR-… 属 华东电子，不允许跨客户核销」 |
| 核销 0 元行 | 拒绝：「第 2 行核销金额为 0，请删除该行」 |
| 已结清应收再被核销 | 拒绝：该行不在可选列表（后端二次校验） |
| 收款单金额为 0 | 拒绝：「收款金额必须大于 0」 |
| 收款单已作废 | 其核销明细全部反向冲回（4.11.3），应收状态回退 |
| 一张收款单核销超过 20 张应收 | 允许（无上限），但前端分页展示（性能保护） |
| 同一张应收在同一收款单里出现两次 | 拒绝（前端禁止重复选择，后端按 receivable_id 去重校验） |

---
## 4.7 信用额度规则

### 4.7.1 公式（唯一版本，任何页面都按这个算）

```
信用额度（credit_limit）：来自 biz_customer.credit_limit，NUMERIC(18,4)，默认 0

占用（occupied_amount）：
  occupied = Σ 未结应收（status IN ('OPEN','PARTIAL_PAID') 的 unpaid_amount）
           + Σ 已提交未出库订单金额（status IN ('SUBMITTED','APPROVED','PARTIAL_OUT') 的未发金额）

  其中「未发金额」= Σ明细行 (qty − shipped_qty) × 该行含税单价（按行算再汇总）

可用额度（available_credit）：
  available = credit_limit − occupied

是否超额（is_over）：
  available < 0（下单时用「本单金额 > available」，出库时用「本单金额 > available + 本单占用」）
```

> **为什么占用要包含"已提交未出库订单"**：如果不含，客户可以连下 10 张 10 万的单都不报错，
> 到出库时全部硬拦，业务直接瘫痪。含进去以后，下单时就能看到超额。
> **为什么出库时还要再算一次**：因为出库那一刻应收才真正产生，"订单占用"要换成"应收占用"，
> 否则会双算（订单未发金额 + 新出的应收 = 同一笔钱算两次）。**规则 4.7.4 给了替换的精确写法。**

### 4.7.2 算例 1：下单 → 出库 → 收款 全链路占用变化

```
设定：credit_limit = 100,000.00，初始 occupied = 0

① 10-20 出库 8 件，价税合计 18,080.00
   → 应收 AR-001 unpaid 18,080.00
   occupied = 18,080.00，available = 81,920.00

② 10-31 订单 SO-001 135,600.00 提交
   订单占用 = 135,600.00
   occupied = 18,080 + 135,600 = 153,680.00
   available = 100,000 − 153,680 = **−53,680.00** → 超额，软拦
   销售申请放行 → 例外 EX-001 放行 53,680.00

③ 11-02 出库 30 件（67,800.00），订单未发金额从 135,600 降到 67,800
   出库后：
     应收 AR-002 unpaid 67,800.00
     订单占用 = 67,800.00
     occupied = 18,080 + 67,800 + 67,800 = 153,680.00（不变 ✅ 因为订单占用与应收占用此消彼长）
   —— 这一步最容易算错：出库时必须**先减订单未发金额，再加应收金额**，否则会双算 67,800

④ 11-28 收款 18,080 核销 AR-001 → AR-001 SETTLED
   occupied = 0 + 67,800 + 67,800 = 135,600.00
   available = 100,000 − 135,600 = −35,600.00（仍超额，因为订单还有 67,800 没发）
```

### 4.7.3 出库时的校验公式（避免双算的精确写法）

```
出库单对应订单行：qty_this = 本次出库数量，含税单价 price_incl
本单含税金额 M = Σ(qty_this × price_incl)      ← 与出库单 total_with_tax 一致

出库前的占用（事务内读）：
  O_before = Σ未结应收.unpaid + Σ(未发金额)

出库后的占用（本单生效后）：
  O_after = (Σ未结应收.unpaid + M) + (Σ(未发金额) − M)
          = Σ未结应收.unpaid + Σ(未发金额)     ← 数学上等于 O_before（若不考虑额度变化）

因此校验用：
  M > credit_limit − O_before + 例外可用额  →  超额（拒绝或看例外）
即：
  credit_limit − O_before ≥ M − 例外可用额
```

**算例**：`credit_limit=100,000`，`O_before = 90,000`（其中应收 20,000 + 未发订单 70,000），
本单出库 `M = 15,000`，本例无例外：

```
credit_limit − O_before = 10,000 < 15,000 → 超额 5,000 → **拒绝出库**（66012）
文案：「客户 华东电子 超额 5,000.00（额度 100,000.00，已占用 90,000.00，本单 15,000.00）。
      请先回款、提额，或申请超额放行。」
```

### 4.7.4 占用口径的一个例外：现结客户

```
若客户 settlement_type = 'CASH'（现结/款到发货）：
  下单占用 = 0（不占额度）
  出库时要求「本单已验证收款」→ 校验该订单关联的收款单已 POSTED 且金额 ≥ 本单金额
  出库后生成应收 + 立刻被该收款核销 → unpaid = 0 → 不占额度
理由：现结客户的额度概念无意义，强行占额度会让现结客户开不了单
```

### 4.7.5 边界

| 场景 | 处理 |
|---|---|
| `credit_limit = 0` | 视为「禁止赊销」：下单软拦、出库硬拦；现结客户不受影响（4.7.4） |
| 客户已冻结 | 下单直接拒（非软拦），出库也拒（M-05） |
| 例外有效期到期 | 自动失效；异常在于「例外是在下单时生成的、出库时可能已过期」→ 出库校验以**有效期为准**，过期即拒绝 |
| 例外额度不够 | 拒绝：「本单超额 8,000.00，例外剩余可用 3,000.00，请追加放行」 |
| 同一订单多次出库 | 例外额度按**累计出库金额**扣减（不是按次） |
| 客户提额后 | 立即生效；已存在的超额标记不会自动清除（需重新提交或审批通过） |
| 降额导致已存在订单超额 | **不追溯**：已批准的订单照常出库（例外仍在有效期内即可）；只有新单受新额度约束 |
| 并发（两单同时提交） | 提交时 `SELECT ... FOR UPDATE` 锁客户行后重算占用；锁顺序固定为「先客户后单据」，避免死锁 |

### 4.7.6 并发算例（说明为什么必须锁）

```
额度 100,000，当前占用 0。两个销售同时提交 80,000 的两张单。

不加锁（错）：
  A 读到 occupied=0 → 80,000 ≤ 100,000 → 通过
  B 读到 occupied=0 → 80,000 ≤ 100,000 → 通过
  结果：占用 160,000，超额 60,000，且都没有审批记录

加锁（对）：
  A 先锁客户行 → 重算 occupied=0 → 通过 → 占用变 80,000 → 提交事务释放锁
  B 拿到锁 → 重算 occupied=80,000 → 80,000 > 20,000 剩余 → 软拦，标超额待批
```

---

## 4.8 序列号出库规则

### 4.8.1 适用范围与强制程度

```
仅 material.serial_required = true 的物料。
该行的出库数量 = 已绑定序列号数（由系统派生，**不允许手工填数量**）。
数量 ≠ 序列号数 → 不许过账（66014）。
```

### 4.8.2 状态与数据写入（严格按实码 F28/F29）

```
对每个被扫的序列号（事务内逐件）：
  UPDATE biz_material_serial
     SET status = 'OUTBOUND',
         outbound_at = now(),          ← COALESCE 语义，已出库的不覆盖（正常也不会走到）
         source_type = 'SALE_OUT',
         source_id = 出库单id,
         update_by = 操作人
   WHERE id = ? AND status = 'IN_STOCK' AND warehouse_id = 本单仓库
  受影响行数必须 = 1；= 0 → 66015「序列号状态已变化，请刷新后重试」

然后（同一事务）：
  recompute_quantity(material_id, warehouse_id)
    → biz_inventory.qty = COUNT(serial WHERE status='IN_STOCK')
    → biz_inventory.amount = qty × avg_cost
  写 OUT 流水（每件一条）：
    txn_type='OUT', biz_type='SALE_OUT', doc_type='STOCK_OUT', doc_id=出库单id,
    doc_no=出库单号, line_no=明细行号, qty=1, unit_cost=avg_cost, amount=1×avg_cost,
    before_*/after_* = 该件处理前后的快照
```

> **为什么不复用 `serial_model::transition_status_with`**：那个函数的 `doc_type` 固定为 `MATERIAL_SERIAL`、
> `biz_type` 固定为 `SERIAL_OUT`，还带 `remark` 文案逻辑。销售出库需要 `doc_type='STOCK_OUT'` 才能
> 按出库单反查流水（`idx_biz_inv_txn_doc`）。**复用它的 `recompute_quantity` 与 `can_transition`，不复用它的写库函数**。

### 4.8.3 扫码交互（前端照着做）

```
① 行内「序列号」列 = 一个常驻焦点的输入框（扫码枪自动回车提交）
② 每次提交：调 POST /business/erp/stock-out/{id}/item/{lineNo}/scan { serial_no }
   成功 → 下方已扫列表追加一行（序号、序列号、所在库位、入库时间）
   失败 → 输入框红框 + 下方红字（文案见 5.6）
③ 计数徽标：已扫 5 / 需 8（需 = 该行数量）；满额后输入框禁用、边框转绿
④ 支持「批量粘贴」：一次粘贴多行，逐行校验，把非法行单独列出来（不整体失败）
⑤ 支持「移除」已扫的某一件（仅 DRAFT 状态）
⑥ 不支持"随机挑号"按钮（决策 ⑤ 的 A 方案已被否）
```

### 4.8.4 五类实时校验与文案

| # | 校验 | 错误码 | 文案 |
|---|---|---|---|
| 1 | 序列号不存在（或已软删） | 66020 | 「序列号 SN-20261031-000009 不存在，请核对后重新扫码」 |
| 2 | 已出库 | 66021 | 「序列号 SN-… 已于 2026-10-20 出库（单据 OUT-20261020-0003），不能重复出库」 |
| 3 | 不在本单仓库 | 66022 | 「序列号 SN-… 当前在 WH-01，与本单仓库 WH-02 不一致」 |
| 4 | 非本行物料 | 66023 | 「序列号 SN-… 属于物料 M-W1A-005，与本行物料 M-W1A-004 不一致」 |
| 5 | 已报废 | 66024 | 「序列号 SN-… 已报废，不能出库」 |
| 6 | 本次重复扫 | 66025 | 「该序列号已在本次出库中（第 1 行）」 |
| 7 | 物料不需要序列号 | 66026 | 「物料 M-W1A-001 不是一物一码物料，本行不需要扫码」 |

### 4.8.5 边界

| 场景 | 处理 |
|---|---|
| 一物一码物料 + 仓库 allow_negative=1 | **负库存天然不可能**（扫不到实物就是没有）。这条要写进测试范围，避免测试白造用例 |
| 一物一码物料数量 > 在库件数 | 过账时报 66010 变体：「物料 成品D 在仓库 WH02 在库 8 件，本次出库 10 件」 |
| 出库单作废（一物一码） | **不允许**（决策 ⑥ / 4.11.2）：序列号不可逆，作废会导致账面数量与件数永久不一致 |
| 序列号跨仓调拨后再出库 | 允许（调拨只改 `warehouse_id`，状态仍 `IN_STOCK`，F28 的 move_serials 不动 status） |
| 一张单同物料两行 | 不允许（前端禁止；后端按 material_id 合并校验，避免一个号在两行都能扫） |

---

## 4.9 单据编号与取号

### 4.9.1 规则

| 单据 | 前缀 | 格式 | 示例 | 重置 |
|---|---|---|---|---|
| 销售订单 | `SO` | `SO-yyyyMMdd-000001` | `SO-20261031-000001` | 按日 |
| 销售出库单 | `OUT` | `OUT-yyyyMMdd-000001` | `OUT-20261031-000001` | 按日 |
| 应收单 | `AR` | `AR-yyyyMMdd-000001` | `AR-20261031-000001` | 按日 |
| 收款单 | `RC` | `RC-yyyyMMdd-000001` | `RC-20261028-000001` | 按日 |
| 凭证 | `PZ` | `PZ-yyyyMMdd-000001` | `PZ-20261031-000002` | 按日 |

### 4.9.2 实现口径（沿用 F12 与实码 `voucher::next_doc_no` / `payable::next_payable_no`）

```
biz_seq 需要新增 4 行：
  ('SO', '销售订单号', 'SO', 'yyyyMMdd', '', 0, 1, '-', 1)
  ('OUT','销售出库单号','OUT','yyyyMMdd', '', 0, 1, '-', 1)
  ('AR', '应收单号', 'AR', 'yyyyMMdd', '', 0, 1, '-', 1)
  ('RC', '收款单号', 'RC', 'yyyyMMdd', '', 0, 1, '-', 1)

取号 = 同一事务内 UPDATE ... RETURNING current_no（幂等补行 + 冲突视为已存在）
生成时机：保存草稿即取号（与批次 1 同口径，草稿也要能被引用）
跳号：进程崩溃允许跳号（不做号回收，审计上"跳号"远比"重号"安全）
日期归属：按单据业务日期取号（补录 10-31 的单填 10-31，号也跟着 10-31）
```

### 4.9.3 算例

```
2026-10-31 当天第 1 张出库单：
  biz_seq(seq_type='OUT', period_key='20261031', current_no=0)
  UPDATE → current_no=1 → format('OUT','20261031',1) = 'OUT-20261031-000001' ✅

跨日重置：
  2026-11-01 首次取号 → period_key 变化 → current_no 重置为 1 → 'OUT-20261101-000001'
  （period_key 不匹配时重置 current_no，实码已实现此逻辑，F12）
```

### 4.9.4 边界

| 场景 | 处理 |
|---|---|
| 同日并发取号 | `UPDATE ... RETURNING` 行锁保证串行，不会重号 |
| 事务回滚 | 号自动回退（取号在同一事务内） |
| 跨月补单 | 号按业务日期，不按操作日期 |
| 号段被人工清空 | 下次取号从 1 开始，可能与历史重号 → `uk_biz_so_no` 会拦住；运维需手工修 `current_no`（写进上线检查清单） |

---

## 4.10 出库过账事务（本批最关键的一个事务）

### 4.10.1 11 步（全在一个事务里，任一步失败整体回滚）

```
① 校验单据
   status 必须是 DRAFT；行数 ≥ 1；每行 qty > 0
   期间校验：out_date 所属期间 status 必须是 'OPEN'（4.12）
   日期范围：今天 − 365 天 ≤ out_date ≤ 今天 + 90 天
② 校验主数据
   客户存在且 status=启用 且未冻结
   仓库存在且 is_leaf=1 且 status=启用
   每行物料存在且启用
③ 信用校验（4.7.3）
   超额且无有效例外 → 66012 拒绝；现结客户走 4.7.4 分支
④ 逐行扣减（按决策 ⑬ 分流）
   普通物料：条件 UPDATE biz_inventory（4.2.1）→ 写 OUT 流水
   一物一码：序列号状态流转 → recompute_quantity → 写 OUT 流水（4.8.2）
⑤ 计算成本并回写明细
   cost_price = avg_cost（快照）；cost_amount = 该行成本（4 位）
⑥ 回写订单（若 out_type='SALE' 且 source_order_id 非空）
   明细行 shipped_qty += qty（CHECK 保证 ≤ qty）
   重算订单状态（4.13.3）
⑦ 生成应收（4.3）
   插 biz_receivable，拿 id
⑧ 生成收入凭证（4.4）
   同事务，凭证状态 POSTED
⑨ 生成成本凭证（4.5）
   同事务，凭证状态 POSTED
⑩ 回写出库单
   status='POSTED'，posted_at/posted_by
   voucher_id=收入凭证，voucher_cost_id=成本凭证，receivable_id=应收
   total_qty/amount/tax/with_tax 按行汇总（与订单同口径）
⑪ 同事务不变式断言（照抄批次 D 调拨的 `assert_settle_invariants`，F30）
   a) 本单动过的库存行 qty ≥ 0（除非该仓 allow_negative=1）
   b) 一物一码物料：qty == COUNT(IN_STOCK)（该物料该仓）
   c) 成本守恒：Σ 本单 OUT 流水 amount == Σ 明细 cost_amount（允许 4 位精度差 0）
   d) 凭证平衡：两张凭证各自 total_debit == total_credit（DB CHECK 兜底）
   任一不成立 → 抛错 → 全事务回滚
```

### 4.10.2 锁顺序（防死锁）

```
统一顺序：客户行 → 单据行 → 库存行（按 material_id, warehouse_id 升序）→ 序列号行（按 id 升序）
理由：批次 1 入库已用「按 item_id 升序」加锁（§3.2.6），本批沿用同一原则；
      出库额外加了客户行（信用校验）与序列号行，都放在最后，避免与入库交叉
```

### 4.10.3 回滚验证算例（测试必做）

```
构造：一张出库单 2 行，行 1 是普通物料（够扣），行 2 是一物一码物料（序列号有 1 个已出库）
预期：
  过账失败，返回 66021
  行 1 的库存**必须完全没动**（qty/amount/流水全无变化）
  订单 shipped_qty 未变
  不存在应收、不存在任何凭证
断言 SQL（测试脚本用）：
  select count(*) from biz_inventory_txn where doc_id = '<出库单id>'   → 0
  select count(*) from biz_receivable where source_id = '<出库单id>'    → 0
  select count(*) from biz_voucher where source_id = '<出库单id>'       → 0
```

### 4.10.4 性能约束

```
单张出库单明细行 ≤ 200 行；一物一码行每行 ≤ 500 件（超过要求拆单）
理由：每件序列号一次 UPDATE + 一条流水 INSERT，500 件约 500 次往返，实测 1–2 秒可接受；
      1000 件以上会让事务持锁时间过长，与并发出库互相阻塞
批量优化（P1）：序列号状态流转改成 `UPDATE ... WHERE id = ANY($1)` 一次更新 N 行，只写 N 条流水
```

---

## 4.11 作废与红冲

### 4.11.1 草稿作废（无副作用）

| 动作 | 条件 | 效果 |
|---|---|---|
| 删除草稿出库单 | `status='DRAFT'` | 软删，无任何库存/财务影响 |
| 草稿作废 | 同上 | 保留痕迹，`status='CANCELLED'`，列表默认不显示 |

### 4.11.2 已过账出库单作废（红冲）

**允许条件（全部满足）**

```
① status='POSTED'
② 作废动作当天所属期间 status='OPEN'（红字记在"作废当天"）
③ 关联应收 status='OPEN'（**已被任何收款核销过 → 拒绝**）
④ 该单据**所有明细行都不是一物一码物料**（决策 ⑥ / 4.8.5：序列号不可逆）
⑤ 库存回补后不会导致负数（`qty + n ≥ 0` 恒成立，因回补是加，不必校验；但若结存当前为负且仓库不许负 → 允许回补）
```

**6 步（同一事务）**

```
① 反向库存流水：txn_type='IN'，biz_type='SALE_OUT_CANCEL'，
   qty=原出库数量，unit_cost=原出库的 unit_cost，amount=原出库流水 amount（**原值冲回，不重算**）
   结存：new_qty = old_qty + 原数量；new_amount = old_amount + 原金额
   若回补后 new_qty 从 0 变正 → avg_cost 用原 unit_cost（因为结存金额是按原成本加回来的）
② 红字凭证 ×2：
   收入凭证红冲（借贷互换：借 6001 / 借 2221.02 / 贷 1122）
   成本凭证红冲（借贷互换：借 1405 / 贷 6401）
   两张都 is_red=1、reverse_of=原凭证id；原凭证 status 不变但仍写 reversed_by
③ 应收冲销：status='CANCELLED'，unpaid_amount=0，received_amount 保持 0（本来就没收）
④ 订单回退：shipped_qty −= 原数量；重算订单状态（OUTBOUND → APPROVED/PARTIAL_OUT）
⑤ 单据 status='CANCELLED'，cancelled_at/by，cancel_reason（必填 ≥5 字）
⑥ 信用占用自动回落（应收已 CANCELLED，不再计入 occupied）
```

**算例**

```
OUT-20261031-000001：成品 E 10 件 @100.00，13%
  金额：价款 1,000.00 / 税 130.00 / 合计 1,130.00
  成本：10 × 2.000000 = 20.00（假设均价 2.00）
  过账后：库存 qty 减 10、amount 减 20.00；应收 AR-001 1,130.00；凭证 PZ-0011（3 行）、PZ-0012（2 行）

作废（假设应收未收）：
  反向流水：IN 10 件，amount 20.00，结存金额回到原值
  红字收入凭证 PZ-0021：借 6001 1,000.00 / 借 2221.02 130.00 / 贷 1122 1,130.00
  红字成本凭证 PZ-0022：借 1405 20.00 / 贷 6401 20.00
  应收 AR-001 → CANCELLED
  订单 shipped_qty 10 → 0，状态 OUTBOUND → APPROVED
  ✅ 本期 6001 发生额 = 1,000.00 + (−1,000.00) = 0.00（红字借贷互换的净效果等价于冲销）
```

### 4.11.3 已过账收款单作废（反核销 + 红冲）

```
允许条件：
  ① status='POSTED'
  ② 收款日期所属期间 status='OPEN'
  ③ **本张收款单的核销，未被"后续预收应用"二次使用**（即不存在由它产生的预收被消费）→ 若已消费则拒绝
步骤（同一事务）：
  ① 逐条反核销：receivable.received_amount −= allocated；unpaid 重算；状态回退（SETTLED→PARTIAL_PAID / →OPEN）
  ② 红字凭证：借贷互换（借 1122/2203 / 贷 1002）
  ③ 收款单 status='CANCELLED'
边界：若应收在收款后又被作废（4.11.2 已拒绝这种情况），不会出现循环
```

### 4.11.4 红冲的三条硬约束（来自实码 F25）

| 约束 | 原因 |
|---|---|
| **借贷互换**，不是金额取负 | `ck_biz_voucher_entry_positive` + `ck_biz_voucher_entry_side` 禁负数 |
| `is_red=1` **必须**配 `reverse_of` | `ck_biz_voucher_red_ref` 是双向 CHECK |
| 红字凭证**不能再被冲销**；已冲销的原凭证不能再冲第二次 | 防循环（`reverse_of` 自引用检查 `ck_biz_voucher_self`） |

---

## 4.12 期间控制与日期边界

### 4.12.1 四处校验点（缺一不可）

| # | 动作 | 用哪个日期定期间 | 期间不 OPEN 时 |
|---|---|---|---|
| 1 | 出库过账 | `out_date` | ❌ 拒绝 66027 |
| 2 | 收款单审核 | `receipt_date` | ❌ 拒绝 66027 |
| 3 | 出库作废（红冲） | **执行作废的当天** | ❌ 拒绝 |
| 4 | 收款作废（红冲） | **执行作废的当天** | ❌ 拒绝 |

文案：
> 「会计期间 2026-09 已结账，禁止新增或修改凭证。如需调整，请联系财务主管在未结账期间处理。」

### 4.12.2 与批次 1 的差异（本批更严）

批次 1 只写「已结账拒绝」；本批**加严到 `NOT_OPENED` 也拒绝**：
实库 `biz_accounting_period.status` 有三态（F19），`NOT_OPENED` 表示"这个月还没开账"，
往里记账属于越权（财务还没确认期初）。错误文案：
> 「会计期间 2026-11 尚未开启，请先由财务开启该期间后再记账。」

### 4.12.3 日期边界

| 项 | 规则 |
|---|---|
| 出库日期可选范围 | 今天 − 365 天 ~ 今天 + 90 天（前端控件限制 + 后端校验） |
| 收款日期可选范围 | 同上 |
| 订单交期 | 允许今天之前（补录），但列表标「已逾期」黄标 |
| 目标期间不存在 | 拒绝：「目标期间 2025-06 不存在，请先初始化该年度期间」 |
| 跨期作废 | 原期间已结账时，红字记入当期，摘要注明原期间（沿用批次 1 §3.7.3） |

---

## 4.13 部分出库与订单进度

### 4.13.1 数量规则

```
每行：0 ≤ shipped_qty ≤ qty（CHECK 兜底，仿 F4 的 ck_biz_po_item_recv）
一次出库：0 < 本次数量 ≤ qty − shipped_qty（应用层校验）
累计：shipped_qty 只能由出库过账增加、出库作废减少，其他任何路径都不许改
```

### 4.13.2 单级进度

```
进度（按金额，比按数量更贴近业务）：
  progress = Σ(已出库行的 with_tax_amount) / 订单 total_with_tax
显示：进度条 + 「已发 67,800.00 / 135,600.00（50%）」
```

### 4.13.3 订单状态流转

```
DRAFT ──提交──▶ SUBMITTED ──审批通过──▶ APPROVED
                   │                       │
                   │                       ├──部分出库──▶ PARTIAL_OUT
                   │                       └──全部出库──▶ OUTBOUND
                   ├──取消──▶ CANCELLED
                   └──关闭──▶ CLOSED（APPROVED/PARTIAL_OUT 也可关闭）

状态判定（每次出库/作废后重算，取唯一来源）：
  if Σ shipped_qty == 0                    → APPROVED（若曾 SUBMITTED 则 SUBMITTED）
  elif Σ shipped_qty  < Σ qty              → PARTIAL_OUT
  else                                      → OUTBOUND
  CLOSED / CANCELLED 不参与重算（终态）
```

### 4.13.4 算例：100 件分两次发（60 + 40）

```
订单 SO-001：100 件 @1,000.00（不含税），13% → 价款 100,000.00 / 税 13,000.00 / 合计 113,000.00

出库 OUT-001：60 件
  价款 60,000.00 / 税 7,800.00 / 合计 67,800.00
  应收 AR-001 67,800.00；凭证 PZ-0011（借 1122 67,800 / 贷 6001 60,000 / 贷 2221.02 7,800）
  成本：60 × 均价；成本凭证 PZ-0012
  订单：shipped_qty=60，进度 60%，状态 PARTIAL_OUT

出库 OUT-002：40 件
  价款 40,000.00 / 税 5,200.00 / 合计 45,200.00
  应收 AR-002 45,200.00；凭证 PZ-0013 / PZ-0014
  订单：shipped_qty=100，进度 100%，状态 OUTBOUND

会计等价性校验（测试断言）：
  Σ应收 = 67,800 + 45,200 = 113,000.00 = 订单合计 ✅
  Σ收入 = 60,000 + 40,000 = 100,000.00 = 订单价款 ✅
  Σ销项税 = 7,800 + 5,200 = 13,000.00 = 订单税额 ✅
  Σ成本凭证 6401 = 一次性出库 100 件的成本（因移动加权不改均价）✅
```

### 4.13.5 边界

| 场景 | 处理 |
|---|---|
| 一次出库数量 > 未发数量 | 拒绝：「本行未发数量仅 40，本次出库 50，超出 10」 |
| 订单已 CLOSED 后还想出库 | 拒绝：「订单已关闭，不能继续出库」 |
| 部分出库后关闭订单 | 剩余未发数量作废；`shipped_qty` 保持原值；已生成的应收不受影响 |
| 出库作废后订单状态 | 重算（OUTBOUND → PARTIAL_OUT 或 APPROVED） |
| 订单行被删除（草稿） | 已出库的行不能删（有 `shipped_qty > 0` 的明细禁止编辑，见 S2-04） |
| 超发（shipped_qty > qty） | DB CHECK 直接拦（`ck_biz_so_item_shipped`），应用层先给友好文案 |

---
# 五、页面结构与交互

## 5.0 视觉与布局约定（沿用批次 1 §4.0，只补 3 条销售侧新增）

批次 1 已定的 4 条（配色莫兰迪四色 → TDesign token、状态标签色统一、列表页骨架、数值显示规范）
**本批全部沿用，不重新定**。前端页面文件落在 `frontend/src/views/business/erp/`（F32）。

本批**补 3 条**：

| # | 新增约定 | 原因 |
|---|---|---|
| V1 | **口径徽标常驻表头**：`[不含税]` / `[含税]`，可点切换 | 与批次 1 一致；销售侧更容易被客户报价带偏（客户口头报价通常是含税价） |
| V2 | **状态标签双维度**：单据状态 + 信用状态并排显示（如 `已批准` `超额待批`） | 销售最需要一眼看出来"这张单卡在哪" |
| V3 | **金额四列对齐**：数量右对齐、单价右对齐 6 位、金额右对齐 2 位、百分比右对齐 2 位 | 财务核对时按列扫，不齐会看错行 |

## 5.1 客户档案页（`/business/erp/customer`，组件 `business/erp/customer/index`）

### 5.1.1 列表

```
筛选区：编码（模糊）/ 名称（模糊）/ 状态（启用/停用）/ 信用状态（正常/接近超额/已超额/已冻结）/ 税号
列表列：编码 | 名称 | 联系人 | 电话 | 信用期(天) | 信用额度 | 已占用 | 可用额度 | 信用状态 | 状态 | 操作
行内操作：详情 | 编辑 | 调额 | 冻结/解冻 | 停用
底部：新增客户 | 导入 | 导出
排序：默认按编码升序；可按「可用额度」升序（找最紧的客户）
高亮：可用额度 < 0 的行整行浅红底；信用状态=已冻结 的行左侧 3px 红条
```

### 5.1.2 详情（三栏布局）

```
左栏（基础信息）：编码/名称/简称/税号/联系人/电话/邮箱/地址/开票抬头/开户行/账号/结算方式/状态
中栏（信用）：信用额度 / 已占用 / 可用额度 / 信用期 / 信用状态 / 应收科目 / [调整额度] [冻结]
             额度变更历史（时间、原值、新值、差额、原因、操作人）—— 取 biz_customer_credit_log
右栏（往来）：
   未结应收：笔数 + 合计金额 + 最早到期日 + 逾期金额
   最近 5 笔收款：日期、单号、金额、核销了几张
   最近 5 张出库：单号、日期、金额
   [查看全部应收] [查看全部收款] （跳列表并带上客户筛选）
```

### 5.1.3 新建/编辑弹窗

```
两列布局；必填标红星：
  编码*   名称*   简称   税号   联系人   电话   邮箱
  地址（多行）  开票抬头  开户行  账号
  结算方式*（月结/现结）  信用期*（天，默认 30）  信用额度*（默认 0）  应收科目*
  分类（字典）  等级（字典）  备注
交互：
  · 编码框右侧「建议」按钮生成 CUS-000007
  · 税号失焦 → 查重：已存在则黄条提示「税号相同的客户已存在：CUS-000003 苏州精密」，不阻止保存
  · 信用额度 > 0 但信用期为 0 → 提示「信用额度大于 0 时建议同时设置信用期，否则到期日=出库日」
  · 应收科目必选且必须是末级负债类贷方科目（1122 及其子科目），否则保存即拒（与批次 1 §3.4.3 同口径）
```

## 5.2 销售订单页（`/business/erp/sale-order`）

### 5.2.1 列表

```
筛选：单号 / 客户 / 订单日期区间 / 状态 / 口径 / 是否超额
列：单号 | 客户 | 订单日期 | 交期 | 仓库 | 价税合计 | 已发进度 | 状态 | 信用 | 操作
进度列：进度条 + 「67,800.00 / 135,600.00」
状态筛选项：草稿 / 已提交 / 已批准 / 部分出库 / 已出库 / 已关闭 / 已取消
行内操作：详情 | 编辑 | 提交 | 审批 | 关闭 | 取消 | 删除草稿 | 拉出库单
```

### 5.2.2 单据页（编辑态）

```
表头：
  [不含税▾] 客户*  订单日期*  交期  仓库*  结算方式（只读，来自客户）  信用期（只读）
  收款条件  备注
明细表（可增删行）：
  行号 | 物料*（弹窗选，显示编码+名称+规格+在库+可承诺量） | 单位 | 数量* | 单价* | 税率* | 金额 | 税额 | 价税合计 | 已发 | 未发 | 备注
  单价框下方灰字：近 90 天该客户成交价 2,000.00 / 2,050.00（P1）
合计行：数量合计 | 价款合计 | 税额合计 | 价税合计
右侧固定面板（实时）：
  客户信用：额度 100,000.00 / 占用 153,680.00 / 可用 −53,680.00
  本单金额：135,600.00
  超出提示（若超额）：红字「超额 53,680.00，提交后需审批放行」
底部按钮：保存草稿 | 提交 | 取消
```

### 5.2.3 交互细节

| 交互 | 行为 |
|---|---|
| 选客户 | 带出结算方式、信用期、税率默认；若客户已冻结 → 红条「客户已冻结，不可开单」并禁用保存 |
| 选物料 | 弹窗列表带「在库」「可承诺量」（3.12.2）；选中后带出单位、税率（**×100 换算**，F17） |
| 改数量/单价/税率 | 行内三金额即时重算（4.1.3） |
| 切换口径徽标 | 确认框 → 全部行重算（单价列在含税口径下显示含税单价） |
| 提交时超额 | 弹「申请超额放行」对话框（3.1.2）；无权限则显示「请联系财务主管放行」 |
| 已出库的行 | 数量/单价置灰不可改；显示「已发 60/100」 |
| 关闭订单 | 弹窗显示「剩余未发 40 件将作废，是否继续？」 |

## 5.3 销售出库页（`/business/erp/stock-out`）

### 5.3.1 列表

```
筛选：单号 / 客户 / 仓库 / 出库日期区间 / 状态 / 来源（订单/手工）/ 应收单号
列：单号 | 客户 | 出库日期 | 仓库 | 数量 | 价税合计 | 成本 | 状态 | 应收单 | 凭证 | 操作
行内操作：详情 | 过账 | 作废 | 打印 | 导出
黄标：负库存出库（remark 命中）的行加「负库存」标签
```

### 5.3.2 单据页（编辑态）

```
表头：[不含税▾] 客户*  出库日期*  仓库*  来源订单（只读/可空）  出库类型（销售/其他）
明细表：
  行号 | 物料* | 单位 | 数量* | 单价* | 税率* | 价款 | 税额 | 价税合计 | 成本单价（过账后显示） | 成本金额（过账后显示） | 序列号（仅一物一码）| 库位（P1）
一物一码行展开区：
  [扫码框（常驻焦点）]  已扫 5 / 需 8
  已扫清单：序号 | 序列号 | 库位 | 入库时间 | [移除]
  批量粘贴按钮
合计行：数量 | 价款 | 税额 | 价税合计 |（过账后）成本合计
底部：保存草稿 | 过账 | 作废 | 打印
```

### 5.3.3 过账确认弹窗（每次必看，内容随数据变）

```
┌ 确认过账 OUT-20261031-000001 ────────────────────────────┐
│ 客户：华东电子      仓库：WH02 分仓      出库日期：2026-10-31 │
│                                                          │
│ 本次出库：8 件                                            │
│   价款      16,000.00                                    │
│   销项税     2,080.00                                    │
│   价税合计  18,080.00                                    │
│                                                          │
│ 出库成本：16,000.00（移动加权，当前均价 2.000000）          │
│                                                          │
│ 过账后生成：                                              │
│   · 应收单 AR-20261031-000001（18,080.00，到期 2026-11-30）│
│   · 收入凭证 1 张（借应收 / 贷收入 + 销项税）              │
│   · 成本凭证 1 张（借主营业务成本 / 贷库存商品）            │
│   · 信用占用 +18,080.00（占用后 171,760.00 / 100,000.00）  │
│                                                          │
│ □ 我已确认货物已实际发出（随货联已打印并交仓管）            │
└──────────────────────────────────────────────────────────┘
      [取消]                                  [确认过账]
```

> 「我已确认」勾选框是**必勾项**（未勾则按钮禁用）。理由与批次 1 §3.3.2 同款：
> 这一步在会计上等于"控制权转移确认"，必须留下人的确认动作，审计可追溯。

## 5.4 应收与收款页

### 5.4.1 应收列表（`/business/erp/receivable`）

```
筛选：客户 / 到期日区间 / 状态 / 是否逾期 / 账龄桶 / 单号
列：应收单号 | 客户 | 来源出库单 | 应收 | 已收 | 未收 | 到期日 | 逾期天数 | 账龄桶 | 状态
统计条（筛选后实时）：合计应收 / 合计未收 / 逾期未收 / 逾期占比
行内：详情 | 查看核销记录
导出：Excel（复用底座导出）
```

### 5.4.2 应收详情

```
上：基础信息（单号、客户、来源出库单、金额三件套、账期、到期日、期间、状态、关联凭证）
中：核销记录表（收款单号 | 收款日期 | 核销金额 | 操作人）
下：关联出库单明细（物料、数量、单价、金额、成本）
```

### 5.4.3 收款单页（`/business/erp/receipt`）

```
表头：客户*  收款日期*  收款方式*  收款金额*  收款账户*（银行存款科目）  银行流水号  附件（回单图片）  备注
核销区：
  [自动 FIFO 核销] [清空]  + 添加应收
  表格：应收单号 | 到期日 | 应收金额 | 已收 | 未收 | 本次核销 | 来源（自动/手工） | [移除]
  合计行：本次核销合计 X；未核销（预收）Y；校验状态 ✅/❌
  底部提示：核销合计不得超过收款金额；未核销部分将挂「预收账款」
底部：保存草稿 | 审核
```

### 5.4.4 核销交互细节

| 交互 | 行为 |
|---|---|
| 点「自动 FIFO 核销」 | 按到期日升序填满；每行来源标「自动」；总额用尽后停止 |
| 手工改某行数字 | 该行来源改标「手工」（黄底），并重算合计与未核销 |
| 核销合计 > 收款金额 | 合计行变红，审核按钮禁用，红字「核销合计超出收款金额 100.00」 |
| 某行核销 > 该应收未收 | 输入框红框，行尾红字「超过未收 200.00」 |
| 选到其他客户的应收 | 该行不可选（列表已按客户过滤）；接口二次校验 66019 |
| 审核后 | 页面转只读，顶部显示「已审核 · 凭证 PZ-20261028-0003」 |
| 作废 | 弹窗要求填原因；提示「将反核销 N 张应收并生成红字凭证」 |

## 5.5 报表页（`/business/erp/report`，一个页面 + 五个 Tab）

```
Tab 1 销售明细表：期间 + 客户 + 物料 + 仓库 → 明细行（含成本、毛利）
Tab 2 客户销售汇总：按客户汇总金额 + 应收余额 + 逾期
Tab 3 毛利分析：按物料/客户分组，毛利率 < 0 红字
Tab 4 应收账龄表：五桶，可切按单据/按客户
Tab 5 勾稽表：四条恒等式，全部 ✅ 才绿
每个 Tab 都支持：导出 Excel、记住上次筛选、打印
```

## 5.6 错误文案总表（开发照抄，测试照此验收）

| 编号 | 错误码 | 文案 |
|---|---|---|
| M-01 | — | 「客户编码 CUS-000001 已存在，请更换」 |
| M-02 | 66002 | 「客户已停用，不能开单」 |
| M-03 | 66001 | 「该客户税号已存在：CUS-000003 苏州精密」 |
| M-04 | 66012 | 「客户 {名称} 超额 {X}（额度 {L}，已占用 {O}，本单 {M}）。请先回款、提额，或申请超额放行」 |
| M-05 | 66028 | 「客户 {名称} 已冻结，禁止开单。如需恢复请联系财务主管」 |
| M-06 | — | 「第 {n} 行数量为 0，请删除或填写数量」 |
| M-07 | — | 「本行未发数量仅 {A}，本次出库 {B}，超出 {C}」 |
| M-08 | 66010 | 「物料 {料号} 在仓库 {仓} 现存 {Q}，本次出库 {N}，超出 {D}。该仓库不允许负库存」 |
| M-09 | 66011 | 「物料 {料号} 在仓库 {仓} 无任何入库记录，负库存出库无法确定成本，请先入库或关闭负库存」 |
| M-10 | — | 「仓库 {仓} 是上级仓库，不能直接出库，请选择末级仓库」 |
| M-11 | 66020 | 「序列号 {SN} 不存在，请核对后重新扫码」 |
| M-12 | 66021 | 「序列号 {SN} 已于 {日期} 出库（单据 {单号}），不能重复出库」 |
| M-13 | 66022 | 「序列号 {SN} 当前在 {仓}，与本单仓库 {仓2} 不一致」 |
| M-14 | 66023 | 「序列号 {SN} 属于物料 {料号}，与本行物料 {料号2} 不一致」 |
| M-15 | 66024 | 「序列号 {SN} 已报废，不能出库」 |
| M-16 | 66025 | 「该序列号已在本次出库中（第 {n} 行）」 |
| M-17 | 66014 | 「物料 {料号} 是一物一码物料，已扫 {A} 件，本行数量 {B}，请扫足或调整数量」 |
| M-18 | 66015 | 「库存/序列号状态已被其他操作修改，请刷新后重试」 |
| M-19 | 66029 | 「一物一码物料的出库单不支持作废（序列号不可回库）。如需处理请联系管理员走反向单据」 |
| M-20 | 66013 | 「该出库单已生成应收单 {AR}，请勿重复过账」 |
| M-21 | 66017 | 「核销合计 {X} 超过收款金额 {Y}，请调整」 |
| M-22 | 66018 | 「应收 {AR} 未收仅 {U}，本次核销 {A} 超出」 |
| M-23 | 66019 | 「不允许跨客户核销：收款单客户 {C1}，应收 {AR} 属 {C2}」 |
| M-24 | 66030 | 「应收 {AR} 已被收款核销，不能作废出库单」 |
| M-25 | 66027 | 「会计期间 {P} 已结账/未开启，禁止记账」 |
| M-26 | 66031 | 「出库日期超出允许范围（今天 − 365 天 ~ 今天 + 90 天）」 |
| M-27 | — | 「凭证规则 {规则码} 未配置或已停用，无法生成凭证。请联系管理员」 |
| M-28 | — | 「科目 {编码} 已停用/非末级，不能记账」 |
| M-29 | 66032 | 「预收余额不足：可用 {A}，本次使用 {B}」 |
| M-30 | 66016 | 「物料 {料号} 在仓库 {仓} 的账面数量 {Q} 与在库序列号件数 {S} 不一致，请先在库存巡检页修复」 |

> 文案编写规范（沿用批次 1）：**说清"是什么 + 差多少 + 怎么办"**，禁止只写「操作失败」。
> 每条文案都必须是**可执行**的（用户看完知道下一步点哪）。

---

# 六、权限模型

## 6.1 权限码命名（与批次 1、与实库 `sys_menu.auth_code` 完全同一套）

```
格式：business:erp:{资源}:{动作}
资源：小写 + 连字符（customer / customer-credit / sale-order / stock-out / receivable / receipt / sale-report）
动作：list / detail / create / update / delete / status / audit / adjust / freeze / approve / print / export / view
唯一来源：sys_menu.auth_code（**本仓没有 sys_permission 表**，F31 已核实）
菜单节点自带 :list 码；按钮节点各占一个码
```

**命名冲突规避**（重要）：
- `receipt` **专指「收款单」**；收货入库单固定叫 `stock-in`（批次 1 已占用）。
- `stock-out` 是销售出库；调拨的出入库用 `transfer`（批次 D 已占用）。
- `customer-credit` 是信用相关的敏感动作；客户主数据用 `customer`。

## 6.2 完整权限码清单（**38 个**，可直接落 `sys_menu` 种子）

| # | 权限码 | 说明 | 默认授予 |
|---|---|---|---|
| 1 | `business:erp:customer:list` | 客户列表 | SUPER_ADMIN、销售 |
| 2 | `business:erp:customer:detail` | 客户详情 | 同上 |
| 3 | `business:erp:customer:create` | 新建客户 | 销售 |
| 4 | `business:erp:customer:update` | 编辑客户 | 销售 |
| 5 | `business:erp:customer:delete` | 删除客户（仅无往来） | SUPER_ADMIN |
| 6 | `business:erp:customer:status` | 停用/启用 | SUPER_ADMIN |
| 7 | `business:erp:customer-credit:adjust` | 调整信用额度 | SUPER_ADMIN、财务主管 |
| 8 | `business:erp:customer-credit:freeze` | 冻结/解冻 | SUPER_ADMIN、财务主管 |
| 9 | `business:erp:customer-credit:approve` | 超额放行 | SUPER_ADMIN、财务主管 |
| 10 | `business:erp:sale-order:list` | 订单列表 | 销售、仓管（只读） |
| 11 | `business:erp:sale-order:detail` | 订单详情 | 同上 |
| 12 | `business:erp:sale-order:create` | 新建订单 | 销售 |
| 13 | `business:erp:sale-order:update` | 编辑订单 | 销售 |
| 14 | `business:erp:sale-order:delete` | 删除草稿 | 销售 |
| 15 | `business:erp:sale-order:status` | 提交/关闭/取消 | 销售 |
| 16 | `business:erp:sale-order:audit` | 审批超额订单 | 财务主管 |
| 17 | `business:erp:sale-order:print` | 打印销售确认单 | 销售 |
| 18 | `business:erp:stock-out:list` | 出库列表 | 仓管、销售（只读） |
| 19 | `business:erp:stock-out:detail` | 出库详情 | 同上 |
| 20 | `business:erp:stock-out:create` | 新建出库单 | 仓管 |
| 21 | `business:erp:stock-out:update` | 编辑出库单 | 仓管 |
| 22 | `business:erp:stock-out:delete` | 删除草稿 | 仓管 |
| 23 | `business:erp:stock-out:status` | 过账/作废 | 仓管（作废建议加二级限制） |
| 24 | `business:erp:stock-out:print` | 打印出库单 | 仓管 |
| 25 | `business:erp:stock-out:export` | 导出 | 仓管、财务 |
| 26 | `business:erp:receivable:list` | 应收列表 | 财务 |
| 27 | `business:erp:receivable:detail` | 应收详情 | 财务、销售（限本人客户） |
| 28 | `business:erp:receivable:export` | 应收导出 | 财务 |
| 29 | `business:erp:receipt:list` | 收款列表 | 财务 |
| 30 | `business:erp:receipt:detail` | 收款详情 | 财务 |
| 31 | `business:erp:receipt:create` | 新建收款单 | 财务 |
| 32 | `business:erp:receipt:update` | 编辑收款单 | 财务 |
| 33 | `business:erp:receipt:delete` | 删除草稿 | 财务 |
| 34 | `business:erp:receipt:audit` | 审核收款单（核销生效 + 生成凭证） | 财务主管 |
| 35 | `business:erp:sale-report:list` | 报表页访问 | 财务、销售、老板 |
| 36 | `business:erp:sale-report:view` | 看毛利与成本 | 财务、老板（**销售默认无**） |
| 37 | `business:erp:sale-report:export` | 报表导出 | 财务、老板 |
| 38 | `business:erp:receivable:apply-prepaid` | 用预收余额核销 | 财务 |

> **为什么毛利要单独一个 `view` 码**：销售能看销售额，但不该看成本与毛利（这是很多公司的红线）。
> 本批把「销售明细表」的**成本/毛利两列**做成按 `sale-report:view` 动态隐藏：
> 无此权限时列不渲染（不是置灰），导出时也不带这两列（否则等于泄漏）。

## 6.3 数据权限（沿用批次 1 §5.3 的做法，不新造机制）

| 表 | 是否有归属维度 | 数据权限策略 |
|---|---|---|
| `biz_customer` | 无（全局主数据） | `ScopeTarget::unscoped`（与批次 1 的物料/供应商同口径） |
| `biz_sale_order` / `_item` | 有 `create_by`（业务员） | 默认 `unscoped`；参数 `erp.sale.order_scope=OWNER` 时按 `create_by = 当前用户` 过滤 |
| `biz_stock_out` / `_item` | 无（仓库维度） | `unscoped`；如需按仓隔离，用 `warehouse_id` + 仓管员岗位配置（批次 4） |
| `biz_receivable` | 无（客户维度） | `unscoped`（财务要看全部）；销售看详情时按客户归属校验（接口层） |
| `biz_receipt` / `_item` | 无 | `unscoped` |

> 本批**不引入新的数据权限机制**：批次的 B 系列（数据权限+列级脱敏）已交付，直接用。
> 若要做「销售只看自己的单」，只需把 `erp.sale.order_scope` 改成 `OWNER`，不改代码。

## 6.4 敏感操作（必须单独授权，默认谁都不给）

| 操作 | 权限码 | 为什么敏感 |
|---|---|---|
| 调整信用额度 | `customer-credit:adjust` | 直接改变风险敞口 |
| 超额放行 | `customer-credit:approve` | 等于人为破一次内控 |
| 冻结客户 | `customer-credit:freeze` | 影响客户能否下单 |
| 出库作废 | `stock-out:status`（建议独立 `stock-out:cancel`，P1） | 红冲会改账 |
| 收款作废 | `receipt:audit` + 原因必填 | 反核销会改应收状态 |
| 看毛利 | `sale-report:view` | 成本即底价 |

## 6.5 按钮级权限矩阵（开发按此挂 `v-privilege`）

| 页面 | 按钮 | 权限码 | 无权限时 |
|---|---|---|---|
| 客户列表 | 新增 | `customer:create` | 隐藏 |
| 客户详情 | 调整额度 | `customer-credit:adjust` | 隐藏 |
| 客户详情 | 冻结 | `customer-credit:freeze` | 隐藏 |
| 订单列表 | 新增/编辑/提交 | `sale-order:create/update/status` | 隐藏 |
| 订单列表 | 审批放行 | `sale-order:audit` | 隐藏（并提示联系财务主管） |
| 出库列表 | 过账/作废 | `stock-out:status` | 隐藏 |
| 出库列表 | 打印 | `stock-out:print` | 隐藏 |
| 应收列表 | 导出 | `receivable:export` | 隐藏 |
| 收款列表 | 审核 | `receipt:audit` | 隐藏 |
| 报表 | 成本/毛利列 | `sale-report:view` | 列不渲染 |

## 6.6 角色初始授权模板（部署时灌入，可再调整）

| 角色 | 授予的权限组 |
|---|---|
| `SUPER_ADMIN`（实库已有） | 全部 38 个 |
| 财务主管（**需新建**，`FINANCE_MGR`） | customer 全部 + credit 全部 + receivable/receipt 全部 + sale-report 全部 + sale-order:audit |
| 会计（**需新建**，`ACCOUNTANT`） | customer:list/detail + receivable:* + receipt:list/detail/create/update + sale-report:list/view/export |
| 销售（**需新建**，`SALES`） | customer:*（除 delete/status）+ sale-order:list/detail/create/update/status/print + stock-out:list/detail + sale-report:list |
| 仓管（**需新建**，`WAREHOUSE`） | stock-out:* + sale-order:list/detail + 只读物料/库存（批次 1 已有码） |

> ⚠️ 实库现在只有 5 个角色（F23：`SUPER_ADMIN`/`MUTEX_A`/`MUTEX_B`/`QA_LIMITED`/`QA_VIEWER`），
> 上面 4 个业务角色**都不存在**。上线前必须二选一：
> ① 由运维在 `sys_role` 里建这 4 个角色（推荐，写进上线清单）；
> ② 或只给 `SUPER_ADMIN` 全权，业务角色后续再建（此时其他人全部 403）。
> **迁移里的授权段对不存在的角色是空操作，不会报错**，所以必须有人真的检查一遍（6.7 第 7 条）。

## 6.7 上线前权限自检清单（9 条，逐条验）

```
1. select count(*) from sys_menu where auth_code like 'business:erp:%' → 应为 61 + 38 = 99
2. select count(*) from sys_menu where right(id::text,4) between '1011' and '1048' → 应为 38（6 菜单 + 32 按钮）
   另：select count(*) from sys_menu where id::text = '20000000-0000-0000-0000-000000000e04' → 应为 1（销售管理目录）
3. 用 SUPER_ADMIN 登录，能看到「销售管理」目录下 6 个菜单，无 404
4. 用无权限账号登录，直接访问 /business/erp/sale-order → 403（不是白屏）
5. 用 SUPER_ADMIN 调 GET /business/erp/receivable/page → 200
6. 用无 receivable:list 的账号调同上接口 → 403
7. 检查角色：select role_code from sys_role → 确认业务角色是否已建；未建则记录「仅超管可用」
8. 报表页用无 sale-report:view 的账号打开 → 成本/毛利列不出现
9. sys_role_menu 里每个新菜单至少有一条授权（超管除外则记录原因）
```

---

# 七、菜单结构

## 7.1 决策：新增 1 个**根级**目录「销售管理」，与现有 4 个 ERP 目录平级

```
现状（实库，F31 —— 今天刚改过，必须按最新）：
  根级（parent_id IS NULL）：
    0946  采购管理   (menu_type=1, sort=62, erp-purchase)   → 采购订单/收货入库/应付账款/供应商/采购明细
    0e01  仓库管理   (1, sort=66, erp-warehouse)
    0e02  财务管理   (1, sort=72, erp-finance)
    0e03  基础数据   (1, sort=76, erp-master-data)
  ⚠️ 原来的顶级目录「0940 ERP」已在 commit 8fa2d1e 中删除，4 个业务目录被提到根级。

本批新增（也挂在根级，sort 插在采购 62 与仓库 66 之间取 64）：
  20000000-…-000e04  销售管理   (1, sort=64, icon=shopping, parent_id=NULL)
    ├─ 1011  客户档案   customer:list
    ├─ 1012  销售订单   sale-order:list
    ├─ 1013  销售出库   stock-out:list
    ├─ 1014  应收管理   receivable:list
    ├─ 1015  收款管理   receipt:list
    └─ 1016  销售报表   sale-report:list
```

**为什么必须是根级**：`sys_menu` 里已经**没有** `0940` 这一行（F31），把它写成父级会得到一个
`parent_id` 指向不存在行的孤儿菜单 —— 前端菜单树渲染时会直接丢掉这一支（不报错，更难查）。

**为什么应收/收款放「销售管理」而不是「财务管理」**：
批次 1 把「应付账款」放在「采购管理」下（id 1009，parent 0946），本批对称 ——
往来账跟着业务线走，财务在「财务管理」下看凭证与总账。这样销售的日常路径是「一条线走完」，
不用在多个一级目录间跳。财务的日常路径是「凭证/总账 + 应收/收款」，也能直达（菜单可收藏）。

## 7.2 完整菜单树（本批新增 7 行：1 目录 + 6 菜单）

| id 后缀 | 标题 | menu_type | auth_code | path | 父级 | component |
|---|---|---|---|---|---|---|
| `0e04` | 销售管理 | 1（目录，**根级**） | `erp-sale` | `/business/erp/sale-mgmt` | `NULL` | — |
| `1011` | 客户档案 | 2 | `business:erp:customer:list` | `/business/erp/customer` | `0e04` | `business/erp/customer/index` |
| `1012` | 销售订单 | 2 | `business:erp:sale-order:list` | `/business/erp/sale-order` | `0e04` | `business/erp/saleorder/index` |
| `1013` | 销售出库 | 2 | `business:erp:stock-out:list` | `/business/erp/stock-out` | `0e04` | `business/erp/stockout/index` |
| `1014` | 应收管理 | 2 | `business:erp:receivable:list` | `/business/erp/receivable` | `0e04` | `business/erp/receivable/index` |
| `1015` | 收款管理 | 2 | `business:erp:receipt:list` | `/business/erp/receipt` | `0e04` | `business/erp/receipt/index` |
| `1016` | 销售报表 | 2 | `business:erp:sale-report:list` | `/business/erp/report` | `0e04` | `business/erp/report/index` |

按钮节点（`menu_type=3`，`is_visible=FALSE`）共 **32 个**，id 从 `1017` 起连续分配到 `1048`
（38 个权限码 = 6 个菜单节点上的 `:list` 码 + 32 个按钮码，清单见 6.2）。

## 7.3 种子数据写法（可直接作为 `V0165__erp_sale_permissions.up.sql` 追加）

```sql
-- ① 目录节点
INSERT INTO sys_menu (id, name, menu_type, auth_code, path, parent_id, component,
                      sort_order, status, icon, title, is_visible, module_code)
VALUES ('20000000-0000-0000-0000-000000000e04'::uuid, 'erp-sale', 1, 'erp-sale',
        '/business/erp/sale-mgmt', NULL,        -- 根级目录（F31：0940 已被 8fa2d1e 删除，不能挂上去）
        NULL, 64, 1, 'shopping', '销售管理', TRUE, 'business')
ON CONFLICT (id) DO NOTHING;

-- ② 菜单节点（auth_code = :list，name 也写 auth_code —— 与 V0151 同口径）
INSERT INTO sys_menu (id, name, menu_type, auth_code, path, parent_id, component,
                      sort_order, status, icon, title, is_visible, module_code)
VALUES
 ('20000000-0000-0000-0000-000000001011'::uuid, 'business:erp:customer:list', 2,
  'business:erp:customer:list', '/business/erp/customer',
  '20000000-0000-0000-0000-000000000e04'::uuid, 'business/erp/customer/index',
  10, 1, 'user', '客户档案', TRUE, 'business'),
 ('20000000-0000-0000-0000-000000001012'::uuid, 'business:erp:sale-order:list', 2,
  'business:erp:sale-order:list', '/business/erp/sale-order',
  '20000000-0000-0000-0000-000000000e04'::uuid, 'business/erp/saleorder/index',
  20, 1, 'file-add', '销售订单', TRUE, 'business'),
 ('20000000-0000-0000-0000-000000001013'::uuid, 'business:erp:stock-out:list', 2,
  'business:erp:stock-out:list', '/business/erp/stock-out',
  '20000000-0000-0000-0000-000000000e04'::uuid, 'business/erp/stockout/index',
  30, 1, 'upload', '销售出库', TRUE, 'business'),
 ('20000000-0000-0000-0000-000000001014'::uuid, 'business:erp:receivable:list', 2,
  'business:erp:receivable:list', '/business/erp/receivable',
  '20000000-0000-0000-0000-000000000e04'::uuid, 'business/erp/receivable/index',
  40, 1, 'receipt', '应收管理', TRUE, 'business'),
 ('20000000-0000-0000-0000-000000001015'::uuid, 'business:erp:receipt:list', 2,
  'business:erp:receipt:list', '/business/erp/receipt',
  '20000000-0000-0000-0000-000000000e04'::uuid, 'business/erp/receipt/index',
  50, 1, 'wallet', '收款管理', TRUE, 'business'),
 ('20000000-0000-0000-0000-000000001016'::uuid, 'business:erp:sale-report:list', 2,
  'business:erp:sale-report:list', '/business/erp/report',
  '20000000-0000-0000-0000-000000000e04'::uuid, 'business/erp/report/index',
  60, 1, 'chart', '销售报表', TRUE, 'business')
ON CONFLICT (id) DO NOTHING;

-- ③ 授权（对不存在的角色是空操作，不报错；真实角色以 F23 为准）
DO $$
DECLARE n_menus integer; n_grants integer;
BEGIN
    SELECT count(*) INTO n_menus FROM sys_menu
     WHERE right(id::text,4) BETWEEN '1011' AND '1048'
       AND auth_code LIKE 'business:erp:%';
    IF n_menus <> 38 THEN
        RAISE EXCEPTION 'V0165 自检失败：预期 38 个销售域权限节点，实际 %', n_menus;
    END IF;
    INSERT INTO sys_role_menu (id, role_id, menu_id)
    SELECT gen_random_uuid(), r.id, m.id
      FROM sys_role r
      CROSS JOIN (SELECT id FROM sys_menu
                   WHERE right(id::text,4) BETWEEN '1011' AND '1048'
                     AND auth_code LIKE 'business:erp:%') m
     WHERE r.deleted_at IS NULL
       AND r.role_code IN ('SUPER_ADMIN','FINANCE_MGR','ACCOUNTANT','SALES','WAREHOUSE')
       AND NOT EXISTS (SELECT 1 FROM sys_role_menu rm WHERE rm.role_id=r.id AND rm.menu_id=m.id);
    GET DIAGNOSTICS n_grants = ROW_COUNT;
    RAISE NOTICE 'V0165：销售域 % 个权限节点已种，授权新增 % 行', n_menus, n_grants;
END $$;
```

> 注意：`sys_menu.name` 对按钮节点写的是 **auth_code 本身**（实码 V0151 的做法，F31 已核实），
> 对目录/菜单写的是**中文名**。`title` 才是给人看的中文。写反了前端会显示权限码。

## 7.4 与既有菜单共存的 3 个注意点

| # | 注意点 | 处理 |
|---|---|---|
| 1 | 菜单在最近两个提交里刚改过两次：`V0154`（按业务线重组成 4 个分组）+ `8fa2d1e`（**去掉 ERP 顶级目录，4 个分组提到根级**） | 本批新增的「销售管理」必须**根级 + 沿用同一套命名规范**（`erp-*` 的 auth_code 前缀、目录 `is_visible=TRUE`、`module_code='business'`），且**开工前重新 `select` 一次 `sys_menu` 确认结构没再变** |
| 2 | 菜单 id 段 `1000–1010` 已用（含 1010 = 应付详情）；其中 **1006 是空号**（历史留白，未见对应菜单行） | 本批从 **1011** 起；**1006 也一并不复用**（空号可能被其他分支的迁移预定，复用会撞 `ON CONFLICT` 静默跳过，反而更难查） |
| 3 | 前端路由是**静态注册**（`component` 字段指定路径），菜单只控制可见性 | 新增 6 个页面必须先有 `.vue` 文件，否则点菜单白屏。开发顺序必须是「先页面后菜单」（9.4） |

---
# 八、验收场景（6 个端到端，可直接转测试用例）

## 8.0 公共前置数据（6 个场景共用，全部来自实库 F20–F22，不用另造）

```
仓库：WH02 分仓（level=2, is_leaf=1, allow_negative=0）
物料：
  M-W1A-004 成品D   FINISHED, serial_required=t, tax_rate=0.1300（注意：比例口径）
  M-W1A-005 成品E   FINISHED, serial_required=f, tax_rate=0.1300
  M-W1A-001 原料A   RAW,      serial_required=f, tax_rate=0.1300
单位：PCS
客户：CUS-T01 华东电子（credit_limit=100,000.00, credit_days=30, 结算方式=月结, 应收科目=1122）
      CUS-T02 江南贸易（credit_limit=50,000.00,  credit_days=15, 结算方式=月结, 应收科目=1122）
      CUS-T03 现结客户甲（credit_limit=0, credit_days=0, 结算方式=现结）
科目：1122 应收账款 / 6001 主营业务收入 / 2221.02 应交税费-销项税 / 6401 主营业务成本 / 2203 预收账款 / 1002 银行存款
凭证规则：SALE_OUT_DEFAULT（借 1122 贷 6001 税 2221.02 tax_side=CREDIT）
          SALE_OUT_COST（借 6401 贷 1405）
          RECEIPT_DEFAULT（借 1002 贷 1122 + 预收 2203）
期间：2026-10 status=OPEN；2026-11 status=OPEN；2026-09 status=CLOSED（用于负例）
```

---

## 场景 1【完整链路】客户建档 → 订单 → 出库 → 应收 → 收款 → 两张凭证 → 总账

| 步 | 操作 | 期望结果（可断言） |
|---|---|---|
| 1 | 新建客户 `CUS-T01 华东电子`，信用额度 100,000.00，信用期 30，应收科目 1122 | 保存成功；`biz_customer` 新增 1 行；`biz_customer_credit_log` 新增 1 行（0 → 100,000.00） |
| 2 | 新建订单：客户 CUS-T01，仓库 WH02，2 行：<br>行1 M-W1A-004 60 台 @2,000.00 13%<br>行2 M-W1A-005 20 台 @100.00 13% | 单号 `SO-<今天>-000001`；行1 价款 120,000.00/税 15,600.00/合计 135,600.00；行2 价款 2,000.00/税 260.00/合计 2,260.00；单合计 122,000.00/15,860.00/**137,860.00** |
| 3 | 提交订单 | 超额：137,860.00 > 100,000.00 → `SUBMITTED`，`credit_flag='OVER'`，有「超额待批」标签 |
| 4 | 财务主管放行 | 生成例外（放行 37,860.00，有效期 30 天）；订单 → `APPROVED` |
| 5 | 先入库足够库存（本场景前置：M-W1A-004 入库 60 件 @2.00；M-W1A-005 入库 20 件 @50.00） | 库存 `WH02/成品D` = 60 件 / 120.00 / avg_cost 2.000000；`WH02/成品E` = 20 件 / 1,000.00 / 50.000000 |
| 6 | 从订单拉出库单（全部 60+20） | 出库单草稿；行1 需扫 60 个序列号（一物一码），行2 只需数量 |
| 7 | 扫 60 个序列号 → 过账 | `OUT-<今天>-000001` POSTED；<br>库存：`成品D` qty 60→0、amount 120.00→**0.00**（清仓倒挤）；`成品E` qty 20→0、amount 1,000.00→0.00<br>序列号 60 个 → `OUTBOUND`<br>`biz_inventory_txn` 新增 61 条 OUT（60 件各 1 条 + 行2 1 条） |
| 8 | 检查应收 | `AR-<今天>-000001`：amount **137,860.00**，received 0，unpaid 137,860.00，due_date = 今天+30，status OPEN |
| 9 | 检查收入凭证 | `PZ-…-0001`（**4 行**，`source_type='SALE_OUT'`）：<br>行1 借 1122 **137,860.00**（aux_customer_id=CUS-T01）<br>行2 贷 6001 **120,000.00**（行1 收入，aux_material_id=M-W1A-004）<br>行3 贷 6001 **2,000.00**（行2 收入，aux_material_id=M-W1A-005）<br>行4 贷 2221.02 **15,860.00**（销项税 13%，无 aux）<br>合计 137,860.00 = 137,860.00 ✅（收入行**按明细行不合并**，见 4.1.5 注） |
| 10 | 检查成本凭证 | `PZ-…-0002`（2 行，`source_type='SALE_OUT_COST'`）：<br>借 6401 **1,120.00**（60×2.00 + 20×50.00）<br>贷 1405 1,120.00（aux_warehouse_id=WH02）✅ |
| 11 | 检查订单 | `shipped_qty`：行1=60、行2=20；状态 `OUTBOUND`；进度 100% |
| 12 | 新建收款单：客户 CUS-T01，金额 137,860.00，FIFO 核销 | 核销 1 行（AR-001 全额）；`AR-001` → `SETTLED`，settled_at 有值；`receipt.allocated=137,860.00`，`unallocated=0.00` |
| 13 | 审核收款单 | `RC-<今天>-000001` POSTED；凭证 `PZ-…-0003`：借 1002 **137,860.00** / 贷 1122 **137,860.00**（aux_customer_id）✅ |
| 14 | 查总账 / 科目余额表 | 1122 本期贷 137,860.00 / 借 137,860.00 → 期末 **0.00**；6001 贷 122,000.00；2221.02 贷 15,860.00；6401 借 1,120.00；1405 贷 1,120.00（与本场景入库借方相抵后等于结存变化）；试算平衡 Σ借 = Σ贷 ✅ |
| 15 | 查信用占用 | occupied = 0（应收已结清，订单已出完）；available = 100,000.00 |
| 16 | 查毛利报表 | 收入 122,000.00 − 成本 1,120.00 = 毛利 **120,880.00**，毛利率 **99.08%** |

**边界断言（必须一并验）**

```
· 行1 的 60 个序列号状态全部 OUTBOUND，且 source_type='SALE_OUT'、source_id=出库单id
· serial_required 物料：biz_inventory.qty == COUNT(series IN_STOCK) = 0 ✅
· 两张凭证的 period 都是当月，voucher_type='AUTO'，status='POSTED'
· 收入凭证 entry_count=4（1 应收 + 2 收入行 + 1 税额行），成本凭证 entry_count=2（ck_biz_voucher_entries ≥ 2）
· 若删掉明细行2，则收入凭证 3 行、金额变 135,600.00（多税率拆行只在税率不同时生效）
· 税率换算：行税额 15,600.00 = 120,000.00 × 13%（不是 × 0.13%）—— 专治 F17 的单位陷阱
```

---

## 场景 2【信用额度】软拦 → 硬拦 → 调额放行

| 步 | 操作 | 期望 |
|---|---|---|
| 1 | 客户 CUS-T02 江南贸易：额度 50,000.00，信用期 15 | — |
| 2 | 出库 1 单 40,000.00（先入库再出库） | 应收 AR-A 40,000.00；occupied = 40,000.00；available = 10,000.00 |
| 3 | 新订单 30,000.00 提交 | **软拦**：occupied = 40,000 + 30,000 = 70,000 > 50,000 → `SUBMITTED` + `OVER`，不拒绝 |
| 4 | 不放行，直接从该订单拉出库单并过账 30,000.00 | **硬拦**：返回 66012，文案 M-04；出库单仍 DRAFT；无应收/无凭证/无流水（4.10.3 的回滚断言） |
| 5 | 财务主管把额度调到 80,000.00 | 调额成功；available = 80,000 − 70,000 = 10,000；写 `credit_log`（50,000 → 80,000） |
| 6 | 重新过账同一张出库单 | **成功**；应收 AR-B 30,000.00；occupied = 70,000；available = 10,000 |
| 7 | 冻结客户 CUS-T02 | 冻结成功 |
| 8 | 再开一张新订单 | **直接拒绝**（不是软拦），66028 / 文案 M-05 |
| 9 | 对已存在的订单拉出库单 | 也拒绝（冻结面向所有出库动作） |
| 10 | 解冻 | 恢复正常 |

**负例断言**

```
· 步骤 4 失败后：select count(*) from biz_inventory_txn where doc_id=<出库单> = 0
· 步骤 4 失败后：select count(*) from biz_receivable where source_id=<出库单> = 0
· 并发：两个线程同时对同一客户提交 30,000 订单（额度 50,000，占用 0）
  → 只有一个成功，另一个软拦（4.7.6 的锁验证）
```

---

## 场景 3【部分出库】100 件分两次发（60 + 40），会计等价性

| 步 | 操作 | 期望 |
|---|---|---|
| 1 | 订单 SO-X：M-W1A-005 100 件 @1,000.00，13% → 113,000.00 | — |
| 2 | 入库 100 件 @600.00（不含税）→ 结存 100 / 60,000.00 / avg_cost 600.000000 | — |
| 3 | 出库 60 件 | 应收 AR-1 67,800.00；收入 60,000.00 / 税 7,800.00；成本 36,000.00；订单 PARTIAL_OUT，进度 60% |
| 4 | 出库 40 件 | 应收 AR-2 45,200.00；收入 40,000.00 / 税 5,200.00；成本 24,000.00；订单 OUTBOUND，进度 100% |
| 5 | 核算等价性 | Σ应收 113,000.00 = 订单合计 ✅；Σ收入 100,000.00 ✅；Σ税 13,000.00 ✅；Σ成本 60,000.00 = 一次性出库成本 ✅ |
| 6 | 库存 | qty 0 / amount 0.00（清仓倒挤） |
| 7 | 作废第 2 张出库单 | 允许（非一物一码）；AR-2 → CANCELLED；红字 2 张；库存回补 40 件 / 24,000.00 / avg_cost 600.000000；订单回退 PARTIAL_OUT，进度 60% |
| 8 | 再作废第 1 张 | 允许；库存回补到 100 件 / 60,000.00 |

**边界**：步骤 3 之后出库 41 件 → 拒绝 M-07（未发仅 40）。

---

## 场景 4【成本结转】尾差、清仓倒挤、负库存

| 步 | 操作 | 期望 |
|---|---|---|
| 1 | M-W1A-001 入库 10 件 @1.11 | 结存 10 / 11.10 / 1.110000 |
| 2 | 再入 3 件 @3.33 | 结存 13 / 21.09 / **1.622308** |
| 3 | 出库 4 件 | 成本 6.489232 → 明细 `cost_amount=6.4892`；结存 9 / 14.6008 / 1.622308 |
| 4 | 出库 5 件 | 成本 8.111540 → `cost_amount=8.1115`；结存 4 / 6.4893 / 1.622308 |
| 5 | 出库 4 件（清仓） | 成本 **6.4893**（= 结存金额，倒挤）；结存 0 / **0.00** |
| 6 | 成本凭证核对 | 6401 借方合计 = round(6.49 + 8.11 + 6.49, 2) = **21.09**；1405 贷方 21.09；勾稽表「成本尾差」= 21.09 − 21.0900 = 0.0000 |
| 7 | 切 WH02 `allow_negative=0`，出库 1 件（库存 0） | 拒绝 66010 / M-08 |
| 8 | 切 WH02 `allow_negative=1`，出库 5 件 | 成功：qty = −5，成本 = 5 × 1.622308 = 8.111540 → 8.1115，amount = 0 − 8.1115 = **−8.1115**；流水 remark 含「负库存出库」 |
| 9 | 再出库（仓库从无入库记录的新仓） | 拒绝 66011 / M-09（找不到最近入库单价） |
| 10 | 入库 8 件 @3.33（负库存后） | qty=3；amount = −8.1115 + 26.64 = 18.5285；avg_cost = 18.5285/3 = **6.176167**（被污染，接受，记日志） |

**专治的坑**：步骤 3–5 若把中间值先舍到 2 位，结存会以每笔 1 分漂移，第 5 步清仓后 `amount ≠ 0`。

---

## 场景 5【收款核销】一张收款单核销三张应收（FIFO）+ 部分收款 + 预收

| 步 | 操作 | 期望 |
|---|---|---|
| 1 | 造 4 张未结应收（客户 CUS-T01）：AR-1 1,130.00 (到期 11-14)、AR-2 565.00 (11-19)、AR-3 226.00 (11-30)、AR-4 5,650.00 (12-05) | unpaid 全等于 amount |
| 2 | 新建收款单 5,000.00，点「自动 FIFO 核销」 | 自动填：AR-1 1,130.00、AR-2 565.00、AR-3 226.00、AR-4 3,079.00；合计 5,000.00 ✅ |
| 3 | 审核 | AR-1/2/3 → SETTLED；AR-4 → PARTIAL_PAID（unpaid 2,571.00）；凭证 3 行：借 1002 5,000.00 / 贷 1122 5,000.00（无预收行，因差额 0） |
| 4 | 再收 6,000.00，手工只核销 AR-4 2,571.00 | 允许（手工优先）；AR-4 → SETTLED；unallocated = 6,000 − 2,571 = **3,429.00** |
| 5 | 审核第二张收款单 | 凭证：借 1002 6,000.00 / 贷 1122 2,571.00 / 贷 2203 **3,429.00**（aux_customer_id）✅ |
| 6 | 新出库产生 AR-5 1,200.00 | — |
| 7 | 用预收核销 AR-5 中的 800.00 | AR-5：received 800 / unpaid 400 / PARTIAL_PAID；预收余额 3,429 − 800 = 2,629.00；凭证 RECEIPT_APPLY：借 2203 800 / 贷 1122 800 |

**负例断言**

```
· 核销合计 6,100 > 收款 6,000 → 拒绝 66017（M-21）
· AR-4 未收 2,571 时核销 2,600 → 拒绝 66018（M-22）
· 用 CUS-T02 的收款单去核 CUS-T01 的应收 → 拒绝 66019（M-23）
· 已 SETTLED 的 AR-1 再出现在可选列表 → 不允许；直接调接口也拒绝
· 场景结束后勾稽：Σ receivable.amount = Σ 出库价税合计；Σ received = Σ 收款 allocated（S5-05 两行 ✅）
```

---

## 场景 6【序列号出库】一物一码扫码 + 不变式

| 步 | 操作 | 期望 |
|---|---|---|
| 1 | M-W1A-004 在 WH02 在库 8 件（实库有此基线，F22） | `select count(*) ... status='IN_STOCK' = 8`；`biz_inventory.qty = 8` |
| 2 | 出库单 3 件，扫 3 个在库号 | 已扫 3/3；过账成功 |
| 3 | 库存与序列号 | qty 8→5；amount 16.00 → 10.00（5 × 2.00，派生重算）；3 个号 → OUTBOUND |
| 4 | 流水 | 3 条 OUT（qty=1，unit_cost=2.000000，amount=2.0000，doc_type='STOCK_OUT'，doc_no=出库单号） |
| 5 | 同一号再扫一次（新单） | 拒绝 66021 / M-12 |
| 6 | 扫一个 WH01 的号 | 拒绝 66022 / M-13 |
| 7 | 扫一个 M-W1A-005 的号 | 拒绝 66023 / M-14 |
| 8 | 扫一个 SCRAPPED 的号 | 拒绝 66024 / M-15 |
| 9 | 扫不存在的号 | 拒绝 66020 / M-11 |
| 10 | 本单扫同一个号两次 | 拒绝 66025 / M-16 |
| 11 | 数量填 5 但只扫 3 个 | 拒绝过账 66014 / M-17 |
| 12 | 作废这张出库单 | **拒绝** 66029 / M-19（一物一码不可作废） |
| 13 | 不变式 | `qty == COUNT(IN_STOCK) = 5` ✅；`amount = 5 × 2.000000 = 10.00` ✅ |
| 14 | 出库 5 件（清仓）后再看 | qty=0、amount=**0.00**（派生天然满足 qty=0→amount=0） |

---

## 8.7 场景覆盖矩阵（给测试排期用）

| 规则 \ 场景 | 1 | 2 | 3 | 4 | 5 | 6 | 单测 |
|---|---|---|---|---|---|---|---|
| 4.1 金额口径（含税/不含税） | ✅ | | ✅ | | | | ✅ |
| 4.2 成本结转（含清仓/负库存） | ✅ | | ✅ | ✅✅ | | ✅ | ✅ |
| 4.3 应收生成 | ✅ | ✅ | ✅ | | ✅ | | ✅ |
| 4.4 收入凭证（含多税率拆行） | ✅ | | ✅ | | | | ✅ |
| 4.5 成本凭证与流水对齐 | ✅ | | ✅ | ✅ | | ✅ | ✅ |
| 4.6 核销（FIFO/手工/预收） | ✅ | | | | ✅✅ | | ✅ |
| 4.7 信用额度（软拦/硬拦/调额/冻结/并发） | ✅ | ✅✅ | | | | | ✅ |
| 4.8 序列号出库（6 类错误） | ✅ | | | | | ✅✅ | ✅ |
| 4.9 取号 | ✅ | | ✅ | | ✅ | ✅ | ✅ |
| 4.10 过账事务与回滚 | ✅ | ✅ | ✅ | | | ✅ | ✅ |
| 4.11 作废与红冲 | | | ✅ | | ✅ | ✅ | ✅ |
| 4.12 期间控制 | | | | | | | ✅（负例：2026-09 CLOSED） |
| 4.13 部分出库与状态机 | | | ✅✅ | | | | ✅ |

**测试用例总数估算**：场景类 6 组 ≈ 90 条；边界/负例 ≈ 120 条；后端单测 ≈ 160 条。
（批次 1 交付 1278 条后端测试，本批按其模块密度估算 150–200 条。）

---
# 九、与批次 1 的衔接（实施规划）

## 9.1 迁移规划（V0160–V0166，七条）

**为什么从 V0160 起**：实库 `schema_migrations` 已到 **155**（V0154 `erp_menu_reorg_business_lines`、
V0155 `erp_menu_title_icon_fix` 均已应用，F21）；V0152/V0153 是空号，但 P3-D2 技术架构卡
已把 **V0152–V0159 预留**给批次 1 的补丁，为免两份文档撞号，本批统一从 **V0160** 起。

| 迁移号 | 内容 | 表 / 对象 | 依赖 |
|---|---|---|---|
| **V0160** | 客户与信用 | `biz_customer`、`biz_customer_credit_log`、`biz_customer_credit_exception` | 无（全新主数据） |
| **V0161** | 销售订单 | `biz_sale_order`、`biz_sale_order_item` | V0160（FK customer） |
| **V0162** | 销售出库 | `biz_stock_out`、`biz_stock_out_item`、`biz_stock_out_item_serial` | V0161、批次 1 的 `biz_inventory`/`biz_material_serial` |
| **V0163** | 应收与收款 | `biz_receivable`、`biz_receipt`、`biz_receipt_item` | V0162 |
| **V0164** | 科目 + 凭证规则扩展 | `biz_account` 新增 6 行；`biz_voucher_rule` 加 `tax_side`/`rule_nature` + 3 条销售侧规则；`biz_voucher_entry` 加 `aux_customer_id`；`ALTER` 不改存量数据 | V0163 |
| **V0165** | 权限与菜单 | `sys_menu` +7 行（1 目录 + 6 菜单）+31 按钮行；`sys_role_menu` 授权 | V0164 |
| **V0166** | 取号种子 | `biz_seq` 新增 4 行（`SO`/`OUT`/`AR`/`RC`） | 无（可与 V0160 同时） |

### 9.1.1 V0164 的科目种子（严格顺延实库 UUID 段，F11）

| 编码 | 名称 | 类别 | 方向 | 级次 | 末级 | UUID 后缀 | 用途 |
|---|---|---|---|---|---|---|---|
| `1122` | 应收账款 | ASSET | DEBIT | 1 | 是 | `…0005` | 应收单、收款核销 |
| `1122.01` | 应收账款-货款 | ASSET | DEBIT | 2 | 是 | `…0006` | 客户级明细（可选，本批默认挂 1122） |
| `6001` | 主营业务收入 | PROFIT_LOSS | CREDIT | 1 | 是 | `…0007` | 销售收入 |
| `2221.02` | 应交税费-销项税 | LIABILITY | CREDIT | 2 | 是 | `…0008` | 销项税 |
| `6401` | 主营业务成本 | PROFIT_LOSS | DEBIT | 1 | 是 | `…0009` | 成本结转 |
| `2203` | 预收账款 | LIABILITY | CREDIT | 1 | 是 | `…0010` | 预收/未核销收款 |
| `1002` | 银行存款 | ASSET | DEBIT | 1 | 是 | `…0011` | 收款 |

> `2221.02` 的父级必须是 `2221`（`level=1`、`is_leaf=0`，实库已存在），插入后要把 `2221.is_leaf` 保持 0
> （实库本就是 0）。**不许建三级科目**（偏差 X2）。

### 9.1.2 迁移的三条硬规矩（沿用 V0151 做法）

```
① 固定 UUID + ON CONFLICT (id) DO NOTHING（重复执行不覆盖运维改过的标题/名称）
② 自检块：种完立刻 count 校验，数目不符就 RAISE EXCEPTION（迁移失败比脏数据好）
③ 角色授权段对不存在的角色是空操作 → 打 NOTICE，不报错（空库首启也能跑）
```

## 9.2 复用（不改）与新增（本批写）

| 类别 | 对象 | 本批动作 |
|---|---|---|
| **复用不改** | `biz_inventory`、`biz_inventory_txn` | 只写数据（`txn_type='OUT'`、`biz_type='SALE_OUT'`），**不改结构** |
| **复用不改** | `biz_voucher`、`biz_voucher_entry`（除加一列） | 复用凭证引擎；`aux_customer_id` 是唯一结构变更（可空，秒级） |
| **复用不改** | `biz_accounting_period` | 只读校验 |
| **复用不改** | `biz_material_serial` | 只改 `status`/`outbound_at`/`source_*`，**不改结构** |
| **复用不改** | `biz_material`（`serial_required`/`tax_rate`/`costing_method`）、`biz_warehouse`（`allow_negative`/`is_leaf`）、`biz_unit` | 只读 |
| **复用不改** | `biz_seq`、`biz_account`、`biz_voucher_rule` | 加数据行（`biz_voucher_rule` 另加 2 列） |
| **扩展** | `biz_voucher_rule` | + `tax_side`、`rule_nature`（决策 ⑦） |
| **扩展** | `biz_voucher_entry` | + `aux_customer_id`（F9） |
| **新增** | `biz_customer`、`biz_customer_credit_log`、`biz_customer_credit_exception` | V0160 |
| **新增** | `biz_sale_order`、`biz_sale_order_item` | V0161 |
| **新增** | `biz_stock_out`、`biz_stock_out_item`、`biz_stock_out_item_serial` | V0162 |
| **新增** | `biz_receivable`、`biz_receipt`、`biz_receipt_item` | V0163 |

**新表共 11 张**（字段口径见附录 A），本批迁移后 `biz_` 表总数 42 → **53 张**。

## 9.3 接口清单（路径 + 权限码，给 backend）

| # | 方法 | 路径 | 说明 | 权限码 |
|---|---|---|---|---|
| 1 | GET | `/business/erp/customer/page` | 客户分页 | `customer:list` |
| 2 | GET | `/business/erp/customer/{id}` | 客户详情（含额度与往来汇总） | `customer:detail` |
| 3 | POST | `/business/erp/customer` | 新建客户 | `customer:create` |
| 4 | PUT | `/business/erp/customer/{id}` | 编辑客户 | `customer:update` |
| 5 | DELETE | `/business/erp/customer/{id}` | 删除（仅无往来） | `customer:delete` |
| 6 | PATCH | `/business/erp/customer/{id}/status` | 停用/启用 | `customer:status` |
| 7 | POST | `/business/erp/customer/{id}/credit/adjust` | 调额 | `customer-credit:adjust` |
| 8 | POST | `/business/erp/customer/{id}/credit/freeze` | 冻结/解冻 | `customer-credit:freeze` |
| 9 | GET | `/business/erp/customer/{id}/credit/logs` | 额度变更历史 | `customer:detail` |
| 10 | GET | `/business/erp/sale-order/page` | 订单分页 | `sale-order:list` |
| 11 | GET | `/business/erp/sale-order/{id}` | 订单详情（含明细与进度） | `sale-order:detail` |
| 12 | POST | `/business/erp/sale-order` | 新建订单（草稿，保存即取号） | `sale-order:create` |
| 13 | PUT | `/business/erp/sale-order/{id}` | 编辑订单 | `sale-order:update` |
| 14 | DELETE | `/business/erp/sale-order/{id}` | 删除草稿 | `sale-order:delete` |
| 15 | PATCH | `/business/erp/sale-order/{id}/status` | 提交/关闭/取消 | `sale-order:status` |
| 16 | POST | `/business/erp/sale-order/{id}/audit` | 审批放行（超额） | `sale-order:audit` |
| 17 | POST | `/business/erp/sale-order/{id}/credit-exception` | 申请超额放行 | `sale-order:status` |
| 18 | GET | `/business/erp/stock-out/page` | 出库分页 | `stock-out:list` |
| 19 | GET | `/business/erp/stock-out/{id}` | 出库详情 | `stock-out:detail` |
| 20 | POST | `/business/erp/stock-out` | 新建出库单 | `stock-out:create` |
| 21 | POST | `/business/erp/stock-out/from-order/{orderId}` | 从订单拉单 | `stock-out:create` |
| 22 | PUT | `/business/erp/stock-out/{id}` | 编辑出库单 | `stock-out:update` |
| 23 | DELETE | `/business/erp/stock-out/{id}` | 删除草稿 | `stock-out:delete` |
| 24 | PATCH | `/business/erp/stock-out/{id}/status` | **过账 / 作废** | `stock-out:status` |
| 25 | POST | `/business/erp/stock-out/{id}/scan` | 扫码（逐件绑定） | `stock-out:update` |
| 26 | GET | `/business/erp/stock-out/{id}/serials` | 已扫序列号清单 | `stock-out:detail` |
| 27 | GET | `/business/erp/stock-out/print/{id}` | 打印数据（A4 三联） | `stock-out:print` |
| 28 | GET | `/business/erp/receivable/page` | 应收分页 | `receivable:list` |
| 29 | GET | `/business/erp/receivable/{id}` | 应收详情（含核销记录） | `receivable:detail` |
| 30 | GET | `/business/erp/receivable/aging` | 账龄分析 | `receivable:list` |
| 31 | GET | `/business/erp/receivable/pending` | 可核销应收（按客户） | `receipt:create` |
| 32 | GET | `/business/erp/receipt/page` | 收款分页 | `receipt:list` |
| 33 | GET | `/business/erp/receipt/{id}` | 收款详情 | `receipt:detail` |
| 34 | POST | `/business/erp/receipt` | 新建收款单 | `receipt:create` |
| 35 | PUT | `/business/erp/receipt/{id}` | 编辑收款单 | `receipt:update` |
| 36 | DELETE | `/business/erp/receipt/{id}` | 删除草稿 | `receipt:delete` |
| 37 | PATCH | `/business/erp/receipt/{id}/status` | 审核 / 作废 | `receipt:audit` |
| 38 | POST | `/business/erp/receipt/{id}/apply-prepaid` | 用预收核销 | `receivable:apply-prepaid` |
| 39 | GET | `/business/erp/report/sale-detail` | 销售明细表 | `sale-report:list` |
| 40 | GET | `/business/erp/report/customer-summary` | 客户销售汇总 | `sale-report:list` |
| 41 | GET | `/business/erp/report/gross-profit` | 毛利分析 | `sale-report:view` |
| 42 | GET | `/business/erp/report/aging` | 应收账龄表 | `sale-report:list` |
| 43 | GET | `/business/erp/report/reconcile` | 销售与应收勾稽表 | `sale-report:view` |

> 路由前缀沿用批次 1（`/business/erp/{resource}/...`，实测 `GET /business/erp/payable/page` 同款）。
> **不新增前缀、不改既有路由**。

## 9.4 开发顺序（依赖决定的，不能乱）

```
第 1 步  V0160 客户与信用（三表）+ 客户 CRUD 接口 + 客户页面
         ← 一切的开头；没有客户无法开单
第 2 步  V0164 科目与凭证规则扩展（**提前**，因为出库过账要不到规则就整链失败）
         + 回归验证批次 1 的采购入库凭证仍是 200/26/226
第 3 步  V0161 销售订单 + 订单页面 + 信用校验（软拦）
第 4 步  V0162 销售出库（普通物料路径先做）+ 过账事务 + 应收 + 两张凭证
         ← 本批技术核心，先跑通普通物料再补序列号
第 5 步  V0162 序列号路径（分流）+ 扫码接口 + 出库页面扫码区
第 6 步  V0163 应收 + 收款 + 核销 + 收款凭证
第 7 步  V0165 权限与菜单 + V0166 取号
第 8 步  报表 5 个 Tab + 勾稽表
第 9 步  验收场景 1–6 全跑 + 权限自检清单（6.7）
```

**三条不能颠倒的依赖**

1. **V0164 必须在出库过账之前**（否则规则缺失，过账事务第一步就失败，调试体验极差）。
2. **普通物料路径先于序列号路径**：序列号路径依赖 `recompute_quantity` 的正确性，
   而它只有在普通路径已能正确扣减/回滚时才好验证。
3. **前端页面先于菜单**（7.4 第 3 条）：菜单是静态路由，页面不存在会白屏。

## 9.5 前端文件清单（6 个页面 + 6 个 api + 复用）

```
frontend/src/views/business/erp/
  customer/index.vue         客户列表 + 详情抽屉 + 新建/编辑弹窗 + 调额弹窗
  customer/components/CreditAdjustModal.vue
  saleorder/index.vue        订单列表
  saleorder/Edit.vue         订单编辑（含明细表、口径徽标、信用面板）
  stockout/index.vue         出库列表
  stockout/Edit.vue          出库编辑（含扫码区）
  stockout/components/ScanPanel.vue
  stockout/components/PostConfirmModal.vue   过账确认弹窗（5.3.3）
  receivable/index.vue       应收列表 + 账龄统计条
  receivable/Detail.vue      应收详情（含核销记录）
  receipt/index.vue          收款列表
  receipt/Edit.vue           收款编辑（含核销表）
  report/index.vue           报表容器（5 个 Tab）
  report/tabs/SaleDetail.vue / CustomerSummary.vue / GrossProfit.vue / Aging.vue / Reconcile.vue
frontend/src/api/business/erp/
  customer.ts  saleorder.ts  stockout.ts  receivable.ts  receipt.ts  salereport.ts
复用（不新写）：
  views/business/erp/shared/ 的单据骨架、DocItemsTable、useDocForm、金额工具函数
  api/business/erp/ 已有的分页/导出封装
```

## 9.6 风险与缓解

| # | 风险 | 影响 | 缓解 |
|---|---|---|---|
| R1 | **税率单位混用**（物料 0.13 vs 单据行 13） | 税额算成 1/100，凭证与申报全错 | 专测（8.0 边界断言），代码层强制一个 `tax_rate_percent()` 转换函数，禁止裸用 |
| R2 | 一物一码物料走了「扣减」路径 | 库存数量被覆盖、件数与账面长期不一致 | 分流实现 + 不变式断言（4.10.1 ⑪b）+ 场景 6 专测 |
| R3 | 成本凭证 2 位 vs 流水 4 位 | 科目余额表与库存金额差几分钱，月结对不上 | 4.5 的对齐规则 + S5-05 勾稽表「成本尾差」行 + 场景 4 断言 |
| R4 | 信用并发（两单同时提交） | 静默超额，无审批痕迹 | `SELECT … FOR UPDATE` 锁客户行 + 场景 2 步 10 并发用例 |
| R5 | 核销并发（两人同时核同一张应收） | 重复核销，应收变负 | 核销时 `SELECT … FOR UPDATE` 应收行 + `ck` 约束兜底（`received ≤ amount`） |
| R6 | 迁移号撞车（与 P3-D2 或批次 1 补丁） | 迁移失败/顺序错乱 | 统一从 V0160 起；**开工第一步查 `schema_migrations` 最大号**（9.1 已核到 155） |
| R7 | 角色不存在导致全 403 | 上线后业务方无法使用 | 6.6 模板 + 6.7 自检第 7 条 + 上线清单 |
| R8 | 出库过账事务过长（500 件序列号） | 锁等待、超时 | 4.10.4 的量级约束 + P1 批量优化方案 |
| R9 | 用户把 `out_date` 填到已结账期间 | 期间校验拦住，但用户体验差 | 前端日期控件限制到 OPEN 区间 + 明确文案 M-25 |

---

# 十、明确不做（防范围蔓延）

| # | 不做的功能 | 为什么不做 | 什么时候做 |
|---|---|---|---|
| 1 | **销售退货 / 销售换货** | 序列号状态机单向不可逆（F29，有单测锁死），退货必须改状态机 + 新增 `RETURNED` 状态 + 重定退货成本口径，本批无此预算 | **批次 3** |
| 2 | **发票管理（开票、红冲、发票台账）** | 需要发票号段、税控对接、与收入凭证的勾稽，是独立子系统 | 批次 3 |
| 3 | **生产 / BOM / 委外** | BOM 是独立实体（父子物料+用量+层级+损耗率），批次 1 §6.3 已明确「不预留字段」 | 批次 3+ |
| 4 | **期末结账 / 三大报表（资产负债表、利润表、现金流量表）** | 结账涉及结转损益、期末调汇、年度结转，是一个完整模块 | 批次 4 |
| 5 | **多币种 / 汇率** | 实库 `currency_code` 字段已有（默认 CNY），但汇率表、本位币折算、汇兑损益都没做 | 批次 4 |
| 6 | **客户价格协议（价格表 / 折扣阶梯 / 价控）** | 决策 ⑨：会引出"下单价与协议价不符拦不拦"的连锁决策，且中小企业普遍线下维护 | 批次 3（有明确需求再做） |
| 7 | **坏账计提 / 坏账核销** | 决策 ⑩：计提比例是会计政策，需要期末结账配合；本批只做逾期预警与冻结 | 批次 4 |
| 8 | **预收款单（独立单据）** | 本批用「收款单 + 未核销余额（2203 预收）」表达，能覆盖 90% 场景；独立预收单还要处理"预收转应收"的单据流 | 批次 3 视需求 |
| 9 | **物流/发货（运单号、承运商、签收状态）** | 与 ERP 主干无关，属 TMS 范畴 | 不做（外部系统对接） |
| 10 | **销售提成 / 业绩考核** | 规则每家公司不同（按回款/按发货/按毛利），属 HR 与 BI 范畴 | 不做（报表出数据，Excel 算） |
| 11 | **库存盘点（销售侧）** | 批次 D 已有 `inventory:audit` 库存巡检与序列号对账，够用 | 已有 |
| 12 | **销售侧审批工作流（多级审批）** | 批次 D 的可编辑工作流可配置，但本批只保留「超额放行」一个审批点，用权限码实现 | 批次 2.5 或按需接工作流 |
| 13 | **`locked_qty` 库存锁定** | 决策 ⑫：防不了并发，却与派生数量打架；用「可承诺量」查询替代 | 不做（正式作废该预留） |
| 14 | **出库单部分作废（作废其中一行）** | 红冲的最小粒度是"整张凭证"，部分作废需要拆凭证，复杂度高 | 批次 3 |

> 这张表的作用是**在评审会上挡住"顺便加一下"的请求**。
> 任何一条要加进本批，都必须同时给出：多几天的工期、砍掉哪条 P0、以及它依赖的表是否已就绪。

---

# 附录 A 表结构与字段口径（给 architect / backend，**字段表，非 DDL**）

> 约定（与批次 1 完全一致）：
> 主键 `id UUID DEFAULT gen_random_uuid()`；审计五件套 `created_at` `updated_at` `create_by` `update_by` `deleted_at`；
> 状态列用 `VARCHAR` + CHECK 枚举（**不用 SMALLINT**，与实库 F2/F5/F7 一致）；
> 精度：数量 `NUMERIC(18,4)`、单价 `NUMERIC(18,6)`、金额 `NUMERIC(18,4)`、税率 `NUMERIC(7,4)`、期间 `VARCHAR(7)`；
> 单号唯一索引一律 `WHERE deleted_at IS NULL`（部分唯一，F3）。

## A.1 表清单（11 张）

| # | 表 | 中文 | 迁移 | 行数量级 |
|---|---|---|---|---|
| 1 | `biz_customer` | 客户档案 | V0160 | 千 |
| 2 | `biz_customer_credit_log` | 额度/信用变更流水（只增） | V0160 | 万 |
| 3 | `biz_customer_credit_exception` | 信用例外（逐单放行） | V0160 | 百 |
| 4 | `biz_sale_order` | 销售订单 | V0161 | 万 |
| 5 | `biz_sale_order_item` | 销售订单明细 | V0161 | 十万 |
| 6 | `biz_stock_out` | 销售出库单 | V0162 | 万 |
| 7 | `biz_stock_out_item` | 出库明细 | V0162 | 十万 |
| 8 | `biz_stock_out_item_serial` | 出库明细-序列号 | V0162 | 百万 |
| 9 | `biz_receivable` | 应收单 | V0163 | 万 |
| 10 | `biz_receipt` | 收款单 | V0163 | 万 |
| 11 | `biz_receipt_item` | 核销明细 | V0163 | 十万 |

## A.2 `biz_customer`（镜像 `biz_supplier`，F18）

| 字段 | 类型 | 空 | 默认 | 说明 |
|---|---|---|---|---|
| `id` | UUID | N | gen_random_uuid() | 主键 |
| `customer_code` | VARCHAR(32) | N | | 编码，保存后不可改 |
| `customer_name` | VARCHAR(200) | N | | 全称 |
| `short_name` | VARCHAR(100) | Y | | 简称 |
| `customer_type` | VARCHAR(20) | N | 'ENTERPRISE' | `ENTERPRISE`/`INDIVIDUAL`/`GOV` |
| `customer_level` | VARCHAR(20) | Y | | 字典（A/B/C 或自定义） |
| `contact_person` | VARCHAR(50) | Y | | |
| `contact_phone` | VARCHAR(30) | Y | | |
| `email` | VARCHAR(100) | Y | | |
| `tax_no` | VARCHAR(50) | Y | | 税号（查重用） |
| `address` | VARCHAR(300) | Y | | |
| `credit_limit` | NUMERIC(18,4) | N | 0 | **信用额度（唯一权威存储）** |
| `credit_days` | INTEGER | N | 30 | 信用期（天） |
| `settlement_type` | VARCHAR(20) | N | 'MONTHLY' | `MONTHLY`/`CASH` |
| `credit_status` | VARCHAR(20) | N | 'NORMAL' | `NORMAL`/`FROZEN` |
| `receivable_account_id` | UUID | N | | 应收科目（末级负债类贷方） |
| `invoice_title` / `invoice_bank_name` / `invoice_bank_account` | VARCHAR | Y | | 开票信息 |
| `status` | SMALLINT | N | 1 | 1=启用 0=停用（与实库 `biz_supplier.status` 同款） |
| `remark` | TEXT | Y | | |

CHECK：
```
ck_biz_customer_credit_limit  CHECK (credit_limit >= 0)
ck_biz_customer_credit_days   CHECK (credit_days >= 0)
ck_biz_customer_settlement    CHECK (settlement_type IN ('MONTHLY','CASH'))
ck_biz_customer_credit_status CHECK (credit_status IN ('NORMAL','FROZEN'))
ck_biz_customer_status        CHECK (status IN (0,1))
```
索引：`uk_biz_customer_code`（部分唯一）、`idx_biz_customer_name`、`idx_biz_customer_tax_no`、`idx_biz_customer_status`

## A.3 `biz_customer_credit_log`（只增，不改不删）

| 字段 | 类型 | 说明 |
|---|---|---|
| `customer_id` | UUID | 客户 |
| `change_type` | VARCHAR(20) | `INIT`/`ADJUST`/`FREEZE`/`UNFREEZE`/`EXCEPTION` |
| `before_limit` / `after_limit` | NUMERIC(18,4) | 变更前后额度 |
| `delta` | NUMERIC(18,4) | 差额（`after − before`） |
| `reason` | VARCHAR(300) | 原因（≥5 字；超上限时 ≥10 字） |
| `operator_id` | UUID | 操作人 |
| `ref_id` | UUID | 关联单据/例外 id（可空） |
| `created_at` | TIMESTAMPTZ | |

CHECK：`ck_biz_credit_log_type`、`ck_biz_credit_log_delta CHECK (delta = after_limit - before_limit)`；
索引 `idx_biz_credit_log_customer (customer_id, created_at DESC)`。**无 `updated_at`/`deleted_at`**（流水只增）。

## A.4 `biz_customer_credit_exception`（逐单放行）

| 字段 | 类型 | 说明 |
|---|---|---|
| `customer_id` | UUID | 客户 |
| `order_id` | UUID | 绑定订单（**不可跨单复用**） |
| `exception_no` | VARCHAR(32) | `EX-yyyyMMdd-000001` |
| `approved_amount` | NUMERIC(18,4) | 放行金额 |
| `used_amount` | NUMERIC(18,4) | 已用金额（按出库累计） |
| `available_amount` | NUMERIC(18,4) | = approved − used |
| `valid_from` / `valid_to` | DATE | 有效期（默认 30 天） |
| `status` | VARCHAR(16) | `ACTIVE`/`EXPIRED`/`REVOKED` |
| `reason` | VARCHAR(300) | 放行理由（≥5 字） |
| `approved_by` / `approved_at` | UUID / TIMESTAMPTZ | 审批人 |

CHECK：`used_amount >= 0`、`used_amount <= approved_amount`、`available = approved − used`、`valid_from <= valid_to`；
索引 `uk_biz_credit_exc_no`、`idx_biz_credit_exc_order (order_id) WHERE status='ACTIVE'`。

## A.5 `biz_sale_order`（逐字段镜像 `biz_purchase_order`，F2）

| 字段 | 类型 | 说明 | 对应批次 1 |
|---|---|---|---|
| `order_no` | VARCHAR(32) | `SO-yyyyMMdd-000001` | `order_no` |
| `customer_id` | UUID N | 客户 | `supplier_id` |
| `order_date` | DATE N | 订单日期 | 同 |
| `expect_date` | DATE | 交期 | 同 |
| `warehouse_id` | UUID N | 发货仓 | 同 |
| `status` | VARCHAR(24) N | `DRAFT`/`SUBMITTED`/`APPROVED`/`PARTIAL_OUT`/`OUTBOUND`/`CLOSED`/`CANCELLED` | `DRAFT`/`SUBMITTED`/`APPROVED`/`PARTIAL_RECEIVED`/`RECEIVED`/`CLOSED`/`CANCELLED` |
| `price_mode` | VARCHAR(8) N | `EXCL`/`INCL` | 同 |
| `currency_code` | CHAR(3) N | CNY | 同 |
| `total_qty` / `total_amount` / `total_tax` / `total_with_tax` | NUMERIC(18,4) | 四个合计 | 同 |
| `credit_days` | INTEGER N | 下单时客户账期快照 | 同 |
| `credit_flag` | VARCHAR(16) N | `NORMAL`/`OVER`（**本批新增**） | — |
| `credit_exception_id` | UUID | 放行例外（可空） | — |
| `settlement_type` | VARCHAR(20) | 结算方式快照 | — |
| `remark` | TEXT | | 同 |
| `submitted_at/by`、`approved_at/by`、`closed_at/by` | | 6 个人时间字段 | 同 |

CHECK（照抄 F2）：`ck_biz_so_status`、`ck_biz_so_tax_sum`、`ck_biz_so_price_mode`、`ck_biz_so_amount`、`ck_biz_so_qty`、`ck_biz_so_credit`、`ck_biz_so_credit_flag`
索引：`uk_biz_so_no`（部分唯一）、`idx_biz_so_date/status/customer/warehouse`、`idx_biz_so_over (credit_flag) WHERE credit_flag='OVER'`

## A.6 `biz_sale_order_item`（镜像 `biz_purchase_order_item`，F4）

| 字段 | 类型 | 说明 |
|---|---|---|
| `order_id` | UUID N | FK → 主表 `ON DELETE CASCADE` |
| `line_no` | INTEGER N | 行号，`uk (order_id, line_no)` |
| `material_id` / `unit_id` | UUID N | 物料、单位 |
| `qty` | NUMERIC(18,4) N | 数量 |
| `price` / `price_incl` | NUMERIC(18,6) N | 不含税/含税单价 |
| `tax_rate` | NUMERIC(7,4) N | **百分数**（13 表示 13%） |
| `amount` / `tax_amount` / `with_tax_amount` | NUMERIC(18,4) N | 三金额 |
| `shipped_qty` | NUMERIC(18,4) N DEFAULT 0 | **已发数量**（对应 `received_qty`） |
| `remark` | VARCHAR(255) | |

CHECK：`ck_biz_so_item_qty (qty > 0)`、`ck_biz_so_item_tax (with_tax_amount = amount + tax_amount)`、
`ck_biz_so_item_tax_rate (0 ≤ tax_rate ≤ 100)`、`ck_biz_so_item_shipped (0 ≤ shipped_qty ≤ qty)`

## A.7 `biz_stock_out`（镜像 `biz_stock_in`，F5）

| 字段 | 类型 | 说明 |
|---|---|---|
| `out_no` | VARCHAR(32) N | `OUT-yyyyMMdd-000001` |
| `out_type` | VARCHAR(20) N | `SALE`/`OTHER`（退货留 `RETURN` 枚举值但本批不可选） |
| `source_order_id` | UUID | 来源订单（可空） |
| `customer_id` | UUID N | 客户（`out_type='SALE'` 时必填，仿 `ck_biz_si_supplier`） |
| `warehouse_id` | UUID N | 仓库在表头 |
| `out_date` | DATE N | 出库日期 |
| `status` | VARCHAR(16) N | `DRAFT`/`POSTED`/`CANCELLED` |
| `currency_code` | CHAR(3) N | CNY |
| `total_qty` / `total_amount` / `total_tax` / `total_with_tax` | NUMERIC(18,4) N | 四个合计 |
| `total_cost` | NUMERIC(18,4) N DEFAULT 0 | **成本合计（本批新增）** |
| `voucher_id` | UUID | 收入凭证 |
| `voucher_cost_id` | UUID | **成本凭证（本批新增的第二个指针）** |
| `receivable_id` | UUID | 应收单 |
| `invoice_status` | VARCHAR(16) N | `NOT_INVOICED`/`INVOICED`（本批恒 NOT_INVOICED，批次 3 回填） |
| `posted_at/by`、`cancelled_at/by`、`cancel_reason` | | 同批次 1 |

CHECK：`ck_biz_so_out_status`、`ck_biz_so_out_type`、`ck_biz_so_out_customer`、`ck_biz_so_out_tax`、`ck_biz_so_out_qty`
索引：`uk_biz_out_no`（部分唯一）、`idx_biz_out_date/status/customer/warehouse`、`idx_biz_out_receivable (receivable_id)`

## A.8 `biz_stock_out_item` 与 `biz_stock_out_item_serial`

**`biz_stock_out_item`**（镜像 `biz_stock_in_item`，F6）

`stock_out_id` N、`line_no` N、`source_item_id`（来源订单行）、`material_id` N、`unit_id` N、
`qty` N、`price`/`price_incl` N、`tax_rate` N、`amount`/`tax_amount`/`with_tax_amount` N、
**`cost_price` NUMERIC(18,6)**、**`cost_amount` NUMERIC(18,4)**、`location_id`、`remark`；
`uk_biz_out_item_line (stock_out_id, line_no)`；CHECK `qty > 0`、`tax = amount + tax_amount`、`cost_amount >= 0`。

**`biz_stock_out_item_serial`**（镜像 `biz_stock_transfer_item_serial`）

`id` `item_id`（出库明细行）`serial_id`（`biz_material_serial.id`）`serial_no`（快照）`created_at` `create_by`；
唯一 `uk_biz_out_item_serial (item_id, serial_id)`；索引 `idx_biz_out_serial (serial_id)`。

## A.9 `biz_receivable`（镜像 `biz_payable`，F7）

| 字段 | 类型 | 说明 | 对应批次 1 |
|---|---|---|---|
| `receivable_no` | VARCHAR(32) N | `AR-yyyyMMdd-000001` | `payable_no` |
| `customer_id` | UUID N | 客户 | `supplier_id` |
| `source_type` | VARCHAR(20) N | `STOCK_OUT` | `STOCK_IN` |
| `source_id` / `source_no` | UUID N / VARCHAR N | 出库单 | 同 |
| `invoice_no` / `invoice_date` | VARCHAR / DATE | 发票（批次 3 回填） | 同 |
| `currency_code` | CHAR(3) N | CNY | 同 |
| `amount` | NUMERIC(18,4) N | 价税合计 | 同 |
| `received_amount` | NUMERIC(18,4) N DEFAULT 0 | **已收**（`paid_amount` 改名） | `paid_amount` |
| `unpaid_amount` | NUMERIC(18,4) N | 未收 | 同 |
| `credit_days` | INTEGER N | 账期 | 同 |
| `due_date` | DATE N | 到期日 | 同 |
| `period_code` | VARCHAR(7) N | 期间 | 同 |
| `status` | VARCHAR(20) N | `OPEN`/`PARTIAL_PAID`/`SETTLED`/`CANCELLED` | 同 |
| `settled_at` | TIMESTAMPTZ | 结清时间 | 同 |
| `voucher_id` | UUID | 收入凭证 | 同 |

CHECK（照抄 F7 的 4 条核心）：`unpaid_amount = amount − received_amount`、`0 ≤ received_amount ≤ amount`、
`SETTLED → unpaid = 0`、`status IN (...)`；
索引：`uk_biz_receivable_no`、**`uk_biz_receivable_source (source_type, source_id) WHERE deleted_at IS NULL`**、
`idx_biz_receivable_customer_status`、`idx_biz_receivable_due`。

## A.10 `biz_receipt` 与 `biz_receipt_item`

**`biz_receipt`**：`receipt_no` N、`customer_id` N、`receipt_date` N、`receipt_method` N（`TRANSFER`/`CASH`/`ACCEPTANCE`/`OTHER`）、
`account_code` VARCHAR(32) N（银行存款科目编码）、`amount` NUMERIC(18,4) N、`allocated_amount` N DEFAULT 0、
`unallocated_amount` N、`status`（`DRAFT`/`POSTED`/`CANCELLED`）、`voucher_id`、`bank_ref`、`remark`、
`audited_at/by`；CHECK `amount > 0`、`allocated ≥ 0`、`allocated + unallocated = amount`、`allocated ≤ amount`；
索引 `uk_biz_receipt_no`、`idx_biz_receipt_customer_date`。

**`biz_receipt_item`**：`receipt_id` N、`receivable_id` N、`allocated_amount` NUMERIC(18,4) N、`source_no`（应收单号快照）、
`created_at`、`create_by`；CHECK `allocated_amount > 0`；唯一 `uk_biz_receipt_item (receipt_id, receivable_id)`；
索引 `idx_biz_receipt_item_recv (receivable_id)`。

## A.11 精度与运算口径（统一，不容各表自定义）

| 项 | 规则 |
|---|---|
| 舍入策略 | 四舍五入（`MidpointAwayFromZero`），**全项目统一**，禁混用银行家舍入 |
| 数量 | `NUMERIC(18,4)`，比较用精确相等（不用浮点） |
| 单价 | `NUMERIC(18,6)`，出库只读（不产生新单价） |
| 金额（库存/流水/凭证） | `NUMERIC(18,4)`，落库前 `round_dp(4)` |
| 金额（单据展示/凭证 2 位） | `round(x, 2)` |
| 税率 | 单据行 `0–100`（百分数）；物料 `0–1`（比例）→ **必须转换** |
| 汇率 | 本批不涉及（单币种） |
| 计算顺序 | 行内先算价款再算税；汇总先逐行舍入再相加；**永远不用"总额 × 税率"** |

---

# 附录 B 需要老大（用户）拍板的 5 条

| # | 待定项 | 本 PRD 的默认 | 备选 | 影响面 |
|---|---|---|---|---|
| 1 | **信用控制强度** | C：下单软拦 + 出库硬拦 | A 只下单拦 / B 只出库拦 / OFF | 3.1；改系统参数即可，不改代码 |
| 2 | **负库存默认策略** | 全部仓库 `allow_negative=0` | 某些仓允许（如备件仓） | 3.2；仓库档案逐仓改 |
| 3 | **收入确认是否要等开票** | **出库即确认**（权责发生制 + 发货即纳税义务） | 等开票（需发票模块，批次 3） | 3.3；若选等开票，本批销售侧无收入凭证，闭环断链 |
| 4 | **收款规则的第二个贷方科目怎么落** | 复用 `tax_account_id` 存 2203（附迁移注释） | 给规则表加 `secondary_credit_account_id`（更干净，改 `load_rule`） | 3.7.3；影响 1 个小函数 |
| 5 | **出库指定库位时，整仓汇总行与库位行如何并存** | 本批只扣整仓汇总行（`location_id IS NULL`），库位库存不启用 | 强制按库位扣减（需要先把历史数据搬到库位行） | 决策 ⑬ / A.8；实库 `biz_inventory_location` 现为 0 行（F22） |

---

# 附录 C 交付信息与自检

## C.1 本文的 6 条一句话总结（给不看全文的人）

1. **做什么**：客户+信用 → 销售订单 → 销售出库（扣库存 + 结转成本）→ 应收 → 收款核销 → 两张凭证 → 总账/报表。
2. **最关键的一个决策**：收入凭证与成本凭证**分两张**（`SALE_OUT` / `SALE_OUT_COST`），因为实库规则表是扁平三科目，一个 `biz_type` 只能出一张固定形状的凭证。
3. **最容易踩的坑**：一物一码物料的账面数量是**派生值**（`COUNT(IN_STOCK)`），出库必须走序列号流转，不能走 `qty −= n`。
4. **最容易被忽略的 1 分钱**：库存金额 4 位、单据 2 位 → 成本凭证末行倒挤 + 勾稽表披露尾差。
5. **本批明确不做**：退货、发票、坏账、价格协议、多币种、结账报表（共 14 条，见第十章）。
6. **和批次 1 的关系**：金额口径 / 移动加权 / 尾差规则 / 取号 / 凭证引擎 / 单据框架 **全部复用，一个字不改**；只新增 11 张表、扩展 2 个表、加 38 个权限码、7 行菜单、7 条迁移（V0160–V0166）。

## C.2 交付自查（逐条可验证）

| # | 要求 | 状态 | 怎么验 |
|---|---|---|---|
| 1 | 文档 ≥1500 行 | ✅ 见 `wc -l` | `wc -l docs/ERP-批次2-产品需求PRD.md` |
| 2 | 每个产品决策有候选对比表 + 结论 + 理由 | ✅ 14 个决策（3.1–3.14）全部有对比表 | `grep -c '候选方案对比'` |
| 3 | 每条业务规则有公式 + 算例 + 边界 | ✅ 13 条规则（4.1–4.13） | `grep -c '^### 4\.'` |
| 4 | 功能清单用表格（编号/功能/说明/优先级） | ✅ 78 项（S1-01 ~ S5-08） | 第二章 5 张表 |
| 5 | 端到端故事 3–4 个角色 | ✅ 5 个（销售内勤/仓管/会计/主管/老板） | 第二章 1.2 |
| 6 | 金额口径/成本法/尾差与批次 1 一致 | ✅ 4.1/4.2 明写"沿用，一字不改" | 对照批次 1 §3.1/§3.2 |
| 7 | 必须先查真实表结构 | ✅ 第 0 章 30 条事实（F1–F30）+ 7 处偏差（X1–X7），全部带查询/文件行号 | 0.1–0.4 |
| 8 | 与批次 1 的衔接明确 | ✅ 第九章（迁移/复用/接口/顺序/风险）+ 9.2 复用清单 | 第九章 |
| 9 | 明确不做（防范围蔓延） | ✅ 14 条 | 第十章 |
| 10 | 只写文档，不写代码 | ✅ 未修改任何 `.rs`/`.vue`/`.sql` | `git status` 只应有本文件 |

## C.3 引用到的实库对象与实码位置（便于复核）

```
实库（172.18.10.144 / cloudpivot）：
  information_schema.columns / pg_constraint / pg_indexes → 42 张 biz_ 表结构
  biz_account（4 行）/ biz_seq（6 行）/ biz_voucher_rule（1 行）
  schema_migrations（最大 version=155）
  sys_menu（61 个 business:erp:* 权限码；ERP 4 个业务目录在根级（8fa2d1e 后已无 0940）；id 段 1000–1010 已用）
  sys_role（5 个角色）

实码（/opt/cloudpivot @ 8fa2d1e，本 PRD 提交前的最新 main）：
  backend/src/business/erp/voucher.rs:80-105, 330-361, 44-51        凭证引擎/规则/红冲
  backend/src/business/erp/payable.rs:220-310                        应付生成（应收镜像）
  backend/src/business/erp/transfer_model.rs:1847-1893, 2129-2200    出库扣减/不变式
  backend/src/business/erp/serial_model.rs:592-629, 658-735, 347-375 派生数量/流水/状态机
  backend/src/shared/error_code.rs:946-1016                          ERP 错误码 66001–66009
  backend/migrations/business/V0151__erp_payable_permissions.up.sql   权限种子写法
```

---

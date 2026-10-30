-- ============================================================
-- V0137：ERP 主数据（6 表）+ 仓储基线补齐（5 表）+ 仓库层级 + 库位（2 表）
--        = 13 张表 DDL + 种子数据 + 空库自检
--
-- 依据（全部照抄，不自创结构）：
--   ① `docs/ERP-仓库管理-技术架构.md`（W0 设计，表清单 §2.1、DDL §2.2–§2.4）
--      = `erp_wh/migrations/V0137__erp_warehouse_location.up.sql` 的 7 张仓储表
--   ② `docs/ERP-批次1-技术架构.md` §2.1.1/§2.1.3/§2.1.5/§2.1.6/§2.2（主数据与采购 6 表，
--      逐字照抄，一个字段都没改；本卡只负责把它们落库）
--   ③ 底座 `V0006__create_sys_dept`（物化路径 `dept_path` 的技法与命名同款）
--
-- 回滚：`V0137__erp_master_data_and_warehouse.down.sql`（有数据即中止 + 逆序 DROP）
--
-- ── 表清单（13 张）────────────────────────────────────────────────────────
--   主数据 6：biz_unit / biz_account / biz_supplier / biz_accounting_period
--             / biz_purchase_order / biz_purchase_order_item
--   基线 5  ：biz_seq / biz_warehouse / biz_material / biz_inventory / biz_inventory_txn
--   本次新增：biz_location / biz_inventory_location
--
-- ── 为什么「主数据 6 表」也在本迁移里 ──────────────────────────────────────
-- 卡面点名 `biz_unit` / `biz_supplier` / `biz_account` / `biz_accounting_period` /
-- `biz_purchase_order{,_item}` 必须落库（卡题：ERP **主数据** + 仓库层级 + 库位）。
-- W0 设计把这 6 张留给批次1 的 V0030/V0031/V0032（见 W0 §2.1 表清单、§12 迁移债），
-- 但批次1 的 V0030–V0039 至今**一个都没落地**（§1.3 实测：`grep -rhoE 'CREATE TABLE biz_[a-z_]+'
-- backend/migrations/` 只有 biz_flow_* / biz_task_* / biz_project* 18 张）。
-- 因此本卡按卡面把 6 张一并建出；`biz_material.unit_id`（NOT NULL 弱引用 biz_unit.id）
-- 与 `biz_material.inventory_account_id`（弱引用 biz_account.id）也因此不再是悬空引用。
--
-- ── 迁移债（与 W0 §12 同口径，必须由总管裁决）─────────────────────────────
-- V0137 上线后，批次1 的 `V0030__create_biz_master_data` / `V0031__create_biz_purchase_order`
-- / `V0032__create_biz_stock` **不得再原样执行**（`CREATE TABLE biz_warehouse` 会因表已存在而失败）。
-- 收口方式（本文件对以下两条路径**都兼容**，POC 两条都真跑过）：
--   ① 批次1 迁移改为「只建本次没建的 7 张」（biz_voucher* / biz_payable* / biz_account_balance
--      / biz_stock_in* …）+ 其余走 ALTER；
--   ② 批次1 先落地（表已存在）→ 本文件所有 CREATE 都是 `IF NOT EXISTS`、
--      所有新列都是 `ADD COLUMN IF NOT EXISTS`，**顺序无关、可重复执行**。
--
-- ── 建模要点（W0 的硬结论，别改）──────────────────────────────────────────
-- ① 仓库树用**物化路径** `warehouse_path`（`/uuid/uuid/`），索引必须同时有
--    `text_pattern_ops` 版本：非 C collation（本机实测 en_US.UTF-8；W0 文档写 zh_CN.UTF-8，
--    两者结论一致）下默认 btree 在 LIKE 前缀上推不出 `~>=~ / ~<~` 边界，只能整列吐给
--    heap 再 Filter（20 万行实测 15.4ms vs 0.11ms，135×，见验收证据 §5）。
-- ② 层级上限 3 级（总仓/分仓/库房），DB `CHECK (level BETWEEN 1 AND 3)`。
-- ③ 库位编码**仓库内唯一**，不是全局唯一。
-- ④ 库位维度不塞进 `biz_inventory`，另建 `biz_inventory_location`（两级账）。
-- ⑤ 弱引用不建外键（与 `sys_user.dept_id` 同约定）；全文件唯一的 FK 是
--    `biz_purchase_order_item.order_id → biz_purchase_order(id) ON DELETE CASCADE`
--    （明细随主单删除，**批次1 §2.2.2 原样**，属于同一迁移内的主子表）。
-- ⑥ 状态列口径：**业务单据状态用 VARCHAR + CHECK 枚举**（`biz_purchase_order.status`
--    VARCHAR(24)、`biz_accounting_period.status` VARCHAR(16)，与批次1 §1.6 一致）；
--    **主数据启用位仍是 SMALLINT 1/0**（`biz_unit/warehouse/material/supplier/account/...`
--    的 status）—— 这两个是不同语义，批次1 §2.1 原样照抄，不要统一。
-- ⑦ 金额/单价：数量与金额 `NUMERIC(18,4)`、单价与均价 `NUMERIC(18,6)`；不存浮点。
--
-- ── 三条写法纪律（V0136 踩过的坑，别改）───────────────────────────────────
-- 1. 文件内**不写** `BEGIN; … COMMIT;` —— 迁移器自己在事务里执行；文件内 COMMIT 会提前
--    提交掉迁移器的事务（V0136 实测 `WARNING: there is already a transaction in progress`）。
-- 2. 状态联动约束一律写 `CASE status WHEN … THEN … ELSE FALSE END` 或显式布尔比较，
--    不写 `A OR (B AND C)`：PG 的 CHECK 在结果为 NULL 时**视为通过**（三值逻辑）。
-- 3. 弱引用不建外键（见上 ⑤ 的唯一例外）。
-- ============================================================

-- ============================================================
-- §0 主数据（照抄 `docs/ERP-批次1-技术架构.md` §2.1 / §2.2，逐字一致）
-- ============================================================

-- ---------- §0.1 `biz_unit` —— 计量单位（§2.1.1）----------
CREATE TABLE IF NOT EXISTS biz_unit (
    id            UUID PRIMARY KEY DEFAULT gen_random_uuid(),       -- 主键
    unit_code     VARCHAR(20)  NOT NULL,                            -- 单位编码（如 PCS / KG / BOX）
    unit_name     VARCHAR(50)  NOT NULL,                            -- 单位名称（如 个 / 千克 / 箱）
    is_base       SMALLINT     NOT NULL DEFAULT 1,                  -- 是否基础单位：1 是 / 0 否（辅助单位）
    base_unit_id  UUID,                                             -- 换算基准单位（弱引用 biz_unit.id，基础单位为空）
    rate          NUMERIC(18,6) NOT NULL DEFAULT 1,                 -- 换算率：1 本单位 = rate 个基础单位
    status        SMALLINT     NOT NULL DEFAULT 1,                  -- 状态：1 启用 / 0 停用
    sort_order    INT          NOT NULL DEFAULT 0,                  -- 排序号
    remark        TEXT,                                             -- 备注
    created_at    TIMESTAMPTZ  NOT NULL DEFAULT CURRENT_TIMESTAMP,  -- 创建时间
    updated_at    TIMESTAMPTZ  NOT NULL DEFAULT CURRENT_TIMESTAMP,  -- 更新时间
    create_by     UUID,                                             -- 创建人（弱引用 sys_user.id）
    update_by     UUID,                                             -- 更新人（弱引用 sys_user.id）
    deleted_at    TIMESTAMPTZ,                                      -- 软删时间（非空即已删除）

    CONSTRAINT ck_biz_unit_rate    CHECK (rate > 0),
    CONSTRAINT ck_biz_unit_is_base CHECK (is_base IN (0, 1)),
    -- 辅助单位必须指明基准单位；基础单位不允许带基准（防自我循环）
    CONSTRAINT ck_biz_unit_base    CHECK ((is_base = 1 AND base_unit_id IS NULL)
                                       OR (is_base = 0 AND base_unit_id IS NOT NULL)),
    CONSTRAINT ck_biz_unit_no_self CHECK (base_unit_id IS NULL OR base_unit_id <> id)
);

COMMENT ON TABLE  biz_unit IS '计量单位（1 BOX = 12 PCS：BOX 行 is_base=0 / base_unit_id=PCS.id / rate=12）';

CREATE UNIQUE INDEX IF NOT EXISTS uk_biz_unit_code      ON biz_unit (unit_code) WHERE deleted_at IS NULL;
CREATE INDEX        IF NOT EXISTS idx_biz_unit_base_unit ON biz_unit (base_unit_id);
CREATE INDEX        IF NOT EXISTS idx_biz_unit_status    ON biz_unit (status);


-- ---------- §0.2 `biz_account` —— 会计科目（§2.1.3）----------
CREATE TABLE IF NOT EXISTS biz_account (
    id                UUID PRIMARY KEY DEFAULT gen_random_uuid(),   -- 主键
    account_code      VARCHAR(30)  NOT NULL,                        -- 科目编码（如 1405 / 2202.01，按级次分段）
    account_name      VARCHAR(120) NOT NULL,                        -- 科目名称（如 库存商品 / 应付账款—暂估应付款）
    parent_id         UUID,                                         -- 父科目 ID（弱引用 biz_account.id，一级科目为空）
    level             SMALLINT     NOT NULL DEFAULT 1,              -- 科目级次（1 一级 / 2 二级 / …）
    account_category  VARCHAR(20)  NOT NULL,                        -- 类别：ASSET 资产 / LIABILITY 负债 / EQUITY 权益 / COST 成本 / PROFIT_LOSS 损益
    balance_direction VARCHAR(6)   NOT NULL,                        -- 余额方向：DEBIT 借 / CREDIT 贷
    is_leaf           SMALLINT     NOT NULL DEFAULT 1,              -- 是否末级科目：1 末级（可挂分录）/ 0 非末级（仅汇总）
    aux_dimensions    VARCHAR(120),                                 -- 辅助核算维度（逗号分隔：SUPPLIER,MATERIAL,WAREHOUSE,DEPT）
    status            SMALLINT     NOT NULL DEFAULT 1,              -- 状态：1 启用 / 0 停用
    remark            TEXT,                                         -- 备注
    created_at        TIMESTAMPTZ  NOT NULL DEFAULT CURRENT_TIMESTAMP,
    updated_at        TIMESTAMPTZ  NOT NULL DEFAULT CURRENT_TIMESTAMP,
    create_by         UUID,
    update_by         UUID,
    deleted_at        TIMESTAMPTZ,

    CONSTRAINT ck_biz_account_category CHECK (account_category IN ('ASSET','LIABILITY','EQUITY','COST','PROFIT_LOSS')),
    CONSTRAINT ck_biz_account_direction CHECK (balance_direction IN ('DEBIT','CREDIT')),
    CONSTRAINT ck_biz_account_level     CHECK (level >= 1),
    CONSTRAINT ck_biz_account_leaf      CHECK (is_leaf IN (0, 1)),
    CONSTRAINT ck_biz_account_no_self   CHECK (parent_id IS NULL OR parent_id <> id)
);

COMMENT ON TABLE  biz_account IS '会计科目（科目编码一旦用过永久占用：凭证会快照 account_code/account_name，软删后复用会让历史凭证指向不同科目）';

-- 科目编码全表唯一：**有意不带** WHERE deleted_at IS NULL（批次1 §2.1.3 唯一的例外条款）
CREATE UNIQUE INDEX IF NOT EXISTS uk_biz_account_code     ON biz_account (account_code);
CREATE INDEX        IF NOT EXISTS idx_biz_account_parent   ON biz_account (parent_id);
CREATE INDEX        IF NOT EXISTS idx_biz_account_category ON biz_account (account_category);
CREATE INDEX        IF NOT EXISTS idx_biz_account_leaf     ON biz_account (is_leaf, status);


-- ---------- §0.3 `biz_supplier` —— 供应商（§2.1.5）----------
CREATE TABLE IF NOT EXISTS biz_supplier (
    id                   UUID PRIMARY KEY DEFAULT gen_random_uuid(), -- 主键
    supplier_code        VARCHAR(40)   NOT NULL,                     -- 供应商编码
    supplier_name        VARCHAR(200)  NOT NULL,                     -- 供应商全称
    short_name           VARCHAR(100),                               -- 简称
    contact_person       VARCHAR(50),                                -- 联系人
    contact_phone        VARCHAR(30),                                -- 联系电话
    email                VARCHAR(120),                               -- 邮箱
    tax_no               VARCHAR(50),                                -- 纳税人识别号
    address              VARCHAR(255),                               -- 地址
    credit_days          INT           NOT NULL DEFAULT 0,           -- 账期天数（0 = 现结）
    credit_limit         NUMERIC(18,4) NOT NULL DEFAULT 0,           -- 信用额度（0 = 不限）
    payable_account_id   UUID,                                       -- 应付科目（弱引用 biz_account.id，为空走规则解析）
    invoice_title        VARCHAR(200),                               -- 开票名称
    invoice_bank_name    VARCHAR(200),                               -- 开票开户行
    invoice_bank_account VARCHAR(64),                                -- 开票银行账号
    status               SMALLINT      NOT NULL DEFAULT 1,           -- 状态：1 启用 / 0 停用
    remark               TEXT,                                       -- 备注
    created_at           TIMESTAMPTZ   NOT NULL DEFAULT CURRENT_TIMESTAMP,
    updated_at           TIMESTAMPTZ   NOT NULL DEFAULT CURRENT_TIMESTAMP,
    create_by            UUID,
    update_by            UUID,
    deleted_at           TIMESTAMPTZ,

    CONSTRAINT ck_biz_supplier_credit_days  CHECK (credit_days >= 0),
    CONSTRAINT ck_biz_supplier_credit_limit CHECK (credit_limit >= 0)
);

COMMENT ON TABLE biz_supplier IS '供应商主数据（payable_account_id 为弱引用，为空时走规则解析）';

CREATE UNIQUE INDEX IF NOT EXISTS uk_biz_supplier_code   ON biz_supplier (supplier_code) WHERE deleted_at IS NULL;
CREATE INDEX        IF NOT EXISTS idx_biz_supplier_name   ON biz_supplier (supplier_name);
CREATE INDEX        IF NOT EXISTS idx_biz_supplier_status ON biz_supplier (status);


-- ---------- §0.4 `biz_accounting_period` —— 会计期间（§2.1.6）----------
-- 本表**无 deleted_at**（期间不可删除，否则历史凭证失去归期依据），模型侧 `#[model(hard_delete)]`。
CREATE TABLE IF NOT EXISTS biz_accounting_period (
    id           UUID PRIMARY KEY DEFAULT gen_random_uuid(),        -- 主键
    period_year  SMALLINT    NOT NULL,                              -- 会计年度（如 2026）
    period_month SMALLINT    NOT NULL,                              -- 会计月份（1-12）
    period_code  VARCHAR(7)  NOT NULL,                              -- 期间编码（yyyy-MM，如 2026-10；凭证据此归期）
    start_date   DATE        NOT NULL,                              -- 期间开始日
    end_date     DATE        NOT NULL,                              -- 期间结束日
    status       VARCHAR(16) NOT NULL DEFAULT 'NOT_OPENED',         -- 期间状态：NOT_OPENED 未开启 / OPEN 已开启 / CLOSED 已结账
    closed_at    TIMESTAMPTZ,                                       -- 结账时间
    closed_by    UUID,                                              -- 结账人
    remark       TEXT,                                              -- 备注
    created_at   TIMESTAMPTZ NOT NULL DEFAULT CURRENT_TIMESTAMP,
    updated_at   TIMESTAMPTZ NOT NULL DEFAULT CURRENT_TIMESTAMP,

    CONSTRAINT ck_biz_period_month  CHECK (period_month BETWEEN 1 AND 12),
    CONSTRAINT ck_biz_period_status CHECK (status IN ('NOT_OPENED','OPEN','CLOSED')),
    CONSTRAINT ck_biz_period_range  CHECK (start_date <= end_date)
);

COMMENT ON TABLE  biz_accounting_period IS '会计期间（status 为 VARCHAR 枚举：业务状态列不用 SMALLINT，与批次1 §1.6 一致）';
COMMENT ON COLUMN biz_accounting_period.status IS '期间状态：NOT_OPENED 未开启 / OPEN 已开启 / CLOSED 已结账（VARCHAR 而非 SMALLINT）';

CREATE UNIQUE INDEX IF NOT EXISTS uk_biz_period_code   ON biz_accounting_period (period_code);
CREATE UNIQUE INDEX IF NOT EXISTS uk_biz_period_ym     ON biz_accounting_period (period_year, period_month);
CREATE INDEX        IF NOT EXISTS idx_biz_period_status ON biz_accounting_period (status);


-- ---------- §0.5 `biz_purchase_order` —— 采购订单主表（§2.2.1）----------
CREATE TABLE IF NOT EXISTS biz_purchase_order (
    id             UUID PRIMARY KEY DEFAULT gen_random_uuid(),      -- 主键
    order_no       VARCHAR(32)   NOT NULL,                          -- 采购订单号（biz_seq 生成，全局唯一）
    supplier_id    UUID          NOT NULL,                          -- 供应商（弱引用 biz_supplier.id）
    order_date     DATE          NOT NULL,                          -- 订单日期
    expect_date    DATE,                                            -- 期望到货日期
    warehouse_id   UUID          NOT NULL,                          -- 默认收货仓库（弱引用 biz_warehouse.id）
    status         VARCHAR(24)   NOT NULL DEFAULT 'DRAFT',          -- 状态机：DRAFT/SUBMITTED/APPROVED/PARTIAL_RECEIVED/RECEIVED/CLOSED/CANCELLED
    currency_code  CHAR(3)       NOT NULL DEFAULT 'CNY',            -- 币种（ISO 4217）
    total_qty      NUMERIC(18,4) NOT NULL DEFAULT 0,                -- 订单数量合计
    total_amount   NUMERIC(18,4) NOT NULL DEFAULT 0,                -- 不含税金额合计
    total_tax      NUMERIC(18,4) NOT NULL DEFAULT 0,                -- 税额合计
    total_with_tax NUMERIC(18,4) NOT NULL DEFAULT 0,                -- 价税合计
    credit_days    INT           NOT NULL DEFAULT 0,                -- 账期天数快照（下单时从供应商带出）
    remark         TEXT,                                            -- 备注
    submitted_at   TIMESTAMPTZ,                                     -- 提交时间
    submitted_by   UUID,                                            -- 提交人
    approved_at    TIMESTAMPTZ,                                     -- 审批通过时间
    approved_by    UUID,                                            -- 审批人
    closed_at      TIMESTAMPTZ,                                     -- 关闭时间
    closed_by      UUID,                                            -- 关闭人
    created_at     TIMESTAMPTZ   NOT NULL DEFAULT CURRENT_TIMESTAMP,
    updated_at     TIMESTAMPTZ   NOT NULL DEFAULT CURRENT_TIMESTAMP,
    create_by      UUID,
    update_by      UUID,
    deleted_at     TIMESTAMPTZ,

    CONSTRAINT ck_biz_po_status CHECK (status IN
        ('DRAFT','SUBMITTED','APPROVED','PARTIAL_RECEIVED','RECEIVED','CLOSED','CANCELLED')),
    CONSTRAINT ck_biz_po_qty    CHECK (total_qty >= 0),
    CONSTRAINT ck_biz_po_amount CHECK (total_amount >= 0 AND total_tax >= 0 AND total_with_tax >= 0),
    -- 价税合计恒等：合计口径必须自洽，防程序算错（数据库层的会计兜底）
    CONSTRAINT ck_biz_po_tax_sum CHECK (total_with_tax = total_amount + total_tax),
    CONSTRAINT ck_biz_po_credit  CHECK (credit_days >= 0)
);

COMMENT ON TABLE  biz_purchase_order IS '采购订单主表（status 为 VARCHAR 状态机；ck_biz_po_tax_sum 是会计兜底）';
COMMENT ON COLUMN biz_purchase_order.status IS '状态机：DRAFT/SUBMITTED/APPROVED/PARTIAL_RECEIVED/RECEIVED/CLOSED/CANCELLED（VARCHAR，不用 SMALLINT）';

CREATE UNIQUE INDEX IF NOT EXISTS uk_biz_po_no      ON biz_purchase_order (order_no) WHERE deleted_at IS NULL;
CREATE INDEX        IF NOT EXISTS idx_biz_po_supplier  ON biz_purchase_order (supplier_id, order_date DESC);
CREATE INDEX        IF NOT EXISTS idx_biz_po_status    ON biz_purchase_order (status);
CREATE INDEX        IF NOT EXISTS idx_biz_po_date      ON biz_purchase_order (order_date DESC);
CREATE INDEX        IF NOT EXISTS idx_biz_po_warehouse ON biz_purchase_order (warehouse_id);


-- ---------- §0.6 `biz_purchase_order_item` —— 采购订单明细（§2.2.2）----------
-- 明细表**无 deleted_at**（随主单编辑整体重建：先删后插，事务内）。
-- 全文件唯一的外键：order_id → biz_purchase_order(id) ON DELETE CASCADE（批次1 §2.2.2 原样）。
CREATE TABLE IF NOT EXISTS biz_purchase_order_item (
    id              UUID PRIMARY KEY DEFAULT gen_random_uuid(),     -- 主键
    order_id        UUID          NOT NULL REFERENCES biz_purchase_order(id) ON DELETE CASCADE, -- 所属订单
    line_no         INT           NOT NULL,                         -- 行号（从 1 起）
    material_id     UUID          NOT NULL,                         -- 物料（弱引用 biz_material.id）
    unit_id         UUID          NOT NULL,                         -- 单位（弱引用 biz_unit.id，下单时快照）
    qty             NUMERIC(18,4) NOT NULL,                         -- 订购数量
    price           NUMERIC(18,6) NOT NULL DEFAULT 0,               -- 不含税单价
    tax_rate        NUMERIC(7,4)  NOT NULL DEFAULT 0,               -- 税率
    amount          NUMERIC(18,4) NOT NULL DEFAULT 0,               -- 不含税金额
    tax_amount      NUMERIC(18,4) NOT NULL DEFAULT 0,               -- 税额
    with_tax_amount NUMERIC(18,4) NOT NULL DEFAULT 0,               -- 价税合计
    received_qty    NUMERIC(18,4) NOT NULL DEFAULT 0,               -- 已收货数量（部分收货累计）
    remark          VARCHAR(255),
    created_at      TIMESTAMPTZ   NOT NULL DEFAULT CURRENT_TIMESTAMP,
    updated_at      TIMESTAMPTZ   NOT NULL DEFAULT CURRENT_TIMESTAMP,

    CONSTRAINT uk_biz_po_item_line UNIQUE (order_id, line_no),
    CONSTRAINT ck_biz_po_item_qty  CHECK (qty > 0),
    CONSTRAINT ck_biz_po_item_recv CHECK (received_qty >= 0 AND received_qty <= qty),
    CONSTRAINT ck_biz_po_item_tax  CHECK (with_tax_amount = amount + tax_amount)
);

COMMENT ON TABLE biz_purchase_order_item IS '采购订单明细（唯一外键 order_id→主表 ON DELETE CASCADE；其余 UUID 列全是弱引用）';

CREATE INDEX IF NOT EXISTS idx_biz_po_item_order    ON biz_purchase_order_item (order_id);
CREATE INDEX IF NOT EXISTS idx_biz_po_item_material ON biz_purchase_order_item (material_id);


-- ============================================================
-- §1 仓储基线补齐（照抄 `docs/ERP-批次1-技术架构.md`，逐字一致）
--   实测：批次1 设计的 V0030–V0039 至今一个都没落地；而仓库层级/库位/库位库存
--   物理上挂在 biz_warehouse / biz_inventory 上 —— 没有它们，本迁移无法建立。
-- ============================================================

-- ---------- §1.1 `biz_seq` —— 单据/序列号取号规则（§2.1.7）----------
CREATE TABLE IF NOT EXISTS biz_seq (
    id           UUID PRIMARY KEY DEFAULT gen_random_uuid(),        -- 主键
    seq_type     VARCHAR(32) NOT NULL,                              -- 取号键：PO 采购单 / IN 入库单 / TR 调拨单 / SN 单品序列号
    seq_name     VARCHAR(50) NOT NULL,                              -- 规则名称（如 采购订单号）
    prefix       VARCHAR(12) NOT NULL,                              -- 编号前缀（如 PO / TR / SN）
    date_pattern VARCHAR(8)  NOT NULL DEFAULT 'yyyyMMdd',           -- 日期段格式：yyyyMMdd / yyyyMM / none
    period_key   VARCHAR(8)  NOT NULL DEFAULT '',                   -- 当前计数周期键（按此键滚动重置）
    current_no   BIGINT      NOT NULL DEFAULT 0,                    -- 当前流水号（已用最大值）
    step         INT         NOT NULL DEFAULT 1,                    -- 步长
    separator    VARCHAR(4)  NOT NULL DEFAULT '-',                  -- 段分隔符
    status       SMALLINT    NOT NULL DEFAULT 1,                    -- 状态：1 启用 / 0 停用
    remark       TEXT,                                              -- 备注
    created_at   TIMESTAMPTZ NOT NULL DEFAULT CURRENT_TIMESTAMP,
    updated_at   TIMESTAMPTZ NOT NULL DEFAULT CURRENT_TIMESTAMP,
    create_by    UUID,
    update_by    UUID,
    deleted_at   TIMESTAMPTZ,

    CONSTRAINT ck_biz_seq_step CHECK (step > 0),
    CONSTRAINT ck_biz_seq_no   CHECK (current_no >= 0),
    CONSTRAINT ck_biz_seq_date CHECK (date_pattern IN ('yyyyMMdd','yyyyMM','none'))
);

COMMENT ON TABLE  biz_seq IS '单据/序列号取号规则（单语句 UPDATE…RETURNING 原子取号，PG 行锁天然无重号）';
COMMENT ON COLUMN biz_seq.seq_type IS '取号键：PO/IN/PAY/VCH 为单据；TR 调拨单；SN 单品序列号（按天滚动）';
COMMENT ON COLUMN biz_seq.period_key IS '当前计数周期键；与本次取号的周期不同则流水归零重来';
COMMENT ON COLUMN biz_seq.step IS '步长；批量取 N 个连号时用 current_no + step*N 一次推进';

CREATE UNIQUE INDEX IF NOT EXISTS uk_biz_seq_type ON biz_seq (seq_type) WHERE deleted_at IS NULL;


-- ---------- §1.2 `biz_warehouse` —— 仓库（§2.1.2 原样 + 层级 4 列）----------
CREATE TABLE IF NOT EXISTS biz_warehouse (
    id             UUID PRIMARY KEY DEFAULT gen_random_uuid(),      -- 主键
    warehouse_code VARCHAR(40)  NOT NULL,                           -- 仓库编码（如 WH01）
    warehouse_name VARCHAR(120) NOT NULL,                           -- 仓库名称
    warehouse_type VARCHAR(20)  NOT NULL DEFAULT 'NORMAL',          -- 类型：NORMAL 普通 / RAW 原料 / FINISHED 成品 / SCRAP 废品 / IN_TRANSIT 在途
    manager        VARCHAR(50),                                     -- 负责人姓名
    allow_negative SMALLINT     NOT NULL DEFAULT 0,                 -- 是否允许负库存：1 允许 / 0 禁止
    address        VARCHAR(255),                                    -- 仓库地址
    status         SMALLINT     NOT NULL DEFAULT 1,                 -- 状态：1 启用 / 0 停用
    sort_order     INT          NOT NULL DEFAULT 0,                 -- 排序号
    remark         TEXT,                                            -- 备注
    created_at     TIMESTAMPTZ  NOT NULL DEFAULT CURRENT_TIMESTAMP, -- 创建时间
    updated_at     TIMESTAMPTZ  NOT NULL DEFAULT CURRENT_TIMESTAMP, -- 更新时间
    create_by      UUID,                                            -- 创建人（弱引用 sys_user.id）
    update_by      UUID,                                            -- 更新人（弱引用 sys_user.id）
    deleted_at     TIMESTAMPTZ,                                     -- 软删时间

    CONSTRAINT ck_biz_warehouse_type CHECK (warehouse_type IN ('NORMAL','RAW','FINISHED','SCRAP','IN_TRANSIT')),
    CONSTRAINT ck_biz_warehouse_neg  CHECK (allow_negative IN (0, 1))
);

COMMENT ON TABLE  biz_warehouse IS '仓库（树形：总仓/分仓/库房三级；IN_TRANSIT 类型用于调拨在途）';
COMMENT ON COLUMN biz_warehouse.warehouse_type IS '仓库类型：NORMAL/RAW/FINISHED/SCRAP/IN_TRANSIT；与层级 level 正交';
COMMENT ON COLUMN biz_warehouse.allow_negative IS '是否允许负库存：1 允许 / 0 禁止（DB 不加 qty>=0 约束，业务策略不落库）';

CREATE UNIQUE INDEX IF NOT EXISTS uk_biz_warehouse_code ON biz_warehouse (warehouse_code) WHERE deleted_at IS NULL;
CREATE INDEX        IF NOT EXISTS idx_biz_warehouse_status ON biz_warehouse (status);
CREATE INDEX        IF NOT EXISTS idx_biz_warehouse_type   ON biz_warehouse (warehouse_type);


-- ---------- §1.3 `biz_material` —— 物料 SKU（§2.1.4 原样；serial_required 见 V0138）----------
CREATE TABLE IF NOT EXISTS biz_material (
    id                   UUID PRIMARY KEY DEFAULT gen_random_uuid(),  -- 主键
    material_code        VARCHAR(40)   NOT NULL,                      -- 物料编码（SKU 编码，全局唯一）
    material_name        VARCHAR(120)  NOT NULL,                      -- 物料名称
    spec                 VARCHAR(200),                                -- 规格型号
    unit_id              UUID          NOT NULL,                      -- 计量单位（弱引用 biz_unit.id）
    material_type        VARCHAR(20)   NOT NULL DEFAULT 'RAW',        -- 类型：RAW 原料 / SEMI 半成品 / FINISHED 成品 / SERVICE 服务
    default_warehouse_id UUID,                                        -- 默认仓库（弱引用 biz_warehouse.id）
    safety_stock         NUMERIC(18,4) NOT NULL DEFAULT 0,            -- 安全库存
    costing_method       VARCHAR(20)   NOT NULL DEFAULT 'MOVING_AVG', -- 计价方式：MOVING_AVG / FIFO / STANDARD
    inventory_account_id UUID,                                        -- 存货科目（弱引用 biz_account.id）
    tax_rate             NUMERIC(7,4)  NOT NULL DEFAULT 0,            -- 默认进项税率
    status               SMALLINT      NOT NULL DEFAULT 1,            -- 状态：1 启用 / 0 停用
    remark               TEXT,                                        -- 备注
    created_at           TIMESTAMPTZ   NOT NULL DEFAULT CURRENT_TIMESTAMP,
    updated_at           TIMESTAMPTZ   NOT NULL DEFAULT CURRENT_TIMESTAMP,
    create_by            UUID,
    update_by            UUID,
    deleted_at           TIMESTAMPTZ,

    CONSTRAINT ck_biz_material_type    CHECK (material_type IN ('RAW','SEMI','FINISHED','SERVICE')),
    CONSTRAINT ck_biz_material_costing CHECK (costing_method IN ('MOVING_AVG','FIFO','STANDARD')),
    CONSTRAINT ck_biz_material_safety  CHECK (safety_stock >= 0),
    CONSTRAINT ck_biz_material_tax     CHECK (tax_rate >= 0 AND tax_rate <= 1)
);

COMMENT ON TABLE  biz_material IS '物料/商品 SKU 主数据';
COMMENT ON COLUMN biz_material.material_code IS '物料编码，全局唯一（软删后可复用）；一物一码物料见 biz_material_serial';
COMMENT ON COLUMN biz_material.unit_id IS '计量单位（弱引用 biz_unit.id，基础单位）';

CREATE UNIQUE INDEX IF NOT EXISTS uk_biz_material_code ON biz_material (material_code) WHERE deleted_at IS NULL;
CREATE INDEX        IF NOT EXISTS idx_biz_material_unit   ON biz_material (unit_id);
CREATE INDEX        IF NOT EXISTS idx_biz_material_wh     ON biz_material (default_warehouse_id);
CREATE INDEX        IF NOT EXISTS idx_biz_material_type   ON biz_material (material_type, status);
CREATE INDEX        IF NOT EXISTS idx_biz_material_status ON biz_material (status);


-- ---------- §1.4 `biz_inventory` —— 仓库级即时库存（§2.3.3 原样，唯一键不动）----------
CREATE TABLE IF NOT EXISTS biz_inventory (
    id           UUID PRIMARY KEY DEFAULT gen_random_uuid(),        -- 主键
    material_id  UUID          NOT NULL,                            -- 物料（弱引用 biz_material.id）
    warehouse_id UUID          NOT NULL,                            -- 仓库（弱引用 biz_warehouse.id）
    qty          NUMERIC(18,4) NOT NULL DEFAULT 0,                  -- 结存数量
    locked_qty   NUMERIC(18,4) NOT NULL DEFAULT 0,                  -- 锁定量（预留）
    avg_cost     NUMERIC(18,6) NOT NULL DEFAULT 0,                  -- 移动加权平均单价
    amount       NUMERIC(18,4) NOT NULL DEFAULT 0,                  -- 结存金额（以本列为准）
    last_txn_at  TIMESTAMPTZ,                                       -- 最近一次出入库时间
    created_at   TIMESTAMPTZ   NOT NULL DEFAULT CURRENT_TIMESTAMP,
    updated_at   TIMESTAMPTZ   NOT NULL DEFAULT CURRENT_TIMESTAMP,

    CONSTRAINT ck_biz_inv_locked CHECK (locked_qty >= 0)
);

COMMENT ON TABLE  biz_inventory IS '仓库级即时库存（物料×仓库 唯一行）；成本与账面金额的唯一真相，库位明细见 biz_inventory_location';
COMMENT ON COLUMN biz_inventory.avg_cost IS '移动加权平均单价，粒度=物料×仓库（不按库位分裂，见架构 §2.6 决策）';
COMMENT ON COLUMN biz_inventory.qty IS '账面数量；serial_required=1 的物料必须等于 biz_material_serial 中 IN_STOCK 的件数';

-- 物料×仓库 唯一（含已删行口径：本表无 deleted_at，物理删表，行永不复用）
CREATE UNIQUE INDEX IF NOT EXISTS uk_biz_inv_mat_wh ON biz_inventory (material_id, warehouse_id);
CREATE INDEX        IF NOT EXISTS idx_biz_inv_warehouse ON biz_inventory (warehouse_id);
CREATE INDEX        IF NOT EXISTS idx_biz_inv_material  ON biz_inventory (material_id);


-- ---------- §1.5 `biz_inventory_txn` —— 库存流水（§2.3.4 原样，追加写）----------
CREATE TABLE IF NOT EXISTS biz_inventory_txn (
    id            UUID PRIMARY KEY DEFAULT gen_random_uuid(),       -- 主键
    material_id   UUID          NOT NULL,                           -- 物料（弱引用 biz_material.id）
    warehouse_id  UUID          NOT NULL,                           -- 仓库（弱引用 biz_warehouse.id）
    txn_type      VARCHAR(8)    NOT NULL,                           -- 方向：IN 入 / OUT 出
    biz_type      VARCHAR(24)   NOT NULL,                           -- 业务类型：PURCHASE_IN/…/TRANSFER_OUT 调拨出/TRANSFER_IN 调拨入
    doc_type      VARCHAR(24)   NOT NULL,                           -- 来源单据类型：STOCK_IN / STOCK_TRANSFER / …
    doc_id        UUID          NOT NULL,                           -- 来源单据 ID
    doc_no        VARCHAR(32)   NOT NULL,                           -- 来源单号（冗余，便于直接查）
    line_no       INT           NOT NULL DEFAULT 1,                 -- 来源单据行号
    qty           NUMERIC(18,4) NOT NULL,                           -- 本次数量（正数，方向看 txn_type）
    unit_cost     NUMERIC(18,6) NOT NULL DEFAULT 0,                 -- 本次单价
    amount        NUMERIC(18,4) NOT NULL DEFAULT 0,                 -- 本次金额（正数）
    before_qty    NUMERIC(18,4) NOT NULL DEFAULT 0,                 -- 变动前结存数量
    before_amount NUMERIC(18,4) NOT NULL DEFAULT 0,                 -- 变动前结存金额
    before_cost   NUMERIC(18,6) NOT NULL DEFAULT 0,                 -- 变动前移动均价
    after_qty     NUMERIC(18,4) NOT NULL DEFAULT 0,                 -- 变动后结存数量
    after_amount  NUMERIC(18,4) NOT NULL DEFAULT 0,                 -- 变动后结存金额
    after_cost    NUMERIC(18,6) NOT NULL DEFAULT 0,                 -- 变动后移动均价
    period_code   VARCHAR(7)    NOT NULL,                           -- 所属会计期间（yyyy-MM）
    operator_id   UUID,                                             -- 操作人
    remark        VARCHAR(255),                                     -- 备注
    created_at    TIMESTAMPTZ   NOT NULL DEFAULT CURRENT_TIMESTAMP, -- 发生时间（追加写）

    CONSTRAINT ck_biz_inv_txn_type CHECK (txn_type IN ('IN','OUT')),
    CONSTRAINT ck_biz_inv_txn_qty  CHECK (qty > 0),
    CONSTRAINT ck_biz_inv_txn_amt  CHECK (amount >= 0)
);

COMMENT ON TABLE  biz_inventory_txn IS '库存流水（追加写，成本追溯的唯一真相；纠错靠红冲不靠 UPDATE）';
COMMENT ON COLUMN biz_inventory_txn.biz_type IS '业务类型：PURCHASE_IN/PURCHASE_RETURN/SALE_OUT/PRODUCTION_IN/ADJUST_IN/ADJUST_OUT/TRANSFER_OUT/TRANSFER_IN（调拨两段：发运 TRANSFER_OUT 进在途仓，签收 TRANSFER_IN 出在途仓）';
COMMENT ON COLUMN biz_inventory_txn.before_qty IS '前后六个快照列是成本可审计性的硬要求，缺一列就无法反算移动加权是否正确';

CREATE INDEX IF NOT EXISTS idx_biz_inv_txn_mat  ON biz_inventory_txn (material_id, warehouse_id, created_at DESC);
CREATE INDEX IF NOT EXISTS idx_biz_inv_txn_doc  ON biz_inventory_txn (doc_type, doc_id);
CREATE INDEX IF NOT EXISTS idx_biz_inv_txn_no   ON biz_inventory_txn (doc_no);
CREATE INDEX IF NOT EXISTS idx_biz_inv_txn_per  ON biz_inventory_txn (period_code, material_id);
CREATE INDEX IF NOT EXISTS idx_biz_inv_txn_time ON biz_inventory_txn (created_at DESC);


-- ============================================================
-- §2 仓库树（`biz_warehouse` 加层级四列）
--
-- 表已在 §1.2 建好（含四列）。这里再做一遍 `ADD COLUMN IF NOT EXISTS`：
-- 目的是兼容「批次1 的 V0030 先跑、表已存在但没有层级列」的顺序（W0 POC 路径 B 实测过）。
-- 四列在 CREATE 里已内联，所以本段在这条路径上是纯 no-op。
-- ============================================================
ALTER TABLE biz_warehouse ADD COLUMN IF NOT EXISTS parent_id      UUID;
ALTER TABLE biz_warehouse ADD COLUMN IF NOT EXISTS level          SMALLINT    NOT NULL DEFAULT 1;
ALTER TABLE biz_warehouse ADD COLUMN IF NOT EXISTS warehouse_path VARCHAR(512);
ALTER TABLE biz_warehouse ADD COLUMN IF NOT EXISTS is_leaf        SMALLINT    NOT NULL DEFAULT 1;

-- 存量行回填：根节点路径 = `/自己id/`（与底座 sys_dept.dept_path 的 `/uuid/uuid/` 同款）
UPDATE biz_warehouse
   SET warehouse_path = '/' || id::text || '/'
 WHERE warehouse_path IS NULL;

-- 回填完再收紧 NOT NULL（若已是 NOT NULL，本句是 no-op）
ALTER TABLE biz_warehouse ALTER COLUMN warehouse_path SET NOT NULL;

COMMENT ON COLUMN biz_warehouse.parent_id      IS '上级仓库 ID（弱引用 biz_warehouse.id；顶级为 NULL）';
COMMENT ON COLUMN biz_warehouse.level          IS '层级：1 总仓 / 2 分仓 / 3 库房（上限 3，见架构 §2.5）';
COMMENT ON COLUMN biz_warehouse.warehouse_path IS '物化路径 `/uuid/uuid/`（与 sys_dept.dept_path 同技法）；子树查询 = warehouse_path LIKE ''/祖先id/%''';
COMMENT ON COLUMN biz_warehouse.is_leaf        IS '是否叶子：1 叶子（可挂库位/可挂库存）/ 0 非叶子（仅汇总）；由应用层维护，DB 无法跨表校验';

-- 约束：与 sys_dept 同为「应用层维护 + 巡检兜底」，DB 只拦本行自洽（不写触发器）
ALTER TABLE biz_warehouse DROP CONSTRAINT IF EXISTS ck_biz_warehouse_level;
ALTER TABLE biz_warehouse
    ADD CONSTRAINT ck_biz_warehouse_level CHECK (level BETWEEN 1 AND 3);

ALTER TABLE biz_warehouse DROP CONSTRAINT IF EXISTS ck_biz_warehouse_no_self;
ALTER TABLE biz_warehouse
    ADD CONSTRAINT ck_biz_warehouse_no_self CHECK (parent_id IS NULL OR parent_id <> id);

-- 顶级 ⟺ level=1：拦「level=2 却没有上级」这种孤儿行（两布尔均非 NULL，无三值逻辑风险）
ALTER TABLE biz_warehouse DROP CONSTRAINT IF EXISTS ck_biz_warehouse_root;
ALTER TABLE biz_warehouse
    ADD CONSTRAINT ck_biz_warehouse_root CHECK ((parent_id IS NULL) = (level = 1));

ALTER TABLE biz_warehouse DROP CONSTRAINT IF EXISTS ck_biz_warehouse_leaf;
ALTER TABLE biz_warehouse
    ADD CONSTRAINT ck_biz_warehouse_leaf CHECK (is_leaf IN (0, 1));

CREATE INDEX IF NOT EXISTS idx_biz_warehouse_parent ON biz_warehouse (parent_id);
-- ⚠ 两个 path 索引不是重复，各有分工（20 万行实测，见 W0 §4.1）：
--   · idx_biz_warehouse_path（默认 btree）：给 `ORDER BY warehouse_path` 整棵树排序与等值查用
--   · idx_biz_warehouse_path_pattern（text_pattern_ops）：给 `LIKE '/祖先/%'` 子树查用
-- 为什么必须第二个：非 C collation（本机实测 en_US.UTF-8）下默认 btree 在 LIKE 前缀上
-- 推不出 `~>=~ / ~<~` 边界，只能把整列吐给 heap 再 Filter（20 万行实测 15.4ms）；
-- 换成 text_pattern_ops 后 Index Cond 有边界，只取目标行（0.11ms，Index Only Scan）。
-- 底座 sys_dept 只建了默认 btree（`idx_sys_dept_path`）→ 同样的退化，但部门树行数小，
-- 实测无感知；业务侧这次按正确写法建，不改底座。
CREATE INDEX IF NOT EXISTS idx_biz_warehouse_path         ON biz_warehouse (warehouse_path);
CREATE INDEX IF NOT EXISTS idx_biz_warehouse_path_pattern ON biz_warehouse (warehouse_path text_pattern_ops);
CREATE INDEX IF NOT EXISTS idx_biz_warehouse_leaf         ON biz_warehouse (is_leaf, status);


-- ============================================================
-- §3 库位 `biz_location`
-- ============================================================
CREATE TABLE IF NOT EXISTS biz_location (
    id            UUID PRIMARY KEY DEFAULT gen_random_uuid(),        -- 主键
    location_code VARCHAR(40)  NOT NULL,                             -- 库位编码（如 A-01-03，仓库内唯一）
    location_name VARCHAR(120),                                      -- 库位名称（可选，如 A区一排三列）
    warehouse_id  UUID         NOT NULL,                             -- 所属仓库（弱引用 biz_warehouse.id，必须是叶子仓库）
    location_type VARCHAR(16)  NOT NULL DEFAULT 'STORAGE',           -- 类型：STORAGE 存储 / PICKING 拣货 / RECEIVING 收货 / SHIPPING 发货 / QUARANTINE 待检 / SCRAP 报废
    is_default    SMALLINT     NOT NULL DEFAULT 0,                   -- 是否该仓默认收货库位：1 是 / 0 否（每仓最多 1 个）
    capacity      NUMERIC(18,4),                                     -- 容量上限（可选；NULL = 不限）
    status        SMALLINT     NOT NULL DEFAULT 1,                   -- 状态：1 启用 / 0 停用（停用后不允许新上架，但存量可继续出）
    sort_order    INT          NOT NULL DEFAULT 0,                   -- 排序号
    remark        TEXT,                                              -- 备注
    created_at    TIMESTAMPTZ  NOT NULL DEFAULT CURRENT_TIMESTAMP,
    updated_at    TIMESTAMPTZ  NOT NULL DEFAULT CURRENT_TIMESTAMP,
    create_by     UUID,
    update_by     UUID,
    deleted_at    TIMESTAMPTZ,

    CONSTRAINT ck_biz_location_type CHECK (location_type IN ('STORAGE','PICKING','RECEIVING','SHIPPING','QUARANTINE','SCRAP')),
    CONSTRAINT ck_biz_location_def  CHECK (is_default IN (0, 1)),
    CONSTRAINT ck_biz_location_cap  CHECK (capacity IS NULL OR capacity >= 0)
);

COMMENT ON TABLE  biz_location IS '库位/货位；只能挂在叶子仓库下（应用层校验 + 巡检 SQL §5.8）';
COMMENT ON COLUMN biz_location.location_code IS '库位编码，**仓库内唯一**（A-01-03 在每个仓库都该这么叫，见架构 §2.7 反例）';
COMMENT ON COLUMN biz_location.warehouse_id  IS '所属仓库（弱引用 biz_warehouse.id）；必须是 is_leaf=1 的仓库';
COMMENT ON COLUMN biz_location.is_default    IS '该仓默认收货库位：1 是（每仓最多 1 个，部分唯一索引兜底）/ 0 否';
COMMENT ON COLUMN biz_location.capacity      IS '容量上限（同基础单位数量）；NULL 表示不限；不做自动超容拦截（批次2 上架策略）';

-- 库位编码：仓库内唯一（软删后可复用）
CREATE UNIQUE INDEX IF NOT EXISTS uk_biz_location_code ON biz_location (warehouse_id, location_code) WHERE deleted_at IS NULL;
-- 每仓最多一个默认库位（部分唯一索引：库侧兜底，不靠应用层自觉）
CREATE UNIQUE INDEX IF NOT EXISTS uk_biz_location_default ON biz_location (warehouse_id) WHERE is_default = 1 AND deleted_at IS NULL;
CREATE INDEX        IF NOT EXISTS idx_biz_location_wh     ON biz_location (warehouse_id, status);
CREATE INDEX        IF NOT EXISTS idx_biz_location_type   ON biz_location (warehouse_id, location_type);


-- ============================================================
-- §4 库位级库存 `biz_inventory_location`（两级账的明细层）
--
-- 与 `biz_inventory` 的关系：
--   biz_inventory           (material, warehouse)            ← 账面数量 + 移动加权成本 + 金额（唯一真相）
--   biz_inventory_location  (material, warehouse, location)  ← 分成数量（可加总回上表）
-- 不变式：Σ location.qty = inventory.qty（同事务双写；巡检 SQL §5.6 兜底）
-- ============================================================
CREATE TABLE IF NOT EXISTS biz_inventory_location (
    id           UUID PRIMARY KEY DEFAULT gen_random_uuid(),        -- 主键
    material_id  UUID          NOT NULL,                            -- 物料（弱引用 biz_material.id）
    warehouse_id UUID          NOT NULL,                            -- 仓库（弱引用 biz_warehouse.id）
    location_id  UUID,                                              -- 库位（弱引用 biz_location.id；NULL = 尚未分配库位）
    qty          NUMERIC(18,4) NOT NULL DEFAULT 0,                  -- 该库位结存数量
    locked_qty   NUMERIC(18,4) NOT NULL DEFAULT 0,                  -- 该库位锁定量（预留：批次2 拣货占用）
    last_txn_at  TIMESTAMPTZ,                                       -- 最近一次变动时间
    created_at   TIMESTAMPTZ   NOT NULL DEFAULT CURRENT_TIMESTAMP,
    updated_at   TIMESTAMPTZ   NOT NULL DEFAULT CURRENT_TIMESTAMP,

    CONSTRAINT ck_biz_inv_loc_locked CHECK (locked_qty >= 0)
);

COMMENT ON TABLE  biz_inventory_location IS '库位级库存明细（物料×仓库×库位）；只记数量不记成本，加总必须等于 biz_inventory.qty';
COMMENT ON COLUMN biz_inventory_location.location_id IS '库位；NULL = 未分配库位（收货待上架/整仓管理）。唯一键用 NULLS NOT DISTINCT，NULL 也参与唯一性判定，避免"同物料同仓多条 NULL 行"';

-- 库位维度唯一：**NULLS NOT DISTINCT**（PG15+，本机 18.6 实测生效）。
-- 不加这一句的话，PG 默认 "NULL 彼此不相等" ⇒ 同一物料同一仓库可以插入 N 条 location_id IS NULL 的行，
-- 汇总时数量翻倍且没有任何报错 —— 这是两级账最阴的一个坑（自检块反例 ⑦ 会验证）。
CREATE UNIQUE INDEX IF NOT EXISTS uk_biz_inv_loc
    ON biz_inventory_location (material_id, warehouse_id, location_id) NULLS NOT DISTINCT;
CREATE INDEX IF NOT EXISTS idx_biz_inv_loc_wh  ON biz_inventory_location (warehouse_id, location_id);
CREATE INDEX IF NOT EXISTS idx_biz_inv_loc_mat ON biz_inventory_location (material_id);


-- ============================================================
-- §5 种子数据（幂等；按业务编码判重，绝不覆盖用户已改的行）
--
-- 覆盖卡面要求：3 计量单位 / 2 仓库（总仓+分仓，验证 parent_id/level/path）
--               / 4 库位（挂分仓下）/ 5 物料（其中 1 个一物一码）
-- 另补：2 会计科目（biz_material.inventory_account_id 的落点）、1 供应商、1 会计期间、1 取号规则(PO)
-- 明确不种：采购订单主/明细（单据是业务数据，不该由迁移伪造；表结构已就绪）
--
-- ⚠ `serial_required` 属 **V0138**（W0 §2.5：`biz_material.serial_required` 在 V0138 加），
--   本迁移**不预先建该列**（不抢别的卡的结构决策）；种子里用 DO 块按「列是否存在」条件标记，
--   于是「只跑 V0137」与「V0137+V0138」两种顺序都正确。
-- ============================================================

-- ---------- §5.1 计量单位（3 个：PCS / KG / BOX）----------
INSERT INTO biz_unit (id, unit_code, unit_name, is_base, base_unit_id, rate, status, sort_order)
SELECT v.id::uuid, v.unit_code, v.unit_name, v.is_base, v.base_unit_id::uuid, v.rate, 1, v.sort_order
  FROM (VALUES
        ('d1370000-0000-4000-8000-000000000001', 'PCS', '个',   1, NULL,                                   1.0,  1),
        ('d1370000-0000-4000-8000-000000000002', 'KG',  '千克', 1, NULL,                                   1.0,  2),
        ('d1370000-0000-4000-8000-000000000003', 'BOX', '箱',   0, 'd1370000-0000-4000-8000-000000000001', 12.0, 3)
       ) AS v(id, unit_code, unit_name, is_base, base_unit_id, rate, sort_order)
 WHERE NOT EXISTS (SELECT 1 FROM biz_unit u WHERE u.unit_code = v.unit_code);

-- ---------- §5.2 会计科目（2 个：1405 库存商品 / 2202 应付账款）----------
INSERT INTO biz_account (id, account_code, account_name, level, account_category, balance_direction, is_leaf, status)
SELECT v.id::uuid, v.account_code, v.account_name, 1, v.account_category, v.balance_direction, 1, 1
  FROM (VALUES
        ('d1370004-0000-4000-8000-000000000001', '1405', '库存商品',  'ASSET',     'DEBIT'),
        ('d1370004-0000-4000-8000-000000000002', '2202', '应付账款',  'LIABILITY', 'CREDIT')
       ) AS v(id, account_code, account_name, account_category, balance_direction)
 WHERE NOT EXISTS (SELECT 1 FROM biz_account a WHERE a.account_code = v.account_code);

-- ---------- §5.3 供应商（1 个）----------
INSERT INTO biz_supplier (id, supplier_code, supplier_name, short_name, credit_days, status)
SELECT v.id::uuid, v.supplier_code, v.supplier_name, v.short_name, 30, 1
  FROM (VALUES
        ('d1370005-0000-4000-8000-000000000001', 'SUP-0001', '示例供应商（种子）', '示例供应商')
       ) AS v(id, supplier_code, supplier_name, short_name)
 WHERE NOT EXISTS (SELECT 1 FROM biz_supplier s WHERE s.supplier_code = v.supplier_code);

-- ---------- §5.4 会计期间（1 个：2026-10 已开启）----------
INSERT INTO biz_accounting_period (id, period_year, period_month, period_code, start_date, end_date, status)
SELECT v.id::uuid, 2026, 10, '2026-10', DATE '2026-10-01', DATE '2026-10-31', 'OPEN'
  FROM (VALUES ('d1370007-0000-4000-8000-000000000001')) AS v(id)
 WHERE NOT EXISTS (SELECT 1 FROM biz_accounting_period p WHERE p.period_code = '2026-10');

-- ---------- §5.5 取号规则（1 条：PO 采购订单号）----------
INSERT INTO biz_seq (id, seq_type, seq_name, prefix, date_pattern, period_key, current_no, step, separator, status)
SELECT v.id::uuid, 'PO', '采购订单号', 'PO', 'yyyyMMdd', '', 0, 1, '-', 1
  FROM (VALUES ('d1370006-0000-4000-8000-000000000001')) AS v(id)
 WHERE NOT EXISTS (SELECT 1 FROM biz_seq q WHERE q.seq_type = 'PO' AND q.deleted_at IS NULL);

-- ---------- §5.6 仓库（2 个：总仓 WH01 → 分仓 WH02）----------
-- 总仓：level=1 / parent_id NULL / warehouse_path='/自己id/' / is_leaf=0（有下级，不可挂库位）
INSERT INTO biz_warehouse (id, warehouse_code, warehouse_name, warehouse_type, address, status, sort_order,
                           parent_id, level, warehouse_path, is_leaf)
SELECT v.id::uuid, v.warehouse_code, v.warehouse_name, v.warehouse_type, v.address, 1, 1,
       NULL, 1, '/' || v.id || '/', 0
  FROM (VALUES
        ('d1370001-0000-4000-8000-000000000001', 'WH01', '总仓（种子）', 'NORMAL', '北京市')
       ) AS v(id, warehouse_code, warehouse_name, warehouse_type, address)
 WHERE NOT EXISTS (SELECT 1 FROM biz_warehouse w WHERE w.warehouse_code = v.warehouse_code);

-- 分仓：level=2 / parent_id=总仓 / warehouse_path='/总仓id/分仓id/' / is_leaf=1（叶子，可挂库位）
INSERT INTO biz_warehouse (id, warehouse_code, warehouse_name, warehouse_type, address, status, sort_order,
                           parent_id, level, warehouse_path, is_leaf)
SELECT v.id::uuid, v.warehouse_code, v.warehouse_name, v.warehouse_type, v.address, 1, 2,
       'd1370001-0000-4000-8000-000000000001', 2,
       '/d1370001-0000-4000-8000-000000000001/' || v.id || '/', 1
  FROM (VALUES
        ('d1370001-0000-4000-8000-000000000002', 'WH02', '分仓（种子）', 'NORMAL', '北京市')
       ) AS v(id, warehouse_code, warehouse_name, warehouse_type, address)
 WHERE NOT EXISTS (SELECT 1 FROM biz_warehouse w WHERE w.warehouse_code = v.warehouse_code)
   AND EXISTS (SELECT 1 FROM biz_warehouse p WHERE p.id = 'd1370001-0000-4000-8000-000000000001');

-- ---------- §5.7 库位（4 个，全挂分仓 WH02）----------
INSERT INTO biz_location (id, location_code, location_name, warehouse_id, location_type, is_default, status, sort_order)
SELECT v.id::uuid, v.location_code, v.location_name,
       'd1370001-0000-4000-8000-000000000002', v.location_type, v.is_default, 1, v.sort_order
  FROM (VALUES
        ('d1370002-0000-4000-8000-000000000001', 'A-01-01', 'A区一排一列', 'STORAGE',   1, 1),
        ('d1370002-0000-4000-8000-000000000002', 'A-01-02', 'A区一排二列', 'STORAGE',   0, 2),
        ('d1370002-0000-4000-8000-000000000003', 'B-01-01', 'B区一排一列', 'PICKING',   0, 3),
        ('d1370002-0000-4000-8000-000000000004', 'RECV-01', '收货暂存区',   'RECEIVING', 0, 4)
       ) AS v(id, location_code, location_name, location_type, is_default, sort_order)
 WHERE NOT EXISTS (SELECT 1 FROM biz_location l
                    WHERE l.warehouse_id = 'd1370001-0000-4000-8000-000000000002'
                      AND l.location_code = v.location_code AND l.deleted_at IS NULL)
   AND EXISTS (SELECT 1 FROM biz_warehouse w WHERE w.id = 'd1370001-0000-4000-8000-000000000002');

-- ---------- §5.8 物料（5 个；按 unit_code 解析 unit_id，避免写死单位 UUID）----------
INSERT INTO biz_material (id, material_code, material_name, spec, unit_id, material_type,
                          default_warehouse_id, safety_stock, inventory_account_id, tax_rate, status)
SELECT v.id::uuid, v.material_code, v.material_name, v.spec,
       (SELECT u.id FROM biz_unit u WHERE u.unit_code = v.unit_code AND u.deleted_at IS NULL),
       v.material_type,
       'd1370001-0000-4000-8000-000000000002',
       v.safety_stock, 'd1370004-0000-4000-8000-000000000001', 0.13, 1
  FROM (VALUES
        ('d1370003-0000-4000-8000-000000000001', 'M-W1A-001', '原料A（种子）',   '25kg/袋', 'KG',  'RAW',      10.0),
        ('d1370003-0000-4000-8000-000000000002', 'M-W1A-002', '原料B（种子）',   '1L/瓶',   'PCS', 'RAW',      20.0),
        ('d1370003-0000-4000-8000-000000000003', 'M-W1A-003', '半成品C（种子）', '标准件',   'PCS', 'SEMI',      5.0),
        ('d1370003-0000-4000-8000-000000000004', 'M-W1A-004', '成品D（种子）',   '整机',     'PCS', 'FINISHED',  0.0),
        ('d1370003-0000-4000-8000-000000000005', 'M-W1A-005', '成品E（种子）',   '整机',     'PCS', 'FINISHED',  0.0)
       ) AS v(id, material_code, material_name, spec, unit_code, material_type, safety_stock)
 WHERE NOT EXISTS (SELECT 1 FROM biz_material m WHERE m.material_code = v.material_code)
   AND EXISTS (SELECT 1 FROM biz_unit u WHERE u.unit_code = v.unit_code AND u.deleted_at IS NULL);

-- ---------- §5.9 一物一码标记（依赖 V0138 加的列；列不存在则跳过并 NOTICE）----------
DO $$
BEGIN
    IF EXISTS (SELECT 1 FROM information_schema.columns
                WHERE table_schema = 'public' AND table_name = 'biz_material'
                  AND column_name = 'serial_required') THEN
        UPDATE biz_material SET serial_required = 1
         WHERE material_code = 'M-W1A-004' AND serial_required <> 1;
        RAISE NOTICE 'V0137 种子：serial_required 列存在（V0138 已应用），M-W1A-004 已标记为一物一码';
    ELSE
        RAISE NOTICE 'V0137 种子：serial_required 列尚未创建（属 V0138），跳过一物一码标记';
    END IF;
END
$$;


-- ============================================================
-- §6 自检（空库必须也能过：首次部署时一张业务数据都没有）
--    只断言「结构」与「约束真的会拦」，不依赖库里已有数据。
-- ============================================================
DO $$
DECLARE
    n_tbl   INT;
    n_col   INT;
    n_uk    INT;
    n_seed  INT;
    blocked BOOLEAN;
    v_id    UUID;
BEGIN
    -- ① 13 张表齐了
    SELECT count(*) INTO n_tbl
      FROM pg_class c JOIN pg_namespace ns ON ns.oid = c.relnamespace
     WHERE ns.nspname = 'public' AND c.relkind = 'r'
       AND c.relname IN ('biz_unit','biz_account','biz_supplier','biz_accounting_period',
                         'biz_purchase_order','biz_purchase_order_item',
                         'biz_seq','biz_warehouse','biz_material','biz_inventory','biz_inventory_txn',
                         'biz_location','biz_inventory_location');
    IF n_tbl <> 13 THEN
        RAISE EXCEPTION 'V0137 自检失败：期望 13 张表，实际 %', n_tbl;
    END IF;

    -- ② 仓库树四列都在，且 warehouse_path 已收紧为 NOT NULL
    SELECT count(*) INTO n_col
      FROM information_schema.columns
     WHERE table_schema = 'public' AND table_name = 'biz_warehouse'
       AND column_name IN ('parent_id','level','warehouse_path','is_leaf')
       AND (column_name <> 'warehouse_path' OR is_nullable = 'NO');
    IF n_col <> 4 THEN
        RAISE EXCEPTION 'V0137 自检失败：biz_warehouse 树形列不全或 warehouse_path 仍可空（% / 4）', n_col;
    END IF;

    -- ③ 关键索引在位（路径 LIKE 前缀 / 库位默认位 / 库位库存 NULLS NOT DISTINCT / 物料×仓库 / 主数据编码）
    SELECT count(*) INTO n_uk
      FROM pg_indexes
     WHERE schemaname = 'public'
       AND indexname IN ('idx_biz_warehouse_path_pattern','uk_biz_location_default','uk_biz_inv_loc',
                         'uk_biz_inv_mat_wh','uk_biz_unit_code','uk_biz_material_code','uk_biz_po_no');
    IF n_uk <> 7 THEN
        RAISE EXCEPTION 'V0137 自检失败：关键索引缺失（% / 7）', n_uk;
    END IF;

    -- ③.5 路径索引的 opclass 真的是 text_pattern_ops（不只看索引名）
    SELECT count(*) INTO n_uk
      FROM pg_index i
      JOIN pg_class ic ON ic.oid = i.indexrelid
      JOIN pg_opclass oc ON oc.oid = i.indclass[0]
     WHERE ic.relname = 'idx_biz_warehouse_path_pattern' AND oc.opcname = 'text_pattern_ops';
    IF n_uk <> 1 THEN
        RAISE EXCEPTION 'V0137 自检失败：idx_biz_warehouse_path_pattern 不是 text_pattern_ops（% / 1）', n_uk;
    END IF;

    -- ④ 反例 1：level=2 但没有上级 → 必须被 ck_biz_warehouse_root 拦住
    blocked := FALSE;
    BEGIN
        INSERT INTO biz_warehouse (warehouse_code, warehouse_name, level, warehouse_path)
        VALUES ('__W1A_ORPHAN__', '自检-孤儿分仓', 2, '/x/');
    EXCEPTION WHEN check_violation THEN
        blocked := TRUE;
    END;
    IF NOT blocked THEN
        RAISE EXCEPTION 'V0137 自检失败：level=2 且 parent_id 为空的孤儿行被放进库（ck_biz_warehouse_root 失效）';
    END IF;

    -- ④ 反例 2：辅助单位不指明基准单位 → 必须被 ck_biz_unit_base 拦住
    blocked := FALSE;
    BEGIN
        INSERT INTO biz_unit (unit_code, unit_name, is_base, base_unit_id)
        VALUES ('__W1A_UNIT__', '自检-坏单位', 0, NULL);
    EXCEPTION WHEN check_violation THEN
        blocked := TRUE;
    END;
    IF NOT blocked THEN
        RAISE EXCEPTION 'V0137 自检失败：is_base=0 却不带 base_unit_id 的单位被放进库（ck_biz_unit_base 失效）';
    END IF;

    -- ④ 反例 3：价税合计不等于「不含税 + 税」→ 必须被 ck_biz_po_tax_sum 拦住（会计兜底）
    blocked := FALSE;
    BEGIN
        INSERT INTO biz_purchase_order (order_no, supplier_id, order_date, warehouse_id,
                                        total_amount, total_tax, total_with_tax)
        VALUES ('__W1A_PO__', gen_random_uuid(), CURRENT_DATE, gen_random_uuid(), 100, 13, 999);
    EXCEPTION WHEN check_violation THEN
        blocked := TRUE;
    END;
    IF NOT blocked THEN
        RAISE EXCEPTION 'V0137 自检失败：价税合计不自洽的采购单被放进库（ck_biz_po_tax_sum 失效）';
    END IF;

    -- ⑤ 反例 4：同一仓库两个默认库位 → 必须被 uk_biz_location_default 拦住
    INSERT INTO biz_warehouse (id, warehouse_code, warehouse_name, level, warehouse_path, is_leaf)
    VALUES ('00000000-0000-0000-0000-00000000d137', '__W1A_ST_WH__', '自检-库位仓', 1, '/st/', 1)
    ON CONFLICT (id) DO NOTHING;

    INSERT INTO biz_location (id, location_code, warehouse_id, is_default)
    VALUES ('00000000-0000-0000-0000-00000000d138', '__W1A_ST_L1__', '00000000-0000-0000-0000-00000000d137', 1)
    ON CONFLICT (id) DO NOTHING;

    blocked := FALSE;
    BEGIN
        INSERT INTO biz_location (location_code, warehouse_id, is_default)
        VALUES ('__W1A_ST_L2__', '00000000-0000-0000-0000-00000000d137', 1);
    EXCEPTION WHEN unique_violation THEN
        blocked := TRUE;
    END;
    IF NOT blocked THEN
        RAISE EXCEPTION 'V0137 自检失败：同一仓库出现第二个默认库位（uk_biz_location_default 失效）';
    END IF;

    -- ⑥ 反例 5：库位库存的 NULL 库位行不能重复（NULLS NOT DISTINCT 是否真的生效）
    INSERT INTO biz_inventory_location (material_id, warehouse_id, location_id, qty)
    VALUES ('00000000-0000-0000-0000-00000000d139', '00000000-0000-0000-0000-00000000d137', NULL, 1)
    ON CONFLICT DO NOTHING;

    blocked := FALSE;
    BEGIN
        INSERT INTO biz_inventory_location (material_id, warehouse_id, location_id, qty)
        VALUES ('00000000-0000-0000-0000-00000000d139', '00000000-0000-0000-0000-00000000d137', NULL, 9);
    EXCEPTION WHEN unique_violation THEN
        blocked := TRUE;
    END;
    IF NOT blocked THEN
        RAISE EXCEPTION 'V0137 自检失败：同物料同仓的 location_id IS NULL 行可重复插入（NULLS NOT DISTINCT 未生效）';
    END IF;

    -- ⑦ 正例：level=1 根仓能入库、路径可回填
    INSERT INTO biz_warehouse (warehouse_code, warehouse_name, level, warehouse_path)
    VALUES ('__W1A_ROOT__', '自检-根仓', 1, '/x/')
    RETURNING id INTO v_id;
    DELETE FROM biz_warehouse WHERE id = v_id;

    -- ⑦ 正例：叶子仓库下能挂库位
    INSERT INTO biz_location (location_code, warehouse_id)
    VALUES ('__W1A_ST_L3__', '00000000-0000-0000-0000-00000000d137');
    DELETE FROM biz_location
     WHERE warehouse_id = '00000000-0000-0000-0000-00000000d137' AND location_code = '__W1A_ST_L3__';

    -- 清场（自检数据一行不留）
    DELETE FROM biz_inventory_location WHERE material_id = '00000000-0000-0000-0000-00000000d139';
    DELETE FROM biz_location           WHERE warehouse_id = '00000000-0000-0000-0000-00000000d137';
    DELETE FROM biz_warehouse          WHERE id = '00000000-0000-0000-0000-00000000d137';
    DELETE FROM biz_purchase_order     WHERE order_no = '__W1A_PO__';
    DELETE FROM biz_unit               WHERE unit_code = '__W1A_UNIT__';
    DELETE FROM biz_warehouse          WHERE warehouse_code = '__W1A_ORPHAN__';

    -- ⑧ 种子到位（不阻断，只播报；若用户自己已建同名编码则本来就不该是我们插的）
    SELECT count(*) INTO n_seed FROM biz_unit WHERE unit_code IN ('PCS','KG','BOX') AND deleted_at IS NULL;
    RAISE NOTICE 'V0137 种子核对：计量单位 % / 3', n_seed;
    SELECT count(*) INTO n_seed FROM biz_warehouse WHERE warehouse_code IN ('WH01','WH02') AND deleted_at IS NULL;
    RAISE NOTICE 'V0137 种子核对：仓库 % / 2（WH01 总仓 → WH02 分仓）', n_seed;
    SELECT count(*) INTO n_seed FROM biz_location
     WHERE location_code IN ('A-01-01','A-01-02','B-01-01','RECV-01') AND deleted_at IS NULL;
    RAISE NOTICE 'V0137 种子核对：库位 % / 4', n_seed;
    SELECT count(*) INTO n_seed FROM biz_material WHERE material_code LIKE 'M-W1A-00_' AND deleted_at IS NULL;
    RAISE NOTICE 'V0137 种子核对：物料 % / 5', n_seed;

    RAISE NOTICE 'V0137 自检通过：13 表 / 树形 4 列 / 7 个关键索引（含 text_pattern_ops）/ 5 条反例拦截 / 2 条正例入库 / 自检数据已清场';
END
$$;

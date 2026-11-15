-- ============================================================
-- V0145：采购订单补金额口径字段 + 收货入库两表（P2-A1 / TSK-202610-000853）
--
-- 依据：
--   ① `docs/ERP-批次1-技术架构.md` §2.2 采购订单（448-530 行）、§2.3 收货入库与库存（532-686 行）
--      —— 卡面明确写「架构文档为准」，DDL 结构照抄；旧规划号 V0031/V0032 不复用（生产已到 V0144）
--   ② `docs/ERP-批次1-产品需求PRD.md` §3.1 金额口径（296-380 行）：price_excl / price_incl /
--      amount_excl / tax_amount / amount_incl / tax_rate 的概念名与恒等式
--   ③ 本卡卡面（P2-A1）：要补的字段清单、CHECK 清单、索引清单、验收六场景
--
-- ── 开工实测的现状（psql \d biz_purchase_order / biz_purchase_order_item，2026-10-31）──
--   `biz_purchase_order` 已有：order_no / supplier_id / order_date / status（7 态 CHECK）/
--       total_qty / total_amount / total_tax / total_with_tax / credit_days / 审计五件套
--   `biz_purchase_order_item` 已有：order_id / line_no / material_id / unit_id / qty(18,4) /
--       price(18,6) / tax_rate(7,4) / amount / tax_amount / with_tax_amount / received_qty
--   → 所以本迁移**只补真正缺的列**，不重建表（硬红线 2）。
--
-- ── 与卡面的差异（逐条，别当 bug；卡面字段名是 PRD 概念名的意译）────────────────
--   ① **金额列名沿用架构 §2.2 的既有列名，不新建 amount_excl / tax_amount / amount_incl**：
--        卡面概念名      → 实际列（架构 §2.2 = 库内现状）
--        amount_excl     → total_amount      （不含税金额合计）
--        tax_amount      → total_tax         （税额合计）
--        amount_incl     → total_with_tax    （价税合计）
--        明细 amount_excl → amount；tax_amount → tax_amount；amount_incl → with_tax_amount
--        明细 price_excl  → price
--      理由：同一个数建两列 = 两个真相源，必然有一天对不上；架构 §2.2 的 DDL 就是这么起的名字，
--      卡面自己也写「架构文档为准」。卡面红线 3 里那句「金额是 amount_excl/tax_amount/amount_incl」
--      按 PRD §3.1.2 的**概念名**理解，落地列名以架构为准。下游 P2-A2 起请用现有列名。
--   ② **三种金额恒等用精确相等，不加 0.01 容差**：卡面写 `abs(amount_incl-(amount_excl+tax_amount))<0.01`，
--      但架构 §2.2 的设计是「税额一律由 价税合计 − 不含税金额 倒挤」（POC-4 实证行级四舍五入产生 ±0.01
--      尾差，所以用减法而不是再乘一次税率），因此恒等式在库内必须**精确成立**；现有
--      `ck_biz_po_tax_sum CHECK (total_with_tax = total_amount + total_tax)` 与
--      `ck_biz_po_item_tax CHECK (with_tax_amount = amount + tax_amount)` 已是精确相等，比 0.01 容差更严，
--      本迁移沿用（新建的收货两表同样用精确相等）。容差式 CHECK 会放过「明显错值」以外的全部错账。
--   ③ `biz_purchase_order` 只补 `price_mode`（口径开关）。卡面列的 supplier_id / order_date / status
--      及三种金额全部已存在；卡面写 `price_mode VARCHAR(8) EXCL/INCL`，而 PRD §3.1.3 叫 `tax_mode`
--      （2=不含税 / 1=含税）。**取卡面的 `price_mode` 文本枚举**：VARCHAR 自解释、前端徽标直接映射，
--      且库内还没有这一列（无历史包袱）。差异已写入卡面留言。
--   ④ `biz_purchase_order_item` 补 `price_incl`（含税单价，PRD §3.1.2 要求 6 位）与 `tax_rate` 区间 CHECK
--      （现有 `tax_rate NUMERIC(7,4)` 没有任何区间约束，任何值都能写进去）。
--      PRD §3.1.2 说 price_incl「由 price_excl ×(1+税率) 派生，不单独存权威值」——但口径 B（含税单价录入）
--      时用户键入的正是含税单价，不存就无法复现/回显同样的输入；故存为快照，权威值仍是 price。
--   ⑤ **收货两表按架构 §2.3 的列名建**（`in_no` / `in_type` / `source_order_id` / `source_item_id` /
--      `total_amount` …），卡面写的 `stock_in_no` / `purchase_order_id` / `total_amount_excl` 是意译：
--        卡面名                   → 实际列（架构 §2.3）
--        stock_in_no              → in_no
--        purchase_order_id        → source_order_id
--        purchase_order_item_id   → source_item_id
--        total_amount_excl        → total_amount
--        total_tax_amount         → total_tax
--        total_amount_incl        → total_with_tax
--        price_excl / price_incl  → price / price_incl
--        amount_excl / amount_incl → amount / with_tax_amount
--      索引名同理按架构：卡面 `uk_stock_in_no` → `uk_biz_si_no`、`idx_stock_in_po` → `idx_biz_si_source`、
--      `idx_stock_in_status` → `idx_biz_si_status`、`idx_stock_in_item_si` → `idx_biz_si_item_in`。
--   ⑥ 两张表都加了架构 DDL **没有、但卡面/PRD 要求**的列：
--        `biz_stock_in.invoice_received BOOLEAN NOT NULL DEFAULT TRUE` —— PRD §3.3.2「发票已随货到达」
--           勾选框（本批只有票货同行一条路径，默认勾选且不可不选；P1 暂估分支届时写 FALSE）。
--        `biz_stock_in_item.location_id` / `lot_no` —— 卡面「库位可空 / 批号可空」；V0139 起库存有库位维度，
--           `location_id` 沿用 `→ biz_inventory_location(id)` 口径（弱引用，不建外键）。
--   ⑦ 精度沿用架构：金额 NUMERIC(18,4)、单价 NUMERIC(18,6)、税率 NUMERIC(7,4)。
--      PRD §3.1.2 要求金额 2 位 —— (18,4) 是超集，2 位口径由应用层舍入保证（架构 §2.2 同样的取舍）。
--   ⑧ 外键口径：**只有** `biz_stock_in_item.stock_in_id → biz_stock_in(id) ON DELETE CASCADE` 是硬外键
--      （架构 §2.3.2 明文）。`supplier_id` / `warehouse_id` / `source_order_id` / `source_item_id` /
--      `material_id` / `unit_id` / `location_id` 全部**弱引用不建外键**，与同链的 `biz_purchase_order` 一致
--      （实测它也没有任何外键），避免主数据调整时把历史单据卡死。
--      `voucher_id` / `payable_id` 是过账时才回填的弱引用（卡面「弱引用，不建外键」）。
--
-- ── 不做什么（红线）───────────────────────────────────────────────────────
--   只建表 / 补列 / 加约束 / 建索引。**不写**业务代码、触发器、视图、取号规则（`biz_seq` 的 SI 规则归 P2-A2）。
--   不改 V0137–V0144（已应用，改 checksum 会让启动 fail-closed）。
--
-- 回滚：`V0145__purchase_order_amounts_and_stock_in.down.sql`
--       （先 DROP stock_in_item → stock_in，再删采购两表的新增列/约束）
--
-- 幂等：CREATE TABLE IF NOT EXISTS / ADD COLUMN IF NOT EXISTS / CREATE INDEX IF NOT EXISTS /
--       ADD CONSTRAINT 走 pg_constraint 存在性判断（PG 的 ADD CONSTRAINT 没有 IF NOT EXISTS）。
--
-- ⚠ 文件内**不写** `BEGIN; … COMMIT;` —— 迁移器自己在事务里执行本文件。
-- ============================================================

-- ============================================================
-- §1 `biz_purchase_order` —— 补录入口径开关
-- ============================================================
-- price_mode：单据级录入口径。EXCL=按不含税单价录入（PRD §3.1.3 口径 A，默认）/
--             INCL=按含税单价录入（口径 B）。切换口径时按各行税率重算金额，故它不属于明细行。
ALTER TABLE biz_purchase_order
    ADD COLUMN IF NOT EXISTS price_mode VARCHAR(8) NOT NULL DEFAULT 'EXCL';

DO $$
BEGIN
    IF NOT EXISTS (
        SELECT 1 FROM pg_constraint
         WHERE conname = 'ck_biz_po_price_mode'
           AND conrelid = 'public.biz_purchase_order'::regclass
    ) THEN
        ALTER TABLE biz_purchase_order
            ADD CONSTRAINT ck_biz_po_price_mode CHECK (price_mode IN ('EXCL', 'INCL'));
    END IF;
END
$$;

-- ============================================================
-- §2 `biz_purchase_order_item` —— 补含税单价 + 税率区间
-- ============================================================
ALTER TABLE biz_purchase_order_item
    ADD COLUMN IF NOT EXISTS price_incl NUMERIC(18,6) NOT NULL DEFAULT 0;

DO $$
BEGIN
    IF NOT EXISTS (
        SELECT 1 FROM pg_constraint
         WHERE conname = 'ck_biz_po_item_tax_rate'
           AND conrelid = 'public.biz_purchase_order_item'::regclass
    ) THEN
        -- PRD §3.1.2：税率 0 / 1 / 3 / 6 / 9 / 13，上限 100（%）
        ALTER TABLE biz_purchase_order_item
            ADD CONSTRAINT ck_biz_po_item_tax_rate CHECK (tax_rate >= 0 AND tax_rate <= 100);
    END IF;
END
$$;

-- ============================================================
-- §3 `biz_stock_in` —— 收货入库单主表（架构 §2.3.1）
-- ============================================================
CREATE TABLE IF NOT EXISTS biz_stock_in (
    id               UUID PRIMARY KEY DEFAULT gen_random_uuid(),      -- 主键
    in_no            VARCHAR(32)   NOT NULL,                          -- 入库单号（IN 前缀，软删后可复用）
    in_type          VARCHAR(20)   NOT NULL DEFAULT 'PURCHASE',       -- PURCHASE 采购入库 / RETURN 退货入库（批次 2）/ OTHER 其他
    source_order_id  UUID,                                            -- 来源采购订单（弱引用 biz_purchase_order.id，可空=无源入库）
    supplier_id      UUID,                                            -- 供应商（弱引用 biz_supplier.id，采购入库必填）
    warehouse_id     UUID          NOT NULL,                          -- 收货仓库（弱引用 biz_warehouse.id）
    in_date          DATE          NOT NULL,                          -- 入库日期（决定所属会计期间）
    status           VARCHAR(24)   NOT NULL DEFAULT 'DRAFT',          -- DRAFT / POSTED / CANCELLED
    currency_code    CHAR(3)       NOT NULL DEFAULT 'CNY',            -- 币种（ISO 4217）
    total_qty        NUMERIC(18,4) NOT NULL DEFAULT 0,                -- 数量合计
    total_amount     NUMERIC(18,4) NOT NULL DEFAULT 0,                -- 不含税金额合计（= 入库成本，PRD 的 amount_excl）
    total_tax        NUMERIC(18,4) NOT NULL DEFAULT 0,                -- 税额合计（PRD 的 tax_amount）
    total_with_tax   NUMERIC(18,4) NOT NULL DEFAULT 0,                -- 价税合计（PRD 的 amount_incl）
    invoice_received BOOLEAN       NOT NULL DEFAULT TRUE,             -- 「发票已随货到达」（PRD §3.3.2；FALSE=暂估，P1）
    voucher_id       UUID,                                            -- 过账生成的凭证（弱引用 biz_voucher.id，冲销后指向红字凭证）
    payable_id       UUID,                                            -- 过账生成的应付单（弱引用 biz_payable.id）
    remark           TEXT,                                            -- 备注
    posted_at        TIMESTAMPTZ,                                     -- 过账时间
    posted_by        UUID,                                            -- 过账人
    cancelled_at     TIMESTAMPTZ,                                     -- 作废时间
    cancelled_by     UUID,                                            -- 作废人
    cancel_reason    VARCHAR(255),                                    -- 作废原因
    created_at       TIMESTAMPTZ   NOT NULL DEFAULT CURRENT_TIMESTAMP,
    updated_at       TIMESTAMPTZ   NOT NULL DEFAULT CURRENT_TIMESTAMP,
    create_by        UUID,
    update_by        UUID,
    deleted_at       TIMESTAMPTZ,

    CONSTRAINT ck_biz_si_status CHECK (status IN ('DRAFT', 'POSTED', 'CANCELLED')),
    CONSTRAINT ck_biz_si_type   CHECK (in_type IN ('PURCHASE', 'RETURN', 'OTHER')),
    -- 价税合计恒等（精确相等，见头注释 ②）
    CONSTRAINT ck_biz_si_tax    CHECK (total_with_tax = total_amount + total_tax),
    CONSTRAINT ck_biz_si_qty    CHECK (total_qty >= 0 AND total_amount >= 0 AND total_tax >= 0 AND total_with_tax >= 0),
    -- 采购入库必须有供应商，否则应付与凭证无法生成
    CONSTRAINT ck_biz_si_supplier CHECK (in_type <> 'PURCHASE' OR supplier_id IS NOT NULL)
);

-- 幂等补齐约束（表已存在但缺约束时；上面 CREATE TABLE 新建的情况本条自动跳过）
DO $$
BEGIN
    IF NOT EXISTS (SELECT 1 FROM pg_constraint
                    WHERE conname = 'ck_biz_si_status' AND conrelid = 'public.biz_stock_in'::regclass) THEN
        ALTER TABLE biz_stock_in ADD CONSTRAINT ck_biz_si_status
            CHECK (status IN ('DRAFT', 'POSTED', 'CANCELLED'));
    END IF;
    IF NOT EXISTS (SELECT 1 FROM pg_constraint
                    WHERE conname = 'ck_biz_si_type' AND conrelid = 'public.biz_stock_in'::regclass) THEN
        ALTER TABLE biz_stock_in ADD CONSTRAINT ck_biz_si_type
            CHECK (in_type IN ('PURCHASE', 'RETURN', 'OTHER'));
    END IF;
    IF NOT EXISTS (SELECT 1 FROM pg_constraint
                    WHERE conname = 'ck_biz_si_tax' AND conrelid = 'public.biz_stock_in'::regclass) THEN
        ALTER TABLE biz_stock_in ADD CONSTRAINT ck_biz_si_tax
            CHECK (total_with_tax = total_amount + total_tax);
    END IF;
    IF NOT EXISTS (SELECT 1 FROM pg_constraint
                    WHERE conname = 'ck_biz_si_qty' AND conrelid = 'public.biz_stock_in'::regclass) THEN
        ALTER TABLE biz_stock_in ADD CONSTRAINT ck_biz_si_qty
            CHECK (total_qty >= 0 AND total_amount >= 0 AND total_tax >= 0 AND total_with_tax >= 0);
    END IF;
    IF NOT EXISTS (SELECT 1 FROM pg_constraint
                    WHERE conname = 'ck_biz_si_supplier' AND conrelid = 'public.biz_stock_in'::regclass) THEN
        ALTER TABLE biz_stock_in ADD CONSTRAINT ck_biz_si_supplier
            CHECK (in_type <> 'PURCHASE' OR supplier_id IS NOT NULL);
    END IF;
END
$$;

-- 单号唯一是**部分索引**（软删后单号可复用，验收 ⑥）：
--   `ON CONFLICT (in_no)` 在这种索引上不能直接推断，要写成
--   `ON CONFLICT (in_no) WHERE deleted_at IS NULL`（P2-A2 取号幂等时会踩到）
CREATE UNIQUE INDEX IF NOT EXISTS uk_biz_si_no     ON biz_stock_in (in_no) WHERE deleted_at IS NULL;
CREATE INDEX        IF NOT EXISTS idx_biz_si_source   ON biz_stock_in (source_order_id);
CREATE INDEX        IF NOT EXISTS idx_biz_si_supplier ON biz_stock_in (supplier_id, in_date DESC);
CREATE INDEX        IF NOT EXISTS idx_biz_si_status   ON biz_stock_in (status);
CREATE INDEX        IF NOT EXISTS idx_biz_si_date     ON biz_stock_in (in_date DESC);
CREATE INDEX        IF NOT EXISTS idx_biz_si_warehouse ON biz_stock_in (warehouse_id, in_date DESC);
CREATE INDEX        IF NOT EXISTS idx_biz_si_voucher  ON biz_stock_in (voucher_id);

-- ============================================================
-- §4 `biz_stock_in_item` —— 收货入库明细（架构 §2.3.2）
-- ============================================================
CREATE TABLE IF NOT EXISTS biz_stock_in_item (
    id              UUID PRIMARY KEY DEFAULT gen_random_uuid(),     -- 主键
    stock_in_id     UUID          NOT NULL REFERENCES biz_stock_in(id) ON DELETE CASCADE, -- 所属入库单（唯一硬外键）
    line_no         INT           NOT NULL,                         -- 行号（从 1 起）
    source_item_id  UUID,                                           -- 来源采购订单明细（弱引用 biz_purchase_order_item.id）
    material_id     UUID          NOT NULL,                         -- 物料（弱引用 biz_material.id）
    unit_id         UUID          NOT NULL,                         -- 单位（弱引用 biz_unit.id，下单时快照）
    qty             NUMERIC(18,4) NOT NULL,                         -- 入库数量
    price           NUMERIC(18,6) NOT NULL DEFAULT 0,               -- 不含税单价（采购价；PRD 的 price_excl）
    price_incl      NUMERIC(18,6) NOT NULL DEFAULT 0,               -- 含税单价（PRD §3.1.2；口径 B 键入值快照）
    tax_rate        NUMERIC(7,4)  NOT NULL DEFAULT 0,               -- 税率（%）
    amount          NUMERIC(18,4) NOT NULL DEFAULT 0,               -- 不含税金额（= 入库成本；PRD 的 amount_excl）
    tax_amount      NUMERIC(18,4) NOT NULL DEFAULT 0,               -- 税额（PRD 的 tax_amount）
    with_tax_amount NUMERIC(18,4) NOT NULL DEFAULT 0,               -- 价税合计（PRD 的 amount_incl）
    cost_price      NUMERIC(18,6) NOT NULL DEFAULT 0,               -- 入库后移动加权单价快照（过账时写入）
    location_id     UUID,                                           -- 库位（弱引用 biz_inventory_location.id，可空）
    lot_no          VARCHAR(64),                                    -- 批号（可空，批次 2 追溯用）
    remark          VARCHAR(255),
    created_at      TIMESTAMPTZ   NOT NULL DEFAULT CURRENT_TIMESTAMP,
    updated_at      TIMESTAMPTZ   NOT NULL DEFAULT CURRENT_TIMESTAMP,

    CONSTRAINT uk_biz_si_item_line  UNIQUE (stock_in_id, line_no),
    CONSTRAINT ck_biz_si_item_qty   CHECK (qty > 0),
    -- 价税合计恒等（精确相等，见头注释 ②）
    CONSTRAINT ck_biz_si_item_tax   CHECK (with_tax_amount = amount + tax_amount),
    CONSTRAINT ck_biz_si_item_rate  CHECK (tax_rate >= 0 AND tax_rate <= 100)
);

DO $$
BEGIN
    IF NOT EXISTS (SELECT 1 FROM pg_constraint
                    WHERE conname = 'ck_biz_si_item_qty' AND conrelid = 'public.biz_stock_in_item'::regclass) THEN
        ALTER TABLE biz_stock_in_item ADD CONSTRAINT ck_biz_si_item_qty CHECK (qty > 0);
    END IF;
    IF NOT EXISTS (SELECT 1 FROM pg_constraint
                    WHERE conname = 'ck_biz_si_item_tax' AND conrelid = 'public.biz_stock_in_item'::regclass) THEN
        ALTER TABLE biz_stock_in_item ADD CONSTRAINT ck_biz_si_item_tax
            CHECK (with_tax_amount = amount + tax_amount);
    END IF;
    IF NOT EXISTS (SELECT 1 FROM pg_constraint
                    WHERE conname = 'ck_biz_si_item_rate' AND conrelid = 'public.biz_stock_in_item'::regclass) THEN
        ALTER TABLE biz_stock_in_item ADD CONSTRAINT ck_biz_si_item_rate
            CHECK (tax_rate >= 0 AND tax_rate <= 100);
    END IF;
    IF NOT EXISTS (SELECT 1 FROM pg_constraint
                    WHERE conname = 'uk_biz_si_item_line' AND conrelid = 'public.biz_stock_in_item'::regclass) THEN
        ALTER TABLE biz_stock_in_item ADD CONSTRAINT uk_biz_si_item_line UNIQUE (stock_in_id, line_no);
    END IF;
END
$$;

CREATE INDEX IF NOT EXISTS idx_biz_si_item_in       ON biz_stock_in_item (stock_in_id);
CREATE INDEX IF NOT EXISTS idx_biz_si_item_material ON biz_stock_in_item (material_id);
CREATE INDEX IF NOT EXISTS idx_biz_si_item_src      ON biz_stock_in_item (source_item_id);

-- ============================================================
-- §5 自检：列 / 约束 / 索引齐了没（缺一条就让迁移失败，而不是留半张表）
-- ============================================================
DO $$
DECLARE
    need_col  TEXT;
    miss      TEXT := '';
    ck_list   TEXT[];
    ck        TEXT;
    idx_list  TEXT[];
    need_idx  TEXT;
BEGIN
    -- 5.1 采购主表：三种金额必须仍是既有列（反检「没建重复列」）
    FOREACH need_col IN ARRAY ARRAY[
        'price_mode', 'supplier_id', 'order_date', 'status',
        'total_amount', 'total_tax', 'total_with_tax'
    ] LOOP
        IF NOT EXISTS (SELECT 1 FROM information_schema.columns
                        WHERE table_schema = 'public' AND table_name = 'biz_purchase_order'
                          AND column_name = need_col) THEN
            miss := miss || ' biz_purchase_order.' || need_col;
        END IF;
    END LOOP;

    -- 5.2 采购明细：qty / price / price_incl / tax_rate / 三金额
    FOREACH need_col IN ARRAY ARRAY[
        'qty', 'price', 'price_incl', 'tax_rate', 'amount', 'tax_amount', 'with_tax_amount'
    ] LOOP
        IF NOT EXISTS (SELECT 1 FROM information_schema.columns
                        WHERE table_schema = 'public' AND table_name = 'biz_purchase_order_item'
                          AND column_name = need_col) THEN
            miss := miss || ' biz_purchase_order_item.' || need_col;
        END IF;
    END LOOP;

    -- 5.3 收货两表的关键列（卡面清单 + 架构 DDL）
    FOREACH need_col IN ARRAY ARRAY[
        'in_no', 'in_type', 'source_order_id', 'supplier_id', 'warehouse_id', 'in_date', 'status',
        'total_qty', 'total_amount', 'total_tax', 'total_with_tax', 'invoice_received',
        'voucher_id', 'payable_id', 'posted_at', 'posted_by',
        'created_at', 'updated_at', 'create_by', 'update_by', 'deleted_at'
    ] LOOP
        IF NOT EXISTS (SELECT 1 FROM information_schema.columns
                        WHERE table_schema = 'public' AND table_name = 'biz_stock_in'
                          AND column_name = need_col) THEN
            miss := miss || ' biz_stock_in.' || need_col;
        END IF;
    END LOOP;

    FOREACH need_col IN ARRAY ARRAY[
        'stock_in_id', 'line_no', 'source_item_id', 'material_id', 'unit_id', 'qty',
        'price', 'price_incl', 'tax_rate', 'amount', 'tax_amount', 'with_tax_amount',
        'cost_price', 'location_id', 'lot_no'
    ] LOOP
        IF NOT EXISTS (SELECT 1 FROM information_schema.columns
                        WHERE table_schema = 'public' AND table_name = 'biz_stock_in_item'
                          AND column_name = need_col) THEN
            miss := miss || ' biz_stock_in_item.' || need_col;
        END IF;
    END LOOP;

    IF miss <> '' THEN
        RAISE EXCEPTION 'V0145 自检失败：缺列 →%', miss;
    END IF;

    -- 5.4 约束
    ck_list := ARRAY[
        'ck_biz_po_price_mode', 'ck_biz_po_item_tax_rate',
        'ck_biz_si_status', 'ck_biz_si_type', 'ck_biz_si_tax', 'ck_biz_si_qty', 'ck_biz_si_supplier',
        'ck_biz_si_item_qty', 'ck_biz_si_item_tax', 'ck_biz_si_item_rate', 'uk_biz_si_item_line'
    ];
    FOREACH ck IN ARRAY ck_list LOOP
        IF NOT EXISTS (SELECT 1 FROM pg_constraint WHERE conname = ck) THEN
            RAISE EXCEPTION 'V0145 自检失败：缺约束 %', ck;
        END IF;
    END LOOP;

    -- 5.5 索引（pg_indexes 的列叫 tablename，不是 table_name）
    idx_list := ARRAY[
        'uk_biz_si_no', 'idx_biz_si_source', 'idx_biz_si_supplier', 'idx_biz_si_status',
        'idx_biz_si_date', 'idx_biz_si_warehouse', 'idx_biz_si_voucher',
        'idx_biz_si_item_in', 'idx_biz_si_item_material', 'idx_biz_si_item_src'
    ];
    FOREACH need_idx IN ARRAY idx_list LOOP
        IF NOT EXISTS (SELECT 1 FROM pg_indexes WHERE schemaname = 'public' AND indexname = need_idx) THEN
            RAISE EXCEPTION 'V0145 自检失败：缺索引 %', need_idx;
        END IF;
    END LOOP;

    -- 5.6 单号唯一索引必须是部分索引（软删后可复用单号）
    IF NOT EXISTS (
        SELECT 1 FROM pg_indexes
         WHERE schemaname = 'public' AND indexname = 'uk_biz_si_no'
           AND indexdef LIKE '%WHERE (deleted_at IS NULL)%'
    ) THEN
        RAISE EXCEPTION 'V0145 自检失败：uk_biz_si_no 不是 WHERE deleted_at IS NULL 的部分索引';
    END IF;

    RAISE NOTICE 'V0145 就绪：采购订单补 price_mode / 明细补 price_incl + 税率区间，收货两表 2 张 / 10 索引 / 11 约束';
END
$$;

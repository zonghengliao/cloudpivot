-- ======================================================
-- V0303：销售链 —— 退货单两张表 + `SR` 取号规则（SO-28）
--
--   ① biz_sales_return       销售退货单主表（单号部分唯一 / 状态机 DRAFT 起 / 价税恒等）
--   ② biz_sales_return_item  退货单明细（硬外键 → 单头 ON DELETE CASCADE，qty > 0）
--   ③ biz_seq(seq_type='SR') 单号取号规则：SR-yyyyMMdd-000001（按天滚动）
--
-- ── 号段（开工实测复核）──────────────────────────────────
-- 卡面写的是 V0269，**已被占用**（V0269 属销售链既有号段，已应用迁移不可改、
-- 指纹校验会炸）。开工实测 backend/migrations/business/ 的最大号 = V0302
-- （V0302__sales_order_contract，SO-27 提交 6f4cf2f），本卡顺延取 **V0303**。
-- 迁移号会被并行卡抢走，**提交前**再 `ls business | tail` 复核一次。
--
-- ── 表名 / 列名口径 ────────────────────────────────────
--  表名 `biz_sales_return` / `biz_sales_return_item`、列名 `return_no` / `return_type` /
--  `total_*` / `stock_in_id` / `receivable_id` / `red_invoice_id` 一律按卡面逐字，
--  与 `docs/architecture/ARCH-7-销售模块设计.md` §15.2.2 一致。
--
--  1) 金额列精度：ARCH-7 §15.2.2 稿面写 `numeric(18,2)`，本仓实际口径是
--     **`NUMERIC(18,4)`**（biz_sales_order / biz_receivable / biz_voucher_entry 全 18,4）。
--     照稿面写 (18,2) 会在合计与凭证对账时静默四舍五入（见
--     skill references/db-table-card-and-migration-probes.md §13），故按仓内口径落 18,4；
--     单价 `NUMERIC(18,6)`、税率 `NUMERIC(7,4)` 与 `biz_sales_order_item` **逐列同型**，
--     退货行从订单行/发货行直接拷贝不会丢精度。
--
--  2) 外键口径（卡面「外键 ON DELETE CASCADE」的落地位置）：
--     * `biz_sales_return_item.return_id` → biz_sales_return(id) **硬外键 ON DELETE CASCADE**
--       —— 明细离开单头无独立意义，卡面点名的 CASCADE 就落在这里（与 biz_sales_order_item /
--       biz_shipment_item 同口径）。
--     * 单头与明细上的**其余引用列全部是弱引用（无外键）**，按 ADR-06「单据链零外键」基线：
--         - `so_id` / `shipment_id`：单据链互相引用（退货 → 订单 → 发货）。建硬外键会让
--           「订单/发货单物理删或迁移演练」被退货单永久绑死，且级联方向语义错（订单消失
--           不该吞掉退货单）。有效性由 handler 建单时校验（SO-29）。
--         - `customer_id` / `warehouse_id` / `material_id` / `location_id`：主数据可软删，
--           历史单据必须永远可读（与 biz_sales_order.customer_id、biz_shipment.customer_id
--           同风格）。
--         - `stock_in_id` / `receivable_id` / `red_invoice_id` / `approval_instance_id`：
--           由过账/审批**反向生成**的回链列，建外键会让过账事务成环（V0283 的 stock_out_id 同理）。
--     本文件末尾自检会断言「单头 0 条外键 + 明细只有 1 条 CASCADE 外键」，防止后人偷偷补硬外键。
--
--  3) `so_item_id` 可空：退货类型 OTHER（无原单）与「原单行已不可考」的历史退货必须能录，
--     故不设 NOT NULL；有值时由 SO-29 按「已发量 − 已退量」校验超退。
--
-- ── 状态域（`ck_biz_sreturn_status`，6 值，ARCH-7 §15.2.4 逐字）──────
--     DRAFT → SUBMITTED → APPROVED → PENDING_POST → POSTED；
--     DRAFT / SUBMITTED / APPROVED → CANCELLED。
--     本卡只建表，**不建状态迁移入口**：迁移边必须登记到 `shared/state_machine`
--     （`DocKind::SalesReturn` + `SALES_RETURN_EDGES`）后由 SO-29 复用 —— 与 SO-04
--     建表不登记 DocKind::SaleOrder 的分工一致。届时 LEGAL_EDGES 与本 CHECK 的域值
--     要逐字对齐（自检会逐值断言）。
--
-- ── 退货类型域（`ck_biz_sreturn_type`，3 值）──────────────
--     QUALITY（质量）/ WRONG_ITEM（错发）/ OTHER（其他），按卡面 + ARCH-7 §15.2.2 逐字。
--
-- ── 金额恒等（`ck_biz_sreturn_amount`）────────────────────
--     `total_with_tax = total_amount + total_tax`（**精确恒等**，与 ck_sales_order_amount
--     同口径）；金额列非负另立一条 `ck_biz_sreturn_amount_nonneg`，
--     这样报错能区分「算错」与「符号错」（红字发票的负金额属于 biz_invoice，不在本表）。
--
-- ── 超退校验（卡面未点名，属 SO-29 覆盖面）────────────────
--     「sum(已退) + 本次 <= 已发货量」（ARCH-7 §15.2.3）是**跨表**判定，
--     PG 的 CHECK 表达不了（不能跨表、不能带子查询）；全仓迁移零 TRIGGER，业务判定
--     一律在 Rust 侧（可读 + 可单测）⇒ 本文件只兜单表条件（qty > 0），超退由 SO-29 的
--     handler/model 在事务内 `SELECT … FOR UPDATE` 后判定。
--
-- ── 幂等 ──────────────────────────────────────────────
-- CREATE TABLE / INDEX IF NOT EXISTS；biz_seq 种子 `INSERT … WHERE NOT EXISTS`。
-- 末尾 DO 块自检：表 / 列数 / 卡面列 / 唯一索引语义 / 外键口径 / CHECK 域值 /
-- 默认值 / 行为探针（插坏数据必须被拒）/ 级联删除 / 探针清场 / 取号规则行。
-- ======================================================

-- ---------- ① biz_sales_return（销售退货单主表） ----------
CREATE TABLE IF NOT EXISTS biz_sales_return (
    id                   UUID PRIMARY KEY DEFAULT gen_random_uuid(),      -- 主键
    return_no            VARCHAR(32)   NOT NULL,                          -- 退货单号（biz_seq 生成，存活行唯一）
    customer_id          UUID          NOT NULL,                          -- 客户（弱引用 biz_customer.id）
    so_id                UUID,                                            -- 原销售订单（弱引用 biz_sales_order.id，可空）
    shipment_id          UUID,                                            -- 原发货单（弱引用 biz_shipment.id，可空）
    return_date          DATE          NOT NULL,                          -- 退货日期（必填，决定会计期间）
    return_type          VARCHAR(16)   NOT NULL,                          -- QUALITY / WRONG_ITEM / OTHER
    warehouse_id         UUID          NOT NULL,                          -- 退回入库的仓库（弱引用 biz_warehouse.id）
    status               VARCHAR(24)   NOT NULL DEFAULT 'DRAFT',           -- 状态机：DRAFT/SUBMITTED/APPROVED/PENDING_POST/POSTED/CANCELLED
    total_amount         NUMERIC(18,4) NOT NULL DEFAULT 0,                -- 不含税金额合计（退货为正数口径）
    total_tax            NUMERIC(18,4) NOT NULL DEFAULT 0,                -- 税额合计
    total_with_tax       NUMERIC(18,4) NOT NULL DEFAULT 0,                -- 价税合计（= total_amount + total_tax）
    stock_in_id          UUID,                                            -- 过账生成的入库单（弱引用 biz_stock_in.id）
    receivable_id        UUID,                                            -- 冲减的应收单（弱引用 biz_receivable.id）
    red_invoice_id       UUID,                                            -- 红字发票（弱引用 biz_invoice.id）
    approval_instance_id UUID,                                            -- 审批实例（弱引用流程引擎实例 id）
    remark               TEXT,                                            -- 备注
    created_at           TIMESTAMPTZ   NOT NULL DEFAULT CURRENT_TIMESTAMP, -- 审计：创建时间
    updated_at           TIMESTAMPTZ   NOT NULL DEFAULT CURRENT_TIMESTAMP, -- 审计：更新时间（model 层维护）
    create_by            UUID,                                            -- 审计：创建人
    update_by            UUID,                                            -- 审计：更新人
    deleted_at           TIMESTAMPTZ,                                     -- 软删时间（NULL = 存活）

    CONSTRAINT ck_biz_sreturn_status
        CHECK (status IN ('DRAFT','SUBMITTED','APPROVED','PENDING_POST','POSTED','CANCELLED')),
    CONSTRAINT ck_biz_sreturn_type
        CHECK (return_type IN ('QUALITY','WRONG_ITEM','OTHER')),
    -- 价税合计恒等（精确相等；符号规则另立一条，报错才能区分算错与符号错）
    CONSTRAINT ck_biz_sreturn_amount
        CHECK (total_with_tax = total_amount + total_tax),
    CONSTRAINT ck_biz_sreturn_amount_nonneg
        CHECK (total_amount >= 0 AND total_tax >= 0 AND total_with_tax >= 0)
);

COMMENT ON TABLE  biz_sales_return IS '销售退货单主表（status 为 VARCHAR 状态机；超退校验在 model 而非库层）';
COMMENT ON COLUMN biz_sales_return.return_no IS '退货单号：SR-yyyyMMdd-000001（biz_seq seq_type=SR 取号，按 CST 的天滚动）';
COMMENT ON COLUMN biz_sales_return.customer_id IS '客户 id（弱引用 biz_customer.id，无外键：客户软删后历史单据仍可读）';
COMMENT ON COLUMN biz_sales_return.so_id IS '原销售订单 id（弱引用，可空：ADR-06 单据链零外键；有效性由 SO-29 建单时校验）';
COMMENT ON COLUMN biz_sales_return.shipment_id IS '原发货单 id（弱引用，可空：SO-29 用它带出可退明细）';
COMMENT ON COLUMN biz_sales_return.return_type IS '退货类型：QUALITY(质量) / WRONG_ITEM(错发) / OTHER(其他)';
COMMENT ON COLUMN biz_sales_return.warehouse_id IS '退回入库仓库（弱引用 biz_warehouse.id，无外键）';
COMMENT ON COLUMN biz_sales_return.status IS '状态机：DRAFT/SUBMITTED/APPROVED/PENDING_POST/POSTED/CANCELLED（ARCH-7 §15.2.4）';
COMMENT ON COLUMN biz_sales_return.total_with_tax IS '价税合计（Numeric(18,4)，ck_biz_sreturn_amount 保证 = total_amount + total_tax）';
COMMENT ON COLUMN biz_sales_return.stock_in_id IS '过账生成的退货入库单 id（弱引用 biz_stock_in.id；可空，无外键以免过账事务成环）';
COMMENT ON COLUMN biz_sales_return.receivable_id IS '冲减的应收单 id（弱引用 biz_receivable.id；可空）';
COMMENT ON COLUMN biz_sales_return.red_invoice_id IS '红字发票 id（弱引用 biz_invoice.id；可空）';
COMMENT ON COLUMN biz_sales_return.approval_instance_id IS '审批实例 id（弱引用流程引擎；可空，未提审时为空）';
COMMENT ON COLUMN biz_sales_return.deleted_at IS '软删标记；单号唯一索引带 WHERE deleted_at IS NULL ⇒ 软删后单号可复用';

-- 单号唯一：**部分唯一索引**（存活行唯一），与 uk_biz_sales_order_no / uk_biz_shipment_no 同口径。
-- ⚠ 索引名统一带 `sreturn` 词根：PG 索引名是 schema 级唯一，与既有表撞名 + IF NOT EXISTS
--   会被**静默跳过**（SO-04 栽过：uk_biz_so_no 已被 biz_stock_out 占用）。
CREATE UNIQUE INDEX IF NOT EXISTS uk_biz_sreturn_no
    ON biz_sales_return (return_no) WHERE deleted_at IS NULL;
CREATE INDEX IF NOT EXISTS idx_biz_sreturn_order
    ON biz_sales_return (so_id, return_date DESC);
CREATE INDEX IF NOT EXISTS idx_biz_sreturn_shipment
    ON biz_sales_return (shipment_id) WHERE shipment_id IS NOT NULL;
CREATE INDEX IF NOT EXISTS idx_biz_sreturn_customer
    ON biz_sales_return (customer_id, return_date DESC);
CREATE INDEX IF NOT EXISTS idx_biz_sreturn_status
    ON biz_sales_return (status);
CREATE INDEX IF NOT EXISTS idx_biz_sreturn_date
    ON biz_sales_return (return_date DESC);
CREATE INDEX IF NOT EXISTS idx_biz_sreturn_stock_in
    ON biz_sales_return (stock_in_id) WHERE stock_in_id IS NOT NULL;

-- ---------- ② biz_sales_return_item（退货单明细） ----------
CREATE TABLE IF NOT EXISTS biz_sales_return_item (
    id           UUID PRIMARY KEY DEFAULT gen_random_uuid(),           -- 主键
    return_id    UUID          NOT NULL,                               -- 所属退货单（硬外键 CASCADE）
    so_item_id   UUID,                                                 -- 对应的原销售订单行（弱引用，可空）
    material_id  UUID          NOT NULL,                               -- 物料（弱引用 biz_material.id）
    qty          NUMERIC(18,4) NOT NULL,                               -- 退货数量（> 0）
    price        NUMERIC(18,6) NOT NULL DEFAULT 0,                     -- 单价（不含税，与订单行同型）
    amount       NUMERIC(18,4) NOT NULL DEFAULT 0,                     -- 行不含税金额
    tax_rate     NUMERIC(7,4)  NOT NULL DEFAULT 0,                     -- 税率百分数（0–100）
    tax_amount   NUMERIC(18,4) NOT NULL DEFAULT 0,                     -- 行税额
    location_id  UUID,                                                 -- 退回库位（弱引用 biz_location.id；NULL = 整仓）
    serial_nos   TEXT[],                                               -- 序列号（一物一码物料才填）
    remark       VARCHAR(500),                                         -- 行备注
    created_at   TIMESTAMPTZ   NOT NULL DEFAULT CURRENT_TIMESTAMP,
    updated_at   TIMESTAMPTZ   NOT NULL DEFAULT CURRENT_TIMESTAMP,

    CONSTRAINT fk_biz_sreturn_item_return
        FOREIGN KEY (return_id) REFERENCES biz_sales_return (id) ON DELETE CASCADE,
    CONSTRAINT ck_biz_sreturn_item_qty
        CHECK (qty > 0),
    CONSTRAINT ck_biz_sreturn_item_rate
        CHECK (tax_rate >= 0 AND tax_rate <= 100),
    CONSTRAINT ck_biz_sreturn_item_price
        CHECK (price >= 0 AND amount >= 0 AND tax_amount >= 0)
);

COMMENT ON TABLE  biz_sales_return_item IS '销售退货单明细（return_id CASCADE；超退判定见 SO-29 的 model，不在库层）';
COMMENT ON COLUMN biz_sales_return_item.return_id IS '所属退货单（硬外键 ON DELETE CASCADE：卡面点名，明细离开单头无独立意义）';
COMMENT ON COLUMN biz_sales_return_item.so_item_id IS '对应的原销售订单行（弱引用 biz_sales_order_item.id，可空：无原单退货留空）';
COMMENT ON COLUMN biz_sales_return_item.qty IS '退货数量（> 0，ck_biz_sreturn_item_qty 库层兜底；累计不超「已发量 − 已退量」由 SO-29 守卫）';
COMMENT ON COLUMN biz_sales_return_item.price IS '单价（不含税，NUMERIC(18,6) 与 biz_sales_order_item.price 同型，拷贝不丢精度）';
COMMENT ON COLUMN biz_sales_return_item.location_id IS '退回库位（弱引用 biz_location.id；NULL = 整仓退货）';
COMMENT ON COLUMN biz_sales_return_item.serial_nos IS '序列号数组（一物一码物料才填，长度须与 qty 一致，由 model 校验）';

CREATE INDEX IF NOT EXISTS idx_biz_sreturn_item_return
    ON biz_sales_return_item (return_id);
CREATE INDEX IF NOT EXISTS idx_biz_sreturn_item_so_item
    ON biz_sales_return_item (so_item_id) WHERE so_item_id IS NOT NULL;
CREATE INDEX IF NOT EXISTS idx_biz_sreturn_item_material
    ON biz_sales_return_item (material_id);

-- ---------- ③ 取号规则：SR（销售退货单号） ----------
INSERT INTO biz_seq (id, seq_type, seq_name, prefix, date_pattern, period_key, current_no, step, separator, status, remark)
SELECT v.id::uuid, 'SR', '销售退货单号', 'SR', 'yyyyMMdd', '', 0, 1, '-', 1,
       '销售退货单单号：SR-yyyyMMdd-000001，按天（CST）滚动'
  FROM (VALUES ('d0303000-0000-4000-8000-000000000001')) AS v(id)
 WHERE NOT EXISTS (SELECT 1 FROM biz_seq q WHERE q.seq_type = 'SR' AND q.deleted_at IS NULL);

-- ---------- ④ 自检：结构 + 外键口径 + 域值行为 + 默认值 + 取号规则 ----------
DO $$
DECLARE
    n INT;
    d TEXT;
    probe_ret  UUID := 'd0303fff-0000-4000-8000-000000000001'::uuid;
    probe_ret2 UUID := 'd0303fff-0000-4000-8000-000000000002'::uuid;
BEGIN
    -- 4.1 两张表都在
    IF to_regclass('public.biz_sales_return') IS NULL
       OR to_regclass('public.biz_sales_return_item') IS NULL THEN
        RAISE EXCEPTION 'V0303 自检失败：biz_sales_return / biz_sales_return_item 未建全';
    END IF;

    -- 4.2 列数逐表比对（多一列少一列都算漂移）
    SELECT count(*) INTO n FROM information_schema.columns
     WHERE table_schema = 'public' AND table_name = 'biz_sales_return';
    IF n <> 22 THEN
        RAISE EXCEPTION 'V0303 自检失败：biz_sales_return 列数 = %（期望 22）', n;
    END IF;
    SELECT count(*) INTO n FROM information_schema.columns
     WHERE table_schema = 'public' AND table_name = 'biz_sales_return_item';
    IF n <> 14 THEN
        RAISE EXCEPTION 'V0303 自检失败：biz_sales_return_item 列数 = %（期望 14）', n;
    END IF;

    -- 4.3 卡面点名的列必须逐字在位
    SELECT count(*) INTO n FROM information_schema.columns
     WHERE table_schema = 'public' AND table_name = 'biz_sales_return'
       AND column_name IN ('id','return_no','customer_id','so_id','shipment_id','return_date',
                           'return_type','warehouse_id','status','total_amount','total_tax',
                           'total_with_tax','stock_in_id','receivable_id','red_invoice_id',
                           'approval_instance_id','remark','created_at','updated_at',
                           'create_by','update_by','deleted_at');
    IF n <> 22 THEN
        RAISE EXCEPTION 'V0303 自检失败：biz_sales_return 卡面列只齐了 %/22', n;
    END IF;
    SELECT count(*) INTO n FROM information_schema.columns
     WHERE table_schema = 'public' AND table_name = 'biz_sales_return_item'
       AND column_name IN ('id','return_id','so_item_id','material_id','qty','price','amount',
                           'tax_rate','tax_amount','location_id','serial_nos','remark',
                           'created_at','updated_at');
    IF n <> 14 THEN
        RAISE EXCEPTION 'V0303 自检失败：biz_sales_return_item 卡面列只齐了 %/14', n;
    END IF;

    -- 4.4 金额/税率列必须与订单行**逐列同型**（拷贝不丢精度）
    SELECT string_agg(format('%s[%s,%s,%s] vs %s[%s,%s,%s]',
                             a.column_name, a.data_type, a.numeric_precision, a.numeric_scale,
                             b.column_name, b.data_type, b.numeric_precision, b.numeric_scale), '; ')
      INTO d
      FROM information_schema.columns a
      JOIN information_schema.columns b
        ON b.table_schema = 'public' AND b.table_name = 'biz_sales_order_item'
       AND (a.column_name, b.column_name) IN (('price','price'), ('amount','amount'),
                                              ('tax_rate','tax_rate'), ('tax_amount','tax_amount'))
     WHERE a.table_schema = 'public' AND a.table_name = 'biz_sales_return_item'
       AND (a.data_type <> b.data_type
            OR coalesce(a.numeric_precision,0) <> coalesce(b.numeric_precision,0)
            OR coalesce(a.numeric_scale,0) <> coalesce(b.numeric_scale,0));
    IF d IS NOT NULL THEN
        RAISE EXCEPTION 'V0303 自检失败：明细金额/税率列与 biz_sales_order_item 不同型（%）', d;
    END IF;

    -- 4.5 单号唯一索引必须是「存活行唯一」的部分索引（软删后单号可复用）
    SELECT indexdef INTO d FROM pg_indexes
     WHERE schemaname = 'public' AND tablename = 'biz_sales_return' AND indexname = 'uk_biz_sreturn_no';
    IF d IS NULL THEN
        RAISE EXCEPTION 'V0303 自检失败：唯一索引 uk_biz_sreturn_no 缺失（是否与既有索引撞名被静默跳过？）';
    END IF;
    IF d NOT LIKE '%UNIQUE%' OR d NOT LIKE '%deleted_at IS NULL%' THEN
        RAISE EXCEPTION 'V0303 自检失败：uk_biz_sreturn_no 不是「存活行唯一」的部分索引（def=%）', d;
    END IF;

    -- 4.6 外键口径：单头 0 条外键（ADR-06 单据链零外键）；明细只有 1 条 CASCADE 外键
    SELECT count(*) INTO n FROM pg_constraint
     WHERE conrelid = 'public.biz_sales_return'::regclass AND contype = 'f';
    IF n <> 0 THEN
        RAISE EXCEPTION 'V0303 自检失败：biz_sales_return 出现了 % 条外键（应为 0：单据链/主数据一律弱引用）', n;
    END IF;
    SELECT count(*) INTO n FROM pg_constraint
     WHERE conrelid = 'public.biz_sales_return_item'::regclass AND contype = 'f';
    IF n <> 1 THEN
        RAISE EXCEPTION 'V0303 自检失败：biz_sales_return_item 外键条数 = %（期望 1：仅 return_id）', n;
    END IF;
    SELECT pg_get_constraintdef(oid) INTO d FROM pg_constraint
     WHERE conname = 'fk_biz_sreturn_item_return' AND contype = 'f';
    IF d IS NULL OR d NOT LIKE '%REFERENCES biz_sales_return(id)%' OR d NOT LIKE '%ON DELETE CASCADE%' THEN
        RAISE EXCEPTION 'V0303 自检失败：明细→单头外键缺失或不是 CASCADE（def=%）', d;
    END IF;

    -- 4.7 四条 CHECK 在位；状态域 / 退货类型域字面量齐全
    IF NOT EXISTS (SELECT 1 FROM pg_constraint WHERE conname = 'ck_biz_sreturn_status') THEN
        RAISE EXCEPTION 'V0303 自检失败：ck_biz_sreturn_status 缺失';
    END IF;
    IF NOT EXISTS (SELECT 1 FROM pg_constraint WHERE conname = 'ck_biz_sreturn_type') THEN
        RAISE EXCEPTION 'V0303 自检失败：ck_biz_sreturn_type 缺失';
    END IF;
    IF NOT EXISTS (SELECT 1 FROM pg_constraint WHERE conname = 'ck_biz_sreturn_amount') THEN
        RAISE EXCEPTION 'V0303 自检失败：ck_biz_sreturn_amount 缺失';
    END IF;
    IF NOT EXISTS (SELECT 1 FROM pg_constraint WHERE conname = 'ck_biz_sreturn_amount_nonneg') THEN
        RAISE EXCEPTION 'V0303 自检失败：ck_biz_sreturn_amount_nonneg 缺失';
    END IF;
    IF NOT EXISTS (SELECT 1 FROM pg_constraint WHERE conname = 'ck_biz_sreturn_item_qty') THEN
        RAISE EXCEPTION 'V0303 自检失败：ck_biz_sreturn_item_qty 缺失（卡面点名）';
    END IF;
    FOREACH d IN ARRAY ARRAY['DRAFT','SUBMITTED','APPROVED','PENDING_POST','POSTED','CANCELLED']
    LOOP
        IF (SELECT pg_get_constraintdef(oid) FROM pg_constraint WHERE conname = 'ck_biz_sreturn_status')
           NOT LIKE '%' || d || '%' THEN
            RAISE EXCEPTION 'V0303 自检失败：ck_biz_sreturn_status 缺域值 %', d;
        END IF;
    END LOOP;
    FOREACH d IN ARRAY ARRAY['QUALITY','WRONG_ITEM','OTHER']
    LOOP
        IF (SELECT pg_get_constraintdef(oid) FROM pg_constraint WHERE conname = 'ck_biz_sreturn_type')
           NOT LIKE '%' || d || '%' THEN
            RAISE EXCEPTION 'V0303 自检失败：ck_biz_sreturn_type 缺域值 %', d;
        END IF;
    END LOOP;

    -- 4.8 默认值：status='DRAFT'、三金额列 = 0
    SELECT column_default INTO d FROM information_schema.columns
     WHERE table_schema = 'public' AND table_name = 'biz_sales_return' AND column_name = 'status';
    IF d IS NULL OR d NOT LIKE '''DRAFT''%' THEN
        RAISE EXCEPTION 'V0303 自检失败：biz_sales_return.status 默认值不是 DRAFT（default=%）', d;
    END IF;
    SELECT count(*) INTO n FROM information_schema.columns
     WHERE table_schema = 'public' AND table_name = 'biz_sales_return'
       AND column_name IN ('total_amount','total_tax','total_with_tax')
       AND column_default LIKE '0%';
    IF n <> 3 THEN
        RAISE EXCEPTION 'V0303 自检失败：三金额列默认值不是 0（齐了 %/3）', n;
    END IF;

    -- 4.9 行为探针
    --     4.9a 状态越界值 → 必须被拒
    BEGIN
        INSERT INTO biz_sales_return (return_no, customer_id, return_date, return_type, warehouse_id, status)
        VALUES ('__V0303_PROBE_STATUS__', gen_random_uuid(), DATE '2026-11-07', 'OTHER', gen_random_uuid(), 'RETURNED');
        RAISE EXCEPTION 'V0303 自检失败：status = RETURNED 未被 ck_biz_sreturn_status 拒绝';
    EXCEPTION WHEN check_violation THEN
        NULL;   -- 期望路径
    END;

    --     4.9b 退货类型越界值 → 必须被拒
    BEGIN
        INSERT INTO biz_sales_return (return_no, customer_id, return_date, return_type, warehouse_id)
        VALUES ('__V0303_PROBE_TYPE__', gen_random_uuid(), DATE '2026-11-07', 'DAMAGED', gen_random_uuid());
        RAISE EXCEPTION 'V0303 自检失败：return_type = DAMAGED 未被 ck_biz_sreturn_type 拒绝';
    EXCEPTION WHEN check_violation THEN
        NULL;   -- 期望路径
    END;

    --     4.9c 价税合计不恒等 → 必须被拒
    BEGIN
        INSERT INTO biz_sales_return (return_no, customer_id, return_date, return_type, warehouse_id,
                                      total_amount, total_tax, total_with_tax)
        VALUES ('__V0303_PROBE_SUM__', gen_random_uuid(), DATE '2026-11-07', 'OTHER', gen_random_uuid(),
                50, 40, 100);
        RAISE EXCEPTION 'V0303 自检失败：total_with_tax ≠ amount + tax 未被 ck_biz_sreturn_amount 拒绝';
    EXCEPTION WHEN check_violation THEN
        NULL;   -- 期望路径
    END;

    --     4.9d 负金额 → 必须被拒
    BEGIN
        INSERT INTO biz_sales_return (return_no, customer_id, return_date, return_type, warehouse_id,
                                      total_amount, total_tax, total_with_tax)
        VALUES ('__V0303_PROBE_NEG__', gen_random_uuid(), DATE '2026-11-07', 'OTHER', gen_random_uuid(),
                -1, -0.13, -1.13);
        RAISE EXCEPTION 'V0303 自检失败：负金额未被 ck_biz_sreturn_amount_nonneg 拒绝';
    EXCEPTION WHEN check_violation THEN
        NULL;   -- 期望路径
    END;

    --     4.9e 正常单头：默认值探针（status / 三金额列都给默认 → 必须能插入）
    INSERT INTO biz_sales_return (id, return_no, customer_id, return_date, return_type, warehouse_id)
    VALUES (probe_ret, '__V0303_PROBE_OK__', gen_random_uuid(), DATE '2026-11-07', 'QUALITY', gen_random_uuid());
    SELECT status || '|' || total_amount::text || '|' || total_tax::text || '|' || total_with_tax::text
      INTO d FROM biz_sales_return WHERE id = probe_ret;
    IF d <> 'DRAFT|0.0000|0.0000|0.0000' THEN
        RAISE EXCEPTION 'V0303 自检失败：不传 status/金额时落库值 = %（期望 DRAFT|0.0000|0.0000|0.0000）', d;
    END IF;

    --     4.9f 明细 qty = 0 → 必须被拒（卡面 qty > 0）
    BEGIN
        INSERT INTO biz_sales_return_item (return_id, material_id, qty)
        VALUES (probe_ret, gen_random_uuid(), 0);
        RAISE EXCEPTION 'V0303 自检失败：qty = 0 未被 ck_biz_sreturn_item_qty 拒绝';
    EXCEPTION WHEN check_violation THEN
        NULL;   -- 期望路径
    END;

    --     4.9g 明细 qty < 0 → 必须被拒
    BEGIN
        INSERT INTO biz_sales_return_item (return_id, material_id, qty)
        VALUES (probe_ret, gen_random_uuid(), -2);
        RAISE EXCEPTION 'V0303 自检失败：qty = -2 未被 ck_biz_sreturn_item_qty 拒绝';
    EXCEPTION WHEN check_violation THEN
        NULL;   -- 期望路径
    END;

    --     4.9h 明细挂到不存在的退货单 → 必须被外键拦住
    BEGIN
        INSERT INTO biz_sales_return_item (return_id, material_id, qty)
        VALUES ('00000000-0000-4000-8000-0000ffffffff'::uuid, gen_random_uuid(), 1);
        RAISE EXCEPTION 'V0303 自检失败：return_id 指向不存在的退货单却插入成功';
    EXCEPTION WHEN foreign_key_violation THEN
        NULL;   -- 期望路径
    END;

    --     4.9i 明细正常插入（含 serial_nos 数组）+ 金额同型落位
    INSERT INTO biz_sales_return_item (return_id, so_item_id, material_id, qty, price, amount,
                                       tax_rate, tax_amount, location_id, serial_nos)
    VALUES (probe_ret, NULL, gen_random_uuid(), 2, 123.456789, 200.0000, 13.0000, 26.0000,
            gen_random_uuid(), ARRAY['SN-0001','SN-0002']);
    SELECT array_length(serial_nos, 1)::text || '|' || price::text INTO d
      FROM biz_sales_return_item WHERE return_id = probe_ret;
    IF d <> '2|123.456789' THEN
        RAISE EXCEPTION 'V0303 自检失败：serial_nos / price 读回 = %（期望 2|123.456789）', d;
    END IF;

    --     4.9j 级联：物理删单头 → 明细必须一起消失（探针明细由此清场）
    DELETE FROM biz_sales_return WHERE id = probe_ret;
    SELECT count(*) INTO n FROM biz_sales_return_item WHERE return_id = probe_ret;
    IF n <> 0 THEN
        RAISE EXCEPTION 'V0303 自检失败：单头删除后明细仍剩 % 行（ON DELETE CASCADE 未生效）', n;
    END IF;

    --     4.9k 单号唯一：存活行重号必须被拒
    INSERT INTO biz_sales_return (id, return_no, customer_id, return_date, return_type, warehouse_id)
    VALUES (probe_ret2, '__V0303_PROBE_UK__', gen_random_uuid(), DATE '2026-11-07', 'OTHER', gen_random_uuid());
    BEGIN
        INSERT INTO biz_sales_return (return_no, customer_id, return_date, return_type, warehouse_id)
        VALUES ('__V0303_PROBE_UK__', gen_random_uuid(), DATE '2026-11-07', 'OTHER', gen_random_uuid());
        RAISE EXCEPTION 'V0303 自检失败：存活行重号未被 uk_biz_sreturn_no 拒绝';
    EXCEPTION WHEN unique_violation THEN
        NULL;   -- 期望路径
    END;

    --     4.9l 软删之后同一单号可复用（部分唯一索引语义）
    UPDATE biz_sales_return SET deleted_at = now() WHERE id = probe_ret2;
    INSERT INTO biz_sales_return (return_no, customer_id, return_date, return_type, warehouse_id)
    VALUES ('__V0303_PROBE_UK__', gen_random_uuid(), DATE '2026-11-07', 'OTHER', gen_random_uuid());
    SELECT count(*) INTO n FROM biz_sales_return
     WHERE return_no = '__V0303_PROBE_UK__' AND deleted_at IS NULL;
    IF n <> 1 THEN
        RAISE EXCEPTION 'V0303 自检失败：软删后同号重插后存活行 = %（期望 1）', n;
    END IF;

    --     4.9m 清场：探针行全部消失
    DELETE FROM biz_sales_return WHERE return_no LIKE '__V0303%';
    SELECT (SELECT count(*) FROM biz_sales_return WHERE return_no LIKE '__V0303%')
         + (SELECT count(*) FROM biz_sales_return_item i
             JOIN biz_sales_return h ON h.id = i.return_id
            WHERE h.return_no LIKE '__V0303%')
      INTO n;
    IF n <> 0 THEN
        RAISE EXCEPTION 'V0303 自检失败：探针残留 % 行', n;
    END IF;

    -- 4.10 取号规则：SR 行在位且口径正确（前缀 / 日号 / 分隔符 / 步长 / 启用）
    SELECT prefix || '|' || date_pattern || '|' || separator || '|' || step || '|' || status
      INTO d
      FROM biz_seq WHERE seq_type = 'SR' AND deleted_at IS NULL;
    IF d IS NULL THEN
        RAISE EXCEPTION 'V0303 自检失败：biz_seq(seq_type=SR) 取号规则行缺失';
    END IF;
    IF d <> 'SR|yyyyMMdd|-|1|1' THEN
        RAISE EXCEPTION 'V0303 自检失败：SR 取号规则口径不对（% ≠ SR|yyyyMMdd|-|1|1）', d;
    END IF;
END $$;

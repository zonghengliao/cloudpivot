-- ======================================================
-- V0283：销售链 —— 发货单两张表 + `SH` 取号规则（SO-08）
--
--   ① biz_shipment       发货单主表（单号部分唯一 / 状态机 DRAFT 起 / 总数量非负）
--   ② biz_shipment_item  发货单明细（硬外键 → 头与订单行，qty > 0）
--   ③ biz_seq(seq_type='SH') 单号取号规则：SH-yyyyMMdd-000001（按天滚动）
--
-- ── 号段（开工实测复核）──────────────────────────────────
-- 写入本文件时 backend/migrations/business/ 最大号 = V0282。卡面写的「V0263」已被
-- V0263__sales_customer_menu_label（SO-03 前端菜单卡）占用，**不能复用号段**
-- （已应用迁移不可改、指纹校验会炸）。本卡顺延取 V0283。
-- 迁移号会被并行卡抢走，**提交前**再 `ls business | tail` 复核一次。
--
-- ── 表名 / 列名口径 ────────────────────────────────────
--  1) 表名 `biz_shipment` / `biz_shipment_item`、列名 `ship_no` / `so_id` /
--     `so_item_id` / `total_qty` 一律按卡面逐字；与
--     `docs/architecture/ARCH-7-销售模块设计.md` §3.1.5 一致，无第三种写法。
--  2) 卡面没点名的 `stock_out_id` **建了**：ARCH-7 §3.1.5 有这一列，且 SO-10
--     （发货过账 → 生成销售出库单）必须拿它回链出库单；现在不建就要在 SO-10 再开一次
--     迁移改表。列可空、无外键（出库单由 SO-10 生成，反向依赖会让过账事务成环）。
--  3) 外键口径与销售订单（V0264）同风格：
--       * `so_id`      → biz_sales_order(id)     **硬外键 ON DELETE RESTRICT**（订单在发货期间
--         不许被物理删；软删走 deleted_at，不触发外键）
--       * `customer_id` / `warehouse_id` / `material_id` / `location_id` 是**弱引用**（无外键）：
--         客户 / 仓库 / 物料 / 库位都是可软删的主数据，历史单据必须永远可读
--       * `shipment_id` → biz_shipment(id)       **硬外键 ON DELETE CASCADE**（明细离开单头无意义）
--       * `so_item_id`  → biz_sales_order_item(id) **硬外键 ON DELETE RESTRICT**（订单行被发货单
--         引用时不许消失，否则「这次发的是哪一行」永久断链）
--
-- ── 状态域（`ck_shipment_status`，4 值）───────────────────
--     DRAFT → PENDING_POST → POSTED；DRAFT / PENDING_POST → CANCELLED。
--     本卡只建表，**不建状态迁移入口**：迁移边必须登记到 `shared/state_machine`
--     （`DocKind::Shipment` + `SHIPMENT_EDGES`）后由 SO-09 复用 —— 与 SO-04 的分工一致
--     （SO-04 建表时不登记 DocKind::SaleOrder，SO-05 才登记）。届时 LEGAL_EDGES 与本 CHECK
--     的域值要逐字对齐（`shipment_model::STATUSES` 已按本 CHECK 逐字声明并用单测钉住）。
--     `POSTED` 由过账动作写入（SO-10），不是人工选的普通值。
--
-- ── 超发（卡面「qty > 0，且不超过订单行未发货量」）────────────────
--     ① `qty > 0` 是**库层 CHECK**（ck_biz_shipment_item_qty），插坏数据直接 23514；
--     ② 「不超过未发货量」是**跨表**判定（订单行 qty - delivered_qty - 其他未过账发货占用），
--        PG 的 CHECK 表达不了（CHECK 不能跨表、不能带子查询），由
--        `business/erp/shipment_model.rs::ensure_qty_within_remaining`（事务内 SELECT … FOR UPDATE
--        锁订单行后再算）守卫，SO-09 的建单 / 改单入口必须调用它。
--        本文件不建触发器：全仓迁移里没有任何 TRIGGER，业务判定一律在 Rust 侧（可读 + 可单测）。
--     ③ `total_qty >= 0` 也在库层（ck_shipment_total_qty）：它是明细汇总值，
--        过账/改单算错时宁可直接 23514，也不要把负数总量写进账。
--
-- ── 幂等 ──────────────────────────────────────────────
-- CREATE TABLE / INDEX IF NOT EXISTS；biz_seq 种子 `INSERT … WHERE NOT EXISTS`。
-- 末尾 DO 块自检：表 / 列数 / 索引 / 外键级联与 RESTRICT / CHECK 行为（插坏数据必须被拒）/
-- 默认值（status=DRAFT、total_qty=0）/ 取号规则行，任一不符 → 迁移失败。
-- ======================================================

-- ---------- ① biz_shipment（发货单主表） ----------
CREATE TABLE IF NOT EXISTS biz_shipment (
    id            UUID PRIMARY KEY DEFAULT gen_random_uuid(),          -- 主键
    ship_no       VARCHAR(32)   NOT NULL,                              -- 发货单号（biz_seq 生成，存活行唯一）
    so_id         UUID          NOT NULL,                              -- 来源销售订单（硬外键 RESTRICT）
    customer_id   UUID          NOT NULL,                              -- 客户（弱引用 biz_customer.id）
    warehouse_id  UUID          NOT NULL,                              -- 发货仓库（弱引用 biz_warehouse.id）
    ship_date     DATE          NOT NULL,                              -- 发货日期
    status        VARCHAR(24)   NOT NULL DEFAULT 'DRAFT',               -- 状态机：DRAFT/PENDING_POST/POSTED/CANCELLED
    total_qty     NUMERIC(18,4) NOT NULL DEFAULT 0,                    -- 明细数量合计
    stock_out_id  UUID,                                                -- 过账后生成的销售出库单（SO-10 回填）
    remark        TEXT,                                                -- 备注
    created_at    TIMESTAMPTZ   NOT NULL DEFAULT CURRENT_TIMESTAMP,     -- 审计：创建时间
    updated_at    TIMESTAMPTZ   NOT NULL DEFAULT CURRENT_TIMESTAMP,     -- 审计：更新时间（model 层维护）
    create_by     UUID,                                                -- 审计：创建人
    update_by     UUID,                                                -- 审计：更新人
    deleted_at    TIMESTAMPTZ,                                         -- 软删时间（NULL = 存活）

    CONSTRAINT fk_biz_shipment_order
        FOREIGN KEY (so_id) REFERENCES biz_sales_order (id) ON DELETE RESTRICT,
    CONSTRAINT ck_shipment_status
        CHECK (status IN ('DRAFT','PENDING_POST','POSTED','CANCELLED')),
    CONSTRAINT ck_shipment_total_qty
        CHECK (total_qty >= 0)
);

COMMENT ON TABLE  biz_shipment IS '发货单主表（status 为 VARCHAR 状态机；超发校验在 shipment_model 而非库层）';
COMMENT ON COLUMN biz_shipment.ship_no IS '发货单号：SH-yyyyMMdd-000001（biz_seq seq_type=SH 取号，按 CST 的天滚动）';
COMMENT ON COLUMN biz_shipment.so_id IS '来源销售订单（硬外键 RESTRICT：发货期间订单不许被物理删）';
COMMENT ON COLUMN biz_shipment.customer_id IS '客户 id（弱引用 biz_customer.id，无外键：客户软删后历史单据仍可读）';
COMMENT ON COLUMN biz_shipment.warehouse_id IS '发货仓库（弱引用 biz_warehouse.id，无外键）';
COMMENT ON COLUMN biz_shipment.status IS '状态机：DRAFT/PENDING_POST/POSTED/CANCELLED（ARCH-7 §4.2）';
COMMENT ON COLUMN biz_shipment.stock_out_id IS '过账后生成的销售出库单 id（SO-10 回填；可空，无外键以免过账事务成环）';
COMMENT ON COLUMN biz_shipment.deleted_at IS '软删标记；单号唯一索引带 WHERE deleted_at IS NULL ⇒ 软删后单号可复用';

-- 单号唯一：**部分唯一索引**（存活行唯一），与 uk_biz_sales_order_no / uk_biz_po_no 同口径。
-- ⚠ 索引名统一带 `shipment` 全词：PG 索引名是 schema 级唯一，与既有表撞名 + IF NOT EXISTS
--   会被**静默跳过**（SO-04 实测栽过：uk_biz_so_no 已被 biz_stock_out 占用）。
CREATE UNIQUE INDEX IF NOT EXISTS uk_biz_shipment_no
    ON biz_shipment (ship_no) WHERE deleted_at IS NULL;
CREATE INDEX IF NOT EXISTS idx_biz_shipment_order  ON biz_shipment (so_id, ship_date DESC);
CREATE INDEX IF NOT EXISTS idx_biz_shipment_customer ON biz_shipment (customer_id, ship_date DESC);
CREATE INDEX IF NOT EXISTS idx_biz_shipment_status ON biz_shipment (status);
CREATE INDEX IF NOT EXISTS idx_biz_shipment_date   ON biz_shipment (ship_date DESC);

-- ---------- ② biz_shipment_item（发货单明细） ----------
CREATE TABLE IF NOT EXISTS biz_shipment_item (
    id           UUID PRIMARY KEY DEFAULT gen_random_uuid(),           -- 主键
    shipment_id  UUID          NOT NULL,                               -- 所属发货单（硬外键 CASCADE）
    so_item_id   UUID          NOT NULL,                               -- 对应的销售订单行（硬外键 RESTRICT）
    material_id  UUID          NOT NULL,                               -- 物料（弱引用 biz_material.id）
    qty          NUMERIC(18,4) NOT NULL,                               -- 本次发货数量（> 0）
    location_id  UUID,                                                 -- 发货库位（弱引用 biz_location.id；NULL = 整仓）
    serial_nos   TEXT[],                                               -- 序列号（序列号物料才填）
    remark       VARCHAR(500),                                         -- 行备注
    created_at   TIMESTAMPTZ   NOT NULL DEFAULT CURRENT_TIMESTAMP,
    updated_at   TIMESTAMPTZ   NOT NULL DEFAULT CURRENT_TIMESTAMP,

    CONSTRAINT fk_biz_shipment_item_shipment
        FOREIGN KEY (shipment_id) REFERENCES biz_shipment (id) ON DELETE CASCADE,
    CONSTRAINT fk_biz_shipment_item_so_item
        FOREIGN KEY (so_item_id) REFERENCES biz_sales_order_item (id) ON DELETE RESTRICT,
    CONSTRAINT ck_biz_shipment_item_qty
        CHECK (qty > 0)
);

COMMENT ON TABLE  biz_shipment_item IS '发货单明细（shipment_id CASCADE / so_item_id RESTRICT；超发判定见 shipment_model）';
COMMENT ON COLUMN biz_shipment_item.so_item_id IS '对应的销售订单行（硬外键 RESTRICT：被发货单引用的订单行不许消失）';
COMMENT ON COLUMN biz_shipment_item.qty IS '本次发货数量（> 0；累计不超订单行 qty - delivered_qty，由 shipment_model 守卫）';
COMMENT ON COLUMN biz_shipment_item.location_id IS '发货库位（弱引用 biz_location.id；NULL = 整仓发货）';
COMMENT ON COLUMN biz_shipment_item.serial_nos IS '序列号数组（一物一码物料才填，长度须与 qty 一致，由 model 校验）';

CREATE INDEX IF NOT EXISTS idx_biz_shipment_item_shipment
    ON biz_shipment_item (shipment_id);
CREATE INDEX IF NOT EXISTS idx_biz_shipment_item_so_item
    ON biz_shipment_item (so_item_id);
CREATE INDEX IF NOT EXISTS idx_biz_shipment_item_material
    ON biz_shipment_item (material_id);

-- ---------- ③ 取号规则：SH（发货单号） ----------
INSERT INTO biz_seq (id, seq_type, seq_name, prefix, date_pattern, period_key, current_no, step, separator, status, remark)
SELECT v.id::uuid, 'SH', '发货单号', 'SH', 'yyyyMMdd', '', 0, 1, '-', 1,
       '发货单单号：SH-yyyyMMdd-000001，按天（CST）滚动'
  FROM (VALUES ('d0283000-0000-4000-8000-000000000001')) AS v(id)
 WHERE NOT EXISTS (SELECT 1 FROM biz_seq q WHERE q.seq_type = 'SH' AND q.deleted_at IS NULL);

-- ---------- ④ 自检：结构 + 域值行为 + 默认值 + 取号规则 ----------
DO $$
DECLARE
    n INT;
    d TEXT;
    probe_order UUID := 'd0283fff-0000-4000-8000-000000000001'::uuid;
    probe_item  UUID := 'd0283fff-0000-4000-8000-000000000002'::uuid;
    probe_ship  UUID := 'd0283fff-0000-4000-8000-000000000003'::uuid;
BEGIN
    -- 4.1 两张表都在
    IF to_regclass('public.biz_shipment') IS NULL
       OR to_regclass('public.biz_shipment_item') IS NULL THEN
        RAISE EXCEPTION 'V0283 自检失败：biz_shipment / biz_shipment_item 未建全';
    END IF;

    -- 4.2 列数逐表比对（多一列少一列都算漂移）
    SELECT count(*) INTO n FROM information_schema.columns
     WHERE table_schema = 'public' AND table_name = 'biz_shipment';
    IF n <> 15 THEN
        RAISE EXCEPTION 'V0283 自检失败：biz_shipment 列数 = %（期望 15）', n;
    END IF;
    SELECT count(*) INTO n FROM information_schema.columns
     WHERE table_schema = 'public' AND table_name = 'biz_shipment_item';
    IF n <> 10 THEN
        RAISE EXCEPTION 'V0283 自检失败：biz_shipment_item 列数 = %（期望 10）', n;
    END IF;

    -- 4.3 卡面点名的列必须逐字在位
    SELECT count(*) INTO n FROM information_schema.columns
     WHERE table_schema = 'public' AND table_name = 'biz_shipment'
       AND column_name IN ('id','ship_no','so_id','customer_id','warehouse_id','ship_date',
                           'status','total_qty','remark','created_at','updated_at',
                           'create_by','update_by','deleted_at','stock_out_id');
    IF n <> 15 THEN
        RAISE EXCEPTION 'V0283 自检失败：biz_shipment 卡面列只齐了 %/15', n;
    END IF;
    SELECT count(*) INTO n FROM information_schema.columns
     WHERE table_schema = 'public' AND table_name = 'biz_shipment_item'
       AND column_name IN ('id','shipment_id','so_item_id','material_id','qty',
                           'location_id','serial_nos','remark','created_at','updated_at');
    IF n <> 10 THEN
        RAISE EXCEPTION 'V0283 自检失败：biz_shipment_item 卡面列只齐了 %/10', n;
    END IF;

    -- 4.4 单号唯一索引必须是「存活行唯一」的部分索引（软删后单号可复用）
    SELECT indexdef INTO d FROM pg_indexes
     WHERE schemaname = 'public' AND tablename = 'biz_shipment' AND indexname = 'uk_biz_shipment_no';
    IF d IS NULL THEN
        RAISE EXCEPTION 'V0283 自检失败：唯一索引 uk_biz_shipment_no 缺失（是否与既有索引撞名被静默跳过？）';
    END IF;
    IF d NOT LIKE '%UNIQUE%' OR d NOT LIKE '%deleted_at IS NULL%' THEN
        RAISE EXCEPTION 'V0283 自检失败：uk_biz_shipment_no 不是「存活行唯一」的部分索引（def=%）', d;
    END IF;

    -- 4.5 外键：明细→单头 CASCADE；明细→订单行 RESTRICT；单头→订单 RESTRICT
    SELECT pg_get_constraintdef(oid) INTO d FROM pg_constraint
     WHERE conname = 'fk_biz_shipment_item_shipment' AND contype = 'f';
    IF d IS NULL OR d NOT LIKE '%REFERENCES biz_shipment(id)%' OR d NOT LIKE '%ON DELETE CASCADE%' THEN
        RAISE EXCEPTION 'V0283 自检失败：明细→单头外键缺失或不是 CASCADE（def=%）', d;
    END IF;
    SELECT pg_get_constraintdef(oid) INTO d FROM pg_constraint
     WHERE conname = 'fk_biz_shipment_item_so_item' AND contype = 'f';
    IF d IS NULL OR d NOT LIKE '%REFERENCES biz_sales_order_item(id)%' OR d NOT LIKE '%ON DELETE RESTRICT%' THEN
        RAISE EXCEPTION 'V0283 自检失败：明细→订单行外键缺失或不是 RESTRICT（def=%）', d;
    END IF;
    SELECT pg_get_constraintdef(oid) INTO d FROM pg_constraint
     WHERE conname = 'fk_biz_shipment_order' AND contype = 'f';
    IF d IS NULL OR d NOT LIKE '%REFERENCES biz_sales_order(id)%' THEN
        RAISE EXCEPTION 'V0283 自检失败：单头→销售订单外键缺失（def=%）', d;
    END IF;

    -- 4.6 三条 CHECK 在位，状态域字面量齐全
    IF NOT EXISTS (SELECT 1 FROM pg_constraint WHERE conname = 'ck_biz_shipment_item_qty') THEN
        RAISE EXCEPTION 'V0283 自检失败：ck_biz_shipment_item_qty 缺失';
    END IF;
    IF NOT EXISTS (SELECT 1 FROM pg_constraint WHERE conname = 'ck_shipment_total_qty') THEN
        RAISE EXCEPTION 'V0283 自检失败：ck_shipment_total_qty 缺失';
    END IF;
    IF NOT EXISTS (SELECT 1 FROM pg_constraint WHERE conname = 'ck_shipment_status') THEN
        RAISE EXCEPTION 'V0283 自检失败：ck_shipment_status 缺失';
    END IF;
    FOREACH d IN ARRAY ARRAY['DRAFT','PENDING_POST','POSTED','CANCELLED']
    LOOP
        IF (SELECT pg_get_constraintdef(oid) FROM pg_constraint WHERE conname = 'ck_shipment_status')
           NOT LIKE '%' || d || '%' THEN
            RAISE EXCEPTION 'V0283 自检失败：ck_shipment_status 缺域值 %', d;
        END IF;
    END LOOP;

    -- 4.7 默认值：status='DRAFT'、total_qty=0
    SELECT column_default INTO d FROM information_schema.columns
     WHERE table_schema = 'public' AND table_name = 'biz_shipment' AND column_name = 'status';
    IF d IS NULL OR d NOT LIKE '''DRAFT''%' THEN
        RAISE EXCEPTION 'V0283 自检失败：biz_shipment.status 默认值不是 DRAFT（default=%）', d;
    END IF;
    SELECT column_default INTO d FROM information_schema.columns
     WHERE table_schema = 'public' AND table_name = 'biz_shipment' AND column_name = 'total_qty';
    IF d IS NULL OR d NOT LIKE '0%' THEN
        RAISE EXCEPTION 'V0283 自检失败：biz_shipment.total_qty 默认值不是 0（default=%）', d;
    END IF;

    -- 4.8 行为探针：先备一张订单头 + 一行明细（探针专用，末尾全部清掉）
    INSERT INTO biz_sales_order (id, so_no, customer_id, order_date)
    VALUES (probe_order, '__V0283_PROBE_SO__', gen_random_uuid(), DATE '2026-11-06');
    INSERT INTO biz_sales_order_item (id, so_id, line_no, material_id, qty, price, amount, tax_rate, tax_amount)
    VALUES (probe_item, probe_order, 1, gen_random_uuid(), 10, 100.000000, 1000.0000, 13.0000, 130.0000);

    --     4.8a 状态越界值 → 必须被拒
    BEGIN
        INSERT INTO biz_shipment (ship_no, so_id, customer_id, warehouse_id, ship_date, status)
        VALUES ('__V0283_PROBE_STATUS__', probe_order, gen_random_uuid(), gen_random_uuid(),
                DATE '2026-11-06', 'SHIPPED');
        RAISE EXCEPTION 'V0283 自检失败：status = SHIPPED 未被 ck_shipment_status 拒绝';
    EXCEPTION WHEN check_violation THEN
        NULL;   -- 期望路径
    END;

    --     4.8b total_qty 为负 → 必须被拒
    BEGIN
        INSERT INTO biz_shipment (ship_no, so_id, customer_id, warehouse_id, ship_date, total_qty)
        VALUES ('__V0283_PROBE_NEG__', probe_order, gen_random_uuid(), gen_random_uuid(),
                DATE '2026-11-06', -1);
        RAISE EXCEPTION 'V0283 自检失败：total_qty = -1 未被 ck_shipment_total_qty 拒绝';
    EXCEPTION WHEN check_violation THEN
        NULL;   -- 期望路径
    END;

    --     4.8c 正常单头：默认值探针（status / total_qty 都给默认 → 必须能插入）
    INSERT INTO biz_shipment (id, ship_no, so_id, customer_id, warehouse_id, ship_date)
    VALUES (probe_ship, '__V0283_PROBE_OK__', probe_order, gen_random_uuid(), gen_random_uuid(),
            DATE '2026-11-06');
    SELECT status || '|' || total_qty::text INTO d FROM biz_shipment WHERE id = probe_ship;
    IF d <> 'DRAFT|0.0000' THEN
        RAISE EXCEPTION 'V0283 自检失败：不传 status/total_qty 时落库值 = %（期望 DRAFT|0.0000）', d;
    END IF;

    --     4.8d 明细 qty = 0 → 必须被拒（卡面 qty > 0）
    BEGIN
        INSERT INTO biz_shipment_item (shipment_id, so_item_id, material_id, qty)
        VALUES (probe_ship, probe_item, gen_random_uuid(), 0);
        RAISE EXCEPTION 'V0283 自检失败：qty = 0 未被 ck_biz_shipment_item_qty 拒绝';
    EXCEPTION WHEN check_violation THEN
        NULL;   -- 期望路径
    END;

    --     4.8e 明细指向不存在的订单行 → 必须被外键拦住
    BEGIN
        INSERT INTO biz_shipment_item (shipment_id, so_item_id, material_id, qty)
        VALUES (probe_ship, '00000000-0000-4000-8000-0000ffffffff'::uuid, gen_random_uuid(), 1);
        RAISE EXCEPTION 'V0283 自检失败：so_item_id 指向不存在的订单行却插入成功';
    EXCEPTION WHEN foreign_key_violation THEN
        NULL;   -- 期望路径
    END;

    --     4.8f 明细正常插入 + serial_nos 数组可读写
    INSERT INTO biz_shipment_item (shipment_id, so_item_id, material_id, qty, location_id, serial_nos)
    VALUES (probe_ship, probe_item, gen_random_uuid(), 4, gen_random_uuid(),
            ARRAY['SN-0001','SN-0002','SN-0003','SN-0004']);
    SELECT array_length(serial_nos, 1)::text INTO d
      FROM biz_shipment_item WHERE shipment_id = probe_ship;
    IF d <> '4' THEN
        RAISE EXCEPTION 'V0283 自检失败：serial_nos 数组长度读回 = %（期望 4）', d;
    END IF;

    --     4.8g 订单行被发货单引用时不许删（RESTRICT）
    --     ⚠ `ON DELETE RESTRICT` 抛的是 SQLSTATE 23001 `restrict_violation`，
    --       不是 23503 `foreign_key_violation`（`NO ACTION` 才是后者）——
    --       只写 foreign_key_violation 会让这段 handler 形同虚设、整个 DO 块直接炸。
    BEGIN
        DELETE FROM biz_sales_order_item WHERE id = probe_item;
        RAISE EXCEPTION 'V0283 自检失败：被发货单引用的订单行居然被删掉了（RESTRICT 未生效）';
    EXCEPTION WHEN restrict_violation OR foreign_key_violation THEN
        NULL;   -- 期望路径
    END;

    --     4.8h 级联：物理删单头 → 明细必须一起消失（探针明细由此清场）
    DELETE FROM biz_shipment WHERE id = probe_ship;
    SELECT count(*) INTO n FROM biz_shipment_item WHERE shipment_id = probe_ship;
    IF n <> 0 THEN
        RAISE EXCEPTION 'V0283 自检失败：单头删除后明细仍剩 % 行（ON DELETE CASCADE 未生效）', n;
    END IF;

    --     4.8i 单号唯一：存活行重号必须被拒
    INSERT INTO biz_shipment (id, ship_no, so_id, customer_id, warehouse_id, ship_date)
    VALUES ('d0283fff-0000-4000-8000-000000000004'::uuid, '__V0283_PROBE_UK__',
            probe_order, gen_random_uuid(), gen_random_uuid(), DATE '2026-11-06');
    BEGIN
        INSERT INTO biz_shipment (ship_no, so_id, customer_id, warehouse_id, ship_date)
        VALUES ('__V0283_PROBE_UK__', probe_order, gen_random_uuid(), gen_random_uuid(), DATE '2026-11-06');
        RAISE EXCEPTION 'V0283 自检失败：存活行重号未被 uk_biz_shipment_no 拒绝';
    EXCEPTION WHEN unique_violation THEN
        NULL;   -- 期望路径
    END;
    --     4.8j 软删之后同一单号可复用（部分唯一索引语义）
    UPDATE biz_shipment SET deleted_at = now()
     WHERE ship_no = '__V0283_PROBE_UK__' AND deleted_at IS NULL;
    INSERT INTO biz_shipment (so_id, customer_id, warehouse_id, ship_date, ship_no)
    VALUES (probe_order, gen_random_uuid(), gen_random_uuid(), DATE '2026-11-06', '__V0283_PROBE_UK__');
    SELECT count(*) INTO n FROM biz_shipment
     WHERE ship_no = '__V0283_PROBE_UK__' AND deleted_at IS NULL;
    IF n <> 1 THEN
        RAISE EXCEPTION 'V0283 自检失败：软删后同号重插后存活行 = %（期望 1）', n;
    END IF;

    --     4.8k 清场：删订单（先删发货单）→ 探针行全部消失
    DELETE FROM biz_shipment WHERE so_id = probe_order;
    DELETE FROM biz_sales_order WHERE id = probe_order;
    SELECT (SELECT count(*) FROM biz_shipment WHERE ship_no LIKE '__V0283%')
         + (SELECT count(*) FROM biz_sales_order WHERE so_no LIKE '__V0283%')
      INTO n;
    IF n <> 0 THEN
        RAISE EXCEPTION 'V0283 自检失败：探针残留 % 行', n;
    END IF;

    -- 4.9 取号规则：SH 行在位且口径正确（前缀 / 日号 / 分隔符 / 步长 / 启用）
    SELECT prefix || '|' || date_pattern || '|' || separator || '|' || step || '|' || status
      INTO d
      FROM biz_seq WHERE seq_type = 'SH' AND deleted_at IS NULL;
    IF d IS NULL THEN
        RAISE EXCEPTION 'V0283 自检失败：biz_seq(seq_type=SH) 取号规则行缺失';
    END IF;
    IF d <> 'SH|yyyyMMdd|-|1|1' THEN
        RAISE EXCEPTION 'V0283 自检失败：SH 取号规则口径不对（% ≠ SH|yyyyMMdd|-|1|1）', d;
    END IF;
END $$;

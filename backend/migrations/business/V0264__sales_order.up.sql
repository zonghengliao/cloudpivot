-- ======================================================
-- V0264：销售链 —— 销售订单两张表 + `SO` 取号规则（SO-04）
--
--   ① biz_sales_order       销售订单主表（单号部分唯一 / 状态机 DRAFT 起 / 价税合计恒等）
--   ② biz_sales_order_item  销售订单明细（真外键 → biz_sales_order.id，超发防线 delivered_qty <= qty）
--   ③ biz_seq(seq_type='SO') 单号取号规则：SO-yyyyMMdd-000001（按天滚动）
--
-- ── 号段（开工实测复核）──────────────────────────────────
-- 写入本文件时 backend/migrations/business/ 最大号 = V0263
--   （V0262/V0263 是客户档案前端菜单卡 t_3f4c05ae 用掉的），
-- 本卡顺延取 V0264。卡面写的「V0261」已被 V0261__sales_customer_permissions 占用
-- （客户权限码），**不能复用号段**（已应用迁移不可改、指纹校验会炸）。
-- 迁移号会被并行卡抢走，**提交前**再 `ls business | tail` 复核一次。
--
-- ── 表名 / 列名口径（卡面为准，与 docs 旧稿不一致的地方逐条记在这里）──────
--  1) 表名 `biz_sales_order` / `biz_sales_order_item`（卡面逐字）。
--     `docs/ERP-批次2-技术架构.md` §2.2 写的是 `biz_sale_order`（单数 sale）——那是
--     批次 2 设计稿，本卡按卡面落地；**后续卡（SO-05 及以后）一律按本表名**，
--     别再照技术架构稿写出 `biz_sale_order`（会静默建出第二套空表）。
--  2) 列名 `so_no` / `so_id` / `delivered_qty`（卡面逐字）；技术架构稿对应
--     `order_no` / `order_id` / `shipped_qty`。同一份稿的 §2.2.1 状态值也是
--     `PARTIAL_SHIPPED` / `SHIPPED`，而 PRD（产品真相，§2.2 S2-07/S2-08 + §4.13.3
--     状态流转图 + 算例 §4.13.4）**全文一致**用 `PARTIAL_OUT` / `OUTBOUND`。
--     ⇒ 状态域以 PRD 为准（7 值，见下），列名以卡面为准。三处口径不许再各写一套。
--  3) `customer_id` 是**弱引用**（无外键）：与 `biz_purchase_order.supplier_id` 同风格，
--     客户走软删（V0260），外键会把「客户软删但历史单据要留」这条业务堵死；
--     引用有效性由 handler 建单时校验（SO-05）。
--     明细 `so_id` 反过来是**硬外键 ON DELETE RESTRICT 的例外：CASCADE** —— 明细离开
--     单头没有独立意义，单头物理删（仅测试/清场）时明细必须一起走。
--
-- ── 状态域（`ck_sales_order_status`，7 值，与 PRD §4.13.3 逐字一致）──────
--     DRAFT → SUBMITTED → APPROVED → {PARTIAL_OUT, OUTBOUND}；CLOSED / CANCELLED 终态。
--     本卡只建表，**不建状态迁移入口**：迁移边必须登记到 `shared/state_machine`
--     （`DocKind::SaleOrder` + `SALES_ORDER_EDGES`）后由 SO-05 复用，届时 LEGAL_EDGES
--     与本 CHECK 的域值要逐字对齐（本文件末尾自检会在域值漂移时炸）。
--     `PARTIAL_OUT` / `OUTBOUND` 由出库过账回写（S3-05 第 9 步），不是人工点出来的。
--
-- ── 金额恒等（卡面点名 `ck_sales_order_amount`）──────────────────
--     `total_with_tax = total_amount + total_tax`（**精确恒等，不是近似**），
--     与 `ck_biz_po_tax_sum` / `ck_biz_so_item_tax` 同口径：算错就是 23514（400），
--     不是 500。金额列全 `NUMERIC(18,4)`，单价 `NUMERIC(18,6)`、税率 `NUMERIC(7,4)`
--     （百分数 0–100，与 `biz_stock_in_item.tax_rate` 同口径）。
--
-- ── 幂等 ──────────────────────────────────────────────
-- CREATE TABLE / INDEX IF NOT EXISTS；biz_seq 种子 `INSERT … WHERE NOT EXISTS`。
-- 末尾 DO 块自检：表 / 列数 / 索引 / 外键级联 / CHECK 行为（插坏数据必须被拒）/
-- 默认值（status=DRAFT、delivered_qty=0）/ 取号规则行，任一不符 → 迁移失败。
-- ======================================================

-- ---------- ① biz_sales_order（销售订单主表） ----------
CREATE TABLE IF NOT EXISTS biz_sales_order (
    id             UUID PRIMARY KEY DEFAULT gen_random_uuid(),          -- 主键
    so_no          VARCHAR(32)   NOT NULL,                              -- 销售订单号（biz_seq 生成，存活行唯一）
    customer_id    UUID          NOT NULL,                              -- 客户（弱引用 biz_customer.id）
    order_date     DATE          NOT NULL,                              -- 订单日期
    delivery_date  DATE,                                                -- 要求交期（可空：未约定）
    status         VARCHAR(24)   NOT NULL DEFAULT 'DRAFT',               -- 状态机：DRAFT/SUBMITTED/APPROVED/PARTIAL_OUT/OUTBOUND/CLOSED/CANCELLED
    total_amount   NUMERIC(18,4) NOT NULL DEFAULT 0,                    -- 不含税金额合计
    total_tax      NUMERIC(18,4) NOT NULL DEFAULT 0,                    -- 税额合计
    total_with_tax NUMERIC(18,4) NOT NULL DEFAULT 0,                    -- 价税合计
    remark         TEXT,                                                -- 备注
    created_at     TIMESTAMPTZ   NOT NULL DEFAULT CURRENT_TIMESTAMP,     -- 审计：创建时间
    updated_at     TIMESTAMPTZ   NOT NULL DEFAULT CURRENT_TIMESTAMP,     -- 审计：更新时间（model 层维护）
    create_by      UUID,                                                -- 审计：创建人
    update_by      UUID,                                                -- 审计：更新人
    deleted_at     TIMESTAMPTZ,                                         -- 软删时间（NULL = 存活）

    CONSTRAINT ck_sales_order_status
        CHECK (status IN ('DRAFT','SUBMITTED','APPROVED','PARTIAL_OUT','OUTBOUND','CLOSED','CANCELLED')),
    CONSTRAINT ck_sales_order_amount_nonneg
        CHECK (total_amount >= 0 AND total_tax >= 0 AND total_with_tax >= 0),
    -- 价税合计恒等（卡面点名约束名）：数据库层的会计兜底，防程序算错
    CONSTRAINT ck_sales_order_amount
        CHECK (total_with_tax = total_amount + total_tax)
);

COMMENT ON TABLE  biz_sales_order IS '销售订单主表（status 为 VARCHAR 状态机；ck_sales_order_amount 是会计兜底）';
COMMENT ON COLUMN biz_sales_order.so_no IS '销售订单号：SO-yyyyMMdd-000001（biz_seq seq_type=SO 取号，按 CST 的天滚动）';
COMMENT ON COLUMN biz_sales_order.customer_id IS '客户 id（弱引用 biz_customer.id，无外键：客户软删后历史单据仍要可读）';
COMMENT ON COLUMN biz_sales_order.status IS '状态机：DRAFT/SUBMITTED/APPROVED/PARTIAL_OUT/OUTBOUND/CLOSED/CANCELLED（PRD §4.13.3 逐字）';
COMMENT ON COLUMN biz_sales_order.total_with_tax IS '价税合计 = total_amount + total_tax（ck_sales_order_amount 精确恒等）';
COMMENT ON COLUMN biz_sales_order.deleted_at IS '软删标记；单号唯一索引带 WHERE deleted_at IS NULL ⇒ 软删后单号可复用（PRD S2-09）';

-- 单号唯一：**部分唯一索引**（存活行唯一），与 uk_biz_po_no / uk_biz_customer_code 同口径
-- ⚠ 索引名必须带 `sales_order` 全词：`uk_biz_so_no` / `idx_biz_so_status` / `idx_biz_so_date`
--   已被**既有** `biz_stock_out` 占用（它有 `so_no` 列 = 来源销售单号）。
--   PG 的索引名是**库内 schema 级唯一**，重名 + `IF NOT EXISTS` 会被**静默跳过** ⇒
--   本表的唯一索引根本不会建出来（首版实测就栽在这，自检 4.4 报「uk_biz_so_no 缺失」）。
CREATE UNIQUE INDEX IF NOT EXISTS uk_biz_sales_order_no
    ON biz_sales_order (so_no) WHERE deleted_at IS NULL;
CREATE INDEX IF NOT EXISTS idx_biz_sales_order_customer ON biz_sales_order (customer_id, order_date DESC);
CREATE INDEX IF NOT EXISTS idx_biz_sales_order_status   ON biz_sales_order (status);
CREATE INDEX IF NOT EXISTS idx_biz_sales_order_date     ON biz_sales_order (order_date DESC);

-- ---------- ② biz_sales_order_item（销售订单明细） ----------
CREATE TABLE IF NOT EXISTS biz_sales_order_item (
    id             UUID PRIMARY KEY DEFAULT gen_random_uuid(),          -- 主键
    so_id          UUID          NOT NULL,                              -- 所属销售订单（硬外键）
    line_no        INTEGER       NOT NULL,                              -- 行号（1 起，单内唯一）
    material_id    UUID          NOT NULL,                              -- 物料（弱引用 biz_material.id）
    qty            NUMERIC(18,4) NOT NULL,                              -- 订单数量（> 0）
    price          NUMERIC(18,6) NOT NULL DEFAULT 0,                    -- 单价（不含税，>= 0）
    amount         NUMERIC(18,4) NOT NULL DEFAULT 0,                    -- 行不含税金额
    tax_rate       NUMERIC(7,4)  NOT NULL DEFAULT 0,                    -- 税率百分数（0–100）
    tax_amount     NUMERIC(18,4) NOT NULL DEFAULT 0,                    -- 行税额
    delivered_qty  NUMERIC(18,4) NOT NULL DEFAULT 0,                    -- 已发货数量（部分发货累计，0..qty）
    created_at     TIMESTAMPTZ   NOT NULL DEFAULT CURRENT_TIMESTAMP,
    updated_at     TIMESTAMPTZ   NOT NULL DEFAULT CURRENT_TIMESTAMP,

    CONSTRAINT fk_biz_sales_order_item_order
        FOREIGN KEY (so_id) REFERENCES biz_sales_order (id) ON DELETE CASCADE,
    CONSTRAINT ck_sales_order_item_line
        CHECK (line_no >= 1),
    CONSTRAINT ck_sales_order_item_qty
        CHECK (qty > 0),
    CONSTRAINT ck_sales_order_item_price
        CHECK (price >= 0),
    CONSTRAINT ck_sales_order_item_rate
        CHECK (tax_rate >= 0 AND tax_rate <= 100),
    CONSTRAINT ck_sales_order_item_amount
        CHECK (amount >= 0 AND tax_amount >= 0),
    -- 超发防线：已发数量既不能为负，也不能超过订单数量（出库过账的累加目标）
    CONSTRAINT ck_sales_order_item_delivered
        CHECK (delivered_qty >= 0 AND delivered_qty <= qty)
);

COMMENT ON TABLE  biz_sales_order_item IS '销售订单明细（真外键 ON DELETE CASCADE；delivered_qty 是部分发货/超发控制的累计列）';
COMMENT ON COLUMN biz_sales_order_item.delivered_qty IS
    '已发货数量（出库过账累加）；ck_sales_order_item_delivered 挡住负值与超发（delivered_qty <= qty）';
COMMENT ON COLUMN biz_sales_order_item.tax_rate IS '税率百分数 0–100（与 biz_stock_in_item.tax_rate 同口径，税额 = amount × rate / 100）';

CREATE UNIQUE INDEX IF NOT EXISTS uk_biz_sales_order_item_line
    ON biz_sales_order_item (so_id, line_no);
CREATE INDEX IF NOT EXISTS idx_biz_sales_order_item_material
    ON biz_sales_order_item (material_id);

-- ---------- ③ 取号规则：SO（销售订单号） ----------
INSERT INTO biz_seq (id, seq_type, seq_name, prefix, date_pattern, period_key, current_no, step, separator, status, remark)
SELECT v.id::uuid, 'SO', '销售订单号', 'SO', 'yyyyMMdd', '', 0, 1, '-', 1,
       '销售订单单号：SO-yyyyMMdd-000001，按天（CST）滚动'
  FROM (VALUES ('d0264000-0000-4000-8000-000000000001')) AS v(id)
 WHERE NOT EXISTS (SELECT 1 FROM biz_seq q WHERE q.seq_type = 'SO' AND q.deleted_at IS NULL);

-- ---------- ④ 自检：结构 + 域值行为 + 默认值 + 取号规则 ----------
DO $$
DECLARE
    n INT;
    d TEXT;
    probe_order UUID := 'd0264fff-0000-4000-8000-000000000001'::uuid;
    probe_item  UUID := 'd0264fff-0000-4000-8000-000000000002'::uuid;
BEGIN
    -- 4.1 两张表都在
    IF to_regclass('public.biz_sales_order') IS NULL
       OR to_regclass('public.biz_sales_order_item') IS NULL THEN
        RAISE EXCEPTION 'V0264 自检失败：biz_sales_order / biz_sales_order_item 未建全';
    END IF;

    -- 4.2 列数逐表比对（多一列少一列都算漂移）
    SELECT count(*) INTO n FROM information_schema.columns
     WHERE table_schema = 'public' AND table_name = 'biz_sales_order';
    IF n <> 15 THEN
        RAISE EXCEPTION 'V0264 自检失败：biz_sales_order 列数 = %（期望 15）', n;
    END IF;
    SELECT count(*) INTO n FROM information_schema.columns
     WHERE table_schema = 'public' AND table_name = 'biz_sales_order_item';
    IF n <> 12 THEN
        RAISE EXCEPTION 'V0264 自检失败：biz_sales_order_item 列数 = %（期望 12）', n;
    END IF;

    -- 4.3 卡面点名的列必须逐字在位
    SELECT count(*) INTO n FROM information_schema.columns
     WHERE table_schema = 'public' AND table_name = 'biz_sales_order'
       AND column_name IN ('id','so_no','customer_id','order_date','delivery_date','status',
                           'total_amount','total_tax','total_with_tax','remark',
                           'created_at','updated_at','create_by','update_by','deleted_at');
    IF n <> 15 THEN
        RAISE EXCEPTION 'V0264 自检失败：biz_sales_order 卡面列只齐了 %/15', n;
    END IF;
    SELECT count(*) INTO n FROM information_schema.columns
     WHERE table_schema = 'public' AND table_name = 'biz_sales_order_item'
       AND column_name IN ('id','so_id','line_no','material_id','qty','price','amount',
                           'tax_rate','tax_amount','delivered_qty','created_at','updated_at');
    IF n <> 12 THEN
        RAISE EXCEPTION 'V0264 自检失败：biz_sales_order_item 卡面列只齐了 %/12', n;
    END IF;

    -- 4.4 单号唯一索引必须是「存活行唯一」的部分索引（软删后单号可复用）
    --     ⚠ 索引名不能叫 uk_biz_so_no：那个名字已被 biz_stock_out 占用，重名会被 IF NOT EXISTS 静默跳过
    SELECT indexdef INTO d FROM pg_indexes
     WHERE schemaname = 'public' AND tablename = 'biz_sales_order' AND indexname = 'uk_biz_sales_order_no';
    IF d IS NULL THEN
        RAISE EXCEPTION 'V0264 自检失败：唯一索引 uk_biz_sales_order_no 缺失（是否与 biz_stock_out 上的同名索引撞名被跳过？）';
    END IF;
    IF d NOT LIKE '%UNIQUE%' OR d NOT LIKE '%deleted_at IS NULL%' THEN
        RAISE EXCEPTION 'V0264 自检失败：uk_biz_sales_order_no 不是「存活行唯一」的部分索引（def=%）', d;
    END IF;

    -- 4.5 明细外键存在且是 ON DELETE CASCADE
    SELECT pg_get_constraintdef(oid) INTO d FROM pg_constraint
     WHERE conname = 'fk_biz_sales_order_item_order' AND contype = 'f';
    IF d IS NULL THEN
        RAISE EXCEPTION 'V0264 自检失败：外键 fk_biz_sales_order_item_order 缺失';
    END IF;
    IF d NOT LIKE '%REFERENCES biz_sales_order(id)%' OR d NOT LIKE '%ON DELETE CASCADE%' THEN
        RAISE EXCEPTION 'V0264 自检失败：明细外键未指向 biz_sales_order(id) 或不是 CASCADE（def=%）', d;
    END IF;

    -- 4.6 三条卡面约束在位，状态域字面量齐全
    IF NOT EXISTS (SELECT 1 FROM pg_constraint WHERE conname = 'ck_sales_order_amount') THEN
        RAISE EXCEPTION 'V0264 自检失败：ck_sales_order_amount 缺失（卡面点名约束名）';
    END IF;
    IF NOT EXISTS (SELECT 1 FROM pg_constraint WHERE conname = 'ck_sales_order_item_delivered') THEN
        RAISE EXCEPTION 'V0264 自检失败：ck_sales_order_item_delivered 缺失';
    END IF;
    SELECT pg_get_constraintdef(oid) INTO d FROM pg_constraint WHERE conname = 'ck_sales_order_status';
    IF d IS NULL THEN
        RAISE EXCEPTION 'V0264 自检失败：ck_sales_order_status 缺失';
    END IF;
    -- 域值逐字（与 PRD §4.13.3 / sales_model::STATUSES 同序）
    FOREACH d IN ARRAY ARRAY['DRAFT','SUBMITTED','APPROVED','PARTIAL_OUT','OUTBOUND','CLOSED','CANCELLED']
    LOOP
        IF (SELECT pg_get_constraintdef(oid) FROM pg_constraint WHERE conname = 'ck_sales_order_status')
           NOT LIKE '%' || d || '%' THEN
            RAISE EXCEPTION 'V0264 自检失败：ck_sales_order_status 缺域值 %', d;
        END IF;
    END LOOP;

    -- 4.7 默认值：status='DRAFT'、delivered_qty=0（漏 DEFAULT 会让最简插入直接 23502）
    SELECT column_default INTO d FROM information_schema.columns
     WHERE table_schema = 'public' AND table_name = 'biz_sales_order' AND column_name = 'status';
    IF d IS NULL OR d NOT LIKE '''DRAFT''%' THEN
        RAISE EXCEPTION 'V0264 自检失败：biz_sales_order.status 默认值不是 DRAFT（default=%）', d;
    END IF;
    SELECT column_default INTO d FROM information_schema.columns
     WHERE table_schema = 'public' AND table_name = 'biz_sales_order_item' AND column_name = 'delivered_qty';
    IF d IS NULL OR d NOT LIKE '0%' THEN
        RAISE EXCEPTION 'V0264 自检失败：biz_sales_order_item.delivered_qty 默认值不是 0（default=%）', d;
    END IF;

    -- 4.8 行为探针（插坏数据必须被拒；探针行末尾随单头删除/自带回滚，不留残渣）
    --     4.8a 价税合计不等于「金额 + 税额」→ 必须 check_violation
    BEGIN
        INSERT INTO biz_sales_order (so_no, customer_id, order_date, total_amount, total_tax, total_with_tax)
        VALUES ('__V0264_PROBE_AMOUNT__', gen_random_uuid(), DATE '2026-11-06', 100, 13, 114);
        RAISE EXCEPTION 'V0264 自检失败：total_with_tax(114) <> 100+13 未被 ck_sales_order_amount 拒绝';
    EXCEPTION WHEN check_violation THEN
        NULL;   -- 期望路径
    END;

    --     4.8b 状态越界值 → 必须被拒
    BEGIN
        INSERT INTO biz_sales_order (so_no, customer_id, order_date, status)
        VALUES ('__V0264_PROBE_STATUS__', gen_random_uuid(), DATE '2026-11-06', 'SHIPPED');
        RAISE EXCEPTION 'V0264 自检失败：status = SHIPPED（技术架构旧稿值）未被 ck_sales_order_status 拒绝';
    EXCEPTION WHEN check_violation THEN
        NULL;   -- 期望路径
    END;

    --     4.8c 正常单头：默认值探针（status / 金额三项都给默认 → 必须能插入）
    INSERT INTO biz_sales_order (id, so_no, customer_id, order_date)
    VALUES (probe_order, '__V0264_PROBE_OK__', gen_random_uuid(), DATE '2026-11-06');
    SELECT status INTO d FROM biz_sales_order WHERE id = probe_order;
    IF d <> 'DRAFT' THEN
        RAISE EXCEPTION 'V0264 自检失败：不传 status 时落库值 = %（期望 DRAFT）', d;
    END IF;

    --     4.8d 明细默认值 + 卡面 qty 约束：qty = 0 必须被拒
    BEGIN
        INSERT INTO biz_sales_order_item (so_id, line_no, material_id, qty)
        VALUES (probe_order, 1, gen_random_uuid(), 0);
        RAISE EXCEPTION 'V0264 自检失败：qty = 0 未被 ck_sales_order_item_qty 拒绝';
    EXCEPTION WHEN check_violation THEN
        NULL;   -- 期望路径
    END;

    --     4.8e 超发：delivered_qty > qty 必须被拒
    BEGIN
        INSERT INTO biz_sales_order_item (so_id, line_no, material_id, qty, delivered_qty)
        VALUES (probe_order, 1, gen_random_uuid(), 10, 11);
        RAISE EXCEPTION 'V0264 自检失败：delivered_qty(11) > qty(10) 未被 ck_sales_order_item_delivered 拒绝';
    EXCEPTION WHEN check_violation THEN
        NULL;   -- 期望路径
    END;

    --     4.8f 明细正常插入：delivered_qty 默认 0
    INSERT INTO biz_sales_order_item (id, so_id, line_no, material_id, qty, price, amount, tax_rate, tax_amount)
    VALUES (probe_item, probe_order, 1, gen_random_uuid(), 10, 100.000000, 1000.0000, 13.0000, 130.0000);
    SELECT delivered_qty::text INTO d FROM biz_sales_order_item WHERE id = probe_item;
    IF d NOT IN ('0.0000', '0') THEN
        RAISE EXCEPTION 'V0264 自检失败：不传 delivered_qty 时落库值 = %（期望 0）', d;
    END IF;

    --     4.8g 孤儿明细（so_id 指向不存在单头）必须被外键拦住
    BEGIN
        INSERT INTO biz_sales_order_item (so_id, line_no, material_id, qty)
        VALUES ('00000000-0000-4000-8000-0000ffffffff'::uuid, 1, gen_random_uuid(), 1);
        RAISE EXCEPTION 'V0264 自检失败：so_id 指向不存在的单头却插入成功（外键未生效）';
    EXCEPTION WHEN foreign_key_violation THEN
        NULL;   -- 期望路径
    END;

    --     4.8h 级联：物理删单头 → 明细必须一起消失（探针行由此清场）
    DELETE FROM biz_sales_order WHERE id = probe_order;
    SELECT count(*) INTO n FROM biz_sales_order_item WHERE so_id = probe_order;
    IF n <> 0 THEN
        RAISE EXCEPTION 'V0264 自检失败：单头删除后明细仍剩 % 行（ON DELETE CASCADE 未生效）', n;
    END IF;
    SELECT count(*) INTO n FROM biz_sales_order WHERE id = probe_order;
    IF n <> 0 THEN
        RAISE EXCEPTION 'V0264 自检失败：探针单头未清掉';
    END IF;

    -- 4.9 取号规则：SO 行在位且口径正确（前缀 / 日号 / 分隔符 / 步长 / 启用）
    SELECT prefix || '|' || date_pattern || '|' || separator || '|' || step || '|' || status
      INTO d
      FROM biz_seq WHERE seq_type = 'SO' AND deleted_at IS NULL;
    IF d IS NULL THEN
        RAISE EXCEPTION 'V0264 自检失败：biz_seq(seq_type=SO) 取号规则行缺失';
    END IF;
    IF d <> 'SO|yyyyMMdd|-|1|1' THEN
        RAISE EXCEPTION 'V0264 自检失败：SO 取号规则口径不对（% ≠ SO|yyyyMMdd|-|1|1）', d;
    END IF;
END $$;

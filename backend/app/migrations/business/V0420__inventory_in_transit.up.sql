-- ============================================================================
-- V0420__inventory_in_transit（M2-01 · 在途库存：表结构 biz_inventory_in_transit · 2026-11-16）
--
-- 卡：t_7ba5b0e2（M2-01【M2·仓库】在途库存：表结构 biz_inventory_in_transit）
-- 拆卡依据 `docs/planning/20261113-新卡清单.md` §5 M2 仓库 的 M2-01 行；
-- 下游：M2-02（发货单过账 → 在途 +）、M2-03（签收确认 → 在途转已出库）、
--       M2-04（在途查询）、M2-05（对账：在途 + 已出 = 发货总量）都读/写本表。
--
-- 【本卡只做一件事】建**一张**表：在途库存账。
--   一行 = 物料 × 发货仓 × 发货单（卡面原话「按物料+仓库+发货单记录在途量」）。
--   不种数据、不种权限码（还没有接口，码要跟接口一起种，见 AGENTS.md 约定 7）、
--   不动任何存量表（`biz_inventory` / `biz_inventory_txn` / `biz_shipment` 一律不 ALTER）。
--
-- ── 为什么另建一张表，而不是写进 biz_inventory / biz_inventory_txn ──────────
--   ① `biz_inventory` 是**结存账**：一个 (物料, 仓库) 一行，记「现在仓里有多少」。
--      在途不是仓里的量（货在路上、账上不扣库存，卡面 M2-02 原话），塞进去就等于
--      虚增可用库存 —— 出库、盘点、可用量全都会算错。
--   ② `biz_inventory_txn` 是**流水**：记「某天动了多少」，没有「这一批还在路上、
--      已签收多少」的状态，而 M2-03 要把在途一笔一笔转成已出库、M2-05 要对账
--      「在途 + 已出 = 发货总量」，需要一个**有状态、可累加签收**的余量行。
--      流水里凑出余量 = 每次查询都做一遍全表累加，且作废/红冲口径一改就静默算错。
--   ③ 三本账（`biz_inventory` / `biz_inventory_location` / `biz_inventory_txn`）
--      的唯一写入口是 `erp/inventory_writer.rs`（P2-5A 门禁）。在途账**不属于**三本账：
--      它不影响库存数量、不生成出入库流水、不参与移动加权成本，所以独立成表，
--      也刻意不进那道写入口闸（`inventory_write_gate.py` 的表名正则
--      `biz_inventory(?:_location|_txn)?` 带 `(?!\w)` 收尾，不会误判本表）。
--
-- ── 三个数量列与「在途 = 发货 − 已签收」这条恒等式 ─────────────────────────
--   `shipped_qty`     发货数量：M2-02 过账时从发货单明细汇总写入，**之后只读**。
--   `received_qty`    累计签收：M2-03 每次签收 `+= n`（不新开行）。
--   `qty_in_transit`  **当前在途量 = shipped_qty − received_qty**，
--                     用 `GENERATED ALWAYS AS … STORED` 由 PG 算，任何写入路径都
--                     不可能写出「在途与两个数不符」的行（与 V0211 的
--                     `biz_stock_take_item.diff_qty` 同款技法，全仓先例就在那里）。
--   为什么不在 Rust 里算：M2-04/M2-05 有多条读路径（单据视角、物料视角、对账），
--   只要有一处写 `qty = qty - n` 而另一处只 `received_qty += n`，
--   两处对「还剩多少在途」就会给出两个答案，而且**不会报错**。
--   生成列把口径收成一份：`M2-05 对账式` 变成 `sum(shipped_qty) = sum(received_qty) + sum(qty_in_transit)`
--   恒成立，剩下的只需对 `sum(shipped_qty)` 与发货单明细对拍。
--
-- ── 状态域（`ck_biz_inv_transit_status`，3 值）──────────────────────────────
--   IN_TRANSIT  在途：`received_qty < shipped_qty`（部分签收时仍是在途，余量就是差值）
--   RECEIVED    全部签收：`received_qty = shipped_qty` 且 `received_at` 必填
--   CANCELLED   已作废：发货单作废/退货，本行不再算在途
--   ⚠ 作废行**保留**（审计：谁在哪天把哪一批在途作废），但 `qty_in_transit` 仍是
--     生成值（作废时不动 shipped/received，因为货「发出过」是事实）。因此
--     **所有在途取数与对账一律带 `status <> 'CANCELLED'`** —— 这条口径写在
--     `COMMENT ON COLUMN biz_inventory_in_transit.status` 上，M2-04/M2-05 照它实现。
--   状态与数量的一致性用 `ck_biz_inv_transit_state` 在库层钉死：IN_TRANSIT 必须还有余量、
--   RECEIVED 必须余量为 0、CANCELLED 不限（它在量上就是「不算」）。
--   ⇒ **M2-03 的签收必须在同一条 UPDATE 里同时推进 `received_qty` 与 `status`**
--     （`SET received_qty = received_qty + n, status = CASE WHEN received_qty + n = shipped_qty
--       THEN 'RECEIVED' ELSE 'IN_TRANSIT' END`）。分两句写会在第一句就被 CHECK 拦下 ——
--     这正是要的性质：不存在「签满了还挂在 IN_TRANSIT」的中间态被提交出去。
--   为什么 CHECK 不直接引用生成列：写检查条件用的是 `shipped_qty` / `received_qty`
--   两个基列（等价、且不依赖 PG 对「CHECK 能否引用生成列」的版本差异）。
--
-- ── 唯一性 = 写入语义（M2-02 的 upsert 键）──────────────────────────────────
--   `uk_biz_inv_transit_key (material_id, warehouse_id, shipment_id)`
--     ⇒ 同一发货单里同一物料的在途只有一行：M2-02 过账就是
--       `INSERT … ON CONFLICT (material_id, warehouse_id, shipment_id) DO UPDATE
--        SET shipped_qty = biz_inventory_in_transit.shipped_qty + EXCLUDED.shipped_qty`
--       （部分过账 / 重算只加一次，靠这张唯一键兜住）。
--     两行的后果：同一批发货被算两遍在途，谁都不会发现。
--   本表**没有 `deleted_at`**（不是漏写）：
--     · 在途行是发货单的派生账，没有「删一行」的业务动作 —— 作废是状态（CANCELLED）；
--     · 发货单物理删（`biz_shipment` 上 `ON DELETE CASCADE`）时本表行一起消失，
--       不会留孤儿（与 `biz_shipment_item` 同口径）；
--     · 留 `deleted_at` 会造出「这行没删但也不算在途」的第二种隐藏状态，
--       查询要同时带 `deleted_at IS NULL AND status <> 'CANCELLED'`，漏一处就静默多算。
--   因此唯一键是**普通**唯一约束（不需要 `WHERE deleted_at IS NULL` 的部分索引）。
--
-- ── 外键口径（与 V0283 发货单同风格）────────────────────────────────────────
--   `shipment_id → biz_shipment(id) ON DELETE CASCADE`：明细级派生账，单头物理删即消失。
--   `material_id` / `warehouse_id` 是**弱引用**（无外键）：物料/仓库可软删，
--   历史在途账必须永远可读（AGENTS.md 约定 13 / 弱引用口径）。
--
-- ── 幂等 / 回滚 ───────────────────────────────────────────────────────────
--   `CREATE TABLE IF NOT EXISTS` + 全部 `CREATE INDEX IF NOT EXISTS`；
--   自检块可反复执行（探针发货单号 `__M201_PROBE_*`，块尾自清场 ⇒ 重放零残留）。
--   down 幂等（`DROP TABLE IF EXISTS`），并加护栏：表里已有非作废在途行时**拒绝回滚**
--   （在途账是 M2-02..M2-05 的工作数据，先由业务把在途签收/作废干净再回滚）。
-- ============================================================================

-- ---------------------------------------------------------------------------
-- ① 前置护栏：在途账依赖的三张表必须已经在（缺表说明迁移没跑到 V0283 / V0137）
-- ---------------------------------------------------------------------------
DO $$
BEGIN
    IF to_regclass('public.biz_shipment') IS NULL THEN
        RAISE EXCEPTION 'V0420: biz_shipment 不存在，请先应用 V0283（销售发货单两张表）';
    END IF;
    IF to_regclass('public.biz_material') IS NULL THEN
        RAISE EXCEPTION 'V0420: biz_material 不存在，请先应用 V0137（ERP 主数据与仓库基线）';
    END IF;
    IF to_regclass('public.biz_warehouse') IS NULL THEN
        RAISE EXCEPTION 'V0420: biz_warehouse 不存在，请先应用 V0137（ERP 主数据与仓库基线）';
    END IF;
    IF to_regclass('public.biz_inventory_in_transit') IS NOT NULL THEN
        RAISE NOTICE 'V0420: biz_inventory_in_transit 已存在，本迁移按幂等路径重放（复验约束/索引）';
    END IF;
    RAISE NOTICE 'V0420: 前置表齐备，开始建在途库存表';
END $$;

-- ---------------------------------------------------------------------------
-- ② 在途库存账表
-- ---------------------------------------------------------------------------
CREATE TABLE IF NOT EXISTS biz_inventory_in_transit (
    id              UUID          PRIMARY KEY DEFAULT gen_random_uuid(),   -- 主键
    shipment_id     UUID          NOT NULL,                                -- 发货单（硬外键 CASCADE）
    material_id     UUID          NOT NULL,                                -- 物料（弱引用 biz_material.id）
    warehouse_id    UUID          NOT NULL,                                -- 发货仓库（弱引用 biz_warehouse.id）
    ship_date       DATE          NOT NULL,                                -- 发货日期（过账时快照，在途账龄口径）
    shipped_qty     NUMERIC(18,4) NOT NULL,                                -- 发货数量（M2-02 过账写入，之后只读）
    received_qty    NUMERIC(18,4) NOT NULL DEFAULT 0,                      -- 累计签收数量（M2-03 累加）
    qty_in_transit  NUMERIC(18,4) GENERATED ALWAYS AS (shipped_qty - received_qty) STORED, -- 当前在途量
    status          VARCHAR(24)   NOT NULL DEFAULT 'IN_TRANSIT',           -- IN_TRANSIT / RECEIVED / CANCELLED
    received_at     TIMESTAMPTZ,                                           -- 全部签收时间（RECEIVED 必填）
    remark          VARCHAR(255),                                          -- 备注（作废原因等）
    created_at      TIMESTAMPTZ   NOT NULL DEFAULT CURRENT_TIMESTAMP,      -- 审计：创建时间
    updated_at      TIMESTAMPTZ   NOT NULL DEFAULT CURRENT_TIMESTAMP,      -- 审计：更新时间（model 层维护）
    create_by       UUID,                                                  -- 审计：创建人（弱引用 sys_user.id）
    update_by       UUID,                                                  -- 审计：更新人（弱引用 sys_user.id）

    CONSTRAINT fk_biz_inv_transit_shipment
        FOREIGN KEY (shipment_id) REFERENCES biz_shipment (id) ON DELETE CASCADE,
    CONSTRAINT ck_biz_inv_transit_status
        CHECK (status IN ('IN_TRANSIT', 'RECEIVED', 'CANCELLED')),
    -- 数量：发货量必须为正（0 发的在途行没有意义）；签收不许为负、不许超过发货量
    CONSTRAINT ck_biz_inv_transit_qty
        CHECK (shipped_qty > 0 AND received_qty >= 0 AND received_qty <= shipped_qty),
    -- 状态 × 数量一致性（用两个基列表达，等价于生成列，不依赖 PG 版本对 CHECK 的限制）
    CONSTRAINT ck_biz_inv_transit_state CHECK (
        (status = 'IN_TRANSIT' AND received_qty < shipped_qty)
        OR (status = 'RECEIVED' AND received_qty = shipped_qty)
        OR status = 'CANCELLED'
    ),
    -- RECEIVED 必须有签收时间（没有时间的「已签收」在报表上是无法追溯的一行）
    CONSTRAINT ck_biz_inv_transit_received_at
        CHECK (status <> 'RECEIVED' OR received_at IS NOT NULL)
);

COMMENT ON TABLE biz_inventory_in_transit IS
    '在途库存账：一行 = 物料 × 发货仓 × 发货单；在途量 = 发货数量 − 累计签收（PG 生成列，天然恒等）';
COMMENT ON COLUMN biz_inventory_in_transit.shipment_id IS
    '发货单 id（硬外键 biz_shipment(id) ON DELETE CASCADE：在途行是发货单的派生账）';
COMMENT ON COLUMN biz_inventory_in_transit.material_id IS
    '物料 id（弱引用 biz_material.id，无外键：物料软删后历史在途账仍可读）';
COMMENT ON COLUMN biz_inventory_in_transit.warehouse_id IS
    '发货仓库 id（弱引用 biz_warehouse.id，即 biz_shipment.warehouse_id，不是收货方所在地）';
COMMENT ON COLUMN biz_inventory_in_transit.ship_date IS
    '发货日期快照（过账时从 biz_shipment.ship_date 写入）：在途账龄口径，不随单头后续改动漂移';
COMMENT ON COLUMN biz_inventory_in_transit.shipped_qty IS
    '发货数量：M2-02 过账时按 (物料, 发货仓, 发货单) 汇总写入，之后只读（追加只能走 upsert 累加）';
COMMENT ON COLUMN biz_inventory_in_transit.received_qty IS
    '累计签收数量：M2-03 每次签收 += n，不新开行；0 <= received_qty <= shipped_qty 由 CHECK 兜住';
COMMENT ON COLUMN biz_inventory_in_transit.qty_in_transit IS
    '当前在途量 = shipped_qty - received_qty（GENERATED ALWAYS … STORED，任何写入路径都写不出不符的行）';
COMMENT ON COLUMN biz_inventory_in_transit.status IS
    'IN_TRANSIT 在途 / RECEIVED 全部签收 / CANCELLED 已作废；⚠所有在途取数与对账必须带 status <> ''CANCELLED''';
COMMENT ON COLUMN biz_inventory_in_transit.received_at IS
    '全部签收时间（status=RECEIVED 时必填，ck_biz_inv_transit_received_at 兜住）';

-- M2-02 的 upsert 键：同一发货单里同一物料 + 同一发货仓只有一行在途
CREATE UNIQUE INDEX IF NOT EXISTS uk_biz_inv_transit_key
    ON biz_inventory_in_transit (material_id, warehouse_id, shipment_id);

-- 按发货单取在途（M2-03 签收确认：拿单头的全部在途行；M2-04 单据视角）
CREATE INDEX IF NOT EXISTS idx_biz_inv_transit_shipment
    ON biz_inventory_in_transit (shipment_id);

-- 按物料 + 仓库汇总在途（M2-04 物料视角 / M2-15 补货建议：可用量 = 库存 + 在途）
-- 只索引在途行：已签收/作废行会长期堆积，但补货口径永远不看它们
CREATE INDEX IF NOT EXISTS idx_biz_inv_transit_open
    ON biz_inventory_in_transit (material_id, warehouse_id)
    WHERE status = 'IN_TRANSIT';

-- 在途账龄（M2-04 筛选「在途超过 N 天」/ 后续库龄卡同款口径）
CREATE INDEX IF NOT EXISTS idx_biz_inv_transit_age
    ON biz_inventory_in_transit (status, ship_date);

-- ---------------------------------------------------------------------------
-- ③ 自检 + 行为探针（任一不符即 RAISE EXCEPTION 回滚整个迁移）
--   探针只用 `__M201_PROBE_*` 单号，块尾一律删除 ⇒ 重放零残留。
-- ---------------------------------------------------------------------------
DO $$
DECLARE
    n INT;
    d TEXT;
    probe_order UUID := 'd0420000-0000-4000-8000-000000000001'::uuid;
    probe_ship  UUID := 'd0420000-0000-4000-8000-000000000002'::uuid;
    probe_ship2 UUID := 'd0420000-0000-4000-8000-000000000003'::uuid;
    probe_mat   UUID := 'd0420000-0000-4000-8000-000000000004'::uuid;
    probe_wh    UUID := 'd0420000-0000-4000-8000-000000000005'::uuid;
BEGIN
    -- ===== 3.1 表在 =====
    IF to_regclass('public.biz_inventory_in_transit') IS NULL THEN
        RAISE EXCEPTION 'V0420 自检失败：biz_inventory_in_transit 未建成';
    END IF;

    -- ===== 3.2 列数逐列比对（多一列少一列都算漂移）=====
    SELECT count(*) INTO n FROM information_schema.columns
     WHERE table_schema = 'public' AND table_name = 'biz_inventory_in_transit';
    IF n <> 15 THEN
        RAISE EXCEPTION 'V0420 自检失败：biz_inventory_in_transit 应有 15 列，实际 % 列', n;
    END IF;

    -- ===== 3.3 关键列的类型 / 精度 / 可空性 / 默认值 =====
    SELECT count(*) INTO n FROM information_schema.columns
     WHERE table_schema = 'public' AND table_name = 'biz_inventory_in_transit'
       AND (   (column_name = 'shipment_id'  AND data_type = 'uuid'      AND is_nullable = 'NO')
            OR (column_name = 'material_id'  AND data_type = 'uuid'      AND is_nullable = 'NO')
            OR (column_name = 'warehouse_id' AND data_type = 'uuid'      AND is_nullable = 'NO')
            OR (column_name = 'ship_date'    AND data_type = 'date'      AND is_nullable = 'NO')
            OR (column_name = 'shipped_qty'  AND numeric_precision = 18 AND numeric_scale = 4 AND is_nullable = 'NO')
            OR (column_name = 'received_qty' AND numeric_precision = 18 AND numeric_scale = 4 AND is_nullable = 'NO')
            OR (column_name = 'status'       AND character_maximum_length = 24 AND is_nullable = 'NO'));
    IF n <> 7 THEN
        RAISE EXCEPTION 'V0420 自检失败：七个关键列的类型/精度/可空性不对（实际符合 % 列）—— 数量列必须是 NUMERIC(18,4)（与 biz_inventory.qty 同口径）', n;
    END IF;

    -- ===== 3.4 `qty_in_transit` 必须是 GENERATED ALWAYS … STORED（不是普通列）=====
    SELECT is_generated || '|' || coalesce(generation_expression, '') || '|' || is_nullable
      INTO d
      FROM information_schema.columns
     WHERE table_schema = 'public' AND table_name = 'biz_inventory_in_transit'
       AND column_name = 'qty_in_transit';
    IF d IS NULL THEN
        RAISE EXCEPTION 'V0420 自检失败：qty_in_transit 列不存在（在途量必须在表里）';
    END IF;
    IF d NOT LIKE 'ALWAYS|%' THEN
        RAISE EXCEPTION 'V0420 自检失败：qty_in_transit 不是生成列（is_generated=%）—— 靠 Rust 算在途量会漂移', d;
    END IF;
    IF d NOT LIKE '%shipped_qty%' OR d NOT LIKE '%received_qty%' THEN
        RAISE EXCEPTION 'V0420 自检失败：qty_in_transit 的生成表达式不是 shipped_qty - received_qty（实际 %）', d;
    END IF;

    -- ===== 3.5 四条 CHECK + 一条外键在位 =====
    SELECT count(*) INTO n FROM pg_constraint
     WHERE conrelid = 'public.biz_inventory_in_transit'::regclass AND contype = 'c'
       AND conname IN ('ck_biz_inv_transit_status', 'ck_biz_inv_transit_qty',
                       'ck_biz_inv_transit_state',  'ck_biz_inv_transit_received_at');
    IF n <> 4 THEN
        RAISE EXCEPTION 'V0420 自检失败：4 条 CHECK 只建成 % 条', n;
    END IF;
    SELECT pg_get_constraintdef(oid) INTO d FROM pg_constraint
     WHERE conname = 'fk_biz_inv_transit_shipment' AND contype = 'f';
    IF d IS NULL OR d NOT LIKE '%REFERENCES biz_shipment(id)%' OR d NOT LIKE '%ON DELETE CASCADE%' THEN
        RAISE EXCEPTION 'V0420 自检失败：发货单外键缺失或不是 CASCADE（def=%）', d;
    END IF;

    -- ===== 3.6 唯一键必须是普通唯一索引（本表无 deleted_at，不是部分索引）=====
    SELECT indexdef INTO d FROM pg_indexes
     WHERE schemaname = 'public' AND tablename = 'biz_inventory_in_transit'
       AND indexname = 'uk_biz_inv_transit_key';
    IF d IS NULL THEN
        RAISE EXCEPTION 'V0420 自检失败：唯一索引 uk_biz_inv_transit_key 缺失（是否与既有索引撞名被静默跳过？）';
    END IF;
    IF d NOT LIKE '%UNIQUE%' OR d LIKE '%WHERE%' THEN
        RAISE EXCEPTION 'V0420 自检失败：uk_biz_inv_transit_key 形状不对（def=%）', d;
    END IF;

    -- ===== 3.7 行为探针：先备一张探针销售订单 + 两张探针发货单 =====
    INSERT INTO biz_sales_order (id, so_no, customer_id, order_date)
    VALUES (probe_order, '__M201_PROBE_SO__', gen_random_uuid(), DATE '2026-11-16');
    INSERT INTO biz_shipment (id, ship_no, so_id, customer_id, warehouse_id, ship_date)
    VALUES (probe_ship,  '__M201_PROBE_SH1__', probe_order, gen_random_uuid(), probe_wh, DATE '2026-11-16'),
           (probe_ship2, '__M201_PROBE_SH2__', probe_order, gen_random_uuid(), probe_wh, DATE '2026-11-16');

    --     3.7a 正常在途行可写；在途量由 PG 算出来 = 100 − 30 = 70
    INSERT INTO biz_inventory_in_transit
        (shipment_id, material_id, warehouse_id, ship_date, shipped_qty, received_qty)
    VALUES (probe_ship, probe_mat, probe_wh, DATE '2026-11-16', 100, 30);
    SELECT qty_in_transit::text || '|' || status || '|' || received_qty::text INTO d
      FROM biz_inventory_in_transit WHERE shipment_id = probe_ship AND material_id = probe_mat;
    IF d <> '70.0000|IN_TRANSIT|30.0000' THEN
        RAISE EXCEPTION 'V0420 自检失败：正常在途行落库值 = %（期望 70.0000|IN_TRANSIT|30.0000）', d;
    END IF;

    --     3.7b 同 (物料, 发货仓, 发货单) 第二行 → 必须被唯一键拒绝（M2-02 upsert 键）
    BEGIN
        INSERT INTO biz_inventory_in_transit
            (shipment_id, material_id, warehouse_id, ship_date, shipped_qty)
        VALUES (probe_ship, probe_mat, probe_wh, DATE '2026-11-16', 5);
        RAISE EXCEPTION 'V0420 自检失败：(物料, 发货仓, 发货单) 重号未被 uk_biz_inv_transit_key 拒绝（同一批在途会被算两遍）';
    EXCEPTION WHEN unique_violation THEN
        NULL;   -- 期望路径
    END;

    --     3.7c 直接写生成列 → 必须被拒（能直接写就等于口径可被绕过）
    BEGIN
        INSERT INTO biz_inventory_in_transit
            (shipment_id, material_id, warehouse_id, ship_date, shipped_qty, qty_in_transit)
        VALUES (probe_ship2, probe_mat, probe_wh, DATE '2026-11-16', 10, 999);
        RAISE EXCEPTION 'V0420 自检失败：直接给 qty_in_transit 赋值竟然成功（生成列被绕过）';
    EXCEPTION WHEN generated_always THEN
        NULL;   -- 期望路径：SQLSTATE 428C9
    END;

    --     3.7d shipped_qty = 0 → 必须被拒
    BEGIN
        INSERT INTO biz_inventory_in_transit
            (shipment_id, material_id, warehouse_id, ship_date, shipped_qty)
        VALUES (probe_ship2, probe_mat, probe_wh, DATE '2026-11-16', 0);
        RAISE EXCEPTION 'V0420 自检失败：shipped_qty = 0 未被 ck_biz_inv_transit_qty 拒绝';
    EXCEPTION WHEN check_violation THEN
        NULL;   -- 期望路径
    END;

    --     3.7e received_qty > shipped_qty → 必须被拒（签收量不能超过发货量）
    BEGIN
        INSERT INTO biz_inventory_in_transit
            (shipment_id, material_id, warehouse_id, ship_date, shipped_qty, received_qty)
        VALUES (probe_ship2, probe_mat, probe_wh, DATE '2026-11-16', 10, 11);
        RAISE EXCEPTION 'V0420 自检失败：received_qty 超过 shipped_qty 未被拒绝（在途会变负数）';
    EXCEPTION WHEN check_violation THEN
        NULL;   -- 期望路径
    END;

    --     3.7f 签满却仍是 IN_TRANSIT → 必须被拒（状态与数量不一致）
    BEGIN
        INSERT INTO biz_inventory_in_transit
            (shipment_id, material_id, warehouse_id, ship_date, shipped_qty, received_qty, status)
        VALUES (probe_ship2, probe_mat, probe_wh, DATE '2026-11-16', 10, 10, 'IN_TRANSIT');
        RAISE EXCEPTION 'V0420 自检失败：签满仍是 IN_TRANSIT 未被 ck_biz_inv_transit_state 拒绝';
    EXCEPTION WHEN check_violation THEN
        NULL;   -- 期望路径
    END;

    --     3.7g RECEIVED 但没有签收时间 → 必须被拒
    BEGIN
        INSERT INTO biz_inventory_in_transit
            (shipment_id, material_id, warehouse_id, ship_date, shipped_qty, received_qty, status)
        VALUES (probe_ship2, probe_mat, probe_wh, DATE '2026-11-16', 10, 10, 'RECEIVED');
        RAISE EXCEPTION 'V0420 自检失败：RECEIVED 无 received_at 未被 ck_biz_inv_transit_received_at 拒绝';
    EXCEPTION WHEN check_violation THEN
        NULL;   -- 期望路径
    END;

    --     3.7h 非法状态值 → 必须被拒
    BEGIN
        INSERT INTO biz_inventory_in_transit
            (shipment_id, material_id, warehouse_id, ship_date, shipped_qty, status)
        VALUES (probe_ship2, probe_mat, probe_wh, DATE '2026-11-16', 10, 'SHIPPED');
        RAISE EXCEPTION 'V0420 自检失败：status = SHIPPED 未被 ck_biz_inv_transit_status 拒绝';
    EXCEPTION WHEN check_violation THEN
        NULL;   -- 期望路径
    END;

    --     3.7i 指向不存在的发货单 → 必须被外键拦住
    BEGIN
        INSERT INTO biz_inventory_in_transit
            (shipment_id, material_id, warehouse_id, ship_date, shipped_qty)
        VALUES ('00000000-0000-4000-8000-0000ffffffff'::uuid, probe_mat, probe_wh,
                DATE '2026-11-16', 1);
        RAISE EXCEPTION 'V0420 自检失败：shipment_id 指向不存在的发货单却插入成功';
    EXCEPTION WHEN foreign_key_violation THEN
        NULL;   -- 期望路径
    END;

    --     3.7j 部分签收：**一条 UPDATE 同时推进 received_qty 与 status**（M2-03 的写入形状）
    --          30 → 60，仍 < 100 ⇒ 状态保持 IN_TRANSIT，在途 100 − 60 = 40
    UPDATE biz_inventory_in_transit
       SET received_qty = received_qty + 30,
           status = CASE WHEN received_qty + 30 = shipped_qty THEN 'RECEIVED' ELSE 'IN_TRANSIT' END
     WHERE shipment_id = probe_ship AND material_id = probe_mat;
    SELECT qty_in_transit::text || '|' || status INTO d
      FROM biz_inventory_in_transit WHERE shipment_id = probe_ship AND material_id = probe_mat;
    IF d <> '40.0000|IN_TRANSIT' THEN
        RAISE EXCEPTION 'V0420 自检失败：部分签收后 = %（期望 40.0000|IN_TRANSIT）', d;
    END IF;

    --     3.7k 签满：仍是**同一条 UPDATE** 把 received_qty 推到 shipped_qty 并置 RECEIVED
    --          （分开两句写会被 ck_biz_inv_transit_state 拦下 —— 这正是要的性质：
    --            不存在「签满了还挂在 IN_TRANSIT」的中间态能被提交）
    UPDATE biz_inventory_in_transit
       SET received_qty = received_qty + 40,
           status = CASE WHEN received_qty + 40 = shipped_qty THEN 'RECEIVED' ELSE 'IN_TRANSIT' END,
           received_at = CASE WHEN received_qty + 40 = shipped_qty THEN now() ELSE received_at END
     WHERE shipment_id = probe_ship AND material_id = probe_mat;
    SELECT qty_in_transit::text || '|' || status INTO d
      FROM biz_inventory_in_transit WHERE shipment_id = probe_ship AND material_id = probe_mat;
    IF d <> '0.0000|RECEIVED' THEN
        RAISE EXCEPTION 'V0420 自检失败：签满后 = %（期望 0.0000|RECEIVED，生成列/状态没有跟随）', d;
    END IF;

    --     3.7l CANCELLED 行不受数量一致性约束（作废后「还有多少在途」不再有意义）
    INSERT INTO biz_inventory_in_transit
        (shipment_id, material_id, warehouse_id, ship_date, shipped_qty, received_qty, status, remark)
    VALUES (probe_ship2, probe_mat, probe_wh, DATE '2026-11-16', 10, 0, 'CANCELLED', 'V0420 探针：作废行');
    SELECT count(*) INTO n FROM biz_inventory_in_transit
     WHERE shipment_id IN (probe_ship, probe_ship2) AND status IN ('RECEIVED', 'CANCELLED');
    IF n <> 2 THEN
        RAISE EXCEPTION 'V0420 自检失败：RECEIVED / CANCELLED 两种终态只写成 % 行（期望 2）', n;
    END IF;

    -- ===== 3.8 级联：物理删发货单 → 在途行必须一起消失（探针由此清场）=====
    DELETE FROM biz_shipment WHERE id IN (probe_ship, probe_ship2);
    SELECT count(*) INTO n FROM biz_inventory_in_transit
     WHERE shipment_id IN (probe_ship, probe_ship2);
    IF n <> 0 THEN
        RAISE EXCEPTION 'V0420 自检失败：删发货单后仍剩 % 行在途（ON DELETE CASCADE 未生效）', n;
    END IF;

    -- ===== 3.9 清场：探针订单/发货单/在途行全部消失 =====
    DELETE FROM biz_sales_order WHERE id = probe_order;
    SELECT (SELECT count(*) FROM biz_inventory_in_transit
             WHERE material_id = probe_mat OR warehouse_id = probe_wh)
         + (SELECT count(*) FROM biz_shipment WHERE ship_no LIKE '__M201_PROBE%')
         + (SELECT count(*) FROM biz_sales_order WHERE so_no LIKE '__M201_PROBE%')
      INTO n;
    IF n <> 0 THEN
        RAISE EXCEPTION 'V0420 自检失败：探针残留 % 行', n;
    END IF;

    RAISE NOTICE 'V0420 自检通过：biz_inventory_in_transit 建表 + 15 列 + 4 条 CHECK + 1 条外键 + 4 条索引 + 11 组行为探针（生成列/唯一键/状态一致/级联）';
END $$;

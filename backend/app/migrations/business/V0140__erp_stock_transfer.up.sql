-- ============================================================
-- V0140：调拨三张表（调拨单主表 + 明细 + 明细序列号子表）
--
-- 依据：
--   ① 卡面 W3a §三（TSK-202610-000681）：三张表 + CHECK + 索引（本次的**契约源**）
--   ② `docs/ERP-仓库管理-技术架构.md`（W0 设计）§2.6「调拨三张表」、§6.1 审批弱引用、
--      §6.2 状态机（DRAFT→PENDING→APPROVED→IN_TRANSIT→DONE / CANCELLED）、
--      §6.4 在途仓、§6.5「为什么用子表而不是 JSONB」
--   做法与 W1a 同口径：**卡面点名的表 ∪ W0 给的结构**，冲突处按卡面，差异逐条写在这里。
--
-- ── 三张表 ────────────────────────────────────────────────────────────────
--   `biz_stock_transfer`            调拨单主表（状态机 + 审批弱引用 + 在途仓 + 审计五件套）
--   `biz_stock_transfer_item`       明细（数量方向由 from/to 表达，qty 恒正；带发运/签收累计量）
--   `biz_stock_transfer_item_serial` 明细序列号子表（一物一码物料必填；**替代卡面提过的 serial_nos JSONB**）
--
-- ── 与卡面 / W0 的差异（逐条，别当 bug）────────────────────────────────────
--   ① 明细数量列名取**卡面的 `qty`**（W0 §2.6 写 `quantity`）。理由：本仓库存侧已经统一叫
--      `biz_inventory.qty` / `biz_inventory_location.qty` / `biz_inventory_txn.qty`，
--      明细沿用 `qty`，W3b 与前端不必再记两套名字；CHECK 名也照卡面 `ck_transfer_item_qty`。
--   ② 主表原因列取**卡面的 `reason VARCHAR(255)`**（W0 写 `transfer_reason VARCHAR(20) DEFAULT 'BALANCE'`）。
--      不并列建两列（同义列是数据不一致的源头）；将来要枚举化，加 CHECK 或用字典即可。
--   ③ 序列号子表按卡面：表名 `biz_stock_transfer_item_serial`、外键 `serial_id → biz_material_serial(id)`、
--      唯一键 `(item_id, serial_id)`。W0 写的是 `biz_stock_transfer_serial(transfer_id, item_id, serial_no)`
--      （用 serial_no 冗余 transfer_id 来拦「同一单内同一件货录两次」）。
--      卡面方案少一层冗余，代价是「同一件货挂在同一调拨单的两条明细下」不会被 DB 拦住 →
--      **W3b 校验时必须自己拦**（跨明细去重）。迁移债含义见 §末。
--   ④ 结构取 W0 的超集：`transit_warehouse_id` / `applicant_*` / `approver_id` / `total_*` /
--      `shipped_qty` / `received_qty` / `unit_cost` / `amount` 与 6 个时间戳全部建出来，
--      否则 W3b 一写状态机就得再来一条 ALTER。
--   ⑤ **没有**建 `uk (transfer_id, line_no)`（W0 §2.6 有）：卡面明细的 `line_no` 默认值就是 1，
--      唯一约束会让「不显式传 line_no 的多行明细」直接失败。行号唯一性交给 W3b 保证；
--      将来要收紧再补一条迁移（不阻断现有写入）。
--   ⑥ `ck_biz_transfer_state`（W0 §2.6 的状态机 CHECK）与 `ck_biz_transfer_cancel_reason`
--      合并成一条 `ck_biz_transfer_state`：CANCELLED 必须同时有 `cancelled_at` 和 `cancel_reason`。
--      写法用 `CASE … ELSE FALSE END`（PG 的 CHECK 结果为 NULL 时视为通过 —— V0137 的三值逻辑坑）。
--   ⑦ 外键：本迁移的三条主外键（`from/to_warehouse_id → biz_warehouse(id)`、
--      `material_id → biz_material(id)`）与三条子表外键都是**硬外键**（卡面显式要求）。
--      与 V0137「弱引用不建外键」的差别是有意的：这里引用的全是 W1a 已落的严格主数据，
--      且仓库/物料被删时调拨单本来就不该存在（PG 的依赖检查即安全阀）。
--      `from_location_id` / `to_location_id` 沿用 `location_id → biz_inventory_location(id)` 口径（见 V0139 头 ①）。
--      代价：建明细前对应的库位库存行必须已存在（W3b 用 create-or-get 保底）。
--   ⑧ 单号唯一索引是**部分索引**（`WHERE deleted_at IS NULL`，照卡面）：软删后单号可复用。
--      注意 `ON CONFLICT (transfer_no)` 在这种部分索引上**不能**直接推断，要用
--      `ON CONFLICT (transfer_no) WHERE deleted_at IS NULL`（W3b 取号幂等时会踩到）。
--
-- ── 不做什么（红线）───────────────────────────────────────────────────────
--   只建表 + 约束 + 索引。**不写**任何业务代码、触发器、视图、取号种子（`biz_seq` 的 TR 规则归 W3b）。
--
-- 回滚：`V0140__erp_stock_transfer.down.sql`（按 serial → item → transfer 逆序 DROP）
--
-- ⚠ 文件内**不写** `BEGIN; … COMMIT;` —— 迁移器自己在事务里执行本文件。
-- ============================================================

-- ============================================================
-- §1 `biz_stock_transfer` —— 调拨单主表
-- ============================================================
CREATE TABLE IF NOT EXISTS biz_stock_transfer (
    id                   UUID PRIMARY KEY DEFAULT gen_random_uuid(),   -- 主键
    transfer_no          VARCHAR(32)  NOT NULL,                        -- 调拨单号 TR-yyyyMMdd-000001（软删后可复用）
    from_warehouse_id    UUID         NOT NULL REFERENCES biz_warehouse(id),  -- 调出仓库
    to_warehouse_id      UUID         NOT NULL REFERENCES biz_warehouse(id),  -- 调入仓库
    transit_warehouse_id UUID         REFERENCES biz_warehouse(id),           -- 在途仓（发运时确定，V0139 种子 WH-TRANSIT）
    status               VARCHAR(16)  NOT NULL DEFAULT 'DRAFT',        -- 状态机：见 ck_transfer_status / ck_biz_transfer_state
    transfer_date        DATE         NOT NULL DEFAULT CURRENT_DATE,   -- 调拨日期
    reason               VARCHAR(255),                                 -- 调拨原因/说明
    applicant_id         UUID,                                         -- 申请人（弱引用 sys_user.id）
    applicant_name       VARCHAR(50),                                  -- 申请人姓名快照（单据留痕，不随改名变）
    approver_id          UUID,                                         -- 审批人（弱引用 sys_user.id）
    approval_instance_id UUID,                                         -- 审批实例（弱引用 biz_flow_instance.id，biz_type='STOCK_TRANSFER'）
    total_qty            NUMERIC(18,4) NOT NULL DEFAULT 0,             -- 合计数量（明细 qty 汇总快照）
    total_amount         NUMERIC(18,4) NOT NULL DEFAULT 0,             -- 合计金额（原价转移的成本快照汇总）
    submitted_at         TIMESTAMPTZ,                                  -- 提交审批时间
    approved_at          TIMESTAMPTZ,                                  -- 审批通过时间
    shipped_at           TIMESTAMPTZ,                                  -- 发运时间（货进在途仓）
    received_at          TIMESTAMPTZ,                                  -- 签收时间（货进调入仓）
    cancelled_at         TIMESTAMPTZ,                                  -- 取消时间
    cancel_reason        VARCHAR(255),                                 -- 取消原因（CANCELLED 必填）
    remark               TEXT,                                         -- 备注
    created_at           TIMESTAMPTZ  NOT NULL DEFAULT CURRENT_TIMESTAMP,  -- 创建时间
    updated_at           TIMESTAMPTZ  NOT NULL DEFAULT CURRENT_TIMESTAMP,  -- 更新时间
    create_by            UUID,                                         -- 创建人（弱引用 sys_user.id）
    update_by            UUID,                                         -- 更新人（弱引用 sys_user.id）
    deleted_at           TIMESTAMPTZ,                                  -- 软删时间（非空即已删除）

    -- ---------- 卡面 §三 的三条 CHECK（名字按卡面，方便 W3b/验收脚本直接引用）----------
    CONSTRAINT ck_transfer_status CHECK (
        status IN ('DRAFT','PENDING','APPROVED','IN_TRANSIT','DONE','CANCELLED')),
    CONSTRAINT ck_transfer_diff_wh CHECK (from_warehouse_id <> to_warehouse_id),

    -- ---------- W0 §2.6 的状态机（时间戳与状态一一对应，防「状态说已发运但没有发运时间」）----------
    CONSTRAINT ck_biz_transfer_state CHECK (CASE status
        WHEN 'DRAFT'      THEN submitted_at IS NULL AND approved_at IS NULL
                               AND shipped_at IS NULL AND received_at IS NULL AND cancelled_at IS NULL
        WHEN 'PENDING'    THEN submitted_at IS NOT NULL AND approved_at IS NULL
                               AND shipped_at IS NULL AND received_at IS NULL AND cancelled_at IS NULL
        WHEN 'APPROVED'   THEN submitted_at IS NOT NULL AND approved_at IS NOT NULL
                               AND shipped_at IS NULL AND received_at IS NULL AND cancelled_at IS NULL
        WHEN 'IN_TRANSIT' THEN submitted_at IS NOT NULL AND approved_at IS NOT NULL
                               AND shipped_at IS NOT NULL AND received_at IS NULL
                               AND transit_warehouse_id IS NOT NULL AND cancelled_at IS NULL
        WHEN 'DONE'       THEN submitted_at IS NOT NULL AND approved_at IS NOT NULL
                               AND shipped_at IS NOT NULL AND received_at IS NOT NULL
                               AND transit_warehouse_id IS NOT NULL AND cancelled_at IS NULL
        WHEN 'CANCELLED'  THEN cancelled_at IS NOT NULL
                               AND cancel_reason IS NOT NULL AND btrim(cancel_reason) <> ''
        ELSE FALSE END),

    -- ---------- 合计不允许为负（方向由 from/to 表达，与明细 qty 恒正一致）----------
    CONSTRAINT ck_biz_transfer_total CHECK (total_qty >= 0 AND total_amount >= 0)
);

COMMENT ON TABLE  biz_stock_transfer IS '仓库间调拨单主表（状态机 DRAFT→PENDING→APPROVED→IN_TRANSIT→DONE/CANCELLED；审批走 biz_flow_instance 弱引用）';
COMMENT ON COLUMN biz_stock_transfer.status IS '状态机：DRAFT/PENDING/APPROVED/IN_TRANSIT/DONE/CANCELLED（VARCHAR 枚举，与 ck_biz_transfer_state 的时间戳口径绑定）';
COMMENT ON COLUMN biz_stock_transfer.transit_warehouse_id IS '在途仓（默认种子 WH-TRANSIT d1390001-0000-4000-8000-000000000001）；IN_TRANSIT/DONE 状态必须非空';
COMMENT ON COLUMN biz_stock_transfer.cancel_reason IS '取消原因：CANCELLED 状态必填非空（ck_biz_transfer_state 强制）';

CREATE UNIQUE INDEX IF NOT EXISTS uk_stock_transfer_no
    ON biz_stock_transfer (transfer_no) WHERE deleted_at IS NULL;
CREATE INDEX IF NOT EXISTS idx_stock_transfer_status
    ON biz_stock_transfer (status);
CREATE INDEX IF NOT EXISTS idx_stock_transfer_from_wh
    ON biz_stock_transfer (from_warehouse_id, transfer_date DESC);
CREATE INDEX IF NOT EXISTS idx_stock_transfer_to_wh
    ON biz_stock_transfer (to_warehouse_id, transfer_date DESC);
CREATE INDEX IF NOT EXISTS idx_stock_transfer_approval
    ON biz_stock_transfer (approval_instance_id) WHERE approval_instance_id IS NOT NULL;

-- ============================================================
-- §2 `biz_stock_transfer_item` —— 调拨明细
--     qty 恒正：方向由主表的 from/to 表达（卡面 §三 ck_transfer_item_qty 的注释原话）
-- ============================================================
CREATE TABLE IF NOT EXISTS biz_stock_transfer_item (
    id               UUID PRIMARY KEY DEFAULT gen_random_uuid(),   -- 主键
    transfer_id      UUID         NOT NULL REFERENCES biz_stock_transfer(id),  -- 所属调拨单
    line_no          INT          NOT NULL DEFAULT 1,              -- 行号（1 起；唯一性由 W3b 保证，见文件头 ⑤）
    material_id      UUID         NOT NULL REFERENCES biz_material(id),        -- 物料
    from_location_id UUID         REFERENCES biz_inventory_location(id),       -- 调出库位（库位库存行 id，可空 = 未指定）
    to_location_id   UUID         REFERENCES biz_inventory_location(id),       -- 调入库位（同上）
    qty              NUMERIC(18,4) NOT NULL,                       -- 调拨数量（恒正）
    shipped_qty      NUMERIC(18,4) NOT NULL DEFAULT 0,             -- 已发运数量（部分发运预留）
    received_qty     NUMERIC(18,4) NOT NULL DEFAULT 0,             -- 已签收数量（部分签收预留）
    unit_cost        NUMERIC(18,6) NOT NULL DEFAULT 0,             -- 单位成本快照（原价转移：调出仓移动加权均价，W0 §6.3 方案 a）
    amount           NUMERIC(18,4) NOT NULL DEFAULT 0,             -- 金额 = qty × unit_cost（调出时快照）
    remark           TEXT,                                          -- 行备注
    created_at       TIMESTAMPTZ  NOT NULL DEFAULT CURRENT_TIMESTAMP,
    updated_at       TIMESTAMPTZ  NOT NULL DEFAULT CURRENT_TIMESTAMP,
    create_by        UUID,
    update_by        UUID,

    -- ---------- 卡面 §三 的数量 CHECK ----------
    CONSTRAINT ck_transfer_item_qty CHECK (qty > 0),

    -- ---------- W0 §2.6 的链式 CHECK（部分发运/部分签收的合法空间）----------
    CONSTRAINT ck_biz_transfer_item_ship  CHECK (shipped_qty >= 0 AND shipped_qty <= qty),
    CONSTRAINT ck_biz_transfer_item_recv  CHECK (received_qty >= 0 AND received_qty <= shipped_qty),
    CONSTRAINT ck_biz_transfer_item_line  CHECK (line_no >= 1),
    CONSTRAINT ck_biz_transfer_item_cost  CHECK (unit_cost >= 0 AND amount >= 0)
);

COMMENT ON TABLE  biz_stock_transfer_item IS '调拨明细（qty 恒正；shipped_qty ≤ qty、received_qty ≤ shipped_qty 留出部分发运/签收空间，本批实现整单发运整单签收）';
COMMENT ON COLUMN biz_stock_transfer_item.qty IS '调拨数量，恒 > 0（方向由主表 from_warehouse_id / to_warehouse_id 表达）';
COMMENT ON COLUMN biz_stock_transfer_item.unit_cost IS '调出仓移动加权均价快照（原价转移：不重算调入仓均价，W0 §6.3 方案 a）';

CREATE INDEX IF NOT EXISTS idx_transfer_item_transfer
    ON biz_stock_transfer_item (transfer_id);
CREATE INDEX IF NOT EXISTS idx_transfer_item_material
    ON biz_stock_transfer_item (material_id);

-- ============================================================
-- §3 `biz_stock_transfer_item_serial` —— 明细的序列号子表（替代 JSONB）
--     W0 §6.5：一物一码物料的调拨必须逐件可追溯 → 子表，不塞 JSONB
-- ============================================================
CREATE TABLE IF NOT EXISTS biz_stock_transfer_item_serial (
    id         UUID PRIMARY KEY DEFAULT gen_random_uuid(),          -- 主键
    item_id    UUID         NOT NULL REFERENCES biz_stock_transfer_item(id),  -- 所属明细行
    serial_id  UUID         NOT NULL REFERENCES biz_material_serial(id),      -- 单品序列号（biz_material_serial.id）
    created_at TIMESTAMPTZ  NOT NULL DEFAULT CURRENT_TIMESTAMP,
    create_by  UUID,

    -- 卡面 §三：同一条明细里同一件货不能录两次
    CONSTRAINT uk_transfer_item_serial UNIQUE (item_id, serial_id)
);

COMMENT ON TABLE  biz_stock_transfer_item_serial IS '调拨明细的序列号子表（一物一码物料必填；替代 serial_nos JSONB，便于反查「这件货调过几次」）';
COMMENT ON COLUMN biz_stock_transfer_item_serial.serial_id IS '单品序列号 id（biz_material_serial.id，硬外键）';

-- 反查：某个序列号被哪些调拨单/明细引用过（售后追溯）
CREATE INDEX IF NOT EXISTS idx_transfer_item_serial_serial
    ON biz_stock_transfer_item_serial (serial_id);

-- ============================================================
-- §4 自检（空库必须也能过：不依赖库里已有业务数据；本轮自检数据跑完清场）
-- ============================================================
DO $$
DECLARE
    n_tbl     INT;
    n_ck      INT;
    n_uk      INT;
    blocked   BOOLEAN;
    v_wh_a    UUID := '00000000-0000-4000-8000-0000000004b1'::uuid;
    v_wh_b    UUID := '00000000-0000-4000-8000-0000000004b2'::uuid;
    v_mat     UUID := '00000000-0000-4000-8000-0000000004b3'::uuid;
    v_ser     UUID := '00000000-0000-4000-8000-0000000004b4'::uuid;
    v_tr      UUID := '00000000-0000-4000-8000-0000000004b5'::uuid;
    v_item    UUID := '00000000-0000-4000-8000-0000000004b6'::uuid;
BEGIN
    -- ① 三张表齐了
    SELECT count(*) INTO n_tbl
      FROM pg_class c JOIN pg_namespace ns ON ns.oid = c.relnamespace
     WHERE ns.nspname = 'public' AND c.relkind = 'r'
       AND c.relname IN ('biz_stock_transfer','biz_stock_transfer_item','biz_stock_transfer_item_serial');
    IF n_tbl <> 3 THEN
        RAISE EXCEPTION 'V0140 自检失败：期望 3 张调拨表，实际 %', n_tbl;
    END IF;

    -- ② 卡面点名的 CHECK 全在（状态 / 同仓 / 明细数量）
    SELECT count(*) INTO n_ck
      FROM pg_constraint
     WHERE conrelid IN ('public.biz_stock_transfer'::regclass, 'public.biz_stock_transfer_item'::regclass)
       AND conname IN ('ck_transfer_status','ck_transfer_diff_wh','ck_transfer_item_qty',
                       'ck_biz_transfer_state','ck_biz_transfer_item_ship','ck_biz_transfer_item_recv');
    IF n_ck <> 6 THEN
        RAISE EXCEPTION 'V0140 自检失败：CHECK 约束缺失（% / 6）', n_ck;
    END IF;

    -- ③ 索引在位（单号部分唯一 / 状态 / 明细外键 / 序列号唯一）
    SELECT count(*) INTO n_uk
      FROM pg_indexes
     WHERE schemaname = 'public'
       AND indexname IN ('uk_stock_transfer_no','idx_stock_transfer_status','idx_transfer_item_transfer',
                         'uk_transfer_item_serial','idx_transfer_item_serial_serial');
    IF n_uk <> 5 THEN
        RAISE EXCEPTION 'V0140 自检失败：关键索引缺失（% / 5）', n_uk;
    END IF;

    -- ④ 单号是部分唯一索引（软删后单号可复用）
    SELECT count(*) INTO n_uk
      FROM pg_indexes
     WHERE schemaname = 'public' AND indexname = 'uk_stock_transfer_no'
       AND indexdef LIKE '%WHERE (deleted_at IS NULL)%';
    IF n_uk <> 1 THEN
        RAISE EXCEPTION 'V0140 自检失败：uk_stock_transfer_no 不是部分索引（WHERE deleted_at IS NULL）';
    END IF;

    -- ⑤ 造最小夹具（仓库 / 物料 / 序列号），全用 '__W3A_ST_*__' 前缀 + 固定 UUID，跑完删净
    INSERT INTO biz_warehouse (id, warehouse_code, warehouse_name, warehouse_type, level, warehouse_path, is_leaf)
    VALUES (v_wh_a, '__W3A_ST_WHA__', '自检-调出仓', 'NORMAL', 1, '/' || v_wh_a || '/', 1),
           (v_wh_b, '__W3A_ST_WHB__', '自检-调入仓', 'NORMAL', 1, '/' || v_wh_b || '/', 1)
    ON CONFLICT (id) DO NOTHING;

    INSERT INTO biz_material (id, material_code, material_name, unit_id)
    VALUES (v_mat, '__W3A_ST_MAT__', '自检-调拨物料', gen_random_uuid())
    ON CONFLICT (id) DO NOTHING;

    INSERT INTO biz_material_serial (id, serial_no, material_id, warehouse_id, status, inbound_at)
    VALUES (v_ser, '__W3A_ST_SN_0001__', v_mat, v_wh_a, 'IN_STOCK', CURRENT_TIMESTAMP)
    ON CONFLICT (id) DO NOTHING;

    -- ⑥ 反例 1：非法 status → 必须被 ck_transfer_status 拦住
    blocked := FALSE;
    BEGIN
        INSERT INTO biz_stock_transfer (id, transfer_no, from_warehouse_id, to_warehouse_id, status)
        VALUES (gen_random_uuid(), '__W3A_ST_BAD_STATUS__', v_wh_a, v_wh_b, 'INVALID');
    EXCEPTION WHEN check_violation THEN
        blocked := TRUE;
    END;
    IF NOT blocked THEN
        RAISE EXCEPTION 'V0140 自检失败：非法 status 被放进库（ck_transfer_status 失效）';
    END IF;

    -- ⑦ 反例 2：同仓调拨（from = to）→ 必须被 ck_transfer_diff_wh 拦住
    blocked := FALSE;
    BEGIN
        INSERT INTO biz_stock_transfer (id, transfer_no, from_warehouse_id, to_warehouse_id)
        VALUES (gen_random_uuid(), '__W3A_ST_SAME_WH__', v_wh_a, v_wh_a);
    EXCEPTION WHEN check_violation THEN
        blocked := TRUE;
    END;
    IF NOT blocked THEN
        RAISE EXCEPTION 'V0140 自检失败：同仓调拨被放进库（ck_transfer_diff_wh 失效）';
    END IF;

    -- ⑧ 反例 3：状态机不自洽（status=DONE 但没有 received_at）→ 必须被 ck_biz_transfer_state 拦住
    blocked := FALSE;
    BEGIN
        INSERT INTO biz_stock_transfer (id, transfer_no, from_warehouse_id, to_warehouse_id, status,
                                        submitted_at, approved_at, shipped_at, transit_warehouse_id)
        VALUES (gen_random_uuid(), '__W3A_ST_BAD_STATE__', v_wh_a, v_wh_b, 'DONE',
                now(), now(), now(), v_wh_a);
    EXCEPTION WHEN check_violation THEN
        blocked := TRUE;
    END;
    IF NOT blocked THEN
        RAISE EXCEPTION 'V0140 自检失败：状态机不自洽的调拨单被放进库（ck_biz_transfer_state 失效）';
    END IF;

    -- ⑨ 正例：一张合法 DRAFT 单 + 一条明细 + 一条序列号子表记录
    INSERT INTO biz_stock_transfer (id, transfer_no, from_warehouse_id, to_warehouse_id, status, reason)
    VALUES (v_tr, '__W3A_ST_TR__', v_wh_a, v_wh_b, 'DRAFT', '自检-正例')
    ON CONFLICT (id) DO NOTHING;

    INSERT INTO biz_stock_transfer_item (id, transfer_id, line_no, material_id, qty, unit_cost, amount)
    VALUES (v_item, v_tr, 1, v_mat, 1.0000, 100.000000, 100.0000)
    ON CONFLICT (id) DO NOTHING;

    INSERT INTO biz_stock_transfer_item_serial (item_id, serial_id)
    VALUES (v_item, v_ser)
    ON CONFLICT (item_id, serial_id) DO NOTHING;
    RAISE NOTICE 'V0140 自检：正例 DRAFT 单 / 明细 / 序列号子表写入成功';

    -- ⑩ 反例 4：明细数量 qty <= 0 → 必须被 ck_transfer_item_qty 拦住
    blocked := FALSE;
    BEGIN
        INSERT INTO biz_stock_transfer_item (transfer_id, material_id, qty)
        VALUES (v_tr, v_mat, 0);
    EXCEPTION WHEN check_violation THEN
        blocked := TRUE;
    END;
    IF NOT blocked THEN
        RAISE EXCEPTION 'V0140 自检失败：qty = 0 的明细被放进库（ck_transfer_item_qty 失效）';
    END IF;

    blocked := FALSE;
    BEGIN
        INSERT INTO biz_stock_transfer_item (transfer_id, material_id, qty)
        VALUES (v_tr, v_mat, -1);
    EXCEPTION WHEN check_violation THEN
        blocked := TRUE;
    END;
    IF NOT blocked THEN
        RAISE EXCEPTION 'V0140 自检失败：qty < 0 的明细被放进库（ck_transfer_item_qty 失效）';
    END IF;

    -- ⑪ 反例 5：同一明细同一序列号录两次 → 必须被 uk_transfer_item_serial 拦住
    blocked := FALSE;
    BEGIN
        INSERT INTO biz_stock_transfer_item_serial (item_id, serial_id) VALUES (v_item, v_ser);
    EXCEPTION WHEN unique_violation THEN
        blocked := TRUE;
    END;
    IF NOT blocked THEN
        RAISE EXCEPTION 'V0140 自检失败：同一明细重复录入同一序列号（uk_transfer_item_serial 失效）';
    END IF;

    -- ⑫ 反例 6：明细指向不存在的物料 → 硬外键必须拦（证明 FK 真的建了）
    blocked := FALSE;
    BEGIN
        INSERT INTO biz_stock_transfer_item (transfer_id, material_id, qty)
        VALUES (v_tr, gen_random_uuid(), 1);
    EXCEPTION WHEN foreign_key_violation THEN
        blocked := TRUE;
    END;
    IF NOT blocked THEN
        RAISE EXCEPTION 'V0140 自检失败：明细的 material_id 悬空引用被放进库（硬外键失效）';
    END IF;

    -- ⑬ 清场（自检数据一行不留；顺序：序列号子表 → 明细 → 主表 → 序列号 → 物料 → 仓库）
    DELETE FROM biz_stock_transfer_item_serial WHERE item_id = v_item;
    DELETE FROM biz_stock_transfer_item        WHERE transfer_id = v_tr;
    DELETE FROM biz_stock_transfer             WHERE id = v_tr;
    DELETE FROM biz_material_serial            WHERE id = v_ser;
    DELETE FROM biz_material                   WHERE id = v_mat;
    DELETE FROM biz_warehouse                  WHERE id IN (v_wh_a, v_wh_b);

    -- ⑭ 清场核对：自检行必须为 0（否则说明上面的 DELETE 有漏）
    SELECT count(*) INTO n_tbl FROM biz_stock_transfer WHERE transfer_no LIKE '__W3A_ST_%';
    IF n_tbl <> 0 THEN
        RAISE EXCEPTION 'V0140 自检失败：清场后仍残留 % 行自检调拨单', n_tbl;
    END IF;

    RAISE NOTICE 'V0140 自检通过：3 表 / 6 CHECK / 5 索引（单号部分唯一）/ 6 条反例拦截 / 1 组正例写入 / 自检数据已清场';
END
$$;

-- ============================================================
-- V0203：出库 / 领用 / 报废 / 盘亏单（`biz_stock_out` + `biz_stock_out_item`）
--
-- 依据：ARCH-3 模块勾稽关系审计 §5 DC-13 / §6 R-P0-1（commit e770860）——
--   判定 **P0**：普通物料**没有任何出库单据**，库存账只增不减
--   ⇒ 1405 存货永远虚增、6401 成本永远为 0。
--   实测：出库只存在于「调拨」（biz_stock_transfer）与「序列号出库/报废」（serial_handler），
--   后端无 biz_stock_out，`frontend/src/views/business/erp/` 22 个目录里无出库页。
--
-- # 本次补的两张表
--
--   biz_stock_out       单头：单号 / 类型 / 仓库 / 出库日期 / 状态 / 合计 / 凭证引用
--   biz_stock_out_item  明细：物料 / 单位 / 数量 / 单价 / 金额 / 库位账行
--
-- # `out_type` 四值（卡面 §1）
--
--   ISSUE 领用   → 成本结转凭证：借 6401 主营业务成本 / 贷 1405 库存商品
--   SCRAP 报废   → 借 1901 待处理财产损溢 / 贷 1405
--   LOSS  盘亏   → 借 1901 待处理财产损溢 / 贷 1405
--   OTHER 其他   → 借 6401（口径同领用：非资产损失的消耗）
--
-- # 状态机（照抄收货单的四态结构）
--
--   DRAFT ──→ PENDING_POST ──→ POSTED ──→ CANCELLED
--     │            │              │
--     └────────────┴──────────────┴─→ CANCELLED（草稿 / 待过账 / 已过账都能作废）
--   PENDING_POST ──→ DRAFT（撤回 / 驳回）
--
--   `PENDING_POST` 是**真的可用状态**（提交待过账 → 由持过账权限的他人过账），
--   不是预留：本版本没有给 STOCK_OUT 配审批流（出库还没有种子流程），
--   所以不接入 flow engine 回调；`PENDING_POST` 承担的是「制单 ≠ 过账」的交接位
--   （配合 `erp.separation_of_duties.post` 开关）。**没有静默降级**：
--   本表不声明任何审批流程，也就不存在「流程没配就直接生效」的语义。
--
-- # 库里几个必须知道的硬约束（照卡面写必然挂）
--
--   ① `biz_inventory_txn.qty CHECK (qty > 0)`：出库流水的数量记**正数**，方向看 `txn_type='OUT'`；
--   ② `biz_inventory_txn.amount CHECK (amount >= 0)`：金额也恒正（冲销记 0 而不是负数）；
--   ③ `biz_inventory` 的唯一键是**表达式索引** uk_biz_inv_mat_wh_loc
--      (material_id, warehouse_id, COALESCE(location_id, 全零 uuid))；
--   ④ 明细 `unit_id` NOT NULL：由物料的基础单位带出，不让客户端传（DC-09 止血口径）；
--   ⑤ 明细 `location_id` 的口径与 `biz_stock_in_item.location_id` **完全一致**：
--      指向 `biz_inventory_location.id`（库位账行），**不是** `biz_location.id`
--      （见 `stockin_write.rs:548-566` 与 V0139 文件头 ①）。NULL = 未分配库位汇总桶。
--
-- # 幂等
--
--   CREATE TABLE / INDEX IF NOT EXISTS；约束走 DO 块；种子用 WHERE NOT EXISTS。
--   文件内**不写** BEGIN/COMMIT —— 迁移器自己在事务里执行（V0136 的坑）。
--
-- 回滚：`V0203__erp_stock_out.down.sql`（先删两表，再撤权限码与取号规则）
-- ============================================================

-- ============================================================
-- §1 单头 `biz_stock_out`
-- ============================================================
CREATE TABLE IF NOT EXISTS biz_stock_out (
    id             UUID PRIMARY KEY DEFAULT gen_random_uuid(),       -- 主键
    out_no         VARCHAR(32)   NOT NULL,                           -- 出库单号（OUT 前缀，软删后可复用）
    out_type       VARCHAR(20)   NOT NULL DEFAULT 'ISSUE',           -- ISSUE 领用 / SCRAP 报废 / LOSS 盘亏 / OTHER 其他
    warehouse_id   UUID          NOT NULL,                           -- 出库仓库（弱引用 biz_warehouse.id）
    out_date       DATE          NOT NULL,                           -- 出库日期（决定所属会计期间）
    status         VARCHAR(24)   NOT NULL DEFAULT 'DRAFT',           -- DRAFT / PENDING_POST / POSTED / CANCELLED
    total_qty      NUMERIC(18,4) NOT NULL DEFAULT 0,                 -- 数量合计
    total_amount   NUMERIC(18,4) NOT NULL DEFAULT 0,                 -- 成本结转金额合计（过账时按账上真实差额写入）
    voucher_id     UUID,                                             -- 过账生成的凭证（弱引用 biz_voucher.id；作废后指向红字凭证）
    reason         VARCHAR(255),                                     -- 出库事由（领用用途 / 报废原因 / 盘亏说明）
    remark         TEXT,                                             -- 备注
    posted_at      TIMESTAMPTZ,                                      -- 过账时间
    posted_by      UUID,                                             -- 过账人
    cancelled_at   TIMESTAMPTZ,                                      -- 作废时间
    cancelled_by   UUID,                                             -- 作废人
    cancel_reason  VARCHAR(255),                                      -- 作废原因（作废必须写，进红字凭证摘要）
    created_at     TIMESTAMPTZ   NOT NULL DEFAULT CURRENT_TIMESTAMP,
    updated_at     TIMESTAMPTZ   NOT NULL DEFAULT CURRENT_TIMESTAMP,
    create_by      UUID,
    update_by      UUID,
    deleted_at     TIMESTAMPTZ,

    CONSTRAINT ck_biz_so_status CHECK (status IN ('DRAFT', 'PENDING_POST', 'POSTED', 'CANCELLED')),
    CONSTRAINT ck_biz_so_type   CHECK (out_type IN ('ISSUE', 'SCRAP', 'LOSS', 'OTHER')),
    -- 数量 / 金额恒非负（金额是「账上真实差额」，负数说明账面脏了，由巡检去查）
    CONSTRAINT ck_biz_so_qty    CHECK (total_qty >= 0 AND total_amount >= 0),
    -- 已过账必须有凭证引用？**不加**：金额差额 ≤ 0 的零成本存量按设计跳过凭证
    -- （与 serial 出库同口径，见 voucher::create_for_stock_out 的 WARN 分支），
    -- 加这条 CHECK 会把「零成本存量出库」整条路堵死。
    -- 作废必须留原因（红字凭证摘要与审计都要）
    CONSTRAINT ck_biz_so_cancel CHECK (status <> 'CANCELLED' OR cancel_reason IS NOT NULL)
);

DO $$
BEGIN
    IF NOT EXISTS (SELECT 1 FROM pg_constraint
                    WHERE conname = 'ck_biz_so_status' AND conrelid = 'public.biz_stock_out'::regclass) THEN
        ALTER TABLE biz_stock_out ADD CONSTRAINT ck_biz_so_status
            CHECK (status IN ('DRAFT', 'PENDING_POST', 'POSTED', 'CANCELLED'));
    END IF;
    IF NOT EXISTS (SELECT 1 FROM pg_constraint
                    WHERE conname = 'ck_biz_so_type' AND conrelid = 'public.biz_stock_out'::regclass) THEN
        ALTER TABLE biz_stock_out ADD CONSTRAINT ck_biz_so_type
            CHECK (out_type IN ('ISSUE', 'SCRAP', 'LOSS', 'OTHER'));
    END IF;
    IF NOT EXISTS (SELECT 1 FROM pg_constraint
                    WHERE conname = 'ck_biz_so_qty' AND conrelid = 'public.biz_stock_out'::regclass) THEN
        ALTER TABLE biz_stock_out ADD CONSTRAINT ck_biz_so_qty CHECK (total_qty >= 0 AND total_amount >= 0);
    END IF;
    IF NOT EXISTS (SELECT 1 FROM pg_constraint
                    WHERE conname = 'ck_biz_so_cancel' AND conrelid = 'public.biz_stock_out'::regclass) THEN
        ALTER TABLE biz_stock_out ADD CONSTRAINT ck_biz_so_cancel
            CHECK (status <> 'CANCELLED' OR cancel_reason IS NOT NULL);
    END IF;
END
$$;

COMMENT ON TABLE  biz_stock_out IS '出库单（领用 / 报废 / 盘亏 / 其他）：三本账负向变动 + 成本结转凭证的唯一入口；补 ARCH-3 DC-13「库存账只增不减」的缺口';
COMMENT ON COLUMN biz_stock_out.out_type IS 'ISSUE 领用（借 6401）/ SCRAP 报废（借 1901）/ LOSS 盘亏（借 1901）/ OTHER 其他（借 6401）';
COMMENT ON COLUMN biz_stock_out.status IS 'DRAFT 草稿 / PENDING_POST 待过账（已提交，等持过账权限的人过账）/ POSTED 已过账 / CANCELLED 已作废';
COMMENT ON COLUMN biz_stock_out.total_amount IS '成本结转金额合计 = Σ 明细 amount（过账时按 biz_inventory 账上真实差额写入，不用 qty×avg 重算）';

-- 单号唯一是**部分索引**（软删后单号可复用，与 uk_biz_si_no 同口径）
CREATE UNIQUE INDEX IF NOT EXISTS uk_biz_so_no        ON biz_stock_out (out_no) WHERE deleted_at IS NULL;
CREATE INDEX        IF NOT EXISTS idx_biz_so_status   ON biz_stock_out (status);
CREATE INDEX        IF NOT EXISTS idx_biz_so_date     ON biz_stock_out (out_date DESC);
CREATE INDEX        IF NOT EXISTS idx_biz_so_warehouse ON biz_stock_out (warehouse_id, out_date DESC);
CREATE INDEX        IF NOT EXISTS idx_biz_so_type     ON biz_stock_out (out_type, out_date DESC);
CREATE INDEX        IF NOT EXISTS idx_biz_so_voucher  ON biz_stock_out (voucher_id);

-- ============================================================
-- §2 明细 `biz_stock_out_item`
-- ============================================================
CREATE TABLE IF NOT EXISTS biz_stock_out_item (
    id            UUID PRIMARY KEY DEFAULT gen_random_uuid(),        -- 主键
    stock_out_id  UUID          NOT NULL REFERENCES biz_stock_out(id) ON DELETE CASCADE, -- 所属出库单（唯一硬外键）
    line_no       INT           NOT NULL,                            -- 行号（从 1 起）
    material_id   UUID          NOT NULL,                            -- 物料（弱引用 biz_material.id）
    unit_id       UUID          NOT NULL,                            -- 单位（弱引用 biz_unit.id；物料基础单位快照）
    qty           NUMERIC(18,4) NOT NULL,                            -- 出库数量
    unit_cost     NUMERIC(18,6) NOT NULL DEFAULT 0,                  -- 过账时的移动加权单价快照（草稿为 0）
    amount        NUMERIC(18,4) NOT NULL DEFAULT 0,                  -- 过账时**账上真实扣减金额**（草稿为 0）
    location_id   UUID,                                              -- 库位账行（弱引用 biz_inventory_location.id，可空=未分配库位桶）
    lot_no        VARCHAR(64),                                       -- 批号（可空）
    remark        VARCHAR(255),
    created_at    TIMESTAMPTZ   NOT NULL DEFAULT CURRENT_TIMESTAMP,
    updated_at    TIMESTAMPTZ   NOT NULL DEFAULT CURRENT_TIMESTAMP,

    CONSTRAINT uk_biz_so_item_line UNIQUE (stock_out_id, line_no),
    CONSTRAINT ck_biz_so_item_qty  CHECK (qty > 0),
    CONSTRAINT ck_biz_so_item_cost CHECK (unit_cost >= 0 AND amount >= 0)
);

DO $$
BEGIN
    IF NOT EXISTS (SELECT 1 FROM pg_constraint
                    WHERE conname = 'ck_biz_so_item_qty' AND conrelid = 'public.biz_stock_out_item'::regclass) THEN
        ALTER TABLE biz_stock_out_item ADD CONSTRAINT ck_biz_so_item_qty CHECK (qty > 0);
    END IF;
    IF NOT EXISTS (SELECT 1 FROM pg_constraint
                    WHERE conname = 'ck_biz_so_item_cost' AND conrelid = 'public.biz_stock_out_item'::regclass) THEN
        ALTER TABLE biz_stock_out_item ADD CONSTRAINT ck_biz_so_item_cost
            CHECK (unit_cost >= 0 AND amount >= 0);
    END IF;
    IF NOT EXISTS (SELECT 1 FROM pg_constraint
                    WHERE conname = 'uk_biz_so_item_line' AND conrelid = 'public.biz_stock_out_item'::regclass) THEN
        ALTER TABLE biz_stock_out_item ADD CONSTRAINT uk_biz_so_item_line UNIQUE (stock_out_id, line_no);
    END IF;
END
$$;

COMMENT ON TABLE  biz_stock_out_item IS '出库单明细；qty 是出库数量，amount 是过账时账上真实扣减的成本（不是 qty×avg 重算）';
COMMENT ON COLUMN biz_stock_out_item.location_id IS '库位账行 id（biz_inventory_location.id，与 biz_stock_in_item.location_id 同口径）；NULL = 未分配库位汇总桶';

CREATE INDEX IF NOT EXISTS idx_biz_so_item_out      ON biz_stock_out_item (stock_out_id);
CREATE INDEX IF NOT EXISTS idx_biz_so_item_material ON biz_stock_out_item (material_id);

-- ============================================================
-- §3 取号规则 `biz_seq(seq_type='STOCK_OUT')`：OUT-yyyyMMdd-000001
-- ============================================================
INSERT INTO biz_seq (id, seq_type, seq_name, prefix, date_pattern, period_key, current_no, step, separator, status, remark)
SELECT v.id::uuid, 'STOCK_OUT', '出库单号', 'OUT', 'yyyyMMdd', '', 0, 1, '-', 1, v.remark
  FROM (VALUES
        ('d2030001-0000-4000-8000-000000000001', '出库单号：OUT-yyyyMMdd-000001，按天（CST）滚动')
       ) AS v(id, remark)
 WHERE NOT EXISTS (SELECT 1 FROM biz_seq WHERE seq_type = 'STOCK_OUT' AND deleted_at IS NULL);

-- ============================================================
-- §4 权限码（7 个：1 个菜单节点 + 6 个按钮）
--    与 `stockout_dto::STOCK_OUT_PERMISSIONS` 逐字一致，否则接口 403。
--
--    business:erp:stock-out:list    列表   GET    /business/erp/stock-out/page
--    business:erp:stock-out:detail  详情   GET    /business/erp/stock-out/{id}
--    business:erp:stock-out:create  新建   POST   /business/erp/stock-out
--    business:erp:stock-out:update  编辑   PUT    /business/erp/stock-out/{id}
--    business:erp:stock-out:delete  删除   DELETE /business/erp/stock-out/{id}
--    business:erp:stock-out:status  状态   PATCH  /business/erp/stock-out/{id}/status
--
--    `:status` 独立于 `:update`：过账会**真的减库存并生成凭证**，与「改几行草稿明细」
--    不是同一类授权（与 business:erp:stock-in:status 同口径）。
--
--    ID 段：1100–1104 已被 V0195（合同）占用，全量扫过 `20000000-...-11xx` 只有这 5 行，
--    1105 起为空段。
-- ============================================================
INSERT INTO sys_menu (id, name, menu_type, auth_code, path, parent_id, component,
                      sort_order, status, icon, title, is_visible, module_code)
VALUES
    ('20000000-0000-0000-0000-000000001105'::uuid, 'business:erp:stock-out:list', 2,
     'business:erp:stock-out:list', '/business/erp/stock-out',
     '20000000-0000-0000-0000-000000000953'::uuid, 'business/erp/stock-out/index',
     7, 1, NULL, '出库单', TRUE, 'business')
ON CONFLICT (id) DO NOTHING;

INSERT INTO sys_menu (id, name, menu_type, auth_code, path, parent_id, component,
                      sort_order, status, icon, title, is_visible, module_code)
VALUES
    ('20000000-0000-0000-0000-000000001106'::uuid, 'business:erp:stock-out:detail', 3,
     'business:erp:stock-out:detail', NULL,
     '20000000-0000-0000-0000-000000001105'::uuid, NULL,
     1, 1, NULL, '出库单详情（含明细与成本结转金额）', FALSE, 'business'),
    ('20000000-0000-0000-0000-000000001107'::uuid, 'business:erp:stock-out:create', 3,
     'business:erp:stock-out:create', NULL,
     '20000000-0000-0000-0000-000000001105'::uuid, NULL,
     2, 1, NULL, '新建出库单（领用 / 报废 / 盘亏）', FALSE, 'business'),
    ('20000000-0000-0000-0000-000000001108'::uuid, 'business:erp:stock-out:update', 3,
     'business:erp:stock-out:update', NULL,
     '20000000-0000-0000-0000-000000001105'::uuid, NULL,
     3, 1, NULL, '编辑出库单（仅草稿）', FALSE, 'business'),
    ('20000000-0000-0000-0000-000000001109'::uuid, 'business:erp:stock-out:delete', 3,
     'business:erp:stock-out:delete', NULL,
     '20000000-0000-0000-0000-000000001105'::uuid, NULL,
     4, 1, NULL, '删除出库单（仅草稿）', FALSE, 'business'),
    ('20000000-0000-0000-0000-000000001110'::uuid, 'business:erp:stock-out:status', 3,
     'business:erp:stock-out:status', NULL,
     '20000000-0000-0000-0000-000000001105'::uuid, NULL,
     5, 1, NULL, '出库单状态流转（提交 / 过账 / 作废）', FALSE, 'business')
ON CONFLICT (id) DO NOTHING;

-- 授权给已存在的角色（空库首启时 sys_role 为空 → 只打 NOTICE，bootstrap 后重放即补齐）
DO $$
DECLARE
    n_roles   integer;
    n_menus   integer;
    n_grants  integer;
BEGIN
    SELECT count(*) INTO n_menus
      FROM sys_menu
     WHERE id IN ('20000000-0000-0000-0000-000000001105'::uuid,
                  '20000000-0000-0000-0000-000000001106'::uuid,
                  '20000000-0000-0000-0000-000000001107'::uuid,
                  '20000000-0000-0000-0000-000000001108'::uuid,
                  '20000000-0000-0000-0000-000000001109'::uuid,
                  '20000000-0000-0000-0000-000000001110'::uuid);
    IF n_menus <> 6 THEN
        RAISE EXCEPTION 'V0203 自检失败：预期 6 个出库权限节点，实际 %', n_menus;
    END IF;

    SELECT count(*) INTO n_roles FROM sys_role WHERE deleted_at IS NULL;
    IF n_roles = 0 THEN
        RAISE NOTICE 'V0203：sys_role 为空（空库首启，迁移先于 bootstrap），跳过授权；bootstrap 后重放本迁移即可补齐';
        RETURN;
    END IF;

    INSERT INTO sys_role_menu (id, role_id, menu_id)
    SELECT gen_random_uuid(), r.id, m.id
      FROM sys_role r
      CROSS JOIN (VALUES
            ('20000000-0000-0000-0000-000000001105'::uuid),
            ('20000000-0000-0000-0000-000000001106'::uuid),
            ('20000000-0000-0000-0000-000000001107'::uuid),
            ('20000000-0000-0000-0000-000000001108'::uuid),
            ('20000000-0000-0000-0000-000000001109'::uuid),
            ('20000000-0000-0000-0000-000000001110'::uuid)) AS m(id)
     WHERE r.deleted_at IS NULL
       AND r.role_code IN ('SUPER_ADMIN', 'ADMIN', 'WAREHOUSE_KEEPER', 'PURCHASER', 'BUYER')
       AND NOT EXISTS (
            SELECT 1 FROM sys_role_menu rm WHERE rm.role_id = r.id AND rm.menu_id = m.id
       );
    GET DIAGNOSTICS n_grants = ROW_COUNT;
    RAISE NOTICE 'V0203：出库单 6 个权限码已种，授权新增 % 行（角色数 %）', n_grants, n_roles;
END $$;

-- ============================================================
-- §5 自检：列 / 约束 / 索引 / 取号规则齐了没（缺一条就让迁移失败，而不是留半张表）
-- ============================================================
DO $$
DECLARE
    need_col TEXT;
    miss     TEXT := '';
    ck       TEXT;
    need_idx TEXT;
    n_seq    integer;
BEGIN
    FOREACH need_col IN ARRAY ARRAY[
        'out_no', 'out_type', 'warehouse_id', 'out_date', 'status',
        'total_qty', 'total_amount', 'voucher_id', 'reason', 'remark',
        'posted_at', 'posted_by', 'cancelled_at', 'cancelled_by', 'cancel_reason',
        'created_at', 'updated_at', 'create_by', 'update_by', 'deleted_at'
    ] LOOP
        IF NOT EXISTS (SELECT 1 FROM information_schema.columns
                        WHERE table_schema = 'public' AND table_name = 'biz_stock_out'
                          AND column_name = need_col) THEN
            miss := miss || ' biz_stock_out.' || need_col;
        END IF;
    END LOOP;

    FOREACH need_col IN ARRAY ARRAY[
        'stock_out_id', 'line_no', 'material_id', 'unit_id', 'qty',
        'unit_cost', 'amount', 'location_id', 'lot_no', 'remark'
    ] LOOP
        IF NOT EXISTS (SELECT 1 FROM information_schema.columns
                        WHERE table_schema = 'public' AND table_name = 'biz_stock_out_item'
                          AND column_name = need_col) THEN
            miss := miss || ' biz_stock_out_item.' || need_col;
        END IF;
    END LOOP;

    IF miss <> '' THEN
        RAISE EXCEPTION 'V0203 自检失败：缺列 →%', miss;
    END IF;

    FOREACH ck IN ARRAY ARRAY[
        'ck_biz_so_status', 'ck_biz_so_type', 'ck_biz_so_qty', 'ck_biz_so_cancel',
        'uk_biz_so_item_line', 'ck_biz_so_item_qty', 'ck_biz_so_item_cost'
    ] LOOP
        IF NOT EXISTS (SELECT 1 FROM pg_constraint WHERE conname = ck) THEN
            RAISE EXCEPTION 'V0203 自检失败：缺约束 %', ck;
        END IF;
    END LOOP;

    FOREACH need_idx IN ARRAY ARRAY[
        'uk_biz_so_no', 'idx_biz_so_status', 'idx_biz_so_date', 'idx_biz_so_warehouse',
        'idx_biz_so_type', 'idx_biz_so_voucher',
        'idx_biz_so_item_out', 'idx_biz_so_item_material'
    ] LOOP
        IF NOT EXISTS (SELECT 1 FROM pg_indexes
                        WHERE schemaname = 'public' AND indexname = need_idx) THEN
            RAISE EXCEPTION 'V0203 自检失败：缺索引 %', need_idx;
        END IF;
    END LOOP;

    -- 单号唯一索引必须是部分索引（软删后可复用单号）
    IF NOT EXISTS (
        SELECT 1 FROM pg_indexes
         WHERE schemaname = 'public' AND indexname = 'uk_biz_so_no'
           AND indexdef LIKE '%WHERE (deleted_at IS NULL)%'
    ) THEN
        RAISE EXCEPTION 'V0203 自检失败：uk_biz_so_no 不是 WHERE deleted_at IS NULL 的部分索引';
    END IF;

    -- 取号规则必须落库（代码 path 还有 ensure_seq_row 兜底，迁移里种是为了
    -- 「生产直接跑 V0203 迁移」后单号格式立刻可控）
    SELECT count(*) INTO n_seq FROM biz_seq
     WHERE seq_type = 'STOCK_OUT' AND deleted_at IS NULL;
    IF n_seq <> 1 THEN
        RAISE EXCEPTION 'V0203 自检失败：biz_seq(STOCK_OUT) 应有 1 行，实际 %', n_seq;
    END IF;

    RAISE NOTICE 'V0203 就绪：出库两表 2 张 / 11 索引 / 7 约束 / 6 权限码 / 1 取号规则';
END $$;

-- ============================================================
-- V0211：盘点单（`biz_stock_take` + `biz_stock_take_item`）—— ARCH-3 §5 **DC-14**（P1）
--
-- # 这条迁移补的是什么
--
-- 改造前：全系统**没有盘点单**（实测 `biz_stock_take` 不存在）。年底盘点时
-- 账面 100 / 实际 92，仓管在系统里**没有地方调平**：只能手改 `biz_inventory`
-- （没有凭证来源、没有审批、没有留痕）→ 审计上说不清「为什么库存变了」。
--
-- 改造后：盘点 = 一张**单据**（账面数快照 → 实盘数 → 过账），过账时三本账按差异
-- 正负分别走入库 / 出库方向的**现成**逻辑，并生成一张「盘点差异凭证」
-- （盘盈 借 1405 / 贷 1901；盘亏 借 1901 / 贷 1405）。作废走原路回滚 + 红冲。
--
-- # 表结构
--
--   biz_stock_take       单头：单号 / 仓库 / 盘点日期 / 全盘·抽盘 / 状态 / 合计 / 凭证 / 审批实例
--   biz_stock_take_item  明细：物料 / 库位 / 账面数（快照）/ 实盘数 / 差异（生成列）/ 单价 / 差异金额
--
-- # 状态机（4 态，与出库单同构）
--
--   DRAFT ──提交审批──▶ PENDING_POST ──过账──▶ POSTED ──作废──▶ CANCELLED
--                          │                                ▲
--                          └──────撤回 / 驳回 ──▶ DRAFT      │
--   DRAFT / PENDING_POST ───────────────────────────────────┘（作废）
--
--   `PENDING_POST` = 「已提交（审批通过 / 显式跳过审批）待过账」：账一分没动，
--   由持过账权限的人过账（`erp.separation_of_duties.post` 开关控制制单≠过账）。
--   **为什么审批通过不直接过账**：过账会真的动三本账并出凭证，让它成为**独立一次点击**
--   （`POST /business/erp/stock-take/{id}/status` target=POSTED），审计链条才完整：
--   谁盘的（制单）/ 谁批的（审批实例）/ 谁过的账（posted_by）三个人可分。
--
-- # `diff_qty` 为什么是生成列
--
--   `diff_qty = actual_qty - book_qty` 由 PG 自己算（STORED），任何写入路径都
--   不可能写出「差异与两数不符」的行 —— 审计要的正是这一点（口径不靠代码自觉）。
--
-- # 审批流（可配，不是硬编码）
--
--   种子流程 `stock_take_post`（biz_type `STOCK_TAKE_POST`）：按**差异金额**
--   （`form.total_diff_amount`）两档分流：
--     · < 10,000      → 仓管主管单级审批
--     · ≥ 10,000      → 仓管主管 → 总经理两级审批（串行）
--   阈值、审批人、超时**全在 `biz_flow_edge.condition_expr` / `biz_flow_node` 里**，
--   客户在「流程定义」里另存版本即可改，不用改代码（与 V0182 / V0209 同一范式）。
--   图快照 / hash 由 `graph.rs::snapshot()` 的 canonical JSON 算出：
--     stock_take_post : cfc6101455d799fb4377353ad57c85dfae8f30956a94ae642c88e1644a6bfb4a
--
-- # 取号
--
--   `biz_seq(seq_type='STOCK_TAKE')` → `ST-yyyyMMdd-000001`（按天 CST 滚动）。
--
-- # 幂等 / 回滚
--
--   CREATE TABLE / INDEX IF NOT EXISTS；约束走 DO 块；种子全部 WHERE NOT EXISTS。
--   文件内**不写** BEGIN/COMMIT（迁移器自己在事务里跑）。
--   回滚：`V0211__erp_stock_take.down.sql`（先终止在跑的审批 → 删流程五层 →
--   删权限 → 删取号 → 删两表 → 零残留自检）。
-- ============================================================

-- ============================================================
-- §1 单头 `biz_stock_take`
-- ============================================================
CREATE TABLE IF NOT EXISTS biz_stock_take (
    id                    UUID PRIMARY KEY DEFAULT gen_random_uuid(),   -- 主键
    take_no               VARCHAR(32)   NOT NULL,                       -- 盘点单号（ST-yyyyMMdd-000001）
    warehouse_id          UUID          NOT NULL,                       -- 盘点仓库（弱引用 biz_warehouse.id）
    take_date             DATE          NOT NULL,                       -- 盘点日期（决定会计期间）
    take_type             VARCHAR(16)   NOT NULL DEFAULT 'FULL',        -- FULL 全盘 / PARTIAL 抽盘
    status                VARCHAR(24)   NOT NULL DEFAULT 'DRAFT',       -- DRAFT / PENDING_POST / POSTED / CANCELLED
    approval_instance_id  UUID,                                         -- 提交审批的实例（弱引用 biz_flow_instance.id）
    voucher_id            UUID,                                         -- 盘点差异凭证（蓝字，弱引用 biz_voucher.id）
    reverse_voucher_id    UUID,                                         -- 作废时生成的红字凭证
    total_book_qty        NUMERIC(18,4) NOT NULL DEFAULT 0,             -- 账面数量合计
    total_actual_qty      NUMERIC(18,4) NOT NULL DEFAULT 0,             -- 实盘数量合计
    total_gain_qty        NUMERIC(18,4) NOT NULL DEFAULT 0,             -- 盘盈数量合计（> 0 部分）
    total_loss_qty        NUMERIC(18,4) NOT NULL DEFAULT 0,             -- 盘亏数量合计（绝对值）
    total_gain_amount     NUMERIC(18,4) NOT NULL DEFAULT 0,             -- 盘盈金额（账上真实增量，过账时写）
    total_loss_amount     NUMERIC(18,4) NOT NULL DEFAULT 0,             -- 盘亏金额（账上真实减量，过账时写）
    remark                VARCHAR(500),                                 -- 备注（盘点范围 / 组织说明）
    posted_at             TIMESTAMPTZ,                                  -- 过账时间
    posted_by             UUID,                                         -- 过账人（审计：谁过的账）
    cancelled_at          TIMESTAMPTZ,                                  -- 作废时间
    cancelled_by          UUID,                                         -- 作废人
    cancel_reason         VARCHAR(255),                                 -- 作废原因（已过账作废必须写，进红字凭证摘要）
    created_at            TIMESTAMPTZ   NOT NULL DEFAULT CURRENT_TIMESTAMP,
    updated_at            TIMESTAMPTZ   NOT NULL DEFAULT CURRENT_TIMESTAMP,
    create_by             UUID,
    update_by             UUID,
    deleted_at            TIMESTAMPTZ,

    CONSTRAINT ck_biz_st_status  CHECK (status IN ('DRAFT', 'PENDING_POST', 'POSTED', 'CANCELLED')),
    CONSTRAINT ck_biz_st_type    CHECK (take_type IN ('FULL', 'PARTIAL')),
    -- 数量 / 金额恒非负：负数说明写反了方向（盘亏记正数，方向看 diff_qty 的正负）
    CONSTRAINT ck_biz_st_totals  CHECK (
        total_book_qty >= 0 AND total_actual_qty >= 0
        AND total_gain_qty >= 0 AND total_loss_qty >= 0
        AND total_gain_amount >= 0 AND total_loss_amount >= 0
    ),
    -- 作废必须留原因（红字凭证摘要与审计都要）
    CONSTRAINT ck_biz_st_cancel  CHECK (status <> 'CANCELLED' OR cancel_reason IS NOT NULL)
);

DO $$
BEGIN
    IF NOT EXISTS (SELECT 1 FROM pg_constraint
                    WHERE conname = 'ck_biz_st_status' AND conrelid = 'public.biz_stock_take'::regclass) THEN
        ALTER TABLE biz_stock_take ADD CONSTRAINT ck_biz_st_status
            CHECK (status IN ('DRAFT', 'PENDING_POST', 'POSTED', 'CANCELLED'));
    END IF;
    IF NOT EXISTS (SELECT 1 FROM pg_constraint
                    WHERE conname = 'ck_biz_st_type' AND conrelid = 'public.biz_stock_take'::regclass) THEN
        ALTER TABLE biz_stock_take ADD CONSTRAINT ck_biz_st_type
            CHECK (take_type IN ('FULL', 'PARTIAL'));
    END IF;
    IF NOT EXISTS (SELECT 1 FROM pg_constraint
                    WHERE conname = 'ck_biz_st_totals' AND conrelid = 'public.biz_stock_take'::regclass) THEN
        ALTER TABLE biz_stock_take ADD CONSTRAINT ck_biz_st_totals CHECK (
            total_book_qty >= 0 AND total_actual_qty >= 0
            AND total_gain_qty >= 0 AND total_loss_qty >= 0
            AND total_gain_amount >= 0 AND total_loss_amount >= 0
        );
    END IF;
    IF NOT EXISTS (SELECT 1 FROM pg_constraint
                    WHERE conname = 'ck_biz_st_cancel' AND conrelid = 'public.biz_stock_take'::regclass) THEN
        ALTER TABLE biz_stock_take ADD CONSTRAINT ck_biz_st_cancel
            CHECK (status <> 'CANCELLED' OR cancel_reason IS NOT NULL);
    END IF;
END
$$;

COMMENT ON TABLE  biz_stock_take IS '盘点单：账实差异的**唯一**调平入口（ARCH-3 DC-14）；过账时三本账按差异正负走入库/出库方向 + 盘点差异凭证；禁止手改 biz_inventory 做盘盈盘亏';
COMMENT ON COLUMN biz_stock_take.take_type IS 'FULL 全盘（建单时把该仓全部库存桶快照成明细）/ PARTIAL 抽盘（只盘指定物料）';
COMMENT ON COLUMN biz_stock_take.status IS 'DRAFT 草稿 → PENDING_POST 待过账（已提交审批 / 显式跳过）→ POSTED 已过账 → CANCELLED 已作废（边表在 state_machine::STOCK_TAKE_EDGES，6 条）';
COMMENT ON COLUMN biz_stock_take.total_gain_amount IS '盘盈金额合计 = Σ 明细 diff_amount（过账时按 biz_inventory 账上真实增量写入，不用 qty×avg 重算）';
COMMENT ON COLUMN biz_stock_take.total_loss_amount IS '盘亏金额合计 = Σ 明细 diff_amount（同上，账上真实减量）';

CREATE UNIQUE INDEX IF NOT EXISTS uk_biz_st_no          ON biz_stock_take (take_no) WHERE deleted_at IS NULL;
CREATE INDEX        IF NOT EXISTS idx_biz_st_status     ON biz_stock_take (status);
CREATE INDEX        IF NOT EXISTS idx_biz_st_date       ON biz_stock_take (take_date DESC);
CREATE INDEX        IF NOT EXISTS idx_biz_st_warehouse  ON biz_stock_take (warehouse_id, take_date DESC);
CREATE INDEX        IF NOT EXISTS idx_biz_st_voucher    ON biz_stock_take (voucher_id);
CREATE INDEX        IF NOT EXISTS idx_biz_st_approval   ON biz_stock_take (approval_instance_id)
    WHERE approval_instance_id IS NOT NULL;

-- ============================================================
-- §2 明细 `biz_stock_take_item`
-- ============================================================
CREATE TABLE IF NOT EXISTS biz_stock_take_item (
    id           UUID PRIMARY KEY DEFAULT gen_random_uuid(),          -- 主键
    take_id      UUID          NOT NULL REFERENCES biz_stock_take(id) ON DELETE CASCADE,
    line_no      INT           NOT NULL,                              -- 行号（从 1 起）
    material_id  UUID          NOT NULL,                              -- 物料（弱引用 biz_material.id）
    unit_id      UUID          NOT NULL,                              -- 单位（弱引用 biz_unit.id；物料基础单位快照）
    location_id  UUID,                                                -- 库位账行（弱引用 biz_inventory_location.id；NULL = 未分配桶）
    book_qty     NUMERIC(18,4) NOT NULL DEFAULT 0,                    -- 账面数（建单时快照，之后不改）
    actual_qty   NUMERIC(18,4) NOT NULL DEFAULT 0,                    -- 实盘数（人工填；建单时预填账面数）
    diff_qty     NUMERIC(18,4) GENERATED ALWAYS AS (actual_qty - book_qty) STORED, -- 差异 = 实盘 - 账面
    unit_cost    NUMERIC(18,6) NOT NULL DEFAULT 0,                    -- 账面成本快照（建单时的移动加权单价）
    diff_amount  NUMERIC(18,4) NOT NULL DEFAULT 0,                    -- 差异金额（过账时按账上真实增量/减量写入）
    remark       VARCHAR(255),
    created_at   TIMESTAMPTZ   NOT NULL DEFAULT CURRENT_TIMESTAMP,
    updated_at   TIMESTAMPTZ   NOT NULL DEFAULT CURRENT_TIMESTAMP,

    CONSTRAINT uk_biz_st_item_line   UNIQUE (take_id, line_no),
    CONSTRAINT ck_biz_st_item_qty    CHECK (book_qty >= 0 AND actual_qty >= 0),
    CONSTRAINT ck_biz_st_item_amount CHECK (unit_cost >= 0 AND diff_amount >= 0)
);

DO $$
BEGIN
    IF NOT EXISTS (SELECT 1 FROM pg_constraint
                    WHERE conname = 'uk_biz_st_item_line' AND conrelid = 'public.biz_stock_take_item'::regclass) THEN
        ALTER TABLE biz_stock_take_item ADD CONSTRAINT uk_biz_st_item_line UNIQUE (take_id, line_no);
    END IF;
    IF NOT EXISTS (SELECT 1 FROM pg_constraint
                    WHERE conname = 'ck_biz_st_item_qty' AND conrelid = 'public.biz_stock_take_item'::regclass) THEN
        ALTER TABLE biz_stock_take_item ADD CONSTRAINT ck_biz_st_item_qty
            CHECK (book_qty >= 0 AND actual_qty >= 0);
    END IF;
    IF NOT EXISTS (SELECT 1 FROM pg_constraint
                    WHERE conname = 'ck_biz_st_item_amount' AND conrelid = 'public.biz_stock_take_item'::regclass) THEN
        ALTER TABLE biz_stock_take_item ADD CONSTRAINT ck_biz_st_item_amount
            CHECK (unit_cost >= 0 AND diff_amount >= 0);
    END IF;
END
$$;

COMMENT ON TABLE  biz_stock_take_item IS '盘点单明细：book_qty 是建单时的账面快照（不可改），actual_qty 是人工实盘，diff_qty 是 PG 生成列（= actual - book，口径不靠代码自觉）';
COMMENT ON COLUMN biz_stock_take_item.location_id IS '库位账行 id（biz_inventory_location.id，与 biz_stock_in_item / biz_stock_out_item 同口径）；NULL = 未分配库位汇总桶';
COMMENT ON COLUMN biz_stock_take_item.diff_amount IS '差异金额：盘盈 = 过账时账上真实增量；盘亏 = 账上真实减量（都不是 qty×avg 重算）';

CREATE INDEX IF NOT EXISTS idx_biz_st_item_take     ON biz_stock_take_item (take_id);
CREATE INDEX IF NOT EXISTS idx_biz_st_item_material ON biz_stock_take_item (material_id);

-- ============================================================
-- §3 取号规则：biz_seq(seq_type='STOCK_TAKE') → ST-yyyyMMdd-000001
-- ============================================================
INSERT INTO biz_seq (id, seq_type, seq_name, prefix, date_pattern, period_key, current_no, step, separator, status, remark)
SELECT v.id::uuid, 'STOCK_TAKE', '盘点单号', 'ST', 'yyyyMMdd', '', 0, 1, '-', 1, v.remark
  FROM (VALUES
        ('d2110001-0000-4000-8000-000000000001', '盘点单号：ST-yyyyMMdd-000001，按天（CST）滚动')
       ) AS v(id, remark)
 WHERE NOT EXISTS (SELECT 1 FROM biz_seq WHERE seq_type = 'STOCK_TAKE' AND deleted_at IS NULL);

-- ============================================================
-- §4 权限码（1 个菜单节点 + 5 个按钮；与 `stocktake_dto::STOCK_TAKE_PERMISSIONS` 逐字一致）
--
--    business:erp:stock-take:list    列表   GET    /business/erp/stock-take/page
--    business:erp:stock-take:detail  详情   GET    /business/erp/stock-take/{id}
--    business:erp:stock-take:create  新建   POST   /business/erp/stock-take
--    business:erp:stock-take:update  编辑   PUT    /business/erp/stock-take/{id}
--    business:erp:stock-take:delete  删除   DELETE /business/erp/stock-take/{id}
--    business:erp:stock-take:status  状态   PATCH  /business/erp/stock-take/{id}/status
--
--    `:status` 独立（提交审批 / **过账会真的动三本账并出凭证** / 作废要红冲），
--    与「改几行草稿实盘数」不是同一类授权（同 stock-in / stock-out 口径）。
-- ============================================================
INSERT INTO sys_menu (id, name, menu_type, auth_code, path, parent_id, component,
                      sort_order, status, icon, title, is_visible, module_code)
VALUES
    ('20000000-0000-0000-0000-000000001111'::uuid, 'business:erp:stock-take:list', 2,
     'business:erp:stock-take:list', '/business/erp/stock-take',
     '20000000-0000-0000-0000-000000000946'::uuid, 'business/erp/stock-take/index',
     90, 1, 'file-copy', '盘点单', TRUE, 'business')
ON CONFLICT (id) DO NOTHING;

INSERT INTO sys_menu (id, name, menu_type, auth_code, path, parent_id, component,
                      sort_order, status, icon, title, is_visible, module_code)
VALUES
    ('20000000-0000-0000-0000-000000001112'::uuid, 'business:erp:stock-take:detail', 3,
     'business:erp:stock-take:detail', NULL,
     '20000000-0000-0000-0000-000000001111'::uuid, NULL,
     1, 1, NULL, '盘点单详情（账面 / 实盘 / 差异 / 凭证）', FALSE, 'business'),
    ('20000000-0000-0000-0000-000000001113'::uuid, 'business:erp:stock-take:create', 3,
     'business:erp:stock-take:create', NULL,
     '20000000-0000-0000-0000-000000001111'::uuid, NULL,
     2, 1, NULL, '新建盘点单（账面数自动快照）', FALSE, 'business'),
    ('20000000-0000-0000-0000-000000001114'::uuid, 'business:erp:stock-take:update', 3,
     'business:erp:stock-take:update', NULL,
     '20000000-0000-0000-0000-000000001111'::uuid, NULL,
     3, 1, NULL, '编辑盘点单（仅草稿：填实盘数）', FALSE, 'business'),
    ('20000000-0000-0000-0000-000000001115'::uuid, 'business:erp:stock-take:delete', 3,
     'business:erp:stock-take:delete', NULL,
     '20000000-0000-0000-0000-000000001111'::uuid, NULL,
     4, 1, NULL, '删除盘点单（仅草稿）', FALSE, 'business'),
    ('20000000-0000-0000-0000-000000001116'::uuid, 'business:erp:stock-take:status', 3,
     'business:erp:stock-take:status', NULL,
     '20000000-0000-0000-0000-000000001111'::uuid, NULL,
     5, 1, NULL, '盘点单状态流转（提交审批 / 过账 / 作废）', FALSE, 'business')
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
     WHERE id IN ('20000000-0000-0000-0000-000000001111'::uuid,
                  '20000000-0000-0000-0000-000000001112'::uuid,
                  '20000000-0000-0000-0000-000000001113'::uuid,
                  '20000000-0000-0000-0000-000000001114'::uuid,
                  '20000000-0000-0000-0000-000000001115'::uuid,
                  '20000000-0000-0000-0000-000000001116'::uuid);
    IF n_menus <> 6 THEN
        RAISE EXCEPTION 'V0211 自检失败：预期 6 个盘点权限节点，实际 %', n_menus;
    END IF;

    SELECT count(*) INTO n_roles FROM sys_role WHERE deleted_at IS NULL;
    IF n_roles = 0 THEN
        RAISE NOTICE 'V0211：sys_role 为空（空库首启，迁移先于 bootstrap），跳过授权；bootstrap 后重放本迁移即可补齐';
        RETURN;
    END IF;

    INSERT INTO sys_role_menu (id, role_id, menu_id)
    SELECT gen_random_uuid(), r.id, m.id
      FROM sys_role r
      CROSS JOIN (VALUES
            ('20000000-0000-0000-0000-000000001111'::uuid),
            ('20000000-0000-0000-0000-000000001112'::uuid),
            ('20000000-0000-0000-0000-000000001113'::uuid),
            ('20000000-0000-0000-0000-000000001114'::uuid),
            ('20000000-0000-0000-0000-000000001115'::uuid),
            ('20000000-0000-0000-0000-000000001116'::uuid)) AS m(id)
     WHERE r.deleted_at IS NULL
       AND r.role_code IN ('SUPER_ADMIN', 'ADMIN', 'WAREHOUSE_KEEPER', 'WAREHOUSE_MANAGER', 'FINANCE_MANAGER')
       AND NOT EXISTS (
            SELECT 1 FROM sys_role_menu rm WHERE rm.role_id = r.id AND rm.menu_id = m.id
       );
    GET DIAGNOSTICS n_grants = ROW_COUNT;
    RAISE NOTICE 'V0211：盘点单 6 个权限码已种，授权新增 % 行（角色数 %）', n_grants, n_roles;
END $$;

-- ============================================================
-- §5 审批流种子：stock_take_post（差异金额两档分流）
-- ============================================================
INSERT INTO biz_flow_def (
    id, flow_code, flow_name, biz_type, category, description, status,
    current_version_id, current_version_no, is_default, allow_auto_approve, owner_dept_id)
SELECT
    '20000000-0000-4000-8000-0000000c2110', 'stock_take_post', '盘点过账审批（标准·差异金额分级）',
    'STOCK_TAKE_POST', '库存管理',
    '盘点单提交审批：按盘点差异金额两档分流（< 1 万仓管主管单级审批 / ≥ 1 万仓管主管 + 总经理两级审批）。'
    '审批通过后单据进入「待过账」，由持过账权限的人过账（三本账 + 盘点差异凭证）。'
    '阈值与审批人都在本流程定义里改，不需要改代码。',
    'PUBLISHED', '20000000-0000-4000-8000-0000000c2111', 1, TRUE, FALSE, NULL
WHERE NOT EXISTS (
    SELECT 1 FROM biz_flow_def WHERE flow_code = 'stock_take_post' AND deleted_at IS NULL
)
AND NOT EXISTS (
    SELECT 1 FROM biz_flow_def
     WHERE biz_type = 'STOCK_TAKE_POST' AND is_default AND deleted_at IS NULL
)
-- 角色必须是种好的那两个（客户用别的 UUID 预建过 → 整条跳过，别种出悬空的 assignee）
AND EXISTS (
    SELECT 1 FROM sys_role
     WHERE id = '20000000-0000-4000-8000-0000000f0004'::uuid AND deleted_at IS NULL
)
AND EXISTS (
    SELECT 1 FROM sys_role
     WHERE id = '20000000-0000-4000-8000-0000000f0003'::uuid AND deleted_at IS NULL
);

INSERT INTO biz_flow_def_version (
    id, def_id, version_no, version_status, is_current, graph_hash, graph_snapshot,
    node_count, edge_count, change_note, published_at, published_by)
SELECT
    '20000000-0000-4000-8000-0000000c2111', '20000000-0000-4000-8000-0000000c2110', 1, 'PUBLISHED', TRUE,
    'cfc6101455d799fb4377353ad57c85dfae8f30956a94ae642c88e1644a6bfb4a',
    $snapshot${"edges":[{"condition":null,"from":"s","is_default":false,"key":"e_start","label":null,"priority":0,"to":"c_amt"},{"condition":{"cmp":"lt","field":"form.total_diff_amount","op":"cmp","value":10000},"from":"c_amt","is_default":false,"key":"e_low","label":"差异金额 < 10000（单级审批）","priority":0,"to":"n_wh"},{"condition":null,"from":"c_amt","is_default":true,"key":"e_low_else","label":"差异金额 ≥ 10000（两级审批）","priority":10,"to":"n_fin"},{"condition":null,"from":"n_wh","is_default":false,"key":"e_wh_end","label":null,"priority":0,"to":"e"},{"condition":null,"from":"n_fin","is_default":false,"key":"e_fin_gm","label":null,"priority":0,"to":"n_gm"},{"condition":null,"from":"n_gm","is_default":false,"key":"e_gm_end","label":null,"priority":0,"to":"e"}],"nodes":[{"allow_robot":false,"approval_mode":null,"approve_ratio":null,"assignee_type":null,"assignees":[],"cc_on_approved":false,"config":{},"empty_assignee":"SKIP","fork_mode":"AND","form_var_key":null,"join_mode":"AND","key":"s","max_remind_times":3,"name":"开始","node_type":"START","reject_veto":true,"remind_before_hours":null,"self_approval":"SKIP","sort_order":1,"superior_levels":1,"timeout_action":"REMIND","timeout_hours":null,"x":0,"y":0},{"allow_robot":false,"approval_mode":null,"approve_ratio":null,"assignee_type":null,"assignees":[],"cc_on_approved":false,"config":{},"empty_assignee":"SKIP","fork_mode":"AND","form_var_key":null,"join_mode":"OR","key":"c_amt","max_remind_times":3,"name":"差异金额分级（< 1 万单级）","node_type":"CONDITION","reject_veto":true,"remind_before_hours":null,"self_approval":"SKIP","sort_order":2,"superior_levels":1,"timeout_action":"REMIND","timeout_hours":null,"x":200,"y":0},{"allow_robot":false,"approval_mode":null,"approve_ratio":null,"assignee_type":"ROLE","assignees":[{"assignee_type":"ROLE","ref_id":"20000000-0000-4000-8000-0000000f0004","sort_order":0}],"cc_on_approved":false,"config":{},"empty_assignee":"FALLBACK_ADMIN","fork_mode":"AND","form_var_key":null,"join_mode":"AND","key":"n_wh","max_remind_times":2,"name":"仓管主管审批（< 1 万）","node_type":"APPROVAL","reject_veto":true,"remind_before_hours":null,"self_approval":"ALLOW","sort_order":3,"superior_levels":1,"timeout_action":"REMIND","timeout_hours":24,"x":420,"y":-140},{"allow_robot":false,"approval_mode":null,"approve_ratio":null,"assignee_type":"ROLE","assignees":[{"assignee_type":"ROLE","ref_id":"20000000-0000-4000-8000-0000000f0004","sort_order":0}],"cc_on_approved":false,"config":{},"empty_assignee":"FALLBACK_ADMIN","fork_mode":"AND","form_var_key":null,"join_mode":"AND","key":"n_fin","max_remind_times":2,"name":"仓管主管审批（≥ 1 万·第一级）","node_type":"APPROVAL","reject_veto":true,"remind_before_hours":null,"self_approval":"ALLOW","sort_order":4,"superior_levels":1,"timeout_action":"REMIND","timeout_hours":24,"x":420,"y":140},{"allow_robot":false,"approval_mode":null,"approve_ratio":null,"assignee_type":"ROLE","assignees":[{"assignee_type":"ROLE","ref_id":"20000000-0000-4000-8000-0000000f0003","sort_order":0}],"cc_on_approved":false,"config":{},"empty_assignee":"FALLBACK_ADMIN","fork_mode":"AND","form_var_key":null,"join_mode":"AND","key":"n_gm","max_remind_times":2,"name":"总经理审批（≥ 1 万·第二级）","node_type":"APPROVAL","reject_veto":true,"remind_before_hours":null,"self_approval":"ALLOW","sort_order":5,"superior_levels":1,"timeout_action":"REMIND","timeout_hours":48,"x":640,"y":140},{"allow_robot":false,"approval_mode":null,"approve_ratio":null,"assignee_type":null,"assignees":[],"cc_on_approved":false,"config":{},"empty_assignee":"SKIP","fork_mode":"AND","form_var_key":null,"join_mode":"OR","key":"e","max_remind_times":3,"name":"结束","node_type":"END","reject_veto":true,"remind_before_hours":null,"self_approval":"SKIP","sort_order":6,"superior_levels":1,"timeout_action":"REMIND","timeout_hours":null,"x":860,"y":0}],"schema_version":1}$snapshot$::jsonb,
    6, 6, 'V0211 种子：盘点过账审批（差异金额两档分流）默认流程', now(), NULL
WHERE EXISTS (SELECT 1 FROM biz_flow_def WHERE id = '20000000-0000-4000-8000-0000000c2110')
ON CONFLICT (def_id, version_no) DO NOTHING;

INSERT INTO biz_flow_node (
    id, version_id, node_key, node_name, node_type, assignee_type, superior_levels,
    form_var_key, approval_mode, approve_ratio, reject_veto, empty_assignee, self_approval,
    allow_robot, join_mode, fork_mode, timeout_hours, timeout_action, remind_before_hours,
    max_remind_times, cc_on_approved, position_x, position_y, sort_order, config)
SELECT * FROM (VALUES
    ('20000000-0000-4000-8000-0000000c2120'::uuid, '20000000-0000-4000-8000-0000000c2111'::uuid, 's'::varchar, '开始'::varchar, 'START'::varchar, NULL::varchar, 1::int, NULL::varchar, NULL::varchar, NULL::numeric, TRUE::bool, 'SKIP'::varchar, 'SKIP'::varchar, FALSE::bool, 'AND'::varchar, 'AND'::varchar, NULL::int, 'REMIND'::varchar, NULL::int, 3::smallint, FALSE::bool, 0::int, 0::int, 1::int, '{}'::jsonb),
    ('20000000-0000-4000-8000-0000000c2121'::uuid, '20000000-0000-4000-8000-0000000c2111'::uuid, 'c_amt'::varchar, '差异金额分级（< 1 万单级）'::varchar, 'CONDITION'::varchar, NULL::varchar, 1::int, NULL::varchar, NULL::varchar, NULL::numeric, TRUE::bool, 'SKIP'::varchar, 'SKIP'::varchar, FALSE::bool, 'OR'::varchar, 'AND'::varchar, NULL::int, 'REMIND'::varchar, NULL::int, 3::smallint, FALSE::bool, 200::int, 0::int, 2::int, '{}'::jsonb),
    ('20000000-0000-4000-8000-0000000c2122'::uuid, '20000000-0000-4000-8000-0000000c2111'::uuid, 'n_wh'::varchar, '仓管主管审批（< 1 万）'::varchar, 'APPROVAL'::varchar, 'ROLE'::varchar, 1::int, NULL::varchar, NULL::varchar, NULL::numeric, TRUE::bool, 'FALLBACK_ADMIN'::varchar, 'ALLOW'::varchar, FALSE::bool, 'AND'::varchar, 'AND'::varchar, 24::int, 'REMIND'::varchar, NULL::int, 2::smallint, FALSE::bool, 420::int, -140::int, 3::int, '{}'::jsonb),
    ('20000000-0000-4000-8000-0000000c2123'::uuid, '20000000-0000-4000-8000-0000000c2111'::uuid, 'n_fin'::varchar, '仓管主管审批（≥ 1 万·第一级）'::varchar, 'APPROVAL'::varchar, 'ROLE'::varchar, 1::int, NULL::varchar, NULL::varchar, NULL::numeric, TRUE::bool, 'FALLBACK_ADMIN'::varchar, 'ALLOW'::varchar, FALSE::bool, 'AND'::varchar, 'AND'::varchar, 24::int, 'REMIND'::varchar, NULL::int, 2::smallint, FALSE::bool, 420::int, 140::int, 4::int, '{}'::jsonb),
    ('20000000-0000-4000-8000-0000000c2124'::uuid, '20000000-0000-4000-8000-0000000c2111'::uuid, 'n_gm'::varchar, '总经理审批（≥ 1 万·第二级）'::varchar, 'APPROVAL'::varchar, 'ROLE'::varchar, 1::int, NULL::varchar, NULL::varchar, NULL::numeric, TRUE::bool, 'FALLBACK_ADMIN'::varchar, 'ALLOW'::varchar, FALSE::bool, 'AND'::varchar, 'AND'::varchar, 48::int, 'REMIND'::varchar, NULL::int, 2::smallint, FALSE::bool, 640::int, 140::int, 5::int, '{}'::jsonb),
    ('20000000-0000-4000-8000-0000000c2125'::uuid, '20000000-0000-4000-8000-0000000c2111'::uuid, 'e'::varchar, '结束'::varchar, 'END'::varchar, NULL::varchar, 1::int, NULL::varchar, NULL::varchar, NULL::numeric, TRUE::bool, 'SKIP'::varchar, 'SKIP'::varchar, FALSE::bool, 'OR'::varchar, 'AND'::varchar, NULL::int, 'REMIND'::varchar, NULL::int, 3::smallint, FALSE::bool, 860::int, 0::int, 6::int, '{}'::jsonb)
) AS v
WHERE EXISTS (SELECT 1 FROM biz_flow_def_version WHERE id = '20000000-0000-4000-8000-0000000c2111')
ON CONFLICT (version_id, node_key) DO NOTHING;

INSERT INTO biz_flow_edge (
    id, version_id, edge_key, from_node_id, to_node_id, condition_expr,
    condition_label, priority, is_default)
SELECT * FROM (VALUES
    ('20000000-0000-4000-8000-0000000c2130'::uuid, '20000000-0000-4000-8000-0000000c2111'::uuid, 'e_start'::varchar, '20000000-0000-4000-8000-0000000c2120'::uuid, '20000000-0000-4000-8000-0000000c2121'::uuid, NULL::jsonb, NULL::varchar, 0::int, FALSE::bool),
    ('20000000-0000-4000-8000-0000000c2131'::uuid, '20000000-0000-4000-8000-0000000c2111'::uuid, 'e_low'::varchar, '20000000-0000-4000-8000-0000000c2121'::uuid, '20000000-0000-4000-8000-0000000c2122'::uuid, '{"op": "cmp", "field": "form.total_diff_amount", "cmp": "lt", "value": 10000}'::jsonb, '差异金额 < 10000（单级审批）'::varchar, 0::int, FALSE::bool),
    ('20000000-0000-4000-8000-0000000c2132'::uuid, '20000000-0000-4000-8000-0000000c2111'::uuid, 'e_low_else'::varchar, '20000000-0000-4000-8000-0000000c2121'::uuid, '20000000-0000-4000-8000-0000000c2123'::uuid, NULL::jsonb, '差异金额 ≥ 10000（两级审批）'::varchar, 10::int, TRUE::bool),
    ('20000000-0000-4000-8000-0000000c2133'::uuid, '20000000-0000-4000-8000-0000000c2111'::uuid, 'e_wh_end'::varchar, '20000000-0000-4000-8000-0000000c2122'::uuid, '20000000-0000-4000-8000-0000000c2125'::uuid, NULL::jsonb, NULL::varchar, 0::int, FALSE::bool),
    ('20000000-0000-4000-8000-0000000c2134'::uuid, '20000000-0000-4000-8000-0000000c2111'::uuid, 'e_fin_gm'::varchar, '20000000-0000-4000-8000-0000000c2123'::uuid, '20000000-0000-4000-8000-0000000c2124'::uuid, NULL::jsonb, NULL::varchar, 0::int, FALSE::bool),
    ('20000000-0000-4000-8000-0000000c2135'::uuid, '20000000-0000-4000-8000-0000000c2111'::uuid, 'e_gm_end'::varchar, '20000000-0000-4000-8000-0000000c2124'::uuid, '20000000-0000-4000-8000-0000000c2125'::uuid, NULL::jsonb, NULL::varchar, 0::int, FALSE::bool)
) AS v
WHERE EXISTS (SELECT 1 FROM biz_flow_def_version WHERE id = '20000000-0000-4000-8000-0000000c2111')
ON CONFLICT (version_id, edge_key) DO NOTHING;

INSERT INTO biz_flow_node_assignee (node_id, assignee_type, ref_id, sort_order)
SELECT * FROM (VALUES
    ('20000000-0000-4000-8000-0000000c2122'::uuid, 'ROLE'::varchar, '20000000-0000-4000-8000-0000000f0004'::uuid, 0::int),
    ('20000000-0000-4000-8000-0000000c2123'::uuid, 'ROLE'::varchar, '20000000-0000-4000-8000-0000000f0004'::uuid, 0::int),
    ('20000000-0000-4000-8000-0000000c2124'::uuid, 'ROLE'::varchar, '20000000-0000-4000-8000-0000000f0003'::uuid, 0::int)
) AS v
WHERE EXISTS (SELECT 1 FROM biz_flow_def_version WHERE id = '20000000-0000-4000-8000-0000000c2111')
ON CONFLICT (node_id, assignee_type, ref_id) DO NOTHING;

-- ============================================================
-- §6 自检（缺一条就让迁移失败，而不是留半张表 / 半张图）
-- ============================================================
DO $$
DECLARE
    need_col TEXT;
    miss     TEXT := '';
    ck       TEXT;
    need_idx TEXT;
    n        integer;
    v_ok     integer;
BEGIN
    FOREACH need_col IN ARRAY ARRAY[
        'take_no', 'warehouse_id', 'take_date', 'take_type', 'status', 'approval_instance_id',
        'voucher_id', 'reverse_voucher_id', 'total_book_qty', 'total_actual_qty',
        'total_gain_qty', 'total_loss_qty', 'total_gain_amount', 'total_loss_amount',
        'remark', 'posted_at', 'posted_by', 'cancelled_at', 'cancelled_by', 'cancel_reason',
        'created_at', 'updated_at', 'create_by', 'update_by', 'deleted_at'
    ] LOOP
        IF NOT EXISTS (SELECT 1 FROM information_schema.columns
                        WHERE table_schema = 'public' AND table_name = 'biz_stock_take'
                          AND column_name = need_col) THEN
            miss := miss || ' biz_stock_take.' || need_col;
        END IF;
    END LOOP;

    FOREACH need_col IN ARRAY ARRAY[
        'take_id', 'line_no', 'material_id', 'unit_id', 'location_id',
        'book_qty', 'actual_qty', 'diff_qty', 'unit_cost', 'diff_amount', 'remark'
    ] LOOP
        IF NOT EXISTS (SELECT 1 FROM information_schema.columns
                        WHERE table_schema = 'public' AND table_name = 'biz_stock_take_item'
                          AND column_name = need_col) THEN
            miss := miss || ' biz_stock_take_item.' || need_col;
        END IF;
    END LOOP;

    IF miss <> '' THEN
        RAISE EXCEPTION 'V0211 自检失败：缺列 →%', miss;
    END IF;

    -- diff_qty 必须是**生成列**（口径由 PG 保证，不许变成普通列被手写）
    SELECT count(*) INTO n FROM information_schema.columns
     WHERE table_schema = 'public' AND table_name = 'biz_stock_take_item'
       AND column_name = 'diff_qty' AND is_generated = 'ALWAYS';
    IF n <> 1 THEN
        RAISE EXCEPTION 'V0211 自检失败：biz_stock_take_item.diff_qty 必须是 GENERATED ALWAYS ... STORED';
    END IF;

    FOREACH ck IN ARRAY ARRAY[
        'ck_biz_st_status', 'ck_biz_st_type', 'ck_biz_st_totals', 'ck_biz_st_cancel',
        'uk_biz_st_item_line', 'ck_biz_st_item_qty', 'ck_biz_st_item_amount'
    ] LOOP
        IF NOT EXISTS (SELECT 1 FROM pg_constraint WHERE conname = ck) THEN
            RAISE EXCEPTION 'V0211 自检失败：缺约束 %', ck;
        END IF;
    END LOOP;

    FOREACH need_idx IN ARRAY ARRAY[
        'uk_biz_st_no', 'idx_biz_st_status', 'idx_biz_st_date', 'idx_biz_st_warehouse',
        'idx_biz_st_voucher', 'idx_biz_st_approval',
        'idx_biz_st_item_take', 'idx_biz_st_item_material'
    ] LOOP
        IF NOT EXISTS (SELECT 1 FROM pg_indexes
                        WHERE schemaname = 'public' AND indexname = need_idx) THEN
            RAISE EXCEPTION 'V0211 自检失败：缺索引 %', need_idx;
        END IF;
    END LOOP;

    -- 取号规则必须落库
    SELECT count(*) INTO n FROM biz_seq WHERE seq_type = 'STOCK_TAKE' AND deleted_at IS NULL;
    IF n <> 1 THEN
        RAISE EXCEPTION 'V0211 自检失败：biz_seq(STOCK_TAKE) 应有 1 行，实际 %', n;
    END IF;

    -- 权限节点
    SELECT count(*) INTO n FROM sys_menu
     WHERE id IN ('20000000-0000-0000-0000-000000001111'::uuid,
                  '20000000-0000-0000-0000-000000001112'::uuid,
                  '20000000-0000-0000-0000-000000001113'::uuid,
                  '20000000-0000-0000-0000-000000001114'::uuid,
                  '20000000-0000-0000-0000-000000001115'::uuid,
                  '20000000-0000-0000-0000-000000001116'::uuid);
    IF n <> 6 THEN
        RAISE EXCEPTION 'V0211 自检失败：权限节点应为 6 个，实际 %', n;
    END IF;

    -- 审批流（客户已有同名流程 → 守卫整条跳过，但结构必须一致）
    IF NOT EXISTS (SELECT 1 FROM biz_flow_def WHERE id = '20000000-0000-4000-8000-0000000c2110') THEN
        RAISE NOTICE 'V0211：stock_take_post 种子跳过（已有同名流程 / 同 biz_type 默认流程 / 角色 UUID 不符）';
    ELSE
        SELECT count(*) INTO v_ok FROM biz_flow_def d
         WHERE d.id = '20000000-0000-4000-8000-0000000c2110'
           AND d.flow_code = 'stock_take_post' AND d.biz_type = 'STOCK_TAKE_POST'
           AND d.status = 'PUBLISHED' AND d.is_default
           AND d.current_version_id = '20000000-0000-4000-8000-0000000c2111'
           AND d.deleted_at IS NULL;
        IF v_ok <> 1 THEN
            RAISE EXCEPTION 'V0211 自检失败：stock_take_post 定义行不满足 PUBLISHED/默认/当前版本';
        END IF;

        SELECT count(*) INTO v_ok FROM biz_flow_def_version v
         WHERE v.id = '20000000-0000-4000-8000-0000000c2111'
           AND v.version_status = 'PUBLISHED' AND v.is_current
           AND v.graph_hash = 'cfc6101455d799fb4377353ad57c85dfae8f30956a94ae642c88e1644a6bfb4a'
           AND v.node_count = 6 AND v.edge_count = 6 AND v.published_at IS NOT NULL;
        IF v_ok <> 1 THEN
            RAISE EXCEPTION 'V0211 自检失败：stock_take_post 版本行（快照/hash/计数）不对';
        END IF;

        SELECT count(*) INTO n FROM biz_flow_node WHERE version_id = '20000000-0000-4000-8000-0000000c2111';
        IF n <> 6 THEN
            RAISE EXCEPTION 'V0211 自检失败：stock_take_post 节点应为 6 个，实际 %', n;
        END IF;
        SELECT count(*) INTO n FROM biz_flow_edge WHERE version_id = '20000000-0000-4000-8000-0000000c2111';
        IF n <> 6 THEN
            RAISE EXCEPTION 'V0211 自检失败：stock_take_post 边应为 6 条，实际 %', n;
        END IF;

        -- 差异金额分级必须落在条件边上（不是代码 if/else）
        SELECT count(*) INTO n FROM biz_flow_edge
         WHERE version_id = '20000000-0000-4000-8000-0000000c2111'
           AND NOT is_default AND condition_expr IS NOT NULL
           AND condition_expr->>'field' = 'form.total_diff_amount'
           AND (condition_expr->>'value')::numeric = 10000;
        IF n <> 1 THEN
            RAISE EXCEPTION 'V0211 自检失败：1 万分级阈值必须落在条件边上（实际 % 条）', n;
        END IF;
        -- 必须恰有一条兜底边
        SELECT count(*) INTO n FROM biz_flow_edge
         WHERE version_id = '20000000-0000-4000-8000-0000000c2111' AND is_default;
        IF n <> 1 THEN
            RAISE EXCEPTION 'V0211 自检失败：条件节点必须有且仅有一条兜底边（实际 % 条）', n;
        END IF;

        -- 三个审批节点：ROLE 来源 + 没人时回退超管（不许静默自动通过）
        SELECT count(*) INTO n FROM biz_flow_node
         WHERE version_id = '20000000-0000-4000-8000-0000000c2111'
           AND node_key IN ('n_wh', 'n_fin', 'n_gm')
           AND node_type = 'APPROVAL' AND assignee_type = 'ROLE'
           AND empty_assignee = 'FALLBACK_ADMIN' AND self_approval = 'ALLOW';
        IF n <> 3 THEN
            RAISE EXCEPTION 'V0211 自检失败：盘点审批三个节点的审批人来源/兜底策略不对（应为 3 行，实际 %）', n;
        END IF;

        -- 两级审批必须串行（n_fin → n_gm），不许会签
        SELECT count(*) INTO n FROM biz_flow_edge
         WHERE version_id = '20000000-0000-4000-8000-0000000c2111' AND edge_key = 'e_fin_gm';
        IF n <> 1 THEN
            RAISE EXCEPTION 'V0211 自检失败：≥ 1 万必须两级串行（缺 e_fin_gm 边）';
        END IF;
        SELECT count(*) INTO n FROM biz_flow_node
         WHERE version_id = '20000000-0000-4000-8000-0000000c2111' AND node_type = 'COUNTERSIGN';
        IF n <> 0 THEN
            RAISE EXCEPTION 'V0211 自检失败：两级审批是串行的，不该有 COUNTERSIGN 会签节点';
        END IF;

        -- END 有两条入边 ⇒ 必须 OR 汇聚（AND 会等人齐 → 审批单永久卡死）
        SELECT count(*) INTO n FROM biz_flow_node
         WHERE version_id = '20000000-0000-4000-8000-0000000c2111' AND node_key = 'e' AND join_mode = 'OR';
        IF n <> 1 THEN
            RAISE EXCEPTION 'V0211 自检失败：stock_take_post 的 END 节点必须 OR 汇聚';
        END IF;
    END IF;

    RAISE NOTICE 'V0211 就绪：盘点两表 / 8 索引 / 7 约束 / 6 权限码 / 1 取号规则 / 1 审批流程（差异金额分档）';
END $$;

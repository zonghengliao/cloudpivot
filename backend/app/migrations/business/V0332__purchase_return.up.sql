-- ======================================================
-- V0332：采购退货闭环（P2-03）—— 退货单两表 + 供应商应退款表 + 取号
--        + 4 个权限码 + 采购管理下的页面与按钮 + 开箱即用的审批流 prt_approve
--
-- ── 为什么是三张表 ─────────────────────────────────────
--   ① `biz_purchase_return` / `biz_purchase_return_item`：退货单（卡面 §① 点名）；
--   ② `biz_supplier_receivable`：**供应商应退款**（对供应商的应收）。
--      卡面 §④ d) 要求「已全额付款的订单退货 → 生成对供应商的应收（或挂账）」。
--      不能塞进 `biz_receivable`（那是销售侧，`ck_biz_ar_amount CHECK (amount > 0)` +
--      `customer_id NOT NULL` 都是客户语义，把供应商 id 写进 customer_id 是造第二个真相源）；
--      也不能用负向应付（`ck_biz_payable_amount CHECK (amount >= 0)` 物理上插不进去）。
--      所以单独一张子台账：`amount > 0`、`received_amount <= amount`、`unsettled_amount`
--      恒等于差额，与应收/应付同构。
--
-- ── 金额口径（与收货过账同源，不重算）─────────────────────
--   退货单价 = 原入库明细的 `price` / `price_incl` / `tax_rate` **快照**（卡面 §④ b：
--   不许改单价）。每行：
--     amount         = round(qty × price, 4)
--     with_tax_amount= round(qty × price_incl, 4)
--     tax_amount     = with_tax_amount − amount        ← 倒挤（与 V0145 的精确恒等一致）
--   单头三金额 = 明细求和；`ck_biz_prt_tax` 保证 `total_with_tax = total_amount + total_tax`
--   精确成立（PRD §3.1.3：税额由价税合计倒挤）。
--
-- ── 幂等 ──────────────────────────────────────────────
--   · `uk_biz_purchase_return_no`：单号存活唯一（软删后可复用）；
--   · `uk_biz_prt_source_stock_out`：**一张退货单只生成一张出库单**
--     （`biz_stock_out.source_type='PURCHASE_RETURN'` + `uk_biz_so_source` 是第二道，
--      这里是本表侧的回链唯一性：`stock_out_id` 非空即唯一）；
--   · `uk_biz_sr_source (source_type, source_id)`：一张退货单只生成一张供应商应退款。
--
-- ── 号段 ──────────────────────────────────────────────
--   写入本文件时 `backend/migrations/business/` 最大号 = V0331（P2-02 采购申请）。
--   本卡取 V0332；提交前再 `ls business | tail` 复核一次（并行卡会抢号段，已应用的不可改）。
-- ======================================================

-- ---------- ① 退货单主表 ----------
CREATE TABLE IF NOT EXISTS biz_purchase_return (
    id                      UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    return_no               VARCHAR(32)   NOT NULL,            -- PRT-yyyyMMdd-000001
    stock_in_id             UUID          NOT NULL,            -- 来源入库单（弱引用 biz_stock_in.id）
    purchase_order_id       UUID,                              -- 来源采购订单（由入库单带出，可空）
    supplier_id             UUID          NOT NULL,            -- 供应商（弱引用 biz_supplier.id）
    warehouse_id            UUID          NOT NULL,            -- 退货出库仓库（= 原入库仓库）
    return_date             DATE          NOT NULL,            -- 退货日期（决定所属会计期间）
    status                  VARCHAR(24)   NOT NULL DEFAULT 'DRAFT',
    total_qty               NUMERIC(18,4) NOT NULL DEFAULT 0,  -- 退货数量合计
    total_amount            NUMERIC(18,4) NOT NULL DEFAULT 0,  -- 不含税金额合计
    total_tax               NUMERIC(18,4) NOT NULL DEFAULT 0,  -- 税额合计
    total_with_tax          NUMERIC(18,4) NOT NULL DEFAULT 0,  -- 价税合计
    reason                  VARCHAR(255)  NOT NULL,            -- 退货原因（审计要能答「为什么退」）
    approval_instance_id    UUID,                              -- 审批实例（弱引用 biz_flow_instance.id）
    stock_out_id            UUID,                              -- 确认时生成的出库单（biz_stock_out.id）
    payable_id              UUID,                              -- 被冲减的应付单（biz_payable.id）
    supplier_receivable_id  UUID,                              -- 已付款部分的供应商应退款
    voucher_id              UUID,                              -- 红字/冲销凭证（biz_voucher.id）
    confirmed_at            TIMESTAMPTZ,
    confirmed_by            UUID,
    cancelled_at            TIMESTAMPTZ,
    cancelled_by            UUID,
    cancel_reason           VARCHAR(255),
    remark                  TEXT,
    created_at              TIMESTAMPTZ   NOT NULL DEFAULT CURRENT_TIMESTAMP,
    updated_at              TIMESTAMPTZ   NOT NULL DEFAULT CURRENT_TIMESTAMP,
    create_by               UUID,
    update_by               UUID,
    deleted_at              TIMESTAMPTZ,

    CONSTRAINT ck_biz_prt_status
        CHECK (status IN ('DRAFT', 'SUBMITTED', 'APPROVED', 'CONFIRMED', 'CANCELLED')),
    -- 价税合计恒等（精确相等，口径同 V0145 / PRD §3.1.3）
    CONSTRAINT ck_biz_prt_tax CHECK (total_with_tax = total_amount + total_tax),
    CONSTRAINT ck_biz_prt_amounts
        CHECK (total_qty >= 0 AND total_amount >= 0 AND total_tax >= 0 AND total_with_tax >= 0),
    CONSTRAINT ck_biz_prt_reason CHECK (btrim(reason) <> ''),
    -- 已确认必须有出库单与凭证（账务完整性；待确认前两列一律 NULL）
    CONSTRAINT ck_biz_prt_confirmed
        CHECK (status <> 'CONFIRMED' OR (stock_out_id IS NOT NULL AND voucher_id IS NOT NULL)),
    -- 已作废必须有原因与时间
    CONSTRAINT ck_biz_prt_cancelled
        CHECK (status <> 'CANCELLED' OR (cancel_reason IS NOT NULL AND cancelled_at IS NOT NULL))
);

CREATE UNIQUE INDEX IF NOT EXISTS uk_biz_purchase_return_no
    ON biz_purchase_return (return_no) WHERE deleted_at IS NULL;
-- 回链唯一：一张退货单最多一张出库单（并发 / 重试的第二道闸，第一道是 uk_biz_so_source）
CREATE UNIQUE INDEX IF NOT EXISTS uk_biz_prt_source_stock_out
    ON biz_purchase_return (stock_out_id) WHERE stock_out_id IS NOT NULL AND deleted_at IS NULL;
CREATE INDEX IF NOT EXISTS idx_biz_prt_stock_in
    ON biz_purchase_return (stock_in_id, status);
CREATE INDEX IF NOT EXISTS idx_biz_prt_supplier
    ON biz_purchase_return (supplier_id, status);
CREATE INDEX IF NOT EXISTS idx_biz_prt_date
    ON biz_purchase_return (return_date DESC, return_no DESC);
CREATE INDEX IF NOT EXISTS idx_biz_prt_payable
    ON biz_purchase_return (payable_id) WHERE payable_id IS NOT NULL;

COMMENT ON TABLE  biz_purchase_return IS '采购退货单（P2-03）：退货单 → 审批 → 红字应付 → 库存回冲 → 凭证';
COMMENT ON COLUMN biz_purchase_return.return_no IS '退货单号：PRT-yyyyMMdd-000001（biz_seq seq_type=PRT，按 CST 的天滚动）';
COMMENT ON COLUMN biz_purchase_return.stock_out_id IS '确认退货时生成的出库单（biz_stock_out，out_type=OTHER + source_type=PURCHASE_RETURN）';
COMMENT ON COLUMN biz_purchase_return.supplier_receivable_id IS '已付款部分转成的供应商应退款（biz_supplier_receivable.id）；未付款部分直接冲减应付';
COMMENT ON COLUMN biz_purchase_return.reason IS '退货原因（必填）：质量 / 短交 / 错发 / 价格 — 审计与供应商对账都要能回答「为什么退」';

-- ---------- ② 退货单明细 ----------
CREATE TABLE IF NOT EXISTS biz_purchase_return_item (
    id                    UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    return_id             UUID          NOT NULL
        REFERENCES biz_purchase_return(id) ON DELETE CASCADE,   -- 唯一硬外键
    line_no               INT           NOT NULL,
    stock_in_item_id      UUID          NOT NULL,               -- 来源入库明细（可退量按它算）
    purchase_order_item_id UUID,                                -- 源头采购明细（回写 received_qty 用）
    material_id           UUID          NOT NULL,
    unit_id               UUID          NOT NULL,
    qty                   NUMERIC(18,4) NOT NULL,
    price                 NUMERIC(18,6) NOT NULL DEFAULT 0,     -- 不含税单价（原入库单价快照，不许改）
    price_incl            NUMERIC(18,6) NOT NULL DEFAULT 0,     -- 含税单价（原入库单价快照）
    tax_rate              NUMERIC(7,4)  NOT NULL DEFAULT 0,     -- 税率（%）
    amount                NUMERIC(18,4) NOT NULL DEFAULT 0,
    tax_amount            NUMERIC(18,4) NOT NULL DEFAULT 0,
    with_tax_amount       NUMERIC(18,4) NOT NULL DEFAULT 0,
    location_id           UUID,                                 -- 原入库库位（出库按它扣）
    remark                VARCHAR(255),
    created_at            TIMESTAMPTZ   NOT NULL DEFAULT CURRENT_TIMESTAMP,
    updated_at            TIMESTAMPTZ   NOT NULL DEFAULT CURRENT_TIMESTAMP,

    CONSTRAINT uk_biz_prti_line UNIQUE (return_id, line_no),
    CONSTRAINT ck_biz_prti_qty  CHECK (qty > 0),
    CONSTRAINT ck_biz_prti_tax  CHECK (with_tax_amount = amount + tax_amount),
    CONSTRAINT ck_biz_prti_rate CHECK (tax_rate >= 0 AND tax_rate <= 100),
    CONSTRAINT ck_biz_prti_amounts CHECK (amount >= 0 AND tax_amount >= 0 AND with_tax_amount >= 0)
);

CREATE INDEX IF NOT EXISTS idx_biz_prti_material ON biz_purchase_return_item (material_id);
CREATE INDEX IF NOT EXISTS idx_biz_prti_stock_in_item
    ON biz_purchase_return_item (stock_in_item_id);

COMMENT ON TABLE biz_purchase_return_item IS '采购退货明细（单价是原入库单价快照，防价格操纵；可退量 ≤ 入库量 − 已退量）';

-- ---------- ③ 供应商应退款（对供应商的应收）----------
CREATE TABLE IF NOT EXISTS biz_supplier_receivable (
    id               UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    sr_no            VARCHAR(32)   NOT NULL,                    -- SR-yyyyMMdd-000001
    supplier_id      UUID          NOT NULL,
    source_type      VARCHAR(24)   NOT NULL DEFAULT 'PURCHASE_RETURN',
    source_id        UUID          NOT NULL,                    -- biz_purchase_return.id
    source_no        VARCHAR(32)   NOT NULL,                    -- 退货单号
    currency_code    CHAR(3)       NOT NULL DEFAULT 'CNY',
    amount           NUMERIC(18,4) NOT NULL,                    -- 供应商应退金额（> 0）
    received_amount  NUMERIC(18,4) NOT NULL DEFAULT 0,          -- 已收回金额
    unsettled_amount NUMERIC(18,4) NOT NULL DEFAULT 0,          -- 未收 = amount − received
    due_date         DATE          NOT NULL,
    period_code      VARCHAR(7)    NOT NULL,                    -- yyyy-MM
    status           VARCHAR(20)   NOT NULL DEFAULT 'OPEN',
    settled_at       TIMESTAMPTZ,
    voucher_id       UUID,
    remark           TEXT,
    created_at       TIMESTAMPTZ   NOT NULL DEFAULT CURRENT_TIMESTAMP,
    updated_at       TIMESTAMPTZ   NOT NULL DEFAULT CURRENT_TIMESTAMP,
    create_by        UUID,
    update_by        UUID,
    deleted_at       TIMESTAMPTZ,

    CONSTRAINT ck_biz_sr_status
        CHECK (status IN ('OPEN', 'PARTIAL', 'SETTLED', 'CANCELLED')),
    CONSTRAINT ck_biz_sr_amount
        CHECK (amount > 0 AND received_amount >= 0 AND received_amount <= amount),
    CONSTRAINT ck_biz_sr_unsettled CHECK (unsettled_amount = amount - received_amount),
    CONSTRAINT ck_biz_sr_settled CHECK (status <> 'SETTLED' OR unsettled_amount = 0)
);

CREATE UNIQUE INDEX IF NOT EXISTS uk_biz_supplier_receivable_no
    ON biz_supplier_receivable (sr_no) WHERE deleted_at IS NULL;
CREATE UNIQUE INDEX IF NOT EXISTS uk_biz_sr_source
    ON biz_supplier_receivable (source_type, source_id) WHERE deleted_at IS NULL;
CREATE INDEX IF NOT EXISTS idx_biz_sr_supplier
    ON biz_supplier_receivable (supplier_id, status);

COMMENT ON TABLE biz_supplier_receivable IS
    '供应商应退款（对供应商的应收）：已付款部分退货后转为「供应商欠我们」，与 biz_receivable 客户语义分开';

-- ---------- ④ 取号规则 PRT / SR ----------
INSERT INTO biz_seq (id, seq_type, seq_name, prefix, date_pattern, period_key, current_no, step, separator, status, remark)
SELECT '20000000-0000-0000-0000-000000003342'::uuid, 'PRT', '采购退货单号', 'PRT', 'yyyyMMdd', '', 0, 1, '-', 1,
       '采购退货单号：PRT-yyyyMMdd-000001，按天（CST）滚动'
 WHERE NOT EXISTS (SELECT 1 FROM biz_seq WHERE seq_type = 'PRT' AND deleted_at IS NULL);

INSERT INTO biz_seq (id, seq_type, seq_name, prefix, date_pattern, period_key, current_no, step, separator, status, remark)
SELECT '20000000-0000-0000-0000-000000003343'::uuid, 'SR', '供应商应退款单号', 'SR', 'yyyyMMdd', '', 0, 1, '-', 1,
       '供应商应退款单号：SR-yyyyMMdd-000001，按天（CST）滚动'
 WHERE NOT EXISTS (SELECT 1 FROM biz_seq WHERE seq_type = 'SR' AND deleted_at IS NULL);

-- ---------- ⑤ 菜单：页面节点 + 4 个按钮 ----------
INSERT INTO sys_menu (id, name, menu_type, auth_code, path, parent_id, component,
                      sort_order, status, icon, title, is_visible, module_code)
VALUES
    ('20000000-0000-0000-0000-000000003341'::uuid, '采购退货', 2,
     'business:erp:purchase-return:list', '/business/erp/purchase-return',
     '20000000-0000-0000-0000-000000000946'::uuid, 'business/erp/purchase-return/index',
     37, 1, 'rollback', '采购退货：选入库单带明细 → 审批 → 库存回冲 + 应付红字 + 凭证',
     TRUE, 'business')
ON CONFLICT (id) DO UPDATE
   SET name        = EXCLUDED.name,
       menu_type   = EXCLUDED.menu_type,
       auth_code   = EXCLUDED.auth_code,
       path        = EXCLUDED.path,
       parent_id   = EXCLUDED.parent_id,
       component   = EXCLUDED.component,
       sort_order  = EXCLUDED.sort_order,
       status      = EXCLUDED.status,
       icon        = EXCLUDED.icon,
       title       = EXCLUDED.title,
       is_visible  = EXCLUDED.is_visible,
       module_code = EXCLUDED.module_code,
       deleted_at  = NULL,
       updated_at  = now();

INSERT INTO sys_menu (id, name, menu_type, auth_code, path, parent_id, component,
                      sort_order, status, icon, title, is_visible, module_code)
VALUES
    ('20000000-0000-0000-0000-000000003344'::uuid, '新建退货单', 3,
     'business:erp:purchase-return:create', NULL, '20000000-0000-0000-0000-000000003341'::uuid,
     NULL, 10, 1, NULL, '选采购入库单带出可退明细（单位 / 单价取原入库快照，不许改）', TRUE, 'business'),
    ('20000000-0000-0000-0000-000000003345'::uuid, '提交审批', 3,
     'business:erp:purchase-return:submit', NULL, '20000000-0000-0000-0000-000000003341'::uuid,
     NULL, 20, 1, NULL, '提交到 prt_approve 审批流（采购经理 → 财务经理两级）', TRUE, 'business'),
    ('20000000-0000-0000-0000-000000003346'::uuid, '确认退货', 3,
     'business:erp:purchase-return:confirm', NULL, '20000000-0000-0000-0000-000000003341'::uuid,
     NULL, 30, 1, NULL, '出库回冲 + 冲减应付（已付款部分转供应商应退款）+ 冲销凭证，同一事务', TRUE, 'business')
ON CONFLICT (id) DO UPDATE
   SET name        = EXCLUDED.name,
       menu_type   = EXCLUDED.menu_type,
       auth_code   = EXCLUDED.auth_code,
       path        = EXCLUDED.path,
       parent_id   = EXCLUDED.parent_id,
       component   = EXCLUDED.component,
       sort_order  = EXCLUDED.sort_order,
       status      = EXCLUDED.status,
       icon        = EXCLUDED.icon,
       title       = EXCLUDED.title,
       is_visible  = EXCLUDED.is_visible,
       module_code = EXCLUDED.module_code,
       deleted_at  = NULL,
       updated_at  = now();

-- ---------- ⑥ 角色授权（逐条说明，角色不存在时自然插入 0 行）----------
--   · PURCHASE_MANAGER：4 个码全给 —— 退货是采购的日常动作（建单 / 提交 / 执行确认）；
--   · WAREHOUSE_MANAGER：list + confirm —— 货在仓库里，出库回冲是仓储执行动作；
--   · FINANCE_MANAGER：list + confirm —— 应付红字与凭证是财务动作；
--   · DEPT_LEADER：只给 list —— 部门要知道退了什么，但不该自己退。
INSERT INTO sys_role_menu (role_id, menu_id)
SELECT r.id, m.menu_id
  FROM (VALUES
        ('PURCHASE_MANAGER',  '20000000-0000-0000-0000-000000003341'::uuid),
        ('PURCHASE_MANAGER',  '20000000-0000-0000-0000-000000003344'::uuid),
        ('PURCHASE_MANAGER',  '20000000-0000-0000-0000-000000003345'::uuid),
        ('PURCHASE_MANAGER',  '20000000-0000-0000-0000-000000003346'::uuid),
        ('WAREHOUSE_MANAGER', '20000000-0000-0000-0000-000000003341'::uuid),
        ('WAREHOUSE_MANAGER', '20000000-0000-0000-0000-000000003346'::uuid),
        ('FINANCE_MANAGER',   '20000000-0000-0000-0000-000000003341'::uuid),
        ('FINANCE_MANAGER',   '20000000-0000-0000-0000-000000003346'::uuid),
        ('DEPT_LEADER',       '20000000-0000-0000-0000-000000003341'::uuid)
       ) AS m(role_code, menu_id)
  JOIN sys_role r ON r.role_code = m.role_code AND r.deleted_at IS NULL
ON CONFLICT (role_id, menu_id) DO NOTHING;

-- ---------- ⑦ 流程定义：采购退货审批 prt_approve ----------
-- 四层守卫：已有 flow_code='prt_approve'、已有同 biz_type 的默认流程、
-- 或审批角色不是我们种的那一套 → 整条跳过，绝不覆盖客户配置。
-- 线性两级：采购经理（业务）→ 财务经理（账务）。财务必须过一道：退货直接冲击应付与存货。
INSERT INTO biz_flow_def (
    id, flow_code, flow_name, biz_type, category, description, status,
    current_version_id, current_version_no, is_default, allow_auto_approve, owner_dept_id)
SELECT
    '20000000-0000-4000-8000-0000000e0331', 'prt_approve', '采购退货审批（标准·两级）',
    'PRT_APPROVE', '采购管理',
    '采购退货提交审批：采购经理确认退货事实与数量，财务经理确认应付冲减与凭证口径。'
    '审批通过才变 APPROVED（此后才能确认退货：库存回冲 + 应付红字 + 冲销凭证）。'
    '驳回或撤回则打回草稿并把意见追加到备注。审批人都在本流程定义里改，不需要改代码。',
    'PUBLISHED', '20000000-0000-4000-8000-0000000e0332', 1, TRUE, FALSE, NULL
WHERE NOT EXISTS (
    SELECT 1 FROM biz_flow_def WHERE flow_code = 'prt_approve' AND deleted_at IS NULL
)
AND NOT EXISTS (
    SELECT 1 FROM biz_flow_def
     WHERE biz_type = 'PRT_APPROVE' AND is_default AND deleted_at IS NULL
)
AND (
    SELECT count(*) FROM sys_role
     WHERE id IN ('20000000-0000-4000-8000-0000000f0002'::uuid,
                  '20000000-0000-4000-8000-0000000f0005'::uuid)
       AND deleted_at IS NULL
) = 2;

INSERT INTO biz_flow_def_version (
    id, def_id, version_no, version_status, is_current, graph_hash, graph_snapshot,
    node_count, edge_count, change_note, published_at, published_by)
SELECT
    '20000000-0000-4000-8000-0000000e0332', '20000000-0000-4000-8000-0000000e0331', 1,
    'PUBLISHED', TRUE,
    '9debbf67fd8f3404a828bd626c614753b308b70e8b7de49c212b86869842507c',
    $snapshot${"edges":[{"condition":null,"from":"s","is_default":false,"key":"e_start","label":null,"priority":0,"to":"n_pm"},{"condition":null,"from":"n_pm","is_default":false,"key":"e_pm_fin","label":null,"priority":0,"to":"n_fin"},{"condition":null,"from":"n_fin","is_default":false,"key":"e_fin_end","label":null,"priority":0,"to":"e"}],"nodes":[{"allow_robot":false,"approval_mode":null,"approve_ratio":null,"assignee_type":null,"assignees":[],"cc_on_approved":false,"config":{},"empty_assignee":"SKIP","fork_mode":"AND","form_var_key":null,"join_mode":"AND","key":"s","max_remind_times":3,"name":"开始","node_type":"START","reject_veto":true,"remind_before_hours":null,"self_approval":"SKIP","sort_order":1,"superior_levels":1,"timeout_action":"REMIND","timeout_hours":null,"x":0,"y":0},{"allow_robot":false,"approval_mode":null,"approve_ratio":null,"assignee_type":"ROLE","assignees":[{"assignee_type":"ROLE","ref_id":"20000000-0000-4000-8000-0000000f0002","sort_order":0}],"cc_on_approved":false,"config":{},"empty_assignee":"FALLBACK_ADMIN","fork_mode":"AND","form_var_key":null,"join_mode":"AND","key":"n_pm","max_remind_times":2,"name":"采购经理审批","node_type":"APPROVAL","reject_veto":true,"remind_before_hours":null,"self_approval":"ALLOW","sort_order":2,"superior_levels":1,"timeout_action":"REMIND","timeout_hours":24,"x":220,"y":0},{"allow_robot":false,"approval_mode":null,"approve_ratio":null,"assignee_type":"ROLE","assignees":[{"assignee_type":"ROLE","ref_id":"20000000-0000-4000-8000-0000000f0005","sort_order":0}],"cc_on_approved":false,"config":{},"empty_assignee":"FALLBACK_ADMIN","fork_mode":"AND","form_var_key":null,"join_mode":"AND","key":"n_fin","max_remind_times":2,"name":"财务经理审批","node_type":"APPROVAL","reject_veto":true,"remind_before_hours":null,"self_approval":"ALLOW","sort_order":3,"superior_levels":1,"timeout_action":"REMIND","timeout_hours":24,"x":440,"y":0},{"allow_robot":false,"approval_mode":null,"approve_ratio":null,"assignee_type":null,"assignees":[],"cc_on_approved":false,"config":{},"empty_assignee":"SKIP","fork_mode":"AND","form_var_key":null,"join_mode":"OR","key":"e","max_remind_times":3,"name":"结束","node_type":"END","reject_veto":true,"remind_before_hours":null,"self_approval":"SKIP","sort_order":4,"superior_levels":1,"timeout_action":"REMIND","timeout_hours":null,"x":660,"y":0}],"schema_version":1}$snapshot$,
    4, 3, 'V0332 种子：采购退货审批（采购经理 → 财务经理 两级）默认流程', now(), NULL
WHERE EXISTS (SELECT 1 FROM biz_flow_def WHERE id = '20000000-0000-4000-8000-0000000e0331')
ON CONFLICT (def_id, version_no) DO NOTHING;

INSERT INTO biz_flow_node (
    id, version_id, node_key, node_name, node_type, assignee_type, superior_levels,
    form_var_key, approval_mode, approve_ratio, reject_veto, empty_assignee, self_approval,
    allow_robot, join_mode, fork_mode, timeout_hours, timeout_action, remind_before_hours,
    max_remind_times, cc_on_approved, position_x, position_y, sort_order, config)
SELECT * FROM (VALUES
    ('20000000-0000-4000-8000-0000000e0351'::uuid, '20000000-0000-4000-8000-0000000e0332'::uuid, 's'::varchar, '开始'::varchar, 'START'::varchar, NULL::varchar, 1::int, NULL::varchar, NULL::varchar, NULL::numeric, TRUE::bool, 'SKIP'::varchar, 'SKIP'::varchar, FALSE::bool, 'AND'::varchar, 'AND'::varchar, NULL::int, 'REMIND'::varchar, NULL::int, 3::smallint, FALSE::bool, 0::int, 0::int, 1::int, '{}'::jsonb),
    ('20000000-0000-4000-8000-0000000e0352'::uuid, '20000000-0000-4000-8000-0000000e0332'::uuid, 'n_pm'::varchar, '采购经理审批'::varchar, 'APPROVAL'::varchar, 'ROLE'::varchar, 1::int, NULL::varchar, NULL::varchar, NULL::numeric, TRUE::bool, 'FALLBACK_ADMIN'::varchar, 'ALLOW'::varchar, FALSE::bool, 'AND'::varchar, 'AND'::varchar, 24::int, 'REMIND'::varchar, NULL::int, 2::smallint, FALSE::bool, 220::int, 0::int, 2::int, '{}'::jsonb),
    ('20000000-0000-4000-8000-0000000e0353'::uuid, '20000000-0000-4000-8000-0000000e0332'::uuid, 'n_fin'::varchar, '财务经理审批'::varchar, 'APPROVAL'::varchar, 'ROLE'::varchar, 1::int, NULL::varchar, NULL::varchar, NULL::numeric, TRUE::bool, 'FALLBACK_ADMIN'::varchar, 'ALLOW'::varchar, FALSE::bool, 'AND'::varchar, 'AND'::varchar, 24::int, 'REMIND'::varchar, NULL::int, 2::smallint, FALSE::bool, 440::int, 0::int, 3::int, '{}'::jsonb),
    ('20000000-0000-4000-8000-0000000e0354'::uuid, '20000000-0000-4000-8000-0000000e0332'::uuid, 'e'::varchar, '结束'::varchar, 'END'::varchar, NULL::varchar, 1::int, NULL::varchar, NULL::varchar, NULL::numeric, TRUE::bool, 'SKIP'::varchar, 'SKIP'::varchar, FALSE::bool, 'OR'::varchar, 'AND'::varchar, NULL::int, 'REMIND'::varchar, NULL::int, 3::smallint, FALSE::bool, 660::int, 0::int, 4::int, '{}'::jsonb)
) AS v
WHERE EXISTS (SELECT 1 FROM biz_flow_def_version WHERE id = '20000000-0000-4000-8000-0000000e0332')
ON CONFLICT (version_id, node_key) DO NOTHING;

INSERT INTO biz_flow_edge (
    id, version_id, edge_key, from_node_id, to_node_id, condition_expr,
    condition_label, priority, is_default)
SELECT * FROM (VALUES
    ('20000000-0000-4000-8000-0000000e0361'::uuid, '20000000-0000-4000-8000-0000000e0332'::uuid, 'e_start'::varchar, '20000000-0000-4000-8000-0000000e0351'::uuid, '20000000-0000-4000-8000-0000000e0352'::uuid, NULL::jsonb, NULL::varchar, 0::int, FALSE::bool),
    ('20000000-0000-4000-8000-0000000e0362'::uuid, '20000000-0000-4000-8000-0000000e0332'::uuid, 'e_pm_fin'::varchar, '20000000-0000-4000-8000-0000000e0352'::uuid, '20000000-0000-4000-8000-0000000e0353'::uuid, NULL::jsonb, NULL::varchar, 0::int, FALSE::bool),
    ('20000000-0000-4000-8000-0000000e0363'::uuid, '20000000-0000-4000-8000-0000000e0332'::uuid, 'e_fin_end'::varchar, '20000000-0000-4000-8000-0000000e0353'::uuid, '20000000-0000-4000-8000-0000000e0354'::uuid, NULL::jsonb, NULL::varchar, 0::int, FALSE::bool)
) AS v
WHERE EXISTS (SELECT 1 FROM biz_flow_def_version WHERE id = '20000000-0000-4000-8000-0000000e0332')
ON CONFLICT (version_id, edge_key) DO NOTHING;

INSERT INTO biz_flow_node_assignee (node_id, assignee_type, ref_id, sort_order)
SELECT * FROM (VALUES
    ('20000000-0000-4000-8000-0000000e0352'::uuid, 'ROLE'::varchar, '20000000-0000-4000-8000-0000000f0002'::uuid, 0::int),
    ('20000000-0000-4000-8000-0000000e0353'::uuid, 'ROLE'::varchar, '20000000-0000-4000-8000-0000000f0005'::uuid, 0::int)
) AS v
WHERE EXISTS (SELECT 1 FROM biz_flow_def_version WHERE id = '20000000-0000-4000-8000-0000000e0332')
ON CONFLICT (node_id, assignee_type, ref_id) DO NOTHING;

-- ---------- ⑧ 自检 ----------
DO $$
DECLARE
    v_missing text;
    v_rows    integer;
    v_id      uuid;
BEGIN
    -- 8.1 三张表
    SELECT string_agg(t, ', ') INTO v_missing
      FROM (VALUES ('biz_purchase_return'), ('biz_purchase_return_item'),
                   ('biz_supplier_receivable')) AS x(t)
     WHERE NOT EXISTS (SELECT 1 FROM information_schema.tables
                        WHERE table_schema = current_schema() AND table_name = x.t);
    IF v_missing IS NOT NULL THEN
        RAISE EXCEPTION 'P2-03: 以下表未建成：%', v_missing;
    END IF;

    -- 8.2 取号规则
    SELECT count(*) INTO v_rows FROM biz_seq
     WHERE seq_type IN ('PRT', 'SR') AND deleted_at IS NULL;
    IF v_rows <> 2 THEN
        RAISE EXCEPTION 'P2-03: biz_seq(PRT/SR) 行数 = %（期望 2）', v_rows;
    END IF;

    -- 8.3 唯一索引在位
    SELECT count(*) INTO v_rows FROM pg_indexes
     WHERE schemaname = current_schema()
       AND indexname IN ('uk_biz_purchase_return_no', 'uk_biz_prti_line',
                         'uk_biz_supplier_receivable_no', 'uk_biz_sr_source',
                         'uk_biz_prt_source_stock_out');
    IF v_rows <> 5 THEN
        RAISE EXCEPTION 'P2-03: 唯一索引数 = %（期望 5）', v_rows;
    END IF;

    -- 8.4 价税恒等约束（精确）
    SELECT count(*) INTO v_rows FROM pg_constraint
     WHERE conrelid = 'public.biz_purchase_return'::regclass
       AND conname = 'ck_biz_prt_tax';
    IF v_rows <> 1 THEN
        RAISE EXCEPTION 'P2-03: biz_purchase_return 缺 ck_biz_prt_tax';
    END IF;

    -- 8.5 菜单：1 个页面 + 3 个按钮，auth_code 全部落在 business:erp:purchase-return:*
    SELECT count(*) INTO v_rows FROM sys_menu
     WHERE id IN ('20000000-0000-0000-0000-000000003341'::uuid,
                  '20000000-0000-0000-0000-000000003344'::uuid,
                  '20000000-0000-0000-0000-000000003345'::uuid,
                  '20000000-0000-0000-0000-000000003346'::uuid)
       AND deleted_at IS NULL
       AND auth_code LIKE 'business:erp:purchase-return:%';
    IF v_rows <> 4 THEN
        RAISE EXCEPTION 'P2-03: 菜单/权限码行数 = %（期望 4）', v_rows;
    END IF;

    -- 8.6 页面节点必须挂在「采购管理」(…946) 下
    SELECT parent_id INTO v_id FROM sys_menu
     WHERE id = '20000000-0000-0000-0000-000000003341'::uuid;
    IF v_id IS DISTINCT FROM '20000000-0000-0000-0000-000000000946'::uuid THEN
        RAISE EXCEPTION 'P2-03: 采购退货页面节点父菜单 = %（期望 采购管理 …946）', v_id;
    END IF;

    -- 8.7 流程（若种子未被守卫跳过）：4 节点 / 3 边 / 快照 hash 一致
    IF EXISTS (SELECT 1 FROM biz_flow_def_version
                WHERE id = '20000000-0000-4000-8000-0000000e0332') THEN
        SELECT count(*) INTO v_rows FROM biz_flow_node
         WHERE version_id = '20000000-0000-4000-8000-0000000e0332'::uuid;
        IF v_rows <> 4 THEN
            RAISE EXCEPTION 'P2-03: prt_approve 节点数 = %（期望 4）', v_rows;
        END IF;
        SELECT count(*) INTO v_rows FROM biz_flow_edge
         WHERE version_id = '20000000-0000-4000-8000-0000000e0332'::uuid;
        IF v_rows <> 3 THEN
            RAISE EXCEPTION 'P2-03: prt_approve 边数 = %（期望 3）', v_rows;
        END IF;
        SELECT count(*) INTO v_rows FROM biz_flow_def_version
         WHERE id = '20000000-0000-4000-8000-0000000e0332'::uuid
           AND node_count = 4 AND edge_count = 3
           AND graph_hash = '9debbf67fd8f3404a828bd626c614753b308b70e8b7de49c212b86869842507c';
        IF v_rows <> 1 THEN
            RAISE EXCEPTION 'P2-03: prt_approve 版本行（快照 hash / 计数）不对';
        END IF;
    ELSE
        RAISE NOTICE 'P2-03: prt_approve 种子被守卫跳过（已有同名流程 / 同 biz_type 默认流程 / 角色 UUID 不符）';
    END IF;

    RAISE NOTICE 'P2-03 自检通过：采购退货三表 + 取号 + 权限码 + 菜单 + prt_approve 流程';
END $$;

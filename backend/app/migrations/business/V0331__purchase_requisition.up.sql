-- ============================================================================
-- V0331：采购申请闭环（P2-02）—— 采购申请单 / 申请明细 + 取号 PRQ
--        + 开箱即用的审批流 pr_approve + 4 个权限码 + 采购管理下的页面与按钮
--
--   ① biz_purchase_requisition       采购申请单（申请部门 / 申请人 / 需求日期 / 状态 / 预算判定 / 转单结果）
--   ② biz_purchase_requisition_item  申请明细（物料 / 数量 / 预估单价 / 税率 / 建议供应商 / 转单后对应采购单行）
--   ③ biz_seq：PRQ（PRQ-yyyyMMdd-000001，按 CST 天滚动）
--   ④ 流程定义 pr_approve（biz_type = PR_APPROVE）：
--        · 预算内      ：采购经理审批
--        · 超预算      ：采购经理审批 → 财务经理审批（**特殊审批**）
--      两档写成流程定义里的**条件边**（`form.over_budget_amount > 0`），不是代码里的 if/else ——
--      阈值与审批岗位客户都能在「审批流程」里改，不需要改代码、不需要发版。
--   ⑤ 4 个权限码 + 1 个页面节点（menu_type=2）+ 4 个按钮（menu_type=3）
--   ⑥ 角色授权：PURCHASE_MANAGER 全给 / DEPT_LEADER 提需求（list+create+submit）/
--      FINANCE_MANAGER 只读（超预算要它批，得能看单）
--
-- ── 号段核对（动手前 `ls backend/migrations/business | tail`）─────────────
--   写入本文件时 business 目录最大号 = V0330__inventory_biz_type_and_sourceless_in_type。
--   sys_menu 已用段：…0187 / …2701 / …3081–3088 / …3161–3173 / …3175–3188 / …3303–3313 / …3321–3326。
--   本卡取 **…3331–3335**（页面 + 3 个按钮 + 取号行，与既有段无交叠）；
--   流程定义取 `…0d0331 / …0d0332`（版本）、节点 `…0d0351–…0d0355`、边 `…0d0361–…0d0365`。
--
-- ── 表口径（与卡面的差异，逐条说明）──────────────────────────────────────
--   1) 卡面只写「申请部门 / 申请人 / 需求日期 / 状态」，但「转采购订单」必须知道
--      **收货仓库**（`biz_purchase_order.warehouse_id` NOT NULL）⇒ 申请单带 `warehouse_id`，
--      转单时由它派生（可在 to-po 请求里显式覆盖）。
--   2) 明细带 `est_price`（预估不含税单价）与 `tax_rate`：转单时采购单 `price_excl`
--      直接取它（卡面第 ① 条的「预估单价」）。**金额由服务端算**（qty × est_price），
--      客户端传 `est_amount` 会被忽略。
--   3) 金额列 `est_amount` 是**申请单级预估合计**（不含税），只用于「预算判定」与列表展示，
--      不是账：申请单不入账、不生成凭证、不占预算。
--   4) 合并采购的落点：`biz_purchase_requisition.po_id/po_no` 指向**同一张**采购订单
--      （多张申请单 → 一张 PO，卡面验收第 4/5 条）；明细的 `po_item_id` 记录
--      「我这行并进了采购单的哪一行」——同物料多张申请单会指向**同一个** po_item_id，
--      这里**刻意不加唯一约束**（那正是合并的语义）。
--   5) 状态域 4 值：DRAFT（建单）→ SUBMITTED（提交审批）→ APPROVED（审批通过，可转单）
--      → CONVERTED（已转采购订单）；驳回 / 撤回由审批回调打回 DRAFT。卡面没有「作废」要求，
--      不造没有入口的死状态。
--   6) 预算联动**只判不占**：申请只是需求，真正占额度的是采购订单提交
--      （`budget_guard::occupy_for_purchase`）。申请阶段若也占，同一笔需求会被算两遍。
--      判定结果落 `budget_checked / over_budget / over_amount / budget_reason` 四列做快照。
--
-- ── 幂等与回滚 ───────────────────────────────────────────────────────────
--   CREATE TABLE / INDEX IF NOT EXISTS；biz_seq 与 sys_menu 用 WHERE NOT EXISTS /
--   ON CONFLICT DO UPDATE；授权 ON CONFLICT DO NOTHING；流程四层 WHERE NOT EXISTS /
--   角色 UUID 校验（客户用别的 UUID 预建过角色 → 整条跳过，不种出悬空 assignee）。
--   末尾 DO 块自检（表 / 唯一索引 / CHECK 真能拒坏数据 / 取号规则 / 权限码承载行 /
--   菜单 path 与父级 / 授权行 / 流程定义与版本 hash / 条件边 / 审批人绑定 / END 汇聚），
--   任一不符 → 迁移失败，不留半套结构。
--   回滚 V0331__purchase_requisition.down.sql：先删授权、再删流程、再删按钮与页面、最后删表。
-- ============================================================================

-- ---------- ① biz_purchase_requisition（采购申请单） ----------
CREATE TABLE IF NOT EXISTS biz_purchase_requisition (
    id                   UUID PRIMARY KEY DEFAULT gen_random_uuid(),      -- 主键
    req_no               VARCHAR(32)   NOT NULL,                          -- 申请单号：PRQ-yyyyMMdd-000001（biz_seq 取号，存活行唯一）
    req_date             DATE          NOT NULL DEFAULT CURRENT_DATE,     -- 申请日期
    dept_id              UUID          NOT NULL,                          -- 申请部门（弱引用 sys_dept.id）
    dept_name            VARCHAR(128),                                    -- 申请部门名快照（部门改名后历史不失真）
    applicant_id         UUID,                                            -- 申请人（弱引用 sys_user.id）
    applicant_name       VARCHAR(64),                                     -- 申请人姓名快照
    demand_date          DATE,                                            -- 需求日期（整单口径；转单时作采购单期望到货日）
    warehouse_id         UUID          NOT NULL,                          -- 收货仓库（弱引用 biz_warehouse.id；转采购订单时派生）
    status               VARCHAR(16)   NOT NULL DEFAULT 'DRAFT',          -- DRAFT / SUBMITTED / APPROVED / CONVERTED
    est_amount           NUMERIC(18,4) NOT NULL DEFAULT 0,                -- 预估金额合计（不含税，服务端算）
    budget_checked       BOOLEAN       NOT NULL DEFAULT FALSE,            -- 提交时是否真的做了预算判定（OFF / 未映射科目 = false）
    over_budget          BOOLEAN       NOT NULL DEFAULT FALSE,            -- 是否超预算（= 需要特殊审批）
    over_amount          NUMERIC(18,4) NOT NULL DEFAULT 0,                -- 超出金额
    budget_reason        TEXT,                                            -- 预算判定说明（人的话，直接给前端展示）
    budget_account_id    UUID,                                            -- 命中的预算科目（弱引用 biz_budget_account.id）
    budget_dept_id       UUID,                                            -- 预算槽部门（= 申请部门快照）
    budget_period_code   VARCHAR(7),                                      -- 预算槽期间（yyyy-MM）
    approval_instance_id UUID,                                            -- 提交审批的实例 id（SUBMITTED/APPROVED 有值）
    converted_at         TIMESTAMPTZ,                                     -- 转采购订单时间
    po_id                UUID,                                            -- 合并转单后的采购订单（多张申请单指向同一 PO）
    po_no                VARCHAR(32),                                     -- 采购订单号快照（列表直接展示，不用 join）
    remark               TEXT,                                            -- 备注
    created_at           TIMESTAMPTZ   NOT NULL DEFAULT CURRENT_TIMESTAMP,
    updated_at           TIMESTAMPTZ   NOT NULL DEFAULT CURRENT_TIMESTAMP,
    create_by            UUID,
    update_by            UUID,
    deleted_at           TIMESTAMPTZ,                                     -- 软删时间（NULL = 存活）

    CONSTRAINT ck_biz_prq_status CHECK (status IN ('DRAFT','SUBMITTED','APPROVED','CONVERTED')),
    CONSTRAINT ck_biz_prq_est_amount CHECK (est_amount >= 0),
    CONSTRAINT ck_biz_prq_over_amount CHECK (over_amount >= 0),
    -- 「已转单」三件套必须同生同死：状态 CONVERTED ⇔ 有 PO 号 + 转单时间
    CONSTRAINT ck_biz_prq_converted
        CHECK ((status = 'CONVERTED') = (po_id IS NOT NULL AND po_no IS NOT NULL
                                         AND converted_at IS NOT NULL)),
    -- 审批中必须有审批实例（否则「谁批的」查不出来）。
    -- 已通过（APPROVED）**不**强制：显式跳过审批（没配流程 + 持 skip-approval 码 + 理由）
    -- 时没有实例，留痕在 remark 里，与采购 / 收货的「显式跳过」口径一致。
    CONSTRAINT ck_biz_prq_instance
        CHECK (status <> 'SUBMITTED' OR approval_instance_id IS NOT NULL),
    -- 超预算必然有超出额；反之超出额 > 0 必然标了超预算
    CONSTRAINT ck_biz_prq_over_consistency
        CHECK (over_budget = (over_amount > 0))
);

COMMENT ON TABLE  biz_purchase_requisition IS '采购申请单（P2-02 需求申请 → 审批 → 转采购订单；多张申请单可合并成一张 PO）';
COMMENT ON COLUMN biz_purchase_requisition.req_no IS '申请单号：PRQ-yyyyMMdd-000001（biz_seq.seq_type=PRQ，按北京时间天滚动）';
COMMENT ON COLUMN biz_purchase_requisition.status IS 'DRAFT 建单 / SUBMITTED 审批中 / APPROVED 审批通过（可转单）/ CONVERTED 已转采购订单';
COMMENT ON COLUMN biz_purchase_requisition.est_amount IS '预估金额合计（不含税）= Σ(qty × est_price)，只用于预算判定与展示，不是账';
COMMENT ON COLUMN biz_purchase_requisition.over_budget IS '提交时点的预算判定快照：TRUE = 超预算，审批流会额外加一道财务经理特殊审批';
COMMENT ON COLUMN biz_purchase_requisition.po_id IS '合并转单后的采购订单 id（多张申请单可指向同一张 PO；一个 PO 只有一个 id 所以不建唯一索引）';

CREATE UNIQUE INDEX IF NOT EXISTS uk_biz_purchase_requisition_no
    ON biz_purchase_requisition (req_no) WHERE deleted_at IS NULL;
CREATE INDEX IF NOT EXISTS idx_biz_purchase_requisition_status
    ON biz_purchase_requisition (status, req_date DESC);
CREATE INDEX IF NOT EXISTS idx_biz_purchase_requisition_dept
    ON biz_purchase_requisition (dept_id, req_date DESC);
CREATE INDEX IF NOT EXISTS idx_biz_purchase_requisition_po
    ON biz_purchase_requisition (po_id) WHERE po_id IS NOT NULL;
CREATE INDEX IF NOT EXISTS idx_biz_purchase_requisition_approval
    ON biz_purchase_requisition (approval_instance_id) WHERE approval_instance_id IS NOT NULL;

-- ---------- ② biz_purchase_requisition_item（申请明细） ----------
-- 明细随主单整体重建（与询价单 / 采购订单同口径：先删后插，事务内）。
CREATE TABLE IF NOT EXISTS biz_purchase_requisition_item (
    id                  UUID PRIMARY KEY DEFAULT gen_random_uuid(),       -- 主键
    requisition_id      UUID          NOT NULL,                           -- 所属申请单
    line_no             INT           NOT NULL,                           -- 行号（1 起，单内唯一）
    material_id         UUID          NOT NULL,                           -- 物料（弱引用 biz_material.id）
    unit_id             UUID          NOT NULL,                           -- 计量单位（建单时由物料带出，客户端不许传）
    qty                 NUMERIC(18,4) NOT NULL,                           -- 申请数量（> 0）
    est_price           NUMERIC(18,6) NOT NULL DEFAULT 0,                 -- 预估不含税单价（转单时作采购单 price_excl）
    tax_rate            NUMERIC(9,4)  NOT NULL DEFAULT 0,                 -- 税率（百分数 0–100）
    demand_date         DATE,                                             -- 行需求日期（缺省取单头）
    suggest_supplier_id UUID,                                             -- 建议供应商（弱引用 biz_supplier.id）
    po_item_id          UUID,                                             -- 转单后并进的采购订单明细行（同物料多张申请单指向同一行，刻意非唯一）
    remark              TEXT,                                             -- 行备注
    created_at          TIMESTAMPTZ   NOT NULL DEFAULT CURRENT_TIMESTAMP,

    CONSTRAINT ck_biz_prqi_qty   CHECK (qty > 0),
    CONSTRAINT ck_biz_prqi_line  CHECK (line_no > 0),
    CONSTRAINT ck_biz_prqi_price CHECK (est_price >= 0),
    CONSTRAINT ck_biz_prqi_tax   CHECK (tax_rate >= 0 AND tax_rate <= 100),
    CONSTRAINT fk_biz_prqi_req FOREIGN KEY (requisition_id)
        REFERENCES biz_purchase_requisition (id) ON DELETE CASCADE
);

COMMENT ON TABLE  biz_purchase_requisition_item IS '采购申请明细（一行 = 一个物料 + 数量 + 预估单价；随主单整体重建）';
COMMENT ON COLUMN biz_purchase_requisition_item.unit_id IS '由物料基础单位带出（DC-09 止血口径，与采购订单 / 询价单明细同源）';
COMMENT ON COLUMN biz_purchase_requisition_item.po_item_id IS '合并转单后并进的采购订单明细行；同物料的多张申请单会指向同一个 po_item_id（合并语义，故意不加唯一约束）';

CREATE UNIQUE INDEX IF NOT EXISTS uk_biz_prqi_line
    ON biz_purchase_requisition_item (requisition_id, line_no);
CREATE INDEX IF NOT EXISTS idx_biz_prqi_material
    ON biz_purchase_requisition_item (material_id);
CREATE INDEX IF NOT EXISTS idx_biz_prqi_po_item
    ON biz_purchase_requisition_item (po_item_id) WHERE po_item_id IS NOT NULL;

-- ---------- ③ 取号规则 PRQ ----------
INSERT INTO biz_seq (id, seq_type, seq_name, prefix, date_pattern, period_key, current_no, step, separator, status, remark)
SELECT '20000000-0000-0000-0000-000000003335'::uuid, 'PRQ', '采购申请单号', 'PRQ', 'yyyyMMdd', '', 0, 1, '-', 1,
       '采购申请单号：PRQ-yyyyMMdd-000001，按天（CST）滚动'
 WHERE NOT EXISTS (SELECT 1 FROM biz_seq WHERE seq_type = 'PRQ' AND deleted_at IS NULL);

-- ---------- ④ 菜单：页面节点 + 4 个按钮 ----------
INSERT INTO sys_menu (id, name, menu_type, auth_code, path, parent_id, component,
                      sort_order, status, icon, title, is_visible, module_code)
VALUES
    ('20000000-0000-0000-0000-000000003331'::uuid, '采购申请', 2,
     'business:erp:purchase-requisition:list', '/business/erp/purchase-requisition',
     '20000000-0000-0000-0000-000000000946'::uuid, 'business/erp/purchase-requisition/index',
     36, 1, 'file-add', '部门提需求 → 审批 → 转采购订单（支持多单合并采购）',
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
    ('20000000-0000-0000-0000-000000003332'::uuid, '新建采购申请', 3,
     'business:erp:purchase-requisition:create', NULL, '20000000-0000-0000-0000-000000003331'::uuid,
     NULL, 10, 1, NULL, '部门提需求：填物料 / 数量 / 预估单价 / 建议供应商（单位由物料带出）', TRUE, 'business'),
    ('20000000-0000-0000-0000-000000003333'::uuid, '提交审批', 3,
     'business:erp:purchase-requisition:submit', NULL, '20000000-0000-0000-0000-000000003331'::uuid,
     NULL, 20, 1, NULL, '提交到审批流（超预算自动加一道财务经理特殊审批）', TRUE, 'business'),
    ('20000000-0000-0000-0000-000000003334'::uuid, '转采购订单', 3,
     'business:erp:purchase-requisition:to-po', NULL, '20000000-0000-0000-0000-000000003331'::uuid,
     NULL, 30, 1, NULL, '把多张已审批申请单的同物料合并成一张采购订单（数量相加）', TRUE, 'business')
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

-- ---------- ⑤ 角色授权（本迁移替人做的决定，逐条说明） ----------
--   · PURCHASE_MANAGER（采购经理）：4 个码全给 —— 提单、提交、转 PO 是采购的日常动作；
--   · DEPT_LEADER（部门主管）：list + create + submit —— 「部门提需求」就是它的活；
--   · FINANCE_MANAGER（财务经理）：只给 list —— 超预算特殊审批要看单，
--     但提单 / 转单不是财务的动作（审批权限由流程引擎按角色展开，不靠这里的菜单码）。
--   · 其余角色不给：SUPER_ADMIN 由框架短路，不需要显式授权。
--   角色不存在时自然插入 0 行（空库首启：角色由历史迁移种，本迁移先跑也能过）。
INSERT INTO sys_role_menu (role_id, menu_id)
SELECT r.id, m.menu_id
  FROM (VALUES
        ('PURCHASE_MANAGER', '20000000-0000-0000-0000-000000003331'::uuid),
        ('PURCHASE_MANAGER', '20000000-0000-0000-0000-000000003332'::uuid),
        ('PURCHASE_MANAGER', '20000000-0000-0000-0000-000000003333'::uuid),
        ('PURCHASE_MANAGER', '20000000-0000-0000-0000-000000003334'::uuid),
        ('DEPT_LEADER',      '20000000-0000-0000-0000-000000003331'::uuid),
        ('DEPT_LEADER',      '20000000-0000-0000-0000-000000003332'::uuid),
        ('DEPT_LEADER',      '20000000-0000-0000-0000-000000003333'::uuid),
        ('FINANCE_MANAGER',  '20000000-0000-0000-0000-000000003331'::uuid)
       ) AS m(role_code, menu_id)
  JOIN sys_role r ON r.role_code = m.role_code AND r.deleted_at IS NULL
ON CONFLICT (role_id, menu_id) DO NOTHING;

-- ---------- ⑥ 流程定义：采购申请审批 pr_approve ----------
-- 四层守卫：已有 flow_code='pr_approve'、已有同 biz_type 的默认流程、
-- 或审批角色不是我们种的那一套（客户用别的 UUID 预建过）→ 整条跳过，绝不覆盖客户配置。
INSERT INTO biz_flow_def (
    id, flow_code, flow_name, biz_type, category, description, status,
    current_version_id, current_version_no, is_default, allow_auto_approve, owner_dept_id)
SELECT
    '20000000-0000-4000-8000-0000000d0331', 'pr_approve', '采购申请审批（标准·超预算加签）',
    'PR_APPROVE', '采购管理',
    '采购申请提交审批：预算内只需采购经理审批；超预算（form.over_budget_amount > 0）'
    '再加一道财务经理特殊审批。审批通过才变 APPROVED（此后才能转采购订单），'
    '驳回或撤回则打回草稿并把意见追加到备注。阈值与审批人都在本流程定义里改，不需要改代码。',
    'PUBLISHED', '20000000-0000-4000-8000-0000000d0332', 1, TRUE, FALSE, NULL
WHERE NOT EXISTS (
    SELECT 1 FROM biz_flow_def WHERE flow_code = 'pr_approve' AND deleted_at IS NULL
)
AND NOT EXISTS (
    SELECT 1 FROM biz_flow_def
     WHERE biz_type = 'PR_APPROVE' AND is_default AND deleted_at IS NULL
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
    '20000000-0000-4000-8000-0000000d0332', '20000000-0000-4000-8000-0000000d0331', 1,
    'PUBLISHED', TRUE,
    'c0abd5f5c2c6ad8ece740cbd5b77d82dac0550097aa746ab0bb2b233d8c5f437',
    $snapshot${"edges":[{"condition":null,"from":"c_budget","is_default":true,"key":"e_within","label":"预算内：采购经理终审","priority":10,"to":"e"},{"condition":{"cmp":"gt","field":"form.over_budget_amount","op":"cmp","value":0},"from":"c_budget","is_default":false,"key":"e_over","label":"超预算：需财务经理特殊审批","priority":0,"to":"n_fin"},{"condition":null,"from":"n_fin","is_default":false,"key":"e_fin_end","label":null,"priority":0,"to":"e"},{"condition":null,"from":"n_pm","is_default":false,"key":"e_pm_check","label":null,"priority":0,"to":"c_budget"},{"condition":null,"from":"s","is_default":false,"key":"e_start","label":null,"priority":0,"to":"n_pm"}],"nodes":[{"allow_robot":false,"approval_mode":null,"approve_ratio":null,"assignee_type":null,"assignees":[],"cc_on_approved":false,"config":{},"empty_assignee":"SKIP","fork_mode":"AND","form_var_key":null,"join_mode":"OR","key":"c_budget","max_remind_times":3,"name":"预算判定","node_type":"CONDITION","reject_veto":true,"remind_before_hours":null,"self_approval":"SKIP","sort_order":3,"superior_levels":1,"timeout_action":"REMIND","timeout_hours":null,"x":420,"y":0},{"allow_robot":false,"approval_mode":null,"approve_ratio":null,"assignee_type":null,"assignees":[],"cc_on_approved":false,"config":{},"empty_assignee":"SKIP","fork_mode":"AND","form_var_key":null,"join_mode":"OR","key":"e","max_remind_times":3,"name":"结束","node_type":"END","reject_veto":true,"remind_before_hours":null,"self_approval":"SKIP","sort_order":5,"superior_levels":1,"timeout_action":"REMIND","timeout_hours":null,"x":880,"y":0},{"allow_robot":false,"approval_mode":null,"approve_ratio":null,"assignee_type":"ROLE","assignees":[{"assignee_type":"ROLE","ref_id":"20000000-0000-4000-8000-0000000f0005","sort_order":0}],"cc_on_approved":false,"config":{},"empty_assignee":"FALLBACK_ADMIN","fork_mode":"AND","form_var_key":null,"join_mode":"AND","key":"n_fin","max_remind_times":2,"name":"财务经理审批（超预算特殊审批）","node_type":"APPROVAL","reject_veto":true,"remind_before_hours":null,"self_approval":"ALLOW","sort_order":4,"superior_levels":1,"timeout_action":"REMIND","timeout_hours":24,"x":640,"y":0},{"allow_robot":false,"approval_mode":null,"approve_ratio":null,"assignee_type":"ROLE","assignees":[{"assignee_type":"ROLE","ref_id":"20000000-0000-4000-8000-0000000f0002","sort_order":0}],"cc_on_approved":false,"config":{},"empty_assignee":"FALLBACK_ADMIN","fork_mode":"AND","form_var_key":null,"join_mode":"AND","key":"n_pm","max_remind_times":2,"name":"采购经理审批","node_type":"APPROVAL","reject_veto":true,"remind_before_hours":null,"self_approval":"ALLOW","sort_order":2,"superior_levels":1,"timeout_action":"REMIND","timeout_hours":24,"x":220,"y":0},{"allow_robot":false,"approval_mode":null,"approve_ratio":null,"assignee_type":null,"assignees":[],"cc_on_approved":false,"config":{},"empty_assignee":"SKIP","fork_mode":"AND","form_var_key":null,"join_mode":"AND","key":"s","max_remind_times":3,"name":"开始","node_type":"START","reject_veto":true,"remind_before_hours":null,"self_approval":"SKIP","sort_order":1,"superior_levels":1,"timeout_action":"REMIND","timeout_hours":null,"x":0,"y":0}],"schema_version":1}$snapshot$,
    5, 5, 'V0331 种子：采购申请审批（预算内采购经理 / 超预算加财务经理）默认流程', now(), NULL
WHERE EXISTS (SELECT 1 FROM biz_flow_def WHERE id = '20000000-0000-4000-8000-0000000d0331')
ON CONFLICT (def_id, version_no) DO NOTHING;

INSERT INTO biz_flow_node (
    id, version_id, node_key, node_name, node_type, assignee_type, superior_levels,
    form_var_key, approval_mode, approve_ratio, reject_veto, empty_assignee, self_approval,
    allow_robot, join_mode, fork_mode, timeout_hours, timeout_action, remind_before_hours,
    max_remind_times, cc_on_approved, position_x, position_y, sort_order, config)
SELECT * FROM (VALUES
    ('20000000-0000-4000-8000-0000000d0351'::uuid, '20000000-0000-4000-8000-0000000d0332'::uuid, 's'::varchar, '开始'::varchar, 'START'::varchar, NULL::varchar, 1::int, NULL::varchar, NULL::varchar, NULL::numeric, TRUE::bool, 'SKIP'::varchar, 'SKIP'::varchar, FALSE::bool, 'AND'::varchar, 'AND'::varchar, NULL::int, 'REMIND'::varchar, NULL::int, 3::smallint, FALSE::bool, 0::int, 0::int, 1::int, '{}'::jsonb),
    ('20000000-0000-4000-8000-0000000d0352'::uuid, '20000000-0000-4000-8000-0000000d0332'::uuid, 'n_pm'::varchar, '采购经理审批'::varchar, 'APPROVAL'::varchar, 'ROLE'::varchar, 1::int, NULL::varchar, NULL::varchar, NULL::numeric, TRUE::bool, 'FALLBACK_ADMIN'::varchar, 'ALLOW'::varchar, FALSE::bool, 'AND'::varchar, 'AND'::varchar, 24::int, 'REMIND'::varchar, NULL::int, 2::smallint, FALSE::bool, 220::int, 0::int, 2::int, '{}'::jsonb),
    ('20000000-0000-4000-8000-0000000d0353'::uuid, '20000000-0000-4000-8000-0000000d0332'::uuid, 'c_budget'::varchar, '预算判定'::varchar, 'CONDITION'::varchar, NULL::varchar, 1::int, NULL::varchar, NULL::varchar, NULL::numeric, TRUE::bool, 'SKIP'::varchar, 'SKIP'::varchar, FALSE::bool, 'OR'::varchar, 'AND'::varchar, NULL::int, 'REMIND'::varchar, NULL::int, 3::smallint, FALSE::bool, 420::int, 0::int, 3::int, '{}'::jsonb),
    ('20000000-0000-4000-8000-0000000d0354'::uuid, '20000000-0000-4000-8000-0000000d0332'::uuid, 'n_fin'::varchar, '财务经理审批（超预算特殊审批）'::varchar, 'APPROVAL'::varchar, 'ROLE'::varchar, 1::int, NULL::varchar, NULL::varchar, NULL::numeric, TRUE::bool, 'FALLBACK_ADMIN'::varchar, 'ALLOW'::varchar, FALSE::bool, 'AND'::varchar, 'AND'::varchar, 24::int, 'REMIND'::varchar, NULL::int, 2::smallint, FALSE::bool, 640::int, 0::int, 4::int, '{}'::jsonb),
    ('20000000-0000-4000-8000-0000000d0355'::uuid, '20000000-0000-4000-8000-0000000d0332'::uuid, 'e'::varchar, '结束'::varchar, 'END'::varchar, NULL::varchar, 1::int, NULL::varchar, NULL::varchar, NULL::numeric, TRUE::bool, 'SKIP'::varchar, 'SKIP'::varchar, FALSE::bool, 'OR'::varchar, 'AND'::varchar, NULL::int, 'REMIND'::varchar, NULL::int, 3::smallint, FALSE::bool, 880::int, 0::int, 5::int, '{}'::jsonb)
) AS v
WHERE EXISTS (SELECT 1 FROM biz_flow_def_version WHERE id = '20000000-0000-4000-8000-0000000d0332')
ON CONFLICT (version_id, node_key) DO NOTHING;

INSERT INTO biz_flow_edge (
    id, version_id, edge_key, from_node_id, to_node_id, condition_expr,
    condition_label, priority, is_default)
SELECT * FROM (VALUES
    ('20000000-0000-4000-8000-0000000d0361'::uuid, '20000000-0000-4000-8000-0000000d0332'::uuid, 'e_start'::varchar, '20000000-0000-4000-8000-0000000d0351'::uuid, '20000000-0000-4000-8000-0000000d0352'::uuid, NULL::jsonb, NULL::varchar, 0::int, FALSE::bool),
    ('20000000-0000-4000-8000-0000000d0362'::uuid, '20000000-0000-4000-8000-0000000d0332'::uuid, 'e_pm_check'::varchar, '20000000-0000-4000-8000-0000000d0352'::uuid, '20000000-0000-4000-8000-0000000d0353'::uuid, NULL::jsonb, NULL::varchar, 0::int, FALSE::bool),
    ('20000000-0000-4000-8000-0000000d0363'::uuid, '20000000-0000-4000-8000-0000000d0332'::uuid, 'e_over'::varchar, '20000000-0000-4000-8000-0000000d0353'::uuid, '20000000-0000-4000-8000-0000000d0354'::uuid, '{"op": "cmp", "field": "form.over_budget_amount", "cmp": "gt", "value": 0}'::jsonb, '超预算：需财务经理特殊审批'::varchar, 0::int, FALSE::bool),
    ('20000000-0000-4000-8000-0000000d0364'::uuid, '20000000-0000-4000-8000-0000000d0332'::uuid, 'e_within'::varchar, '20000000-0000-4000-8000-0000000d0353'::uuid, '20000000-0000-4000-8000-0000000d0355'::uuid, NULL::jsonb, '预算内：采购经理终审'::varchar, 10::int, TRUE::bool),
    ('20000000-0000-4000-8000-0000000d0365'::uuid, '20000000-0000-4000-8000-0000000d0332'::uuid, 'e_fin_end'::varchar, '20000000-0000-4000-8000-0000000d0354'::uuid, '20000000-0000-4000-8000-0000000d0355'::uuid, NULL::jsonb, NULL::varchar, 0::int, FALSE::bool)
) AS v
WHERE EXISTS (SELECT 1 FROM biz_flow_def_version WHERE id = '20000000-0000-4000-8000-0000000d0332')
ON CONFLICT (version_id, edge_key) DO NOTHING;

INSERT INTO biz_flow_node_assignee (node_id, assignee_type, ref_id, sort_order)
SELECT * FROM (VALUES
    ('20000000-0000-4000-8000-0000000d0352'::uuid, 'ROLE'::varchar, '20000000-0000-4000-8000-0000000f0002'::uuid, 0::int),
    ('20000000-0000-4000-8000-0000000d0354'::uuid, 'ROLE'::varchar, '20000000-0000-4000-8000-0000000f0005'::uuid, 0::int)
) AS v
WHERE EXISTS (SELECT 1 FROM biz_flow_def_version WHERE id = '20000000-0000-4000-8000-0000000d0332')
ON CONFLICT (node_id, assignee_type, ref_id) DO NOTHING;

-- ---------- ⑦ 自检 ----------
DO $$
DECLARE
    v_missing text;
    v_rows    integer;
    v_path    text;
    v_parent  uuid;
    v_component text;
    v_id      uuid;
BEGIN
    -- ① 两张表 + 取号规则
    SELECT string_agg(t, ', ') INTO v_missing
      FROM (VALUES ('biz_purchase_requisition'), ('biz_purchase_requisition_item')) AS x(t)
     WHERE NOT EXISTS (SELECT 1 FROM information_schema.tables
                        WHERE table_schema = current_schema() AND table_name = x.t);
    IF v_missing IS NOT NULL THEN
        RAISE EXCEPTION 'P2-02: 以下表未建成：%', v_missing;
    END IF;

    SELECT count(*) INTO v_rows FROM biz_seq WHERE seq_type = 'PRQ' AND deleted_at IS NULL;
    IF v_rows <> 1 THEN
        RAISE EXCEPTION 'P2-02: biz_seq(seq_type=PRQ) 行数 = %（期望 1）', v_rows;
    END IF;

    -- ② 唯一索引必须在位
    SELECT count(*) INTO v_rows FROM pg_indexes
     WHERE schemaname = current_schema()
       AND indexname IN ('uk_biz_purchase_requisition_no', 'uk_biz_prqi_line');
    IF v_rows <> 2 THEN
        RAISE EXCEPTION 'P2-02: 唯一索引数 = %（期望 2）', v_rows;
    END IF;

    -- ③ CHECK 必须真的能拒坏数据（不是写了一行注释）
    v_id := gen_random_uuid();
    BEGIN
        INSERT INTO biz_purchase_requisition (req_no, dept_id, warehouse_id, status)
        VALUES ('SELFTEST-BAD-STATUS', v_id, v_id, 'WHATEVER');
        RAISE EXCEPTION 'P2-02: 非法状态竟然被接受';
    EXCEPTION WHEN check_violation THEN
        NULL;  -- 期望：被 ck_biz_prq_status 拒
    END;
    BEGIN
        -- 「已转单」但没有 PO 号 ⇒ 必须被 ck_biz_prq_converted 拒
        INSERT INTO biz_purchase_requisition (req_no, dept_id, warehouse_id, status, approval_instance_id)
        VALUES ('SELFTEST-BAD-CONVERTED', v_id, v_id, 'CONVERTED', v_id);
        RAISE EXCEPTION 'P2-02: 已转单却没有 PO 号竟然被接受';
    EXCEPTION WHEN check_violation THEN
        NULL;
    END;
    BEGIN
        -- 审批中却没有审批实例 ⇒ 必须被 ck_biz_prq_instance 拒
        INSERT INTO biz_purchase_requisition (req_no, dept_id, warehouse_id, status)
        VALUES ('SELFTEST-BAD-INSTANCE', v_id, v_id, 'SUBMITTED');
        RAISE EXCEPTION 'P2-02: 审批中却没有审批实例竟然被接受';
    EXCEPTION WHEN check_violation THEN
        NULL;
    END;

    -- ④ 权限码承载行（每个码必须有且仅有 1 行）
    SELECT string_agg(code, ', ') INTO v_missing
      FROM (VALUES
            ('business:erp:purchase-requisition:list'),
            ('business:erp:purchase-requisition:create'),
            ('business:erp:purchase-requisition:submit'),
            ('business:erp:purchase-requisition:to-po')
           ) AS expected(code)
     WHERE NOT EXISTS (SELECT 1 FROM sys_menu m
                        WHERE m.auth_code = expected.code AND m.deleted_at IS NULL);
    IF v_missing IS NOT NULL THEN
        RAISE EXCEPTION 'P2-02: 以下权限码没有承载行：%', v_missing;
    END IF;

    -- ⑤ 页面节点字段（path 必须与前端路由逐字一致，否则侧边栏点进去 404）
    SELECT path, component, parent_id INTO v_path, v_component, v_parent
      FROM sys_menu WHERE id = '20000000-0000-0000-0000-000000003331'::uuid AND deleted_at IS NULL;
    IF v_path IS DISTINCT FROM '/business/erp/purchase-requisition'
       OR v_component IS DISTINCT FROM 'business/erp/purchase-requisition/index'
       OR v_parent IS DISTINCT FROM '20000000-0000-0000-0000-000000000946'::uuid THEN
        RAISE EXCEPTION 'P2-02: 采购申请页面节点字段不对（path=%, component=%, parent=%）',
            v_path, v_component, v_parent;
    END IF;

    -- ⑥ 3 个按钮都挂在页面节点下
    SELECT count(*) INTO v_rows FROM sys_menu
     WHERE parent_id = '20000000-0000-0000-0000-000000003331'::uuid AND menu_type = 3
       AND deleted_at IS NULL;
    IF v_rows <> 3 THEN
        RAISE EXCEPTION 'P2-02: 采购申请按钮数 = %（期望 3）', v_rows;
    END IF;

    -- ⑦ 流程定义与版本（客户已有同名流程 / 角色 UUID 不符 → 整条跳过，这里只验跳过后的形态）
    IF NOT EXISTS (SELECT 1 FROM biz_flow_def WHERE id = '20000000-0000-4000-8000-0000000d0331') THEN
        RAISE NOTICE 'P2-02：pr_approve 种子跳过（已有同名流程 / 同 biz_type 默认流程 / 角色 UUID 不符）';
    ELSE
        SELECT count(*) INTO v_rows FROM biz_flow_def d
         WHERE d.id = '20000000-0000-4000-8000-0000000d0331' AND d.flow_code = 'pr_approve'
           AND d.biz_type = 'PR_APPROVE' AND d.status = 'PUBLISHED' AND d.is_default
           AND d.current_version_id = '20000000-0000-4000-8000-0000000d0332'
           AND d.deleted_at IS NULL;
        IF v_rows <> 1 THEN
            RAISE EXCEPTION 'P2-02: pr_approve 定义行不满足 PUBLISHED/默认/当前版本';
        END IF;

        SELECT count(*) INTO v_rows FROM biz_flow_def_version v
         WHERE v.id = '20000000-0000-4000-8000-0000000d0332'
           AND v.def_id = '20000000-0000-4000-8000-0000000d0331'
           AND v.version_status = 'PUBLISHED' AND v.is_current
           AND v.graph_hash = 'c0abd5f5c2c6ad8ece740cbd5b77d82dac0550097aa746ab0bb2b233d8c5f437'
           AND v.graph_snapshot IS NOT NULL AND v.published_at IS NOT NULL
           AND v.node_count = 5 AND v.edge_count = 5;
        IF v_rows <> 1 THEN
            RAISE EXCEPTION 'P2-02: pr_approve 版本行（快照/hash/计数）不对';
        END IF;

        SELECT count(*) INTO v_rows FROM biz_flow_node WHERE version_id = '20000000-0000-4000-8000-0000000d0332';
        IF v_rows <> 5 THEN
            RAISE EXCEPTION 'P2-02: pr_approve 节点数 = %（期望 5）', v_rows;
        END IF;
        SELECT count(*) INTO v_rows FROM biz_flow_edge WHERE version_id = '20000000-0000-4000-8000-0000000d0332';
        IF v_rows <> 5 THEN
            RAISE EXCEPTION 'P2-02: pr_approve 边数 = %（期望 5）', v_rows;
        END IF;

        -- 超预算分支必须真的落在条件边上（不是代码里 if/else）
        SELECT count(*) INTO v_rows FROM biz_flow_edge
         WHERE version_id = '20000000-0000-4000-8000-0000000d0332'
           AND NOT is_default AND condition_expr IS NOT NULL
           AND condition_expr->>'field' = 'form.over_budget_amount'
           AND condition_expr->>'cmp' = 'gt';
        IF v_rows <> 1 THEN
            RAISE EXCEPTION 'P2-02: 超预算条件边应为 1 条，实际 %', v_rows;
        END IF;
        -- 另一条必须是兜底边（否则预算内无路可走 → 实例卡死）
        SELECT count(*) INTO v_rows FROM biz_flow_edge
         WHERE version_id = '20000000-0000-4000-8000-0000000d0332' AND is_default AND priority = 10;
        IF v_rows <> 1 THEN
            RAISE EXCEPTION 'P2-02: 兜底边（is_default + priority=10）应为 1 条，实际 %', v_rows;
        END IF;

        -- 两个审批节点都必须「有人可派 + 没人时回退超管」（不许静默自动通过）
        SELECT count(*) INTO v_rows FROM biz_flow_node
         WHERE version_id = '20000000-0000-4000-8000-0000000d0332'
           AND node_key IN ('n_pm', 'n_fin')
           AND node_type = 'APPROVAL' AND assignee_type = 'ROLE'
           AND empty_assignee = 'FALLBACK_ADMIN' AND self_approval = 'ALLOW';
        IF v_rows <> 2 THEN
            RAISE EXCEPTION 'P2-02: 两个审批节点的审批人来源/兜底策略不对（应为 2 行，实际 %）', v_rows;
        END IF;

        -- 审批人必须逐节点绑到对应角色（采购经理 / 财务经理）
        SELECT count(*) INTO v_rows FROM biz_flow_node_assignee a
          JOIN biz_flow_node n ON n.id = a.node_id
         WHERE n.version_id = '20000000-0000-4000-8000-0000000d0332'
           AND ((n.node_key = 'n_pm'  AND a.ref_id = '20000000-0000-4000-8000-0000000f0002'::uuid)
             OR (n.node_key = 'n_fin' AND a.ref_id = '20000000-0000-4000-8000-0000000f0005'::uuid));
        IF v_rows <> 2 THEN
            RAISE EXCEPTION 'P2-02: 审批人角色绑定与两档岗位不符（应 2 行，实际 %）', v_rows;
        END IF;

        -- END 有两条入边 ⇒ 必须 OR 汇聚（AND 会等人齐 → 审批单永久卡死）
        SELECT count(*) INTO v_rows FROM biz_flow_node
         WHERE version_id = '20000000-0000-4000-8000-0000000d0332' AND node_key = 'e' AND join_mode = 'OR';
        IF v_rows <> 1 THEN
            RAISE EXCEPTION 'P2-02: pr_approve 的 END 节点必须 OR 汇聚';
        END IF;
        -- 串行链：采购经理必须是第一个审批节点（START 的直接下游）
        SELECT count(*) INTO v_rows FROM biz_flow_edge e
          JOIN biz_flow_node f ON f.id = e.from_node_id
          JOIN biz_flow_node t ON t.id = e.to_node_id
         WHERE e.version_id = '20000000-0000-4000-8000-0000000d0332'
           AND f.node_key = 's' AND t.node_key = 'n_pm';
        IF v_rows <> 1 THEN
            RAISE EXCEPTION 'P2-02: START 必须直连采购经理审批节点';
        END IF;
    END IF;

    RAISE NOTICE 'P2-02: 采购申请两表 + 取号 + 4 个权限码 + 菜单 + pr_approve 流程自检通过';
END $$;

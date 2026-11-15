-- ======================================================
-- V0227：预算管理四表 + 8 个权限码 + 预算管理菜单（ARCH-6 §3.2 / §6.7）
--
-- 表（DDL 逐字取自 ARCH-6 §3.2.1–§3.2.5，未自创列名）：
--   · biz_budget              预算（一条记录 = 一个版本）
--   · biz_budget_period       月度分解（12 行）
--   · biz_budget_usage        占用 / 执行事实（**不含任何「已用金额」缓存列**）
--   · biz_budget_subject_map  业务口径 → 预算科目（采购单没有科目列，只能靠它）
--
-- ── 本章最要紧的三条设计约束（照着建表就能落地，不靠代码自觉）──────
-- ① **可用余额绝不落库**：`uk_biz_budget_slot_active` 只保证「一个槽同时一个 ACTIVE 版本」，
--    可用 = 最新 ACTIVE 额度 − Σ有效 usage（OCCUPIED + CONSUMED），现算（ARCH-6 红线 2）。
--    usage 表里只有「事实」（这次占了多少），没有「汇总」。
-- ② `uk_biz_budget_usage_source` = 同一张单对同一槽只有一条有效记录 → 双击提交 /
--    重试 / 幂等重放都被库挡住（ARCH-6 §3.6.1）。
-- ③ `ck_biz_budget_usage_year` 把 `fiscal_year = left(period_code,4)` 钉死 —— 冗余存年份是
--    为了让年度闸走索引（不做函数计算），冗余必须有约束兜住（ARCH-6 §3.2.4）。
--
-- ── 权限码（8 个，唯一来源就是 sys_menu.auth_code）────────────────
--   budget:list / create / update / delete / submit / adjust / report / mapping
-- 审批不需要新码：走流程引擎的 `business:flow:approve`（与 po_approve 同款）。
--
-- ── 菜单 ─────────────────────────────────────────────────
--   预算管理（目录，menu_type=1）→ 预算编制 / 预算执行 / 预算科目映射（menu_type=2）
--   + 编制页的 5 个按钮码（menu_type=3：新建 / 编辑 / 删除 / 提交 / 调整）
--
-- ── 幂等 ─────────────────────────────────────────────────
-- 建表 `IF NOT EXISTS`；索引 `IF NOT EXISTS`；菜单/权限 `ON CONFLICT (id) DO NOTHING`。
-- 末尾 `DO $$` 自检四张表、关键约束、8 个权限码与 4 个菜单，任一不符即迁移失败。
--
-- ── 回滚 ─────────────────────────────────────────────────
-- `V0227__erp_budget_tables.up.sql` 的 down：删菜单/权限 → 删四表（CASCADE 只影响自身索引）。
-- ======================================================

-- ---------- ① biz_budget（一条记录 = 一个版本） ----------
CREATE TABLE IF NOT EXISTS biz_budget (
    id                     UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    budget_no              VARCHAR(32)   NOT NULL,
    fiscal_year            SMALLINT      NOT NULL,
    dept_id                UUID          NOT NULL,
    dept_name              VARCHAR(120)  NOT NULL,
    account_id             UUID          NOT NULL,
    account_code           VARCHAR(30)   NOT NULL,
    account_name           VARCHAR(120)  NOT NULL,
    total_amount           NUMERIC(18,4) NOT NULL DEFAULT 0,
    control_mode           VARCHAR(8),
    version_no             INT           NOT NULL DEFAULT 1,
    supersedes_id          UUID,
    change_type            VARCHAR(16)   NOT NULL DEFAULT 'ORIGINAL',
    change_reason          VARCHAR(500),
    status                 VARCHAR(16)   NOT NULL DEFAULT 'DRAFT',
    approval_instance_id   UUID,
    effective_from         DATE,
    submitted_at           TIMESTAMPTZ,
    submitted_by           UUID,
    approved_at            TIMESTAMPTZ,
    approved_by            UUID,
    retired_at             TIMESTAMPTZ,
    remark                 TEXT,
    created_at             TIMESTAMPTZ   NOT NULL DEFAULT CURRENT_TIMESTAMP,
    updated_at             TIMESTAMPTZ   NOT NULL DEFAULT CURRENT_TIMESTAMP,
    create_by              UUID,
    update_by              UUID,
    deleted_at             TIMESTAMPTZ,

    CONSTRAINT ck_biz_budget_year     CHECK (fiscal_year BETWEEN 2000 AND 2100),
    CONSTRAINT ck_biz_budget_amount   CHECK (total_amount >= 0),
    CONSTRAINT ck_biz_budget_version  CHECK (version_no >= 1),
    CONSTRAINT ck_biz_budget_mode     CHECK (control_mode IS NULL
                                             OR control_mode IN ('OFF','WARN','BLOCK')),
    CONSTRAINT ck_biz_budget_change   CHECK (change_type IN ('ORIGINAL','ADJUST')),
    CONSTRAINT ck_biz_budget_reason   CHECK (change_type <> 'ADJUST' OR change_reason IS NOT NULL),
    CONSTRAINT ck_biz_budget_chain    CHECK ((version_no = 1 AND supersedes_id IS NULL)
                                          OR (version_no > 1 AND supersedes_id IS NOT NULL)),
    CONSTRAINT ck_biz_budget_self     CHECK (supersedes_id IS NULL OR supersedes_id <> id),
    CONSTRAINT ck_biz_budget_inst     CHECK (
        deleted_at IS NOT NULL
        OR (status IN ('SUBMITTED') AND approval_instance_id IS NOT NULL)
        OR (status NOT IN ('SUBMITTED') AND approval_instance_id IS NULL)
        OR (status IN ('ACTIVE','SUPERSEDED')))
);

COMMENT ON TABLE  biz_budget IS '预算（一条记录 = 一个版本；调整 = 新增版本行，不改旧行）';
COMMENT ON COLUMN biz_budget.total_amount IS '年度额度；月度分解在 biz_budget_period，Σ = 本列（应用层校验 + 巡检 J5）';
COMMENT ON COLUMN biz_budget.control_mode IS '控制策略：NULL 继承 sys_config erp.budget.control_mode；OFF/WARN/BLOCK';
COMMENT ON COLUMN biz_budget.account_code IS '科目编码快照：科目改名后历史预算的语义不漂移（与 biz_voucher_entry 同理由）';

CREATE UNIQUE INDEX IF NOT EXISTS uk_biz_budget_no
    ON biz_budget (budget_no) WHERE deleted_at IS NULL;
CREATE UNIQUE INDEX IF NOT EXISTS uk_biz_budget_slot_active
    ON biz_budget (dept_id, account_id, fiscal_year)
    WHERE status = 'ACTIVE' AND deleted_at IS NULL;
CREATE UNIQUE INDEX IF NOT EXISTS uk_biz_budget_slot_version
    ON biz_budget (dept_id, account_id, fiscal_year, version_no)
    WHERE deleted_at IS NULL;
CREATE INDEX IF NOT EXISTS idx_biz_budget_year_dept
    ON biz_budget (fiscal_year, dept_id, account_id);
CREATE INDEX IF NOT EXISTS idx_biz_budget_status
    ON biz_budget (status);
CREATE INDEX IF NOT EXISTS idx_biz_budget_account
    ON biz_budget (account_id, fiscal_year);
CREATE INDEX IF NOT EXISTS idx_biz_budget_approval
    ON biz_budget (approval_instance_id)
    WHERE approval_instance_id IS NOT NULL;

-- ---------- ② biz_budget_period（月度分解） ----------
CREATE TABLE IF NOT EXISTS biz_budget_period (
    id           UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    budget_id    UUID          NOT NULL REFERENCES biz_budget(id) ON DELETE CASCADE,
    period_code  VARCHAR(7)    NOT NULL,
    period_month SMALLINT      NOT NULL,
    amount       NUMERIC(18,4) NOT NULL DEFAULT 0,
    remark       VARCHAR(255),
    created_at   TIMESTAMPTZ   NOT NULL DEFAULT CURRENT_TIMESTAMP,
    updated_at   TIMESTAMPTZ   NOT NULL DEFAULT CURRENT_TIMESTAMP,

    CONSTRAINT uk_biz_budget_period UNIQUE (budget_id, period_code),
    CONSTRAINT ck_biz_budget_period_month CHECK (period_month BETWEEN 1 AND 12),
    CONSTRAINT ck_biz_budget_period_code  CHECK (period_code ~ '^\d{4}-(0[1-9]|1[0-2])$'),
    CONSTRAINT ck_biz_budget_period_amt   CHECK (amount >= 0),
    CONSTRAINT ck_biz_budget_period_match
        CHECK (right(period_code, 2)::smallint = period_month)
);

COMMENT ON TABLE biz_budget_period IS '预算月度分解：每个版本 12 行，Σ amount = biz_budget.total_amount';

CREATE INDEX IF NOT EXISTS idx_biz_budget_period_budget ON biz_budget_period (budget_id);
CREATE INDEX IF NOT EXISTS idx_biz_budget_period_code   ON biz_budget_period (period_code);

-- ---------- ③ biz_budget_usage（占用 / 执行事实） ----------
CREATE TABLE IF NOT EXISTS biz_budget_usage (
    id             UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    dept_id        UUID          NOT NULL,
    account_id     UUID          NOT NULL,
    fiscal_year    SMALLINT      NOT NULL,
    period_code    VARCHAR(7)    NOT NULL,
    amount         NUMERIC(18,4) NOT NULL,
    amount_net     NUMERIC(18,4) NOT NULL DEFAULT 0,
    source_type    VARCHAR(24)   NOT NULL,
    source_id      UUID          NOT NULL,
    source_no      VARCHAR(32)   NOT NULL,
    source_line_id UUID,
    status         VARCHAR(12)   NOT NULL DEFAULT 'OCCUPIED',
    consumed_at    TIMESTAMPTZ,
    voucher_id     UUID,
    released_at    TIMESTAMPTZ,
    release_reason VARCHAR(255),
    budget_id      UUID,
    over_budget    SMALLINT      NOT NULL DEFAULT 0,
    over_amount    NUMERIC(18,4) NOT NULL DEFAULT 0,
    created_at     TIMESTAMPTZ   NOT NULL DEFAULT CURRENT_TIMESTAMP,
    updated_at     TIMESTAMPTZ   NOT NULL DEFAULT CURRENT_TIMESTAMP,
    create_by      UUID,
    deleted_at     TIMESTAMPTZ,

    CONSTRAINT ck_biz_budget_usage_status
        CHECK (status IN ('OCCUPIED','CONSUMED','RELEASED')),
    CONSTRAINT ck_biz_budget_usage_amount CHECK (amount > 0 AND amount_net >= 0),
    CONSTRAINT ck_biz_budget_usage_source
        CHECK (source_type IN ('EXPENSE_REPORT','PURCHASE_ORDER')),
    CONSTRAINT ck_biz_budget_usage_over   CHECK (over_budget IN (0,1)),
    CONSTRAINT ck_biz_budget_usage_period CHECK (period_code ~ '^\d{4}-(0[1-9]|1[0-2])$'),
    CONSTRAINT ck_biz_budget_usage_done   CHECK (
        (status = 'RELEASED') = (released_at IS NOT NULL)
    ),
    CONSTRAINT ck_biz_budget_usage_consumed CHECK (
        status <> 'CONSUMED' OR (consumed_at IS NOT NULL)
    ),
    -- ARCH-6 §3.2.4：冗余年份必须与期间码一致（年度闸走索引的前提）
    CONSTRAINT ck_biz_budget_usage_year   CHECK (fiscal_year = left(period_code, 4)::smallint)
);

COMMENT ON TABLE biz_budget_usage IS
    '预算占用/执行记录（只增状态不改金额；不绑预算版本，绑 dept×account×year 槽）';
COMMENT ON COLUMN biz_budget_usage.budget_id IS
    '占用时命中的预算版本：仅供审计追溯「当时额度是多少」，可用余额计算不使用本列（D5）';
COMMENT ON COLUMN biz_budget_usage.over_amount IS
    '提交时点的超支额快照：额度后来被调高，不能让历史单据的「当时超没超」跟着变';

CREATE UNIQUE INDEX IF NOT EXISTS uk_biz_budget_usage_source
    ON biz_budget_usage (source_type, source_id, dept_id, account_id, fiscal_year)
    WHERE deleted_at IS NULL;
CREATE INDEX IF NOT EXISTS idx_biz_budget_usage_slot
    ON biz_budget_usage (dept_id, account_id, fiscal_year, status)
    WHERE deleted_at IS NULL;
CREATE INDEX IF NOT EXISTS idx_biz_budget_usage_slot_period
    ON biz_budget_usage (dept_id, account_id, period_code, status)
    WHERE deleted_at IS NULL;
CREATE INDEX IF NOT EXISTS idx_biz_budget_usage_source_id
    ON biz_budget_usage (source_type, source_id);

-- ---------- ④ biz_budget_subject_map（采购单 → 预算科目） ----------
CREATE TABLE IF NOT EXISTS biz_budget_subject_map (
    id           UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    map_type     VARCHAR(16)   NOT NULL,
    ref_id       UUID,
    ref_code     VARCHAR(40),
    account_id   UUID          NOT NULL,
    dept_id      UUID,
    priority     INT           NOT NULL DEFAULT 0,
    status       SMALLINT      NOT NULL DEFAULT 1,
    remark       TEXT,
    created_at   TIMESTAMPTZ   NOT NULL DEFAULT CURRENT_TIMESTAMP,
    updated_at   TIMESTAMPTZ   NOT NULL DEFAULT CURRENT_TIMESTAMP,
    create_by    UUID,
    update_by    UUID,
    deleted_at   TIMESTAMPTZ,

    CONSTRAINT ck_biz_bsm_type CHECK (map_type IN ('MATERIAL','MATERIAL_TYPE','WAREHOUSE','DEFAULT')),
    CONSTRAINT ck_biz_bsm_ref  CHECK ((map_type = 'DEFAULT' AND ref_id IS NULL)
                                   OR (map_type <> 'DEFAULT' AND ref_id IS NOT NULL)),
    CONSTRAINT ck_biz_bsm_status CHECK (status IN (0,1))
);

COMMENT ON TABLE biz_budget_subject_map IS
    '业务口径→预算科目映射（采购单没有科目列，只能靠它；报销单直接取明细的费用类型科目，不经此表）';

CREATE UNIQUE INDEX IF NOT EXISTS uk_biz_bsm_target
    ON biz_budget_subject_map (map_type, ref_id, COALESCE(dept_id, '00000000-0000-0000-0000-000000000000'::uuid))
    WHERE deleted_at IS NULL;
CREATE INDEX IF NOT EXISTS idx_biz_bsm_account ON biz_budget_subject_map (account_id);
CREATE INDEX IF NOT EXISTS idx_biz_bsm_dept    ON biz_budget_subject_map (dept_id) WHERE dept_id IS NOT NULL;

-- ---------- ⑤ 权限码与菜单 ----------
-- 目录：预算管理（挂在 财务管理 e02 下）
INSERT INTO sys_menu (id, name, menu_type, auth_code, path, parent_id, component,
                      sort_order, status, icon, title, is_visible, module_code)
VALUES
    ('20000000-0000-0000-0000-000000001300'::uuid, '预算管理', 1, '',
     '/business/erp/budget', '20000000-0000-0000-0000-000000000e02'::uuid, NULL,
     25, 1, 'money', '预算管理（编制 / 执行 / 科目映射）', TRUE, 'business'),
    ('20000000-0000-0000-0000-000000001301'::uuid, '预算编制', 2, 'business:erp:budget:list',
     '/business/erp/budget', '20000000-0000-0000-0000-000000001300'::uuid,
     'business/erp/budget/index', 10, 1, 'money', '预算编制（部门 × 科目 × 期间）', TRUE, 'business'),
    ('20000000-0000-0000-0000-000000001302'::uuid, '预算执行', 2, 'business:erp:budget:report',
     '/business/erp/budget/report', '20000000-0000-0000-0000-000000001300'::uuid,
     'business/erp/budget/report', 20, 1, 'chart-bar', '预算执行（已用 / 可用 / 执行率）', TRUE, 'business'),
    ('20000000-0000-0000-0000-000000001303'::uuid, '预算科目映射', 2, 'business:erp:budget:mapping',
     '/business/erp/budget/mapping', '20000000-0000-0000-0000-000000001300'::uuid,
     'business/erp/budget/mapping', 30, 1, 'link', '预算科目映射（采购 → 预算科目）', TRUE, 'business'),
    -- 编制页的按钮权限（menu_type=3）
    ('20000000-0000-0000-0000-000000001311'::uuid, 'business:erp:budget:create', 3, 'business:erp:budget:create',
     NULL, '20000000-0000-0000-0000-000000001301'::uuid, NULL, 10, 1, NULL, '新建预算', FALSE, 'business'),
    ('20000000-0000-0000-0000-000000001312'::uuid, 'business:erp:budget:update', 3, 'business:erp:budget:update',
     NULL, '20000000-0000-0000-0000-000000001301'::uuid, NULL, 20, 1, NULL, '编辑预算（仅草稿）', FALSE, 'business'),
    ('20000000-0000-0000-0000-000000001313'::uuid, 'business:erp:budget:delete', 3, 'business:erp:budget:delete',
     NULL, '20000000-0000-0000-0000-000000001301'::uuid, NULL, 30, 1, NULL, '删除预算（仅草稿）', FALSE, 'business'),
    ('20000000-0000-0000-0000-000000001314'::uuid, 'business:erp:budget:submit', 3, 'business:erp:budget:submit',
     NULL, '20000000-0000-0000-0000-000000001301'::uuid, NULL, 40, 1, NULL, '提交预算审批（budget_approve）', FALSE, 'business'),
    ('20000000-0000-0000-0000-000000001315'::uuid, 'business:erp:budget:adjust', 3, 'business:erp:budget:adjust',
     NULL, '20000000-0000-0000-0000-000000001301'::uuid, NULL, 50, 1, NULL, '预算调整（新增版本 → 走审批）', FALSE, 'business')
ON CONFLICT (id) DO NOTHING;

-- 授权：与 ARCH-6 §6.7.2 同口径（超管 + 管理 + 财务两档 + 部门主管）
INSERT INTO sys_role_menu (id, role_id, menu_id)
SELECT gen_random_uuid(), r.id, m.id
  FROM sys_role r
  CROSS JOIN (VALUES
        ('20000000-0000-0000-0000-000000001300'::uuid),
        ('20000000-0000-0000-0000-000000001301'::uuid),
        ('20000000-0000-0000-0000-000000001302'::uuid),
        ('20000000-0000-0000-0000-000000001303'::uuid),
        ('20000000-0000-0000-0000-000000001311'::uuid),
        ('20000000-0000-0000-0000-000000001312'::uuid),
        ('20000000-0000-0000-0000-000000001313'::uuid),
        ('20000000-0000-0000-0000-000000001314'::uuid),
        ('20000000-0000-0000-0000-000000001315'::uuid)) AS m(id)
 WHERE r.deleted_at IS NULL
   AND r.role_code IN ('SUPER_ADMIN', 'ADMIN', 'FINANCE_MANAGER', 'FINANCE_DIRECTOR', 'DEPT_LEADER')
   AND NOT EXISTS (SELECT 1 FROM sys_role_menu rm WHERE rm.role_id = r.id AND rm.menu_id = m.id);

-- ---------- ⑥ 自检 ----------
DO $$
DECLARE
    n int;
BEGIN
    -- 6.1 四张表
        IF to_regclass('public.biz_budget') IS NULL
           OR to_regclass('public.biz_budget_period') IS NULL
           OR to_regclass('public.biz_budget_usage') IS NULL
           OR to_regclass('public.biz_budget_subject_map') IS NULL THEN
            RAISE EXCEPTION 'V0227 自检失败：预算四表未全部建成';
        END IF;

        -- 6.2 关键索引（版本化与幂等的库级兜底）
        IF NOT EXISTS (SELECT 1 FROM pg_indexes WHERE indexname = 'uk_biz_budget_slot_active')
           OR NOT EXISTS (SELECT 1 FROM pg_indexes WHERE indexname = 'uk_biz_budget_slot_version')
           OR NOT EXISTS (SELECT 1 FROM pg_indexes WHERE indexname = 'uk_biz_budget_usage_source') THEN
            RAISE EXCEPTION 'V0227 自检失败：预算关键唯一索引缺失';
        END IF;

        -- 6.3 冗余年份约束
        IF NOT EXISTS (SELECT 1 FROM pg_constraint WHERE conname = 'ck_biz_budget_usage_year') THEN
            RAISE EXCEPTION 'V0227 自检失败：ck_biz_budget_usage_year 缺失';
        END IF;

        -- 6.4 8 个权限码
        SELECT count(*) INTO n FROM sys_menu
         WHERE auth_code IN ('business:erp:budget:list','business:erp:budget:create','business:erp:budget:update','business:erp:budget:delete',
                             'business:erp:budget:submit','business:erp:budget:adjust','business:erp:budget:report','business:erp:budget:mapping')
           AND deleted_at IS NULL;
        IF n <> 8 THEN
            RAISE EXCEPTION 'V0227 自检失败：预算权限码种了 % 个（期望 8）', n;
        END IF;

        -- 6.5 4 个菜单（1 目录 + 3 页面）
        SELECT count(*) INTO n FROM sys_menu
         WHERE id IN ('20000000-0000-0000-0000-000000001300'::uuid,
                      '20000000-0000-0000-0000-000000001301'::uuid,
                      '20000000-0000-0000-0000-000000001302'::uuid,
                      '20000000-0000-0000-0000-000000001303'::uuid)
           AND deleted_at IS NULL;
        IF n <> 4 THEN
            RAISE EXCEPTION 'V0227 自检失败：预算菜单只种了 % 个（期望 4）', n;
        END IF;
END $$;

-- ======================================================
-- V0242：费用报销四张表 + 8 条费用类型种子 + 权限码 + 菜单（ARCH-6 §4.2 / §6.7）
--
--   ① biz_expense_type     费用类型（主数据：借方科目 / 可抵扣 / 受预算 / 单笔上限）
--   ② biz_expense_report   报销单主表（金额三件套 + 付款三列 + 审批实例）
--   ③ biz_expense_item     明细（随主单整体重建，无 deleted_at）
--   ④ biz_expense_payment  付款记录（一张单可分次付）
--
-- ── 号段（实测复核）────────────────────────────────────
-- 开工时 backend/migrations/business/ 最大号 = V0241（P2-2B-FIX 菜单归位）。
-- 设计 §6.9 预留的 V0221/V0222/V0228 已被 FIN-1/FIN-2 占走，本卡顺延：
--   V0242 四张表 + 种子 + 权限        ← 本文件
--   V0243 expense 审批流（金额三档）
--   V0244 2 条凭证规则 + 2 处 CHECK 放宽 + 1 条 sys_config
--
-- ── 三道金额恒等式（本章红线 2：全部库级 CHECK，不许只靠代码）──
--   明细：with_tax_amount = amount + tax_amount
--   主表：total_with_tax  = total_amount + total_tax
--   付款：unpaid_amount   = total_with_tax - paid_amount
--
-- ── 与应付的关系（D1）────────────────────────────────────
-- 报销**不复用** biz_payable / biz_payment：债权人（员工 vs 供应商）、
-- 到期日（无 vs 账期）、明细（有 vs 无）、科目（2241.01 vs 2202）四处约束冲突。
-- biz_expense_payment 的结构与 biz_payment 刻意同构，为将来「多态付款表」留演进空间。
--
-- 幂等：CREATE TABLE IF NOT EXISTS / CREATE INDEX IF NOT EXISTS / 种子 NOT EXISTS /
--       sys_menu ON CONFLICT (id) DO NOTHING。末尾 DO 块自检，任一不符 → 迁移失败。
-- ======================================================

-- ---------- ① biz_expense_type（费用类型） ----------
CREATE TABLE IF NOT EXISTS biz_expense_type (
    id                UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    type_code         VARCHAR(40)   NOT NULL,
    type_name         VARCHAR(120)  NOT NULL,
    account_id        UUID          NOT NULL,
    account_code      VARCHAR(30)   NOT NULL,
    tax_deductible    SMALLINT      NOT NULL DEFAULT 1,
    budget_controlled SMALLINT      NOT NULL DEFAULT 1,
    need_invoice_no   SMALLINT      NOT NULL DEFAULT 1,
    amount_limit      NUMERIC(18,4) NOT NULL DEFAULT 0,
    sort_order        INT           NOT NULL DEFAULT 0,
    status            SMALLINT      NOT NULL DEFAULT 1,
    remark            TEXT,
    created_at        TIMESTAMPTZ   NOT NULL DEFAULT CURRENT_TIMESTAMP,
    updated_at        TIMESTAMPTZ   NOT NULL DEFAULT CURRENT_TIMESTAMP,
    create_by         UUID,
    update_by         UUID,
    deleted_at        TIMESTAMPTZ,

    CONSTRAINT ck_biz_exp_type_deduct  CHECK (tax_deductible IN (0,1)),
    CONSTRAINT ck_biz_exp_type_budget  CHECK (budget_controlled IN (0,1)),
    CONSTRAINT ck_biz_exp_type_invoice CHECK (need_invoice_no IN (0,1)),
    CONSTRAINT ck_biz_exp_type_limit   CHECK (amount_limit >= 0),
    CONSTRAINT ck_biz_exp_type_status  CHECK (status IN (0,1))
);

CREATE UNIQUE INDEX IF NOT EXISTS uk_biz_exp_type_code
    ON biz_expense_type (type_code) WHERE deleted_at IS NULL;
CREATE INDEX IF NOT EXISTS idx_biz_exp_type_account ON biz_expense_type (account_id);
CREATE INDEX IF NOT EXISTS idx_biz_exp_type_status  ON biz_expense_type (status, sort_order);

COMMENT ON TABLE biz_expense_type IS
    '费用类型主数据：每个类型绑一个借方费用科目 + 进项税可抵扣标志 + 是否受预算控制';
COMMENT ON COLUMN biz_expense_type.account_code IS
    '科目编码快照：与 biz_budget.account_code 同理，科目改名后历史明细的语义不漂移';

-- ---------- ② biz_expense_report（报销单主表） ----------
CREATE TABLE IF NOT EXISTS biz_expense_report (
    id                    UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    report_no             VARCHAR(32)   NOT NULL,
    reporter_id           UUID          NOT NULL,
    reporter_name         VARCHAR(50)   NOT NULL,
    dept_id               UUID          NOT NULL,
    dept_name             VARCHAR(120)  NOT NULL,
    title                 VARCHAR(200)  NOT NULL,
    expense_date          DATE          NOT NULL,
    period_code           VARCHAR(7)    NOT NULL,
    total_amount          NUMERIC(18,4) NOT NULL DEFAULT 0,
    total_tax             NUMERIC(18,4) NOT NULL DEFAULT 0,
    total_with_tax        NUMERIC(18,4) NOT NULL DEFAULT 0,
    item_count            INT           NOT NULL DEFAULT 0,
    over_budget           SMALLINT      NOT NULL DEFAULT 0,
    over_amount           NUMERIC(18,4) NOT NULL DEFAULT 0,
    budget_mode           VARCHAR(8),
    status                VARCHAR(16)   NOT NULL DEFAULT 'DRAFT',
    approval_instance_id  UUID,
    voucher_id            UUID,
    paid_amount           NUMERIC(18,4) NOT NULL DEFAULT 0,
    unpaid_amount         NUMERIC(18,4) NOT NULL DEFAULT 0,
    pay_status            VARCHAR(12)   NOT NULL DEFAULT 'UNPAID',
    last_pay_date         DATE,
    submitted_at          TIMESTAMPTZ,
    submitted_by          UUID,
    approved_at           TIMESTAMPTZ,
    approved_by           UUID,
    cancel_reason         VARCHAR(255),
    cancelled_at          TIMESTAMPTZ,
    cancelled_by          UUID,
    remark                TEXT,
    created_at            TIMESTAMPTZ   NOT NULL DEFAULT CURRENT_TIMESTAMP,
    updated_at            TIMESTAMPTZ   NOT NULL DEFAULT CURRENT_TIMESTAMP,
    create_by             UUID,
    update_by             UUID,
    deleted_at            TIMESTAMPTZ,

    CONSTRAINT ck_biz_exp_report_status CHECK (status IN
        ('DRAFT','SUBMITTED','APPROVED','PAID','CANCELLED')),
    CONSTRAINT ck_biz_exp_report_pay_status CHECK (pay_status IN
        ('UNPAID','PARTIAL','PAID')),
    CONSTRAINT ck_biz_exp_report_amount CHECK (
        total_amount >= 0 AND total_tax >= 0 AND total_with_tax >= 0),
    CONSTRAINT ck_biz_exp_report_tax_sum CHECK (total_with_tax = total_amount + total_tax),
    CONSTRAINT ck_biz_exp_report_items CHECK (item_count >= 1),
    CONSTRAINT ck_biz_exp_report_paid CHECK (
        paid_amount >= 0 AND paid_amount <= total_with_tax
        AND unpaid_amount = total_with_tax - paid_amount),
    CONSTRAINT ck_biz_exp_report_pay_match CHECK (
        (pay_status = 'UNPAID'  AND paid_amount = 0)
     OR (pay_status = 'PARTIAL' AND paid_amount > 0 AND paid_amount < total_with_tax)
     OR (pay_status = 'PAID'    AND paid_amount = total_with_tax)),
    CONSTRAINT ck_biz_exp_report_paid_state CHECK (pay_status <> 'PAID' OR status = 'PAID'),
    CONSTRAINT ck_biz_exp_report_over CHECK (over_budget IN (0,1)),
    CONSTRAINT ck_biz_exp_report_cancel CHECK (status <> 'CANCELLED' OR cancel_reason IS NOT NULL),
    CONSTRAINT ck_biz_exp_report_inst CHECK (
        deleted_at IS NOT NULL
        OR (status = 'SUBMITTED' AND approval_instance_id IS NOT NULL)
        OR (status IN ('APPROVED','PAID'))
        OR (status NOT IN ('SUBMITTED','APPROVED','PAID') AND approval_instance_id IS NULL)),
    CONSTRAINT ck_biz_exp_report_period CHECK (period_code ~ '^\d{4}-(0[1-9]|1[0-2])$')
);

COMMENT ON TABLE biz_expense_report IS
    '费用报销单（对员工，不挂供应商；审批通过即入账，付款分次冲平 2241.01）';
COMMENT ON COLUMN biz_expense_report.period_code IS
    '入账期间：按审批通过日所属期间重算（不是 expense_date 所属期间，理由见 ARCH-6 §4.5.4）';
COMMENT ON COLUMN biz_expense_report.over_amount IS
    '提交时点的超预算额快照（额度事后调整不改历史）';

CREATE UNIQUE INDEX IF NOT EXISTS uk_biz_exp_report_no
    ON biz_expense_report (report_no) WHERE deleted_at IS NULL;
CREATE INDEX IF NOT EXISTS idx_biz_exp_report_reporter
    ON biz_expense_report (reporter_id, expense_date DESC);
CREATE INDEX IF NOT EXISTS idx_biz_exp_report_dept
    ON biz_expense_report (dept_id, period_code, status);
CREATE INDEX IF NOT EXISTS idx_biz_exp_report_status
    ON biz_expense_report (status, submitted_at DESC);
CREATE INDEX IF NOT EXISTS idx_biz_exp_report_pay
    ON biz_expense_report (pay_status, approved_at)
    WHERE status IN ('APPROVED','PAID');
CREATE INDEX IF NOT EXISTS idx_biz_exp_report_voucher
    ON biz_expense_report (voucher_id) WHERE voucher_id IS NOT NULL;
CREATE INDEX IF NOT EXISTS idx_biz_exp_report_approval
    ON biz_expense_report (approval_instance_id) WHERE approval_instance_id IS NOT NULL;
CREATE INDEX IF NOT EXISTS idx_biz_exp_report_no_status ON biz_expense_report (report_no, status);

-- ---------- ③ biz_expense_item（明细） ----------
CREATE TABLE IF NOT EXISTS biz_expense_item (
    id              UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    report_id       UUID          NOT NULL REFERENCES biz_expense_report(id) ON DELETE CASCADE,
    line_no         INT           NOT NULL,
    expense_date    DATE          NOT NULL,
    expense_type_id UUID          NOT NULL,
    type_code       VARCHAR(40)   NOT NULL,
    type_name       VARCHAR(120)  NOT NULL,
    account_id      UUID          NOT NULL,
    account_code    VARCHAR(30)   NOT NULL,
    account_name    VARCHAR(120)  NOT NULL,
    amount          NUMERIC(18,4) NOT NULL DEFAULT 0,
    tax_rate        NUMERIC(7,4)  NOT NULL DEFAULT 0,
    tax_amount      NUMERIC(18,4) NOT NULL DEFAULT 0,
    with_tax_amount NUMERIC(18,4) NOT NULL DEFAULT 0,
    invoice_no      VARCHAR(50),
    invoice_date    DATE,
    invoice_type    VARCHAR(16),
    summary         VARCHAR(255)  NOT NULL,
    remark          VARCHAR(255),
    created_at      TIMESTAMPTZ   NOT NULL DEFAULT CURRENT_TIMESTAMP,
    updated_at      TIMESTAMPTZ   NOT NULL DEFAULT CURRENT_TIMESTAMP,

    CONSTRAINT uk_biz_exp_item_line UNIQUE (report_id, line_no),
    CONSTRAINT ck_biz_exp_item_amt  CHECK (amount >= 0 AND tax_amount >= 0 AND with_tax_amount >= 0),
    CONSTRAINT ck_biz_exp_item_tax  CHECK (with_tax_amount = amount + tax_amount),
    CONSTRAINT ck_biz_exp_item_rate CHECK (tax_rate >= 0 AND tax_rate <= 1),
    CONSTRAINT ck_biz_exp_item_pos  CHECK (with_tax_amount > 0),
    CONSTRAINT ck_biz_exp_item_inv  CHECK (invoice_type IS NULL OR invoice_type IN
        ('VAT_SPECIAL','VAT_NORMAL','E_INVOICE','NONE'))
);

CREATE INDEX IF NOT EXISTS idx_biz_exp_item_report ON biz_expense_item (report_id, line_no);
CREATE INDEX IF NOT EXISTS idx_biz_exp_item_type   ON biz_expense_item (expense_type_id);
CREATE INDEX IF NOT EXISTS idx_biz_exp_item_acct   ON biz_expense_item (account_id);
CREATE INDEX IF NOT EXISTS idx_biz_exp_item_date   ON biz_expense_item (expense_date);

COMMENT ON TABLE biz_expense_item IS
    '报销明细：每条一行费用（一类型一税率）；借方科目按类型带出并快照，凭证按科目聚合出分录';

-- ---------- ④ biz_expense_payment（付款记录） ----------
CREATE TABLE IF NOT EXISTS biz_expense_payment (
    id           UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    payment_no   VARCHAR(32)   NOT NULL,
    report_id    UUID          NOT NULL REFERENCES biz_expense_report(id),
    amount       NUMERIC(18,4) NOT NULL,
    pay_date     DATE          NOT NULL,
    pay_method   VARCHAR(32),
    pay_account_id UUID,
    voucher_id   UUID,
    period_code  VARCHAR(7)    NOT NULL,
    status       VARCHAR(12)   NOT NULL DEFAULT 'PAID',
    remark       TEXT,
    created_at   TIMESTAMPTZ   NOT NULL DEFAULT CURRENT_TIMESTAMP,
    updated_at   TIMESTAMPTZ   NOT NULL DEFAULT CURRENT_TIMESTAMP,
    create_by    UUID,
    update_by    UUID,
    deleted_at   TIMESTAMPTZ,

    CONSTRAINT ck_biz_exp_pay_amount CHECK (amount > 0),
    CONSTRAINT ck_biz_exp_pay_status CHECK (status IN ('PAID','CANCELLED')),
    CONSTRAINT ck_biz_exp_pay_period CHECK (period_code ~ '^\d{4}-(0[1-9]|1[0-2])$')
);

CREATE UNIQUE INDEX IF NOT EXISTS uk_biz_exp_pay_no
    ON biz_expense_payment (payment_no) WHERE deleted_at IS NULL;
CREATE INDEX IF NOT EXISTS idx_biz_exp_pay_report
    ON biz_expense_payment (report_id, pay_date);
CREATE INDEX IF NOT EXISTS idx_biz_exp_pay_voucher
    ON biz_expense_payment (voucher_id) WHERE voucher_id IS NOT NULL;

COMMENT ON TABLE biz_expense_payment IS
    '报销付款记录（一张报销单可分次付款；每笔一张凭证 借 2241.01 / 贷 1002）';

-- ---------- ⑤ 8 条费用类型种子（ARCH-6 §4.2.1） ----------
-- 科目一律按编码解析（5602.0x 由 V0226 / FIN-1 种好，5711 为存量科目）；
-- 解析不到 → 整条不种（下面自检会报出来，宁可迁移失败也不种半成品）。
INSERT INTO biz_expense_type (
    type_code, type_name, account_id, account_code, tax_deductible,
    budget_controlled, need_invoice_no, amount_limit, sort_order, status, remark)
SELECT v.code, v.name, a.id, v.acct, v.deduct, v.budget, 1, 0, v.sort, 1, v.remark
  FROM (VALUES
        ('TRAVEL',  '差旅费',      '5602.02', 1::smallint, 1::smallint, 10, '交通、住宿、差旅补贴'),
        ('MEAL',    '业务招待费',  '5602.04', 0::smallint, 1::smallint, 20, '准则：招待费进项不得抵扣'),
        ('OFFICE',  '办公费',      '5602.03', 1::smallint, 1::smallint, 30, '文具、耗材、打印'),
        ('COMM',    '通讯费',      '5602.05', 1::smallint, 1::smallint, 40, '话费、宽带'),
        ('TRAIN',   '培训费',      '5602.06', 1::smallint, 1::smallint, 50, '外训、资料'),
        ('WELFARE', '职工福利费',  '5602.07', 0::smallint, 1::smallint, 60, '团建、节日慰问（福利费进项不得抵扣）'),
        ('OTHER',   '其他管理费用','5602.09', 1::smallint, 1::smallint, 70, '兜底'),
        ('PENALTY', '罚款/滞纳金', '5711',    0::smallint, 0::smallint, 80, '营业外支出，不编预算')
       ) AS v(code, name, acct, deduct, budget, sort, remark)
  JOIN biz_account a ON a.account_code = v.acct AND a.deleted_at IS NULL AND a.status = 1
 WHERE NOT EXISTS (SELECT 1 FROM biz_expense_type t
                    WHERE t.type_code = v.code AND t.deleted_at IS NULL);

-- ---------- ⑥ 权限码与菜单（12 个权限码 + 1 目录 + 3 页面 + 10 按钮） ----------
-- 号段 0015xx：0014xx 已给固定资产（V0229），0013xx 给预算（V0227）。
INSERT INTO sys_menu (id, name, menu_type, auth_code, path, parent_id, component,
                      sort_order, status, icon, title, is_visible, module_code)
VALUES
    -- 目录：费用报销（挂在 财务管理 e02 下）
    ('20000000-0000-0000-0000-000000001500'::uuid, '费用报销', 1, '',
     '/business/erp/expense', '20000000-0000-0000-0000-000000000e02'::uuid, NULL,
     27, 1, 'money', '费用报销（报销单 / 待付款 / 费用类型）', TRUE, 'business'),
    ('20000000-0000-0000-0000-000000001501'::uuid, '我的报销', 2, 'business:erp:expense:list',
     '/business/erp/expense', '20000000-0000-0000-0000-000000001500'::uuid,
     'business/erp/expense/index', 10, 1, 'invoice', '我的报销（新建 / 提交 / 撤回 / 作废）', TRUE, 'business'),
    ('20000000-0000-0000-0000-000000001502'::uuid, '待付款', 2, 'business:erp:expense:pay',
     '/business/erp/expense/pay', '20000000-0000-0000-0000-000000001500'::uuid,
     'business/erp/expense/pay/index', 20, 1, 'wallet', '待付款（已审批未付清的报销单）', TRUE, 'business'),
    ('20000000-0000-0000-0000-000000001503'::uuid, '费用类型', 2, 'business:erp:expense-type:list',
     '/business/erp/expense-type', '20000000-0000-0000-0000-000000001500'::uuid,
     'business/erp/expense-type/index', 30, 1, 'folder', '费用类型（科目 / 可抵扣 / 受预算 / 上限）', TRUE, 'business'),
    -- 我的报销页的按钮权限
    ('20000000-0000-0000-0000-000000001511'::uuid, 'business:erp:expense:create', 3, 'business:erp:expense:create',
     NULL, '20000000-0000-0000-0000-000000001501'::uuid, NULL, 10, 1, NULL, '新建报销单', FALSE, 'business'),
    ('20000000-0000-0000-0000-000000001512'::uuid, 'business:erp:expense:update', 3, 'business:erp:expense:update',
     NULL, '20000000-0000-0000-0000-000000001501'::uuid, NULL, 20, 1, NULL, '编辑报销单（草稿）', FALSE, 'business'),
    ('20000000-0000-0000-0000-000000001513'::uuid, 'business:erp:expense:delete', 3, 'business:erp:expense:delete',
     NULL, '20000000-0000-0000-0000-000000001501'::uuid, NULL, 30, 1, NULL, '删除报销单（草稿软删）', FALSE, 'business'),
    ('20000000-0000-0000-0000-000000001514'::uuid, 'business:erp:expense:submit', 3, 'business:erp:expense:submit',
     NULL, '20000000-0000-0000-0000-000000001501'::uuid, NULL, 40, 1, NULL, '提交审批（预算守卫 + 发起流程）', FALSE, 'business'),
    ('20000000-0000-0000-0000-000000001515'::uuid, 'business:erp:expense:cancel', 3, 'business:erp:expense:cancel',
     NULL, '20000000-0000-0000-0000-000000001501'::uuid, NULL, 50, 1, NULL, '作废（已入账的红冲凭证）', FALSE, 'business'),
    ('20000000-0000-0000-0000-000000001516'::uuid, 'business:erp:expense:detail', 3, 'business:erp:expense:detail',
     NULL, '20000000-0000-0000-0000-000000001501'::uuid, NULL, 60, 1, NULL, '报销详情（审批轨迹 / 凭证 / 付款记录）', FALSE, 'business'),
    -- 待付款页的按钮权限
    ('20000000-0000-0000-0000-000000001521'::uuid, 'business:erp:expense:pay', 3, 'business:erp:expense:pay',
     NULL, '20000000-0000-0000-0000-000000001502'::uuid, NULL, 10, 1, NULL, '付款登记（生成付款凭证）', FALSE, 'business'),
    -- 费用类型页的按钮权限
    ('20000000-0000-0000-0000-000000001531'::uuid, 'business:erp:expense-type:create', 3, 'business:erp:expense-type:create',
     NULL, '20000000-0000-0000-0000-000000001503'::uuid, NULL, 10, 1, NULL, '新建费用类型', FALSE, 'business'),
    ('20000000-0000-0000-0000-000000001532'::uuid, 'business:erp:expense-type:update', 3, 'business:erp:expense-type:update',
     NULL, '20000000-0000-0000-0000-000000001503'::uuid, NULL, 20, 1, NULL, '编辑费用类型', FALSE, 'business'),
    ('20000000-0000-0000-0000-000000001533'::uuid, 'business:erp:expense-type:delete', 3, 'business:erp:expense-type:delete',
     NULL, '20000000-0000-0000-0000-000000001503'::uuid, NULL, 30, 1, NULL, '删除费用类型', FALSE, 'business')
ON CONFLICT (id) DO NOTHING;

-- 授权：与 ARCH-6 §6.7.2 同口径（超管 + 管理 + 财务两档 + 部门主管）
INSERT INTO sys_role_menu (id, role_id, menu_id)
SELECT gen_random_uuid(), r.id, m.id
  FROM sys_role r
  CROSS JOIN (VALUES
        ('20000000-0000-0000-0000-000000001500'::uuid),
        ('20000000-0000-0000-0000-000000001501'::uuid),
        ('20000000-0000-0000-0000-000000001502'::uuid),
        ('20000000-0000-0000-0000-000000001503'::uuid),
        ('20000000-0000-0000-0000-000000001511'::uuid),
        ('20000000-0000-0000-0000-000000001512'::uuid),
        ('20000000-0000-0000-0000-000000001513'::uuid),
        ('20000000-0000-0000-0000-000000001514'::uuid),
        ('20000000-0000-0000-0000-000000001515'::uuid),
        ('20000000-0000-0000-0000-000000001516'::uuid),
        ('20000000-0000-0000-0000-000000001521'::uuid),
        ('20000000-0000-0000-0000-000000001531'::uuid),
        ('20000000-0000-0000-0000-000000001532'::uuid),
        ('20000000-0000-0000-0000-000000001533'::uuid)) AS m(id)
 WHERE r.deleted_at IS NULL
   AND r.role_code IN ('SUPER_ADMIN', 'ADMIN', 'FINANCE_MANAGER', 'FINANCE_DIRECTOR', 'DEPT_LEADER')
   AND NOT EXISTS (SELECT 1 FROM sys_role_menu rm WHERE rm.role_id = r.id AND rm.menu_id = m.id);

-- ---------- ⑦ 自检 ----------
DO $$
DECLARE
    n int;
BEGIN
    -- 7.1 四张表
    IF to_regclass('public.biz_expense_type') IS NULL
       OR to_regclass('public.biz_expense_report') IS NULL
       OR to_regclass('public.biz_expense_item') IS NULL
       OR to_regclass('public.biz_expense_payment') IS NULL THEN
        RAISE EXCEPTION 'V0242 自检失败：报销四张表未全部建成';
    END IF;

    -- 7.2 三道金额恒等式 + 状态域 CHECK 必须在位
    IF NOT EXISTS (SELECT 1 FROM pg_constraint WHERE conname = 'ck_biz_exp_report_tax_sum')
       OR NOT EXISTS (SELECT 1 FROM pg_constraint WHERE conname = 'ck_biz_exp_report_paid')
       OR NOT EXISTS (SELECT 1 FROM pg_constraint WHERE conname = 'ck_biz_exp_item_tax')
       OR NOT EXISTS (SELECT 1 FROM pg_constraint WHERE conname = 'ck_biz_exp_report_pay_match') THEN
        RAISE EXCEPTION 'V0242 自检失败：报销金额恒等式 / 付款自洽 CHECK 缺失';
    END IF;

    -- 7.3 唯一索引（单号 / 明细行号 / 付款单号）
    IF NOT EXISTS (SELECT 1 FROM pg_indexes WHERE indexname = 'uk_biz_exp_report_no')
       OR NOT EXISTS (SELECT 1 FROM pg_indexes WHERE indexname = 'uk_biz_exp_item_line')
       OR NOT EXISTS (SELECT 1 FROM pg_indexes WHERE indexname = 'uk_biz_exp_pay_no') THEN
        RAISE EXCEPTION 'V0242 自检失败：报销唯一索引缺失';
    END IF;

    -- 7.4 科目必须先存在（本迁移不代建科目）
    IF NOT EXISTS (SELECT 1 FROM biz_account WHERE account_code = '2241.01' AND deleted_at IS NULL) THEN
        RAISE EXCEPTION 'V0242 自检失败：科目 2241.01 其他应付款-员工垫付不存在（V0226 种子缺失）';
    END IF;
    IF NOT EXISTS (SELECT 1 FROM biz_account WHERE account_code = '5602.02' AND deleted_at IS NULL) THEN
        RAISE EXCEPTION 'V0242 自检失败：科目 5602.02 管理费用-差旅费不存在（V0226 种子缺失）';
    END IF;

    -- 7.5 8 条费用类型种子
    SELECT count(*) INTO n FROM biz_expense_type
     WHERE deleted_at IS NULL
       AND type_code IN ('TRAVEL','MEAL','OFFICE','COMM','TRAIN','WELFARE','OTHER','PENALTY');
    IF n <> 8 THEN
        RAISE EXCEPTION 'V0242 自检失败：费用类型种子只种了 % 条（期望 8；检查 5602.0x / 5711 科目是否齐全）', n;
    END IF;
    -- 不可抵扣的类型必须 tax_deductible = 0（招待费 / 福利费 / 罚款）
    SELECT count(*) INTO n FROM biz_expense_type
     WHERE deleted_at IS NULL AND type_code IN ('MEAL','WELFARE','PENALTY') AND tax_deductible <> 0;
    IF n <> 0 THEN
        RAISE EXCEPTION 'V0242 自检失败：% 条不可抵扣类型的 tax_deductible 不是 0', n;
    END IF;

    -- 7.6 12 个权限码（`expense:pay` 在页面行与按钮行各出现一次，按 distinct 数）
    SELECT count(DISTINCT auth_code) INTO n FROM sys_menu
     WHERE deleted_at IS NULL AND auth_code IN (
        'business:erp:expense:list','business:erp:expense:create','business:erp:expense:update',
        'business:erp:expense:delete','business:erp:expense:submit','business:erp:expense:cancel',
        'business:erp:expense:pay','business:erp:expense:detail',
        'business:erp:expense-type:list','business:erp:expense-type:create',
        'business:erp:expense-type:update','business:erp:expense-type:delete');
    IF n <> 12 THEN
        RAISE EXCEPTION 'V0242 自检失败：报销权限码种了 % 个（期望 12）', n;
    END IF;

    -- 7.7 目录 + 3 个页面菜单
    SELECT count(*) INTO n FROM sys_menu
     WHERE id IN ('20000000-0000-0000-0000-000000001500'::uuid,
                  '20000000-0000-0000-0000-000000001501'::uuid,
                  '20000000-0000-0000-0000-000000001502'::uuid,
                  '20000000-0000-0000-0000-000000001503'::uuid)
       AND deleted_at IS NULL;
    IF n <> 4 THEN
        RAISE EXCEPTION 'V0242 自检失败：报销菜单种了 % 行（期望 4：1 目录 + 3 页面）', n;
    END IF;
END $$;

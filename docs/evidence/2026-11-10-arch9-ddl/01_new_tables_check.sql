-- ARCH-9 新表 DDL 可执行性 + 约束齿口验证（临时库，验完即 DROP）
-- 对应 docs/architecture/ARCH-9-多组织与合并报表设计.md §3.1
\set ON_ERROR_STOP 1

CREATE EXTENSION IF NOT EXISTS pgcrypto;

-- stubs：真实库里已存在（sys_user 由 system/V0001 建），此处只造主键列
CREATE TABLE sys_user (id UUID PRIMARY KEY DEFAULT gen_random_uuid());

-- ==================== ① biz_org ====================
CREATE TABLE IF NOT EXISTS biz_org (
    id                 UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    org_code           VARCHAR(40)  NOT NULL,
    org_name           VARCHAR(200) NOT NULL,
    short_name         VARCHAR(60),
    parent_id          UUID,
    org_path           VARCHAR(500),
    org_level          SMALLINT     NOT NULL DEFAULT 1,
    is_legal_entity    SMALLINT     NOT NULL DEFAULT 1,
    is_group_root      SMALLINT     NOT NULL DEFAULT 0,
    credit_code        VARCHAR(30),
    tax_no             VARCHAR(40),
    base_currency      VARCHAR(8)   NOT NULL DEFAULT 'CNY',
    fiscal_year_start_month SMALLINT NOT NULL DEFAULT 1,
    ownership_ratio    NUMERIC(7,4) NOT NULL DEFAULT 100,
    contact_person     VARCHAR(50),
    contact_phone      VARCHAR(30),
    address            VARCHAR(255),
    status             VARCHAR(16)  NOT NULL DEFAULT 'ACTIVE',
    sort_order         INT          NOT NULL DEFAULT 0,
    remark             TEXT,
    created_at         TIMESTAMPTZ  NOT NULL DEFAULT CURRENT_TIMESTAMP,
    updated_at         TIMESTAMPTZ  NOT NULL DEFAULT CURRENT_TIMESTAMP,
    create_by          UUID,
    update_by          UUID,
    deleted_at         TIMESTAMPTZ,

    CONSTRAINT ck_biz_org_status   CHECK (status IN ('ACTIVE','DISABLED')),
    CONSTRAINT ck_biz_org_level    CHECK (org_level >= 1),
    CONSTRAINT ck_biz_org_legal    CHECK (is_legal_entity IN (0,1)),
    CONSTRAINT ck_biz_org_root     CHECK (is_group_root IN (0,1)),
    CONSTRAINT ck_biz_org_month    CHECK (fiscal_year_start_month BETWEEN 1 AND 12),
    CONSTRAINT ck_biz_org_ratio    CHECK (ownership_ratio > 0 AND ownership_ratio <= 100),
    CONSTRAINT ck_biz_org_no_self  CHECK (parent_id IS NULL OR parent_id <> id),
    CONSTRAINT ck_biz_org_credit   CHECK ((is_legal_entity = 1)
                                        OR (is_legal_entity = 0 AND credit_code IS NULL))
);
CREATE UNIQUE INDEX IF NOT EXISTS uk_biz_org_code ON biz_org (org_code) WHERE deleted_at IS NULL;
CREATE INDEX        IF NOT EXISTS idx_biz_org_parent ON biz_org (parent_id);
CREATE INDEX        IF NOT EXISTS idx_biz_org_path   ON biz_org (org_path);
CREATE INDEX        IF NOT EXISTS idx_biz_org_status ON biz_org (status, sort_order);

-- ==================== ② biz_org_user ====================
CREATE TABLE IF NOT EXISTS biz_org_user (
    id         UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    org_id     UUID NOT NULL REFERENCES biz_org(id) ON DELETE RESTRICT,
    user_id    UUID NOT NULL REFERENCES sys_user(id) ON DELETE CASCADE,
    is_default SMALLINT NOT NULL DEFAULT 0,
    created_at TIMESTAMPTZ NOT NULL DEFAULT CURRENT_TIMESTAMP,
    create_by  UUID,
    CONSTRAINT uk_biz_org_user UNIQUE (org_id, user_id),
    CONSTRAINT ck_biz_org_user_default CHECK (is_default IN (0,1))
);
CREATE UNIQUE INDEX IF NOT EXISTS uk_biz_org_user_default
    ON biz_org_user (user_id) WHERE is_default = 1;
CREATE INDEX IF NOT EXISTS idx_biz_org_user_user ON biz_org_user (user_id);

-- ==================== ③ biz_org_account ====================
CREATE TABLE IF NOT EXISTS biz_org_account (
    id         UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    org_id     UUID NOT NULL REFERENCES biz_org(id) ON DELETE CASCADE,
    account_id UUID NOT NULL,
    status     SMALLINT NOT NULL DEFAULT 1,
    sort_order INT      NOT NULL DEFAULT 0,
    remark     TEXT,
    created_at TIMESTAMPTZ NOT NULL DEFAULT CURRENT_TIMESTAMP,
    updated_at TIMESTAMPTZ NOT NULL DEFAULT CURRENT_TIMESTAMP,
    create_by  UUID,
    update_by  UUID,
    CONSTRAINT uk_biz_org_account UNIQUE (org_id, account_id),
    CONSTRAINT ck_biz_org_account_status CHECK (status IN (0,1))
);
CREATE INDEX IF NOT EXISTS idx_biz_org_account_org ON biz_org_account (org_id, status);

-- ==================== ④ biz_consol_worksheet ====================
CREATE TABLE IF NOT EXISTS biz_consol_worksheet (
    id             UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    group_org_id   UUID NOT NULL REFERENCES biz_org(id) ON DELETE RESTRICT,
    period_code    VARCHAR(7)  NOT NULL,
    base_currency  VARCHAR(8)  NOT NULL DEFAULT 'CNY',
    status         VARCHAR(16) NOT NULL DEFAULT 'DRAFT',
    member_count   INT         NOT NULL DEFAULT 0,
    offset_debit   NUMERIC(20,4) NOT NULL DEFAULT 0,
    offset_credit  NUMERIC(20,4) NOT NULL DEFAULT 0,
    diff_amount    NUMERIC(20,4) NOT NULL DEFAULT 0,
    generated_at   TIMESTAMPTZ,
    generated_by   UUID,
    confirmed_at   TIMESTAMPTZ,
    confirmed_by   UUID,
    unfreeze_count INT         NOT NULL DEFAULT 0,
    last_reason    VARCHAR(500),
    remark         TEXT,
    created_at     TIMESTAMPTZ NOT NULL DEFAULT CURRENT_TIMESTAMP,
    updated_at     TIMESTAMPTZ NOT NULL DEFAULT CURRENT_TIMESTAMP,
    create_by      UUID,
    update_by      UUID,
    deleted_at     TIMESTAMPTZ,

    CONSTRAINT ck_biz_consol_ws_status CHECK (status IN ('DRAFT','GENERATED','CONFIRMED','CANCELLED')),
    CONSTRAINT ck_biz_consol_ws_period CHECK (period_code ~ '^[0-9]{4}-(0[1-9]|1[0-2])$'),
    CONSTRAINT ck_biz_consol_ws_count  CHECK (member_count >= 0 AND unfreeze_count >= 0),
    CONSTRAINT ck_biz_consol_ws_balance CHECK (offset_debit = offset_credit)
);
CREATE UNIQUE INDEX IF NOT EXISTS uk_biz_consol_ws
    ON biz_consol_worksheet (group_org_id, period_code)
    WHERE deleted_at IS NULL AND status <> 'CANCELLED';
CREATE INDEX IF NOT EXISTS idx_biz_consol_ws_period ON biz_consol_worksheet (period_code, status);

-- ==================== ⑤ biz_consol_scope ====================
CREATE TABLE IF NOT EXISTS biz_consol_scope (
    id            UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    worksheet_id  UUID NOT NULL REFERENCES biz_consol_worksheet(id) ON DELETE CASCADE,
    member_org_id UUID NOT NULL REFERENCES biz_org(id) ON DELETE RESTRICT,
    included      SMALLINT NOT NULL DEFAULT 1,
    exclude_reason VARCHAR(200),
    period_closed SMALLINT NOT NULL DEFAULT 0,
    created_at    TIMESTAMPTZ NOT NULL DEFAULT CURRENT_TIMESTAMP,
    CONSTRAINT uk_biz_consol_scope UNIQUE (worksheet_id, member_org_id),
    CONSTRAINT ck_biz_consol_scope_in CHECK (included IN (0,1)),
    CONSTRAINT ck_biz_consol_scope_reason CHECK (included = 1 OR exclude_reason IS NOT NULL)
);

-- ==================== ⑥ biz_consol_rule ====================
CREATE TABLE IF NOT EXISTS biz_consol_rule (
    id                 UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    group_org_id       UUID NOT NULL REFERENCES biz_org(id) ON DELETE CASCADE,
    rule_code          VARCHAR(40)  NOT NULL,
    rule_name          VARCHAR(120) NOT NULL,
    rule_type          VARCHAR(24)  NOT NULL,
    side_a_account     VARCHAR(120) NOT NULL,
    side_b_account     VARCHAR(120) NOT NULL,
    debit_account_code VARCHAR(30)  NOT NULL,
    credit_account_code VARCHAR(30) NOT NULL,
    tolerance_amount   NUMERIC(20,4) NOT NULL DEFAULT 0.01,
    is_system          SMALLINT NOT NULL DEFAULT 0,
    status             SMALLINT NOT NULL DEFAULT 1,
    sort_order         INT      NOT NULL DEFAULT 0,
    remark             TEXT,
    created_at         TIMESTAMPTZ NOT NULL DEFAULT CURRENT_TIMESTAMP,
    updated_at         TIMESTAMPTZ NOT NULL DEFAULT CURRENT_TIMESTAMP,
    create_by          UUID,
    update_by          UUID,
    deleted_at         TIMESTAMPTZ,
    CONSTRAINT ck_biz_consol_rule_type CHECK (rule_type IN ('IC_AR_AP','IC_REV_COST','IC_CASH')),
    CONSTRAINT ck_biz_consol_rule_sys  CHECK (is_system IN (0,1)),
    CONSTRAINT ck_biz_consol_rule_stat CHECK (status IN (0,1)),
    CONSTRAINT ck_biz_consol_rule_tol  CHECK (tolerance_amount >= 0)
);
CREATE UNIQUE INDEX IF NOT EXISTS uk_biz_consol_rule
    ON biz_consol_rule (group_org_id, rule_code) WHERE deleted_at IS NULL;
CREATE INDEX IF NOT EXISTS idx_biz_consol_rule_group ON biz_consol_rule (group_org_id, status, sort_order);

-- ==================== ⑦ biz_consol_offset ====================
CREATE TABLE IF NOT EXISTS biz_consol_offset (
    id            UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    worksheet_id  UUID NOT NULL REFERENCES biz_consol_worksheet(id) ON DELETE CASCADE,
    line_no       INT  NOT NULL,
    rule_id       UUID REFERENCES biz_consol_rule(id) ON DELETE SET NULL,
    source_type   VARCHAR(8) NOT NULL DEFAULT 'AUTO',
    from_org_id   UUID,
    to_org_id     UUID,
    side_a_amount NUMERIC(20,4) NOT NULL DEFAULT 0,
    side_b_amount NUMERIC(20,4) NOT NULL DEFAULT 0,
    offset_amount NUMERIC(20,4) NOT NULL DEFAULT 0,
    diff_amount   NUMERIC(20,4) NOT NULL DEFAULT 0,
    account_code  VARCHAR(30)  NOT NULL,
    account_name  VARCHAR(120) NOT NULL,
    debit_amount  NUMERIC(20,4) NOT NULL DEFAULT 0,
    credit_amount NUMERIC(20,4) NOT NULL DEFAULT 0,
    reason        VARCHAR(500),
    created_at    TIMESTAMPTZ NOT NULL DEFAULT CURRENT_TIMESTAMP,
    create_by     UUID,
    CONSTRAINT uk_biz_consol_offset_line UNIQUE (worksheet_id, line_no),
    CONSTRAINT ck_biz_consol_offset_src  CHECK (source_type IN ('AUTO','MANUAL')),
    CONSTRAINT ck_biz_consol_offset_side  CHECK ((debit_amount = 0 AND credit_amount > 0)
                                              OR (debit_amount > 0 AND credit_amount = 0)),
    CONSTRAINT ck_biz_consol_offset_nonneg CHECK (side_a_amount >= 0 AND side_b_amount >= 0
                                              AND offset_amount >= 0 AND diff_amount >= 0),
    CONSTRAINT ck_biz_consol_offset_reason CHECK (source_type = 'AUTO' OR reason IS NOT NULL),
    CONSTRAINT ck_biz_consol_offset_rule   CHECK (source_type = 'MANUAL' OR rule_id IS NOT NULL)
);
CREATE INDEX IF NOT EXISTS idx_biz_consol_offset_ws   ON biz_consol_offset (worksheet_id, line_no);
CREATE INDEX IF NOT EXISTS idx_biz_consol_offset_pair ON biz_consol_offset (from_org_id, to_org_id);
CREATE INDEX IF NOT EXISTS idx_biz_consol_offset_acct ON biz_consol_offset (account_code);

-- ==================== 种子：默认公司 + 3 条系统抵消规则 ====================
INSERT INTO biz_org (id, org_code, org_name, short_name, parent_id, org_path, org_level,
                     is_legal_entity, is_group_root, base_currency, status, sort_order, remark)
VALUES ('00000000-0000-4000-8000-000000000001'::uuid, 'HQ001', '云枢总部', '总部', NULL,
        '/00000000-0000-4000-8000-000000000001/', 1, 1, 1, 'CNY', 'ACTIVE', 10,
        'ARCH-9 存量数据承接主体')
ON CONFLICT (id) DO NOTHING;

INSERT INTO biz_consol_rule (group_org_id, rule_code, rule_name, rule_type,
                             side_a_account, side_b_account,
                             debit_account_code, credit_account_code, is_system, sort_order)
VALUES
 ('00000000-0000-4000-8000-000000000001'::uuid, 'IC_AR_AP',   '内部往来-应收应付', 'IC_AR_AP',
  '1122,1221', '2202,2241', '2202', '1122', 1, 10),
 ('00000000-0000-4000-8000-000000000001'::uuid, 'IC_REV_COST','内部购销-收入成本', 'IC_REV_COST',
  '6001', '6401', '6401', '6001', 1, 20),
 ('00000000-0000-4000-8000-000000000001'::uuid, 'IC_CASH',    '内部资金往来', 'IC_CASH',
  '1123,2203', '1123,2203', '2203', '1123', 1, 30);

-- 幂等性：种子重放不报错
INSERT INTO biz_consol_rule (group_org_id, rule_code, rule_name, rule_type,
                             side_a_account, side_b_account,
                             debit_account_code, credit_account_code, is_system, sort_order)
SELECT '00000000-0000-4000-8000-000000000001'::uuid, 'IC_AR_AP', '内部往来-应收应付', 'IC_AR_AP',
       '1122,1221', '2202,2241', '2202', '1122', 1, 10
 WHERE NOT EXISTS (SELECT 1 FROM biz_consol_rule
                    WHERE group_org_id='00000000-0000-4000-8000-000000000001'::uuid
                      AND rule_code='IC_AR_AP' AND deleted_at IS NULL);

-- ==================== 约束齿口测试（每条必须被拒）====================
DO $$
DECLARE
    g UUID := '00000000-0000-4000-8000-000000000001';
    ws UUID;
    a  UUID := gen_random_uuid();
    rule UUID;
    n  INT := 0;
    u  UUID := gen_random_uuid();
BEGIN
    -- T1 status 非法
    BEGIN
        INSERT INTO biz_org (org_code, org_name, status) VALUES ('X1','X','BAD');
        RAISE EXCEPTION 'T1 失败：非法 status 被接受';
    EXCEPTION WHEN check_violation THEN n := n + 1;
    END;
    -- T2 管理节点带信用代码
    BEGIN
        INSERT INTO biz_org (org_code, org_name, is_legal_entity, credit_code)
        VALUES ('X2','X',0,'91310000XXXXXXXXXX');
        RAISE EXCEPTION 'T2 失败：管理节点带 credit_code 被接受';
    EXCEPTION WHEN check_violation THEN n := n + 1;
    END;
    -- T3 底稿借贷不等
    BEGIN
        INSERT INTO biz_consol_worksheet (group_org_id, period_code, status, offset_debit, offset_credit)
        VALUES (g,'2026-11','GENERATED',100,90);
        RAISE EXCEPTION 'T3 失败：offset_debit<>offset_credit 被接受';
    EXCEPTION WHEN check_violation THEN n := n + 1;
    END;
    -- 建一份合法底稿供后续用
    INSERT INTO biz_consol_worksheet (group_org_id, period_code, status, member_count, offset_debit, offset_credit)
    VALUES (g,'2026-11','GENERATED',2,100,100) RETURNING id INTO ws;

    -- T4 同(主体,期间)第二份未作废底稿
    BEGIN
        INSERT INTO biz_consol_worksheet (group_org_id, period_code, status)
        VALUES (g,'2026-11','DRAFT');
        RAISE EXCEPTION 'T4 失败：重复底稿被接受';
    EXCEPTION WHEN unique_violation THEN n := n + 1;
    END;
    -- T4b CANCELLED 允许
    INSERT INTO biz_consol_worksheet (group_org_id, period_code, status) VALUES (g,'2026-11','CANCELLED');
    n := n + 1;

    -- T5 期间格式/月份越界
    BEGIN
        INSERT INTO biz_consol_worksheet (group_org_id, period_code) VALUES (g,'2026-13');
        RAISE EXCEPTION 'T5 失败：非法 period_code 被接受';
    EXCEPTION WHEN check_violation THEN n := n + 1;
    END;

    -- T6 MANUAL 行无 reason
    SELECT id INTO rule FROM biz_consol_rule WHERE rule_code='IC_AR_AP' AND group_org_id=g;
    BEGIN
        INSERT INTO biz_consol_offset (worksheet_id, line_no, source_type, account_code, account_name, debit_amount)
        VALUES (ws,1,'MANUAL','2202','应付账款',10);
        RAISE EXCEPTION 'T6 失败：MANUAL 无 reason 被接受';
    EXCEPTION WHEN check_violation THEN n := n + 1;
    END;
    -- T7 双边同非零
    BEGIN
        INSERT INTO biz_consol_offset (worksheet_id, line_no, rule_id, account_code, account_name, debit_amount, credit_amount)
        VALUES (ws,2,rule,'2202','应付账款',10,10);
        RAISE EXCEPTION 'T7 失败：双边非零被接受';
    EXCEPTION WHEN check_violation THEN n := n + 1;
    END;
    -- T8 AUTO 行无 rule_id
    BEGIN
        INSERT INTO biz_consol_offset (worksheet_id, line_no, source_type, account_code, account_name, credit_amount)
        VALUES (ws,3,'AUTO','1122','应收账款',10);
        RAISE EXCEPTION 'T8 失败：AUTO 无 rule_id 被接受';
    EXCEPTION WHEN check_violation THEN n := n + 1;
    END;
    -- 合法 AUTO 行
    INSERT INTO biz_consol_offset (worksheet_id, line_no, rule_id, source_type, from_org_id, to_org_id,
                                   side_a_amount, side_b_amount, offset_amount, diff_amount,
                                   account_code, account_name, credit_amount)
    VALUES (ws,4,rule,'AUTO',a,u,990,1000,990,10,'1122','应收账款',990);
    n := n + 1;

    -- T9 included=0 无 reason
    BEGIN
        INSERT INTO biz_consol_scope (worksheet_id, member_org_id, included) VALUES (ws,u,0);
        RAISE EXCEPTION 'T9 失败：included=0 无原因被接受';
    EXCEPTION WHEN check_violation THEN n := n + 1;
    END;

    -- T10 同一用户两个默认公司（换一家公司，撞 uk_biz_org_user_default）
    INSERT INTO sys_user (id) VALUES (u);          -- 先让 FK 成立
    INSERT INTO biz_org (org_code, org_name, org_path) VALUES ('BJ001','北京公司','/x/bj/');
    INSERT INTO biz_org_user (org_id, user_id, is_default) VALUES (g, u, 1);
    BEGIN
        INSERT INTO biz_org_user (org_id, user_id, is_default)
        SELECT id, u, 1 FROM biz_org WHERE org_code='BJ001';
        RAISE EXCEPTION 'T10 失败：同一用户两个默认公司被接受';
    EXCEPTION WHEN unique_violation THEN n := n + 1;
    END;

    RAISE NOTICE 'ARCH-9 DDL 齿口测试通过：% 条断言', n;
END $$;

-- 汇总
SELECT 'biz_org' t, count(*) FROM biz_org
UNION ALL SELECT 'biz_consol_rule', count(*) FROM biz_consol_rule
UNION ALL SELECT 'biz_consol_worksheet', count(*) FROM biz_consol_worksheet
UNION ALL SELECT 'biz_consol_offset', count(*) FROM biz_consol_offset;

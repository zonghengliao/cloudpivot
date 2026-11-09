-- ARCH-9-2 人力资源模块：新增 10 表 DDL 校验（整段事务 + ROLLBACK，零残留）
-- 复现：PGPASSWORD=alun psql -h 127.0.0.1 -U alun -d cloudpivot_test -v ON_ERROR_STOP=1 \
--         -f docs/evidence/2026-11-10-arch9-2-hr-ddl/01_new_tables_check.sql
BEGIN;
CREATE SCHEMA arch92_hr;
SET search_path TO arch92_hr;

-- ========== ① 员工主档 ==========
CREATE TABLE biz_employee (
    id                    UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    org_id                UUID NOT NULL,
    emp_no                VARCHAR(32)  NOT NULL,
    name                  VARCHAR(50)  NOT NULL,
    gender                SMALLINT     NOT NULL DEFAULT 0,
    birth_date            DATE,
    id_card               VARCHAR(18),
    id_card_type          VARCHAR(16)  NOT NULL DEFAULT 'ID_CARD',
    mobile                VARCHAR(20),
    email                 VARCHAR(100),
    dept_id               UUID         NOT NULL,
    position_code         VARCHAR(32),
    job_level             VARCHAR(20),
    employment_type       VARCHAR(16)  NOT NULL DEFAULT 'FULL_TIME',
    hire_date             DATE         NOT NULL,
    probation_end_date    DATE,
    leave_date            DATE,
    leave_type            VARCHAR(16),
    status                VARCHAR(16)  NOT NULL DEFAULT 'PROBATION',
    education             VARCHAR(16),
    school                VARCHAR(100),
    major                 VARCHAR(100),
    graduate_date         DATE,
    native_place          VARCHAR(100),
    address               VARCHAR(200),
    emergency_contact     VARCHAR(50),
    emergency_phone       VARCHAR(20),
    emergency_relation    VARCHAR(20),
    bank_name             VARCHAR(100),
    bank_branch           VARCHAR(200),
    bank_account          VARCHAR(40),
    bank_account_name     VARCHAR(50),
    user_id               UUID,
    remark                TEXT,
    created_at            TIMESTAMPTZ  NOT NULL DEFAULT CURRENT_TIMESTAMP,
    updated_at            TIMESTAMPTZ  NOT NULL DEFAULT CURRENT_TIMESTAMP,
    create_by             UUID,
    update_by             UUID,
    deleted_at            TIMESTAMPTZ,
    CONSTRAINT ck_biz_employee_gender    CHECK (gender IN (0, 1, 2)),
    CONSTRAINT ck_biz_employee_idtype    CHECK (id_card_type IN ('ID_CARD', 'PASSPORT', 'HK_MO_TW', 'FOREIGN')),
    CONSTRAINT ck_biz_employee_emptype   CHECK (employment_type IN ('FULL_TIME','PROBATION','INTERN','OUTSOURCE','PART_TIME','DISPATCH')),
    CONSTRAINT ck_biz_employee_status    CHECK (status IN ('PROBATION','ACTIVE','SUSPENDED','RESIGNED')),
    CONSTRAINT ck_biz_employee_leavetype CHECK (leave_type IS NULL OR leave_type IN ('RESIGN','DISMISS','RETIRE','CONTRACT_END','DEATH')),
    CONSTRAINT ck_biz_employee_leave     CHECK ((status = 'RESIGNED') = (leave_date IS NOT NULL)),
    CONSTRAINT ck_biz_employee_leavetype2 CHECK (status <> 'RESIGNED' OR leave_type IS NOT NULL),
    CONSTRAINT ck_biz_employee_dates     CHECK (leave_date IS NULL OR leave_date >= hire_date),
    CONSTRAINT ck_biz_employee_probation CHECK (probation_end_date IS NULL OR probation_end_date >= hire_date),
    CONSTRAINT ck_biz_employee_idcard_len CHECK (id_card IS NULL OR length(id_card) BETWEEN 6 AND 18)
);

CREATE UNIQUE INDEX uk_biz_employee_no ON biz_employee (org_id, emp_no) WHERE deleted_at IS NULL;
CREATE UNIQUE INDEX uk_biz_employee_user ON biz_employee (user_id) WHERE user_id IS NOT NULL AND deleted_at IS NULL;
CREATE UNIQUE INDEX uk_biz_employee_idcard ON biz_employee (org_id, id_card) WHERE id_card IS NOT NULL AND deleted_at IS NULL;
CREATE INDEX idx_biz_employee_dept   ON biz_employee (org_id, dept_id, status) WHERE deleted_at IS NULL;
CREATE INDEX idx_biz_employee_status ON biz_employee (org_id, status) WHERE deleted_at IS NULL;
CREATE INDEX idx_biz_employee_hire   ON biz_employee (org_id, hire_date DESC) WHERE deleted_at IS NULL;

-- ========== ② 劳动合同 ==========
CREATE TABLE biz_employee_contract (
    id                UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    org_id            UUID NOT NULL,
    employee_id       UUID NOT NULL REFERENCES biz_employee(id) ON DELETE RESTRICT,
    contract_no       VARCHAR(32) NOT NULL,
    contract_type     VARCHAR(16) NOT NULL DEFAULT 'FIXED_TERM',
    sign_date         DATE        NOT NULL,
    start_date        DATE        NOT NULL,
    end_date          DATE,
    probation_months  SMALLINT    NOT NULL DEFAULT 0,
    status            VARCHAR(16) NOT NULL DEFAULT 'ACTIVE',
    terminated_date   DATE,
    terminate_reason  VARCHAR(255),
    renewed_from_id   UUID REFERENCES biz_employee_contract(id) ON DELETE SET NULL,
    remark            TEXT,
    created_at        TIMESTAMPTZ NOT NULL DEFAULT CURRENT_TIMESTAMP,
    updated_at        TIMESTAMPTZ NOT NULL DEFAULT CURRENT_TIMESTAMP,
    create_by         UUID,
    update_by         UUID,
    deleted_at        TIMESTAMPTZ,
    CONSTRAINT ck_biz_emp_contract_type   CHECK (contract_type IN ('FIXED_TERM','UNFIXED_TERM','INTERN','LABOR_SERVICE','DISPATCH')),
    CONSTRAINT ck_biz_emp_contract_status CHECK (status IN ('ACTIVE','EXPIRED','TERMINATED','RENEWED')),
    CONSTRAINT ck_biz_emp_contract_range  CHECK (end_date IS NULL OR end_date >= start_date),
    CONSTRAINT ck_biz_emp_contract_sign   CHECK (sign_date <= start_date),
    CONSTRAINT ck_biz_emp_contract_prob   CHECK (probation_months BETWEEN 0 AND 6),
    CONSTRAINT ck_biz_emp_contract_term   CHECK ((status = 'TERMINATED') = (terminated_date IS NOT NULL)),
    CONSTRAINT ck_biz_emp_contract_unfixed CHECK (contract_type <> 'UNFIXED_TERM' OR end_date IS NULL)
);

CREATE UNIQUE INDEX uk_biz_emp_contract_no ON biz_employee_contract (org_id, contract_no) WHERE deleted_at IS NULL;
CREATE INDEX idx_biz_emp_contract_emp   ON biz_employee_contract (employee_id, start_date DESC) WHERE deleted_at IS NULL;
CREATE INDEX idx_biz_emp_contract_due   ON biz_employee_contract (org_id, end_date) WHERE status = 'ACTIVE' AND deleted_at IS NULL;

-- ========== ③ 员工薪资标准（版本化） ==========
CREATE TABLE biz_employee_salary (
    id              UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    org_id          UUID NOT NULL,
    employee_id     UUID NOT NULL REFERENCES biz_employee(id) ON DELETE RESTRICT,
    item_code       VARCHAR(32)   NOT NULL,
    amount          NUMERIC(18,4) NOT NULL,
    effective_from  DATE          NOT NULL,
    effective_to    DATE,
    remark          VARCHAR(200),
    created_at      TIMESTAMPTZ   NOT NULL DEFAULT CURRENT_TIMESTAMP,
    updated_at      TIMESTAMPTZ   NOT NULL DEFAULT CURRENT_TIMESTAMP,
    create_by       UUID,
    update_by       UUID,
    deleted_at      TIMESTAMPTZ,
    CONSTRAINT ck_biz_emp_salary_amount CHECK (amount >= 0),
    CONSTRAINT ck_biz_emp_salary_range  CHECK (effective_to IS NULL OR effective_to >= effective_from)
);

CREATE UNIQUE INDEX uk_biz_emp_salary
    ON biz_employee_salary (employee_id, item_code, effective_from) WHERE deleted_at IS NULL;
CREATE INDEX idx_biz_emp_salary_emp ON biz_employee_salary (employee_id, effective_from DESC) WHERE deleted_at IS NULL;

-- ========== ④ 考勤期间（月度批次） ==========
CREATE TABLE biz_attendance_period (
    id                    UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    org_id                UUID NOT NULL,
    period_code           VARCHAR(7)   NOT NULL,
    period_start          DATE         NOT NULL,
    period_end            DATE         NOT NULL,
    status                VARCHAR(16)  NOT NULL DEFAULT 'DRAFT',
    employee_count        INT          NOT NULL DEFAULT 0,
    should_days_total     NUMERIC(12,2) NOT NULL DEFAULT 0,
    actual_days_total     NUMERIC(12,2) NOT NULL DEFAULT 0,
    ot_hours_total        NUMERIC(12,2) NOT NULL DEFAULT 0,
    import_task_id        UUID,
    submitted_at          TIMESTAMPTZ,
    submitted_by          UUID,
    locked_at             TIMESTAMPTZ,
    locked_by             UUID,
    remark                TEXT,
    created_at            TIMESTAMPTZ  NOT NULL DEFAULT CURRENT_TIMESTAMP,
    updated_at            TIMESTAMPTZ  NOT NULL DEFAULT CURRENT_TIMESTAMP,
    create_by             UUID,
    update_by             UUID,
    deleted_at            TIMESTAMPTZ,
    CONSTRAINT ck_biz_att_period_code   CHECK (period_code ~ '^[0-9]{4}-(0[1-9]|1[0-2])$'),
    CONSTRAINT ck_biz_att_period_range  CHECK (period_end >= period_start),
    CONSTRAINT ck_biz_att_period_status CHECK (status IN ('DRAFT','SUBMITTED','LOCKED')),
    CONSTRAINT ck_biz_att_period_counts CHECK (employee_count >= 0 AND should_days_total >= 0
                                           AND actual_days_total >= 0 AND ot_hours_total >= 0)
);

CREATE UNIQUE INDEX uk_biz_att_period ON biz_attendance_period (org_id, period_code) WHERE deleted_at IS NULL;

-- ========== ⑤ 考勤记录（一人一月一行） ==========
CREATE TABLE biz_attendance_record (
    id                  UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    org_id              UUID NOT NULL,
    period_id           UUID NOT NULL REFERENCES biz_attendance_period(id) ON DELETE CASCADE,
    employee_id         UUID NOT NULL REFERENCES biz_employee(id) ON DELETE RESTRICT,
    employee_no         VARCHAR(32)  NOT NULL,
    employee_name       VARCHAR(50)  NOT NULL,
    dept_id             UUID,
    should_days         NUMERIC(6,2) NOT NULL DEFAULT 0,
    actual_days         NUMERIC(6,2) NOT NULL DEFAULT 0,
    leave_paid_days     NUMERIC(6,2) NOT NULL DEFAULT 0,
    leave_unpaid_days   NUMERIC(6,2) NOT NULL DEFAULT 0,
    sick_days           NUMERIC(6,2) NOT NULL DEFAULT 0,
    absent_days         NUMERIC(6,2) NOT NULL DEFAULT 0,
    ot_hours_workday    NUMERIC(8,2) NOT NULL DEFAULT 0,
    ot_hours_weekend    NUMERIC(8,2) NOT NULL DEFAULT 0,
    ot_hours_holiday    NUMERIC(8,2) NOT NULL DEFAULT 0,
    late_count          INT          NOT NULL DEFAULT 0,
    early_leave_count   INT          NOT NULL DEFAULT 0,
    source              VARCHAR(16)  NOT NULL DEFAULT 'IMPORT',
    remark              TEXT,
    created_at          TIMESTAMPTZ  NOT NULL DEFAULT CURRENT_TIMESTAMP,
    updated_at          TIMESTAMPTZ  NOT NULL DEFAULT CURRENT_TIMESTAMP,
    create_by           UUID,
    update_by           UUID,
    deleted_at          TIMESTAMPTZ,
    CONSTRAINT ck_biz_att_rec_nonneg CHECK (should_days >= 0 AND actual_days >= 0
        AND leave_paid_days >= 0 AND leave_unpaid_days >= 0 AND sick_days >= 0 AND absent_days >= 0
        AND ot_hours_workday >= 0 AND ot_hours_weekend >= 0 AND ot_hours_holiday >= 0
        AND late_count >= 0 AND early_leave_count >= 0),
    CONSTRAINT ck_biz_att_rec_source CHECK (source IN ('IMPORT','MANUAL','LEAVE_SYNC'))
);

CREATE UNIQUE INDEX uk_biz_att_rec ON biz_attendance_record (period_id, employee_id) WHERE deleted_at IS NULL;
CREATE INDEX idx_biz_att_rec_emp ON biz_attendance_record (employee_id, period_id);

-- ========== ⑥ 请假单 ==========
CREATE TABLE biz_leave_request (
    id                    UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    org_id                UUID NOT NULL,
    leave_no              VARCHAR(32) NOT NULL,
    employee_id           UUID NOT NULL REFERENCES biz_employee(id) ON DELETE RESTRICT,
    employee_no           VARCHAR(32) NOT NULL,
    employee_name         VARCHAR(50) NOT NULL,
    dept_id               UUID,
    leave_type            VARCHAR(16) NOT NULL,
    start_time            TIMESTAMPTZ NOT NULL,
    end_time              TIMESTAMPTZ NOT NULL,
    duration_days         NUMERIC(6,2) NOT NULL,
    paid                  SMALLINT    NOT NULL DEFAULT 1,
    status                VARCHAR(16) NOT NULL DEFAULT 'DRAFT',
    reason                VARCHAR(500),
    approval_instance_id  UUID,
    submitted_at          TIMESTAMPTZ,
    submitted_by          UUID,
    cancelled_at          TIMESTAMPTZ,
    cancelled_by          UUID,
    cancel_reason         VARCHAR(255),
    remark                TEXT,
    created_at            TIMESTAMPTZ NOT NULL DEFAULT CURRENT_TIMESTAMP,
    updated_at            TIMESTAMPTZ NOT NULL DEFAULT CURRENT_TIMESTAMP,
    create_by             UUID,
    update_by             UUID,
    deleted_at            TIMESTAMPTZ,
    CONSTRAINT ck_biz_leave_type   CHECK (leave_type IN ('ANNUAL','SICK','PERSONAL','MARRIAGE','MATERNITY','PATERNITY','BEREAVEMENT','COMPENSATORY','OTHER')),
    CONSTRAINT ck_biz_leave_status CHECK (status IN ('DRAFT','SUBMITTED','APPROVED','CANCELLED')),
    CONSTRAINT ck_biz_leave_range  CHECK (end_time > start_time),
    CONSTRAINT ck_biz_leave_days   CHECK (duration_days > 0),
    CONSTRAINT ck_biz_leave_paid   CHECK (paid IN (0, 1)),
    CONSTRAINT ck_biz_leave_cancel CHECK (status <> 'CANCELLED' OR cancel_reason IS NOT NULL)
);

CREATE UNIQUE INDEX uk_biz_leave_no ON biz_leave_request (org_id, leave_no) WHERE deleted_at IS NULL;
CREATE INDEX idx_biz_leave_emp  ON biz_leave_request (employee_id, start_time DESC) WHERE deleted_at IS NULL;
CREATE INDEX idx_biz_leave_stat ON biz_leave_request (org_id, status) WHERE deleted_at IS NULL;

-- ========== ⑦ 薪资项定义（公式配置） ==========
CREATE TABLE biz_salary_item (
    id                    UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    org_id                UUID NOT NULL,
    item_code             VARCHAR(32) NOT NULL,
    item_name             VARCHAR(50) NOT NULL,
    item_type             VARCHAR(16) NOT NULL,
    calc_type             VARCHAR(16) NOT NULL,
    source                VARCHAR(24) NOT NULL DEFAULT 'INPUT',
    formula               VARCHAR(500),
    sort_order            INT         NOT NULL DEFAULT 10,
    debit_account_code    VARCHAR(30),
    credit_account_code   VARCHAR(30),
    is_taxable            SMALLINT    NOT NULL DEFAULT 1,
    is_active             SMALLINT    NOT NULL DEFAULT 1,
    is_system             SMALLINT    NOT NULL DEFAULT 0,
    remark                VARCHAR(500),
    created_at            TIMESTAMPTZ NOT NULL DEFAULT CURRENT_TIMESTAMP,
    updated_at            TIMESTAMPTZ NOT NULL DEFAULT CURRENT_TIMESTAMP,
    create_by             UUID,
    update_by             UUID,
    deleted_at            TIMESTAMPTZ,
    CONSTRAINT ck_biz_sal_item_type    CHECK (item_type IN ('EARNING','PRE_DEDUCT','WITHHOLD','EMPLOYER','SUMMARY')),
    CONSTRAINT ck_biz_sal_item_calc    CHECK (calc_type IN ('FIXED','FORMULA','INPUT')),
    CONSTRAINT ck_biz_sal_item_source  CHECK (source IN ('EMPLOYEE_STANDARD','ATTENDANCE','INPUT','SYSTEM','MES')),
    CONSTRAINT ck_biz_sal_item_formula CHECK (calc_type <> 'FORMULA' OR formula IS NOT NULL),
    CONSTRAINT ck_biz_sal_item_negflag CHECK (is_taxable IN (0,1) AND is_active IN (0,1) AND is_system IN (0,1)),
    CONSTRAINT ck_biz_sal_item_account CHECK (
        (item_type IN ('EARNING','PRE_DEDUCT','EMPLOYER') AND debit_account_code IS NOT NULL AND credit_account_code IS NOT NULL)
        OR (item_type = 'WITHHOLD' AND credit_account_code IS NOT NULL)
        OR (item_type = 'SUMMARY' AND formula IS NOT NULL)
    )
);

CREATE UNIQUE INDEX uk_biz_sal_item_code ON biz_salary_item (org_id, item_code) WHERE deleted_at IS NULL;
CREATE UNIQUE INDEX uk_biz_sal_item_sort ON biz_salary_item (org_id, sort_order) WHERE deleted_at IS NULL;

-- ========== ⑧ 薪资单（一头） ==========
CREATE TABLE biz_salary_sheet (
    id                    UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    org_id                UUID NOT NULL,
    sheet_no              VARCHAR(32) NOT NULL,
    period_code           VARCHAR(7)  NOT NULL,
    period_start          DATE        NOT NULL,
    period_end            DATE        NOT NULL,
    status                VARCHAR(16) NOT NULL DEFAULT 'DRAFT',
    attendance_period_id  UUID REFERENCES biz_attendance_period(id) ON DELETE RESTRICT,
    employee_count        INT           NOT NULL DEFAULT 0,
    earning_total         NUMERIC(20,4) NOT NULL DEFAULT 0,
    pre_deduct_total      NUMERIC(20,4) NOT NULL DEFAULT 0,
    gross_total           NUMERIC(20,4) NOT NULL DEFAULT 0,
    withhold_total        NUMERIC(20,4) NOT NULL DEFAULT 0,
    net_total             NUMERIC(20,4) NOT NULL DEFAULT 0,
    si_company_total      NUMERIC(20,4) NOT NULL DEFAULT 0,
    hf_company_total      NUMERIC(20,4) NOT NULL DEFAULT 0,
    accrual_voucher_id    UUID,
    accrual_voucher_no    VARCHAR(32),
    payment_voucher_id    UUID,
    payment_voucher_no    VARCHAR(32),
    reversal_voucher_id   UUID,
    approval_instance_id  UUID,
    calculated_at         TIMESTAMPTZ,
    calculated_by         UUID,
    submitted_at          TIMESTAMPTZ,
    submitted_by          UUID,
    posted_at             TIMESTAMPTZ,
    posted_by             UUID,
    reversed_at           TIMESTAMPTZ,
    reversed_by           UUID,
    reverse_reason        VARCHAR(255),
    remark                TEXT,
    created_at            TIMESTAMPTZ NOT NULL DEFAULT CURRENT_TIMESTAMP,
    updated_at            TIMESTAMPTZ NOT NULL DEFAULT CURRENT_TIMESTAMP,
    create_by             UUID,
    update_by             UUID,
    deleted_at            TIMESTAMPTZ,
    CONSTRAINT ck_biz_sal_sheet_code   CHECK (period_code ~ '^[0-9]{4}-(0[1-9]|1[0-2])$'),
    CONSTRAINT ck_biz_sal_sheet_range  CHECK (period_end >= period_start),
    CONSTRAINT ck_biz_sal_sheet_status CHECK (status IN ('DRAFT','CALCULATED','SUBMITTED','APPROVED','POSTED','REVERSED')),
    CONSTRAINT ck_biz_sal_sheet_idem   CHECK (gross_total = net_total + withhold_total),
    CONSTRAINT ck_biz_sal_sheet_gross  CHECK (gross_total = earning_total - pre_deduct_total),
    CONSTRAINT ck_biz_sal_sheet_nonneg CHECK (earning_total >= 0 AND pre_deduct_total >= 0
                                            AND gross_total >= 0 AND withhold_total >= 0 AND net_total >= 0
                                            AND si_company_total >= 0 AND hf_company_total >= 0)
);

CREATE UNIQUE INDEX uk_biz_sal_sheet_no     ON biz_salary_sheet (org_id, sheet_no) WHERE deleted_at IS NULL;
CREATE UNIQUE INDEX uk_biz_sal_sheet_period ON biz_salary_sheet (org_id, period_code) WHERE deleted_at IS NULL;

-- ========== ⑨ 薪资明细（一人一行汇总） ==========
CREATE TABLE biz_salary_detail (
    id                  UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    org_id              UUID NOT NULL,
    sheet_id            UUID NOT NULL REFERENCES biz_salary_sheet(id) ON DELETE CASCADE,
    employee_id         UUID NOT NULL REFERENCES biz_employee(id) ON DELETE RESTRICT,
    employee_no         VARCHAR(32) NOT NULL,
    employee_name       VARCHAR(50) NOT NULL,
    dept_id             UUID,
    dept_name           VARCHAR(100),
    user_id             UUID,
    position_code       VARCHAR(32),
    job_level           VARCHAR(20),
    should_days         NUMERIC(6,2)  NOT NULL DEFAULT 0,
    actual_days         NUMERIC(6,2)  NOT NULL DEFAULT 0,
    leave_unpaid_days   NUMERIC(6,2)  NOT NULL DEFAULT 0,
    ot_hours            NUMERIC(8,2)  NOT NULL DEFAULT 0,
    earning_amount      NUMERIC(18,4) NOT NULL DEFAULT 0,
    pre_deduct_amount   NUMERIC(18,4) NOT NULL DEFAULT 0,
    gross_amount        NUMERIC(18,4) NOT NULL DEFAULT 0,
    social_personal     NUMERIC(18,4) NOT NULL DEFAULT 0,
    fund_personal       NUMERIC(18,4) NOT NULL DEFAULT 0,
    iit_amount          NUMERIC(18,4) NOT NULL DEFAULT 0,
    other_withhold      NUMERIC(18,4) NOT NULL DEFAULT 0,
    withhold_amount     NUMERIC(18,4) NOT NULL DEFAULT 0,
    net_amount          NUMERIC(18,4) NOT NULL DEFAULT 0,
    si_company          NUMERIC(18,4) NOT NULL DEFAULT 0,
    hf_company          NUMERIC(18,4) NOT NULL DEFAULT 0,
    remark              VARCHAR(500),
    created_at          TIMESTAMPTZ NOT NULL DEFAULT CURRENT_TIMESTAMP,
    updated_at          TIMESTAMPTZ NOT NULL DEFAULT CURRENT_TIMESTAMP,
    CONSTRAINT ck_biz_sal_det_gross   CHECK (gross_amount = earning_amount - pre_deduct_amount),
    CONSTRAINT ck_biz_sal_det_withold CHECK (withhold_amount = social_personal + fund_personal + iit_amount + other_withhold),
    CONSTRAINT ck_biz_sal_det_net     CHECK (gross_amount = net_amount + withhold_amount),
    CONSTRAINT ck_biz_sal_det_nonneg  CHECK (earning_amount >= 0 AND pre_deduct_amount >= 0 AND gross_amount >= 0
                                           AND social_personal >= 0 AND fund_personal >= 0 AND iit_amount >= 0
                                           AND other_withhold >= 0 AND withhold_amount >= 0 AND net_amount >= 0
                                           AND si_company >= 0 AND hf_company >= 0)
);

CREATE UNIQUE INDEX uk_biz_sal_det ON biz_salary_detail (sheet_id, employee_id);
CREATE INDEX idx_biz_sal_det_emp  ON biz_salary_detail (employee_id, sheet_id);
CREATE INDEX idx_biz_sal_det_user ON biz_salary_detail (user_id) WHERE user_id IS NOT NULL;
CREATE INDEX idx_biz_sal_det_dept ON biz_salary_detail (org_id, dept_id);

-- ========== ⑩ 薪资明细项目（公式解释层） ==========
CREATE TABLE biz_salary_detail_item (
    id             UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    org_id         UUID NOT NULL,
    sheet_id       UUID NOT NULL REFERENCES biz_salary_sheet(id) ON DELETE CASCADE,
    detail_id      UUID NOT NULL REFERENCES biz_salary_detail(id) ON DELETE CASCADE,
    item_id        UUID REFERENCES biz_salary_item(id) ON DELETE SET NULL,
    item_code      VARCHAR(32) NOT NULL,
    item_name      VARCHAR(50) NOT NULL,
    item_type      VARCHAR(16) NOT NULL,
    calc_type      VARCHAR(16) NOT NULL,
    amount         NUMERIC(18,4) NOT NULL DEFAULT 0,
    expr_snapshot  VARCHAR(500),
    calc_note      VARCHAR(500),
    line_no        INT NOT NULL DEFAULT 1,
    created_at     TIMESTAMPTZ NOT NULL DEFAULT CURRENT_TIMESTAMP,
    CONSTRAINT ck_biz_sal_ditem_type CHECK (item_type IN ('EARNING','PRE_DEDUCT','WITHHOLD','EMPLOYER','SUMMARY')),
    CONSTRAINT ck_biz_sal_ditem_calc CHECK (calc_type IN ('FIXED','FORMULA','INPUT')),
    CONSTRAINT uk_biz_sal_ditem UNIQUE (detail_id, item_code)
);

CREATE INDEX idx_biz_sal_ditem_sheet ON biz_salary_detail_item (sheet_id, item_code);
CREATE INDEX idx_biz_sal_ditem_item  ON biz_salary_detail_item (item_id);

-- ============================================================
-- 断言（成对：该拒的拒 + 该放的放），末尾 RAISE NOTICE 报告通过条数
-- ============================================================
DO $$
DECLARE
    ok int := 0;
    org_a uuid := '00000000-0000-4000-8000-00000000000a';
    org_b uuid := '00000000-0000-4000-8000-00000000000b';
    dept  uuid := gen_random_uuid();
    emp1  uuid := gen_random_uuid();
    emp2  uuid := gen_random_uuid();
    usr1  uuid := gen_random_uuid();
    sheet uuid := gen_random_uuid();
    det   uuid := gen_random_uuid();
    per   uuid := gen_random_uuid();
BEGIN
    -- ① 员工：正常插入（无账号 + 有账号）都放行
    INSERT INTO biz_employee (id, org_id, emp_no, name, dept_id, hire_date, status, leave_date, leave_type)
    VALUES (emp1, org_a, 'ARCH92-0001', '张三', dept, DATE '2024-03-01', 'ACTIVE', NULL, NULL);
    INSERT INTO biz_employee (id, org_id, emp_no, name, dept_id, hire_date, user_id, status)
    VALUES (emp2, org_a, 'ARCH92-0002', '李四', dept, DATE '2025-01-06', usr1, 'PROBATION');
    ok := ok + 1;

    -- ② 同 org 重复工号：拒
    BEGIN
        INSERT INTO biz_employee (org_id, emp_no, name, dept_id, hire_date) VALUES (org_a, 'ARCH92-0001', 'X', dept, DATE '2024-01-01');
        RAISE EXCEPTION '断言失败：同公司重复工号应被拒';
    EXCEPTION WHEN unique_violation THEN ok := ok + 1; END;

    -- ③ 不同 org 同工号：放（多公司）
    INSERT INTO biz_employee (org_id, emp_no, name, dept_id, hire_date) VALUES (org_b, 'ARCH92-0001', '王五', dept, DATE '2024-01-01');
    ok := ok + 1;

    -- ④ 同一账号挂两个员工档案：拒（一人一档）
    BEGIN
        INSERT INTO biz_employee (org_id, emp_no, name, dept_id, hire_date, user_id) VALUES (org_a, 'ARCH92-0003', 'X2', dept, DATE '2024-01-01', usr1);
        RAISE EXCEPTION '断言失败：同一 user_id 挂两档应被拒';
    EXCEPTION WHEN unique_violation THEN ok := ok + 1; END;

    -- ⑤ 同 org 重复身份证：拒
    UPDATE biz_employee SET id_card = '110101199001010011' WHERE id = emp1;
    BEGIN
        INSERT INTO biz_employee (org_id, emp_no, name, dept_id, hire_date, id_card)
        VALUES (org_a, 'ARCH92-0004', 'X3', dept, DATE '2024-01-01', '110101199001010011');
        RAISE EXCEPTION '断言失败：同公司重复身份证应被拒';
    EXCEPTION WHEN unique_violation THEN ok := ok + 1; END;

    -- ⑥ 离职必须有 leave_date；有 leave_date 必须是 RESIGNED（双向）
    BEGIN
        INSERT INTO biz_employee (org_id, emp_no, name, dept_id, hire_date, status, leave_type)
        VALUES (org_a, 'ARCH92-0005', 'X4', dept, DATE '2024-01-01', 'RESIGNED', 'RESIGN');
        RAISE EXCEPTION '断言失败：RESIGNED 无 leave_date 应被拒';
    EXCEPTION WHEN check_violation THEN ok := ok + 1; END;
    BEGIN
        UPDATE biz_employee SET status = 'RESIGNED' WHERE id = emp1;  -- 无 leave_date
        RAISE EXCEPTION '断言失败：RESIGNED 无 leave_date 应被拒（UPDATE 路径）';
    EXCEPTION WHEN check_violation THEN ok := ok + 1; END;

    -- ⑦ 离职日期早于入职：拒
    BEGIN
        UPDATE biz_employee SET status='RESIGNED', leave_date=DATE '2020-01-01', leave_type='RESIGN' WHERE id = emp1;
        RAISE EXCEPTION '断言失败：leave_date < hire_date 应被拒';
    EXCEPTION WHEN check_violation THEN ok := ok + 1; END;

    -- ⑧ 状态域外值：拒
    BEGIN
        UPDATE biz_employee SET status = 'FIRED' WHERE id = emp1;
        RAISE EXCEPTION '断言失败：状态域外值应被拒';
    EXCEPTION WHEN check_violation THEN ok := ok + 1; END;

    -- ⑨ org_id 为 NULL：拒（硬隔离维度不许缺）
    BEGIN
        INSERT INTO biz_employee (org_id, emp_no, name, dept_id, hire_date) VALUES (NULL, 'ARCH92-0006', 'X5', dept, DATE '2024-01-01');
        RAISE EXCEPTION '断言失败：org_id NULL 应被拒';
    EXCEPTION WHEN not_null_violation THEN ok := ok + 1; END;

    -- ⑩ 合同：end < start 拒；正常放；TERMINATED 必须有 terminated_date
    INSERT INTO biz_employee_contract (org_id, employee_id, contract_no, sign_date, start_date, end_date)
    VALUES (org_a, emp2, 'HT-ARCH92-01', DATE '2025-01-01', DATE '2025-01-06', DATE '2028-01-05');
    ok := ok + 1;
    BEGIN
        INSERT INTO biz_employee_contract (org_id, employee_id, contract_no, sign_date, start_date, end_date)
        VALUES (org_a, emp2, 'HT-ARCH92-02', DATE '2025-01-01', DATE '2025-01-06', DATE '2024-01-05');
        RAISE EXCEPTION '断言失败：合同 end<start 应被拒';
    EXCEPTION WHEN check_violation THEN ok := ok + 1; END;
    BEGIN
        INSERT INTO biz_employee_contract (org_id, employee_id, contract_no, sign_date, start_date, status)
        VALUES (org_a, emp2, 'HT-ARCH92-03', DATE '2025-01-01', DATE '2025-01-06', 'TERMINATED');
        RAISE EXCEPTION '断言失败：TERMINATED 无 terminated_date 应被拒';
    EXCEPTION WHEN check_violation THEN ok := ok + 1; END;

    -- ⑪ 薪资标准：同日同项重复拒；区间倒置拒；负数拒
    INSERT INTO biz_employee_salary (org_id, employee_id, item_code, amount, effective_from)
    VALUES (org_a, emp2, 'BASE', 8000, DATE '2025-01-06');
    ok := ok + 1;
    BEGIN
        INSERT INTO biz_employee_salary (org_id, employee_id, item_code, amount, effective_from)
        VALUES (org_a, emp2, 'BASE', 9000, DATE '2025-01-06');
        RAISE EXCEPTION '断言失败：同员工同项同生效日应被拒';
    EXCEPTION WHEN unique_violation THEN ok := ok + 1; END;
    BEGIN
        INSERT INTO biz_employee_salary (org_id, employee_id, item_code, amount, effective_from, effective_to)
        VALUES (org_a, emp2, 'POST', 500, DATE '2025-06-01', DATE '2025-05-01');
        RAISE EXCEPTION '断言失败：生效区间倒置应被拒';
    EXCEPTION WHEN check_violation THEN ok := ok + 1; END;
    BEGIN
        INSERT INTO biz_employee_salary (org_id, employee_id, item_code, amount, effective_from)
        VALUES (org_a, emp2, 'POST', -1, DATE '2025-07-01');
        RAISE EXCEPTION '断言失败：负薪资标准应被拒';
    EXCEPTION WHEN check_violation THEN ok := ok + 1; END;

    -- ⑫ 考勤期间：2026-13 拒（正则），2026-12 放；同 org 重复拒
    BEGIN
        INSERT INTO biz_attendance_period (org_id, period_code, period_start, period_end)
        VALUES (org_a, '2026-13', DATE '2026-12-01', DATE '2026-12-31');
        RAISE EXCEPTION '断言失败：2026-13 应被拒';
    EXCEPTION WHEN check_violation THEN ok := ok + 1; END;
    INSERT INTO biz_attendance_period (id, org_id, period_code, period_start, period_end)
    VALUES (per, org_a, '2026-12', DATE '2026-12-01', DATE '2026-12-31');
    ok := ok + 1;
    BEGIN
        INSERT INTO biz_attendance_period (org_id, period_code, period_start, period_end)
        VALUES (org_a, '2026-12', DATE '2026-12-01', DATE '2026-12-31');
        RAISE EXCEPTION '断言失败：同公司同考勤期间重复应被拒';
    EXCEPTION WHEN unique_violation THEN ok := ok + 1; END;

    -- ⑬ 考勤记录：负天数拒；同期间同人重复拒；期间删除级联
    INSERT INTO biz_attendance_record (org_id, period_id, employee_id, employee_no, employee_name,
                                       should_days, actual_days, leave_unpaid_days, ot_hours_workday)
    VALUES (org_a, per, emp2, 'ARCH92-0002', '李四', 22, 20, 2, 10);
    ok := ok + 1;
    BEGIN
        INSERT INTO biz_attendance_record (org_id, period_id, employee_id, employee_no, employee_name, absent_days)
        VALUES (org_a, per, emp1, 'ARCH92-0001', '张三', -1);
        RAISE EXCEPTION '断言失败：负天数应被拒';
    EXCEPTION WHEN check_violation THEN ok := ok + 1; END;
    BEGIN
        INSERT INTO biz_attendance_record (org_id, period_id, employee_id, employee_no, employee_name)
        VALUES (org_a, per, emp2, 'ARCH92-0002', '李四');
        RAISE EXCEPTION '断言失败：同期间同员工重复应被拒';
    EXCEPTION WHEN unique_violation THEN ok := ok + 1; END;

    -- ⑭ 请假单：end<=start 拒；CANCELLED 无原因拒
    INSERT INTO biz_leave_request (org_id, leave_no, employee_id, employee_no, employee_name, leave_type,
                                   start_time, end_time, duration_days, paid)
    VALUES (org_a, 'LV-ARCH92-01', emp2, 'ARCH92-0002', '李四', 'PERSONAL',
            TIMESTAMPTZ '2026-12-03 09:00+08', TIMESTAMPTZ '2026-12-03 18:00+08', 1, 0);
    ok := ok + 1;
    BEGIN
        INSERT INTO biz_leave_request (org_id, leave_no, employee_id, employee_no, employee_name, leave_type,
                                       start_time, end_time, duration_days)
        VALUES (org_a, 'LV-ARCH92-02', emp2, 'ARCH92-0002', '李四', 'SICK',
                TIMESTAMPTZ '2026-12-04 18:00+08', TIMESTAMPTZ '2026-12-04 09:00+08', 1);
        RAISE EXCEPTION '断言失败：请假 end<=start 应被拒';
    EXCEPTION WHEN check_violation THEN ok := ok + 1; END;
    BEGIN
        UPDATE biz_leave_request SET status = 'CANCELLED' WHERE leave_no = 'LV-ARCH92-01';
        RAISE EXCEPTION '断言失败：CANCELLED 无原因应被拒';
    EXCEPTION WHEN check_violation THEN ok := ok + 1; END;

    -- ⑮ 薪资项：FORMULA 无公式拒；EARNING 缺科目拒；同 org 重复 item_code 拒；sort 冲突拒
    INSERT INTO biz_salary_item (org_id, item_code, item_name, item_type, calc_type, source, formula,
                                 debit_account_code, credit_account_code, sort_order)
    VALUES (org_a, 'ATT_BONUS', '全勤奖', 'EARNING', 'FORMULA', 'ATTENDANCE',
            'IF({ATT_ABSENT_DAYS} == 0, 200, 0)', '5602.10', '2211.01', 60);
    ok := ok + 1;
    BEGIN
        INSERT INTO biz_salary_item (org_id, item_code, item_name, item_type, calc_type, debit_account_code, credit_account_code)
        VALUES (org_a, 'BAD1', '公式缺失', 'EARNING', 'FORMULA', '5602.10', '2211.01');
        RAISE EXCEPTION '断言失败：FORMULA 无公式应被拒';
    EXCEPTION WHEN check_violation THEN ok := ok + 1; END;
    BEGIN
        INSERT INTO biz_salary_item (org_id, item_code, item_name, item_type, calc_type)
        VALUES (org_a, 'BAD2', '科目缺失', 'EARNING', 'FIXED');
        RAISE EXCEPTION '断言失败：EARNING 缺科目应被拒';
    EXCEPTION WHEN check_violation THEN ok := ok + 1; END;
    BEGIN
        INSERT INTO biz_salary_item (org_id, item_code, item_name, item_type, calc_type, debit_account_code, credit_account_code, sort_order)
        VALUES (org_a, 'ATT_BONUS', '重复编码', 'EARNING', 'FIXED', '5602.10', '2211.01', 70);
        RAISE EXCEPTION '断言失败：同公司重复薪资项编码应被拒';
    EXCEPTION WHEN unique_violation THEN ok := ok + 1; END;
    BEGIN
        INSERT INTO biz_salary_item (org_id, item_code, item_name, item_type, calc_type, debit_account_code, credit_account_code, sort_order)
        VALUES (org_a, 'ATT_BONUS2', '排序冲突', 'EARNING', 'FIXED', '5602.10', '2211.01', 60);
        RAISE EXCEPTION '断言失败：同公司 sort_order 冲突应被拒';
    EXCEPTION WHEN unique_violation THEN ok := ok + 1; END;
    -- WITHHOLD 项：只要求贷方科目，借方为空放行
    INSERT INTO biz_salary_item (org_id, item_code, item_name, item_type, calc_type, source, credit_account_code, sort_order)
    VALUES (org_a, 'IIT', '个人所得税', 'WITHHOLD', 'INPUT', 'INPUT', '2221.03', 130);
    ok := ok + 1;

    -- ⑯ 薪资单：恒等式破坏拒；同 org 同期间重复拒；status 域外拒
    INSERT INTO biz_salary_sheet (id, org_id, sheet_no, period_code, period_start, period_end,
                                  earning_total, pre_deduct_total, gross_total, withhold_total, net_total,
                                  si_company_total, hf_company_total)
    VALUES (sheet, org_a, 'SAL-ARCH92-01', '2026-12', DATE '2026-12-01', DATE '2026-12-31',
            10689.66, 735.63, 9954.03, 1800.00, 8154.03, 1680.00, 960.00);
    ok := ok + 1;
    BEGIN
        INSERT INTO biz_salary_sheet (org_id, sheet_no, period_code, period_start, period_end,
                                      earning_total, pre_deduct_total, gross_total, withhold_total, net_total)
        VALUES (org_a, 'SAL-ARCH92-02', '2027-01', DATE '2027-01-01', DATE '2027-01-31',
                1000, 0, 1000, 100, 800);   -- 1000 <> 800 + 100
        RAISE EXCEPTION '断言失败：应发 ≠ 实发 + 代扣 应被拒';
    EXCEPTION WHEN check_violation THEN ok := ok + 1; END;
    BEGIN
        INSERT INTO biz_salary_sheet (org_id, sheet_no, period_code, period_start, period_end, gross_total, net_total, withhold_total)
        VALUES (org_a, 'SAL-ARCH92-03', '2026-12', DATE '2026-12-01', DATE '2026-12-31', 0, 0, 0);
        RAISE EXCEPTION '断言失败：同公司同期间重复薪资单应被拒';
    EXCEPTION WHEN unique_violation THEN ok := ok + 1; END;
    BEGIN
        UPDATE biz_salary_sheet SET status = 'PAID' WHERE id = sheet;
        RAISE EXCEPTION '断言失败：薪资单状态域外值应被拒';
    EXCEPTION WHEN check_violation THEN ok := ok + 1; END;

    -- ⑰ 薪资明细：恒等式破坏拒；一人两行拒；员工被硬删拒（RESTRICT）
    INSERT INTO biz_salary_detail (id, org_id, sheet_id, employee_id, employee_no, employee_name, dept_id, dept_name, user_id,
                                   should_days, actual_days, leave_unpaid_days, ot_hours,
                                   earning_amount, pre_deduct_amount, gross_amount,
                                   social_personal, fund_personal, iit_amount, other_withhold, withhold_amount,
                                   net_amount, si_company, hf_company)
    VALUES (det, org_a, sheet, emp2, 'ARCH92-0002', '李四', dept, '测试部', usr1,
            22, 20, 2, 10, 10689.66, 735.63, 9954.03, 840.00, 960.00, 0, 0, 1800.00, 8154.03, 1680.00, 960.00);
    ok := ok + 1;
    BEGIN
        INSERT INTO biz_salary_detail (org_id, sheet_id, employee_id, employee_no, employee_name,
                                       earning_amount, pre_deduct_amount, gross_amount,
                                       social_personal, withhold_amount, net_amount)
        VALUES (org_a, sheet, emp2, 'ARCH92-0002', '李四', 1000, 0, 1000, 100, 100, 900);
        RAISE EXCEPTION '断言失败：同一薪资单同一员工两行应被拒';
    EXCEPTION WHEN unique_violation THEN ok := ok + 1; END;
    BEGIN
        INSERT INTO biz_salary_detail (org_id, sheet_id, employee_id, employee_no, employee_name,
                                       earning_amount, pre_deduct_amount, gross_amount,
                                       social_personal, fund_personal, iit_amount, other_withhold, withhold_amount,
                                       net_amount)
        VALUES (org_a, sheet, emp1, 'ARCH92-0001', '张三', 1000, 0, 1000, 50, 50, 0, 0, 100, 950);  -- 1000 <> 950+100
        RAISE EXCEPTION '断言失败：明细 应发 ≠ 实发 + 代扣 应被拒';
    EXCEPTION WHEN check_violation THEN ok := ok + 1; END;
    BEGIN
        DELETE FROM biz_employee WHERE id = emp2;
        RAISE EXCEPTION '断言失败：员工被薪资明细引用时不许硬删';
    EXCEPTION WHEN foreign_key_violation OR restrict_violation THEN ok := ok + 1; END;

    -- ⑱ 明细项目：同员工同项重复拒；负数放行（补发/冲回）
    INSERT INTO biz_salary_detail_item (org_id, sheet_id, detail_id, item_code, item_name, item_type, calc_type, amount, line_no)
    VALUES (org_a, sheet, det, 'BASE', '基本工资', 'EARNING', 'FIXED', 8000, 10);
    INSERT INTO biz_salary_detail_item (org_id, sheet_id, detail_id, item_code, item_name, item_type, calc_type, amount, line_no)
    VALUES (org_a, sheet, det, 'PERF', '绩效奖金', 'EARNING', 'INPUT', -200, 20);
    ok := ok + 2;
    BEGIN
        INSERT INTO biz_salary_detail_item (org_id, sheet_id, detail_id, item_code, item_name, item_type, calc_type, amount)
        VALUES (org_a, sheet, det, 'BASE', '基本工资', 'EARNING', 'FIXED', 8000);
        RAISE EXCEPTION '断言失败：同明细同薪资项重复应被拒';
    EXCEPTION WHEN unique_violation THEN ok := ok + 1; END;

    -- ⑲ 级联：删薪资单 → 明细与明细项一起消失（不留孤儿）
    DELETE FROM biz_salary_sheet WHERE id = sheet;
    IF EXISTS (SELECT 1 FROM biz_salary_detail WHERE id = det) THEN
        RAISE EXCEPTION '断言失败：删薪资单后明细未级联删除';
    END IF;
    IF EXISTS (SELECT 1 FROM biz_salary_detail_item WHERE detail_id = det) THEN
        RAISE EXCEPTION '断言失败：删薪资单后明细项未级联删除';
    END IF;
    ok := ok + 1;

    -- ⑳ 删考勤期间 → 考勤记录级联消失
    DELETE FROM biz_attendance_period WHERE id = per;
    IF EXISTS (SELECT 1 FROM biz_attendance_record WHERE period_id = per) THEN
        RAISE EXCEPTION '断言失败：删考勤期间后记录未级联删除';
    END IF;
    ok := ok + 1;

    RAISE NOTICE 'ARCH-9-2 新表断言通过 % 条', ok;
    IF ok <> 45 THEN
        RAISE EXCEPTION '断言条数不符：期望 45，实际 %（有断言被删或被跳过，不许静默通过）', ok;
    END IF;
END $$;

SELECT 'ARCH-9-2 新表 DDL 校验完成（即将 ROLLBACK，零残留）' AS result;
ROLLBACK;


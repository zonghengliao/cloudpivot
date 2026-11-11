-- ============================================================================
-- V0383：人力资源模块底座 · 10 张表 + 索引 + 注释 + 末尾自检（ARCH-9-2 §3.1）
--
-- 卡：t_65a3dc14（HR-01【HR】迁移 V0378）
-- 依据：docs/architecture/ARCH-9-人力资源模块设计.md
--       §3.1（10 张表完整 DDL，真库跑通过：docs/evidence/2026-11-10-arch9-2-hr-ddl/
--             01_new_tables_check.sql → 45 条断言 RC=0）、§4（业务规则与巡检断言）、
--       §7.1（迁移文件清单）、§7.3（回滚方案）
--
-- # 号段为什么是 V0383，而不是卡面 / 设计稿 §7.1 写的 V0378
--   设计稿给人力资源预留 V0378~V0388，但成本核算（ARCH-9 成本）**先落地并已提交**，
--   V0378~V0382 五个号已被实占：
--     V0378__cost_menu_and_perm / V0379__cost_seq_and_dict / V0380__cost_lot_backfill
--     V0381__cost_flow_def / V0382__stock_in_production_sourceless
--   nik 的迁移加载器对版本号做**跨目录全局唯一**校验（loader.rs「版本号跨目录全局唯一」），
--   重号 ⇒ 服务启动直接失败（AGENTS.md 约定 6：动手前先 ls 核对号段）。
--   故 **HR 全系列整体后移 5 号**：
--     V0378→V0383 hr_tables（本卡）
--     V0379→V0384 hr_accounts（HR-02）      V0380→V0385 hr_seq_dict（HR-03）
--     V0381→V0386 hr_menu_perms（HR-04）    V0382→V0387 hr_mask_config（HR-05）
--     V0383→V0388 hr_attachment_doc_types（HR-05）
--     V0384→V0389 hr_leave_flow / V0385→V0390 hr_salary_flow /
--     V0386→V0391 hr_adjust_resign_flow / V0387→V0392 hr_salary_items_seed /
--     V0388→V0393 hr_selfcheck
--   偏差已写进卡内汇报，并回填到设计文档 §7.1 的号段说明。
--
-- # 本卡范围（只建新表，不改任何存量表）
--   10 张表：biz_employee / biz_employee_contract / biz_employee_salary /
--            biz_attendance_period / biz_attendance_record / biz_leave_request /
--            biz_salary_item / biz_salary_sheet / biz_salary_detail /
--            biz_salary_detail_item
--   以及它们的索引 + 注释 + 本文件末尾的自检断言。
--   科目 / 号段 / 字典 / 菜单 / 脱敏 / 配置 / 附件 CHECK 放宽 / 审批流 / 薪资项种子
--   各有自己的卡（HR-02~HR-05、HR-26/38 等），**不在本文件里**；本迁移不出现
--   ALTER TABLE / DROP / TRUNCATE，不与任何存量表改造卡撞车。
--
-- # 幂等（重复执行结果不变）
--   建表 / 建索引一律 IF NOT EXISTS；注释用 COMMENT ON（覆盖式，重复执行同值）；
--   自检探针行写在固定 probe 键上，断言后立即删除 ⇒ 重复执行零残留。
--
-- # 自检为什么要写进迁移（不是可选项）
--   本仓迁移「文件内不写 BEGIN/COMMIT、由迁移器开事务」：自检失败 ⇒ 整笔回滚 ⇒
--   不存在「表建了一半」。要拦的是**不报错但会算错账**的静默错误：
--     · 唯一键漏了 `WHERE deleted_at IS NULL` ⇒ 软删后同工号/同期间再也建不出来；
--     · CHECK 少一条 ⇒ 一条脏数据入口（离职无日期、薪资单三方恒等式不成立…）；
--     · 金额列写窄成 NUMERIC(18,2) ⇒ 第 3/4 位小数被四舍五入，工资表对不平；
--     · 外键方向错（明细不级联）⇒ 删单头留孤儿行。
--   分两段：A 形态（to_regclass / pg_indexes / pg_constraint / information_schema）、
--           B 行为（真插探针行验证「该拒的拒、该放的放」，含级联删除）。
--
-- # 与「姓名：快照 vs 实时 join」（AGENTS.md 约定 13）的关系
--   审计/历史类存快照：attendance_record.employee_no/employee_name、
--                       leave_request.employee_no/employee_name、
--                       salary_detail.employee_no/employee_name/dept_name；
--   主数据类 join 实时取名：列表/详情的部门名、岗位名（biz_employee 只存 dept_id）。
--
-- # 回滚
--   见 V0383__hr_tables.down.sql：有业务数据（未删员工 / 已过账薪资单）就拒绝回滚，
--   否则按 FK 逆序 DROP，并断言零残留。
-- ============================================================================

-- ----------------------------------------------------------------------------
-- ① biz_employee —— 员工主档（人事维度，与登录账号 sys_user 是 1:0..1）
-- ----------------------------------------------------------------------------
CREATE TABLE IF NOT EXISTS biz_employee (
    id                    UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    org_id                UUID NOT NULL,                    -- 所属公司（ARCH-9-1 硬隔离维度）
    emp_no                VARCHAR(32)  NOT NULL,            -- 工号（EMP-0001，公司内唯一，软删可复用）
    name                  VARCHAR(50)  NOT NULL,            -- 姓名（主数据：改名后所有单据显示新名）
    gender                SMALLINT     NOT NULL DEFAULT 0,  -- 0 未知 / 1 男 / 2 女
    birth_date            DATE,
    id_card               VARCHAR(18),                      -- 身份证号（出参按 sys_field_mask 脱敏）
    id_card_type          VARCHAR(16)  NOT NULL DEFAULT 'ID_CARD',
    mobile                VARCHAR(20),
    email                 VARCHAR(100),
    dept_id               UUID         NOT NULL,            -- 人事归属部门（sys_dept.id，弱引用不建 FK）
    position_code         VARCHAR(32),                      -- 岗位（字典 hr_position）
    job_level             VARCHAR(20),                      -- 职级（字典 hr_job_level）
    employment_type       VARCHAR(16)  NOT NULL DEFAULT 'FULL_TIME',
    hire_date             DATE         NOT NULL,            -- 入职日期
    probation_end_date    DATE,                             -- 试用期截止（转正提醒用）
    leave_date            DATE,                             -- 离职日期（status=RESIGNED 时必填，双向 CHECK）
    leave_type            VARCHAR(16),
    status                VARCHAR(16)  NOT NULL DEFAULT 'PROBATION',
    education             VARCHAR(16),                      -- 学历（字典 hr_education）
    school                VARCHAR(100),
    major                 VARCHAR(100),
    graduate_date         DATE,
    native_place          VARCHAR(100),
    address               VARCHAR(200),
    emergency_contact     VARCHAR(50),                      -- 紧急联系人
    emergency_phone       VARCHAR(20),
    emergency_relation    VARCHAR(20),
    bank_name             VARCHAR(100),                     -- 工资卡开户行
    bank_branch           VARCHAR(200),
    bank_account          VARCHAR(40),                      -- 工资卡号（出参脱敏）
    bank_account_name     VARCHAR(50),
    user_id               UUID,                             -- ★ 关联登录账号（可空、唯一；见 §4.1.1）
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
    -- 离职语义双向：有 leave_date ⇔ 状态是 RESIGNED（防「标了离职但没日期」与「有离职日但还在职」两种脏数据）
    CONSTRAINT ck_biz_employee_leave     CHECK ((status = 'RESIGNED') = (leave_date IS NOT NULL)),
    CONSTRAINT ck_biz_employee_leavetype2 CHECK (status <> 'RESIGNED' OR leave_type IS NOT NULL),
    CONSTRAINT ck_biz_employee_dates     CHECK (leave_date IS NULL OR leave_date >= hire_date),
    CONSTRAINT ck_biz_employee_probation CHECK (probation_end_date IS NULL OR probation_end_date >= hire_date),
    CONSTRAINT ck_biz_employee_idcard_len CHECK (id_card IS NULL OR length(id_card) BETWEEN 6 AND 18)
);

CREATE UNIQUE INDEX IF NOT EXISTS uk_biz_employee_no     ON biz_employee (org_id, emp_no) WHERE deleted_at IS NULL;
CREATE UNIQUE INDEX IF NOT EXISTS uk_biz_employee_user   ON biz_employee (user_id) WHERE user_id IS NOT NULL AND deleted_at IS NULL;
CREATE UNIQUE INDEX IF NOT EXISTS uk_biz_employee_idcard ON biz_employee (org_id, id_card) WHERE id_card IS NOT NULL AND deleted_at IS NULL;
CREATE INDEX        IF NOT EXISTS idx_biz_employee_dept   ON biz_employee (org_id, dept_id, status) WHERE deleted_at IS NULL;
CREATE INDEX        IF NOT EXISTS idx_biz_employee_status ON biz_employee (org_id, status) WHERE deleted_at IS NULL;
CREATE INDEX        IF NOT EXISTS idx_biz_employee_hire   ON biz_employee (org_id, hire_date DESC) WHERE deleted_at IS NULL;

-- ----------------------------------------------------------------------------
-- ② biz_employee_contract —— 劳动合同台账（1:N，链式续签）
-- ----------------------------------------------------------------------------
CREATE TABLE IF NOT EXISTS biz_employee_contract (
    id                UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    org_id            UUID NOT NULL,
    employee_id       UUID NOT NULL REFERENCES biz_employee(id) ON DELETE RESTRICT,
    contract_no       VARCHAR(32) NOT NULL,                 -- 合同编号（公司内唯一，人工填，不取号：必须与纸质一致）
    contract_type     VARCHAR(16) NOT NULL DEFAULT 'FIXED_TERM',
    sign_date         DATE        NOT NULL,                 -- 签订日
    start_date        DATE        NOT NULL,
    end_date          DATE,                                 -- 无固定期限为 NULL
    probation_months  SMALLINT    NOT NULL DEFAULT 0,
    status            VARCHAR(16) NOT NULL DEFAULT 'ACTIVE',
    terminated_date   DATE,
    terminate_reason  VARCHAR(255),
    renewed_from_id   UUID REFERENCES biz_employee_contract(id) ON DELETE SET NULL,  -- 续签链
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
    CONSTRAINT ck_biz_emp_contract_prob   CHECK (probation_months BETWEEN 0 AND 6),   -- 劳动合同法第 19 条上限
    CONSTRAINT ck_biz_emp_contract_term   CHECK ((status = 'TERMINATED') = (terminated_date IS NOT NULL)),
    CONSTRAINT ck_biz_emp_contract_unfixed CHECK (contract_type <> 'UNFIXED_TERM' OR end_date IS NULL)
);

CREATE UNIQUE INDEX IF NOT EXISTS uk_biz_emp_contract_no  ON biz_employee_contract (org_id, contract_no) WHERE deleted_at IS NULL;
CREATE INDEX        IF NOT EXISTS idx_biz_emp_contract_emp ON biz_employee_contract (employee_id, start_date DESC) WHERE deleted_at IS NULL;
CREATE INDEX        IF NOT EXISTS idx_biz_emp_contract_due ON biz_employee_contract (org_id, end_date) WHERE status = 'ACTIVE' AND deleted_at IS NULL;

-- ----------------------------------------------------------------------------
-- ③ biz_employee_salary —— 员工薪资标准（按生效日期版本化，区间不许重叠）
-- ----------------------------------------------------------------------------
CREATE TABLE IF NOT EXISTS biz_employee_salary (
    id              UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    org_id          UUID NOT NULL,
    employee_id     UUID NOT NULL REFERENCES biz_employee(id) ON DELETE RESTRICT,
    item_code       VARCHAR(32)   NOT NULL,                 -- 薪资项编码（弱引用 biz_salary_item.item_code，不建 FK）
    amount          NUMERIC(18,4) NOT NULL,                 -- 标准金额（月标准，正数）
    effective_from  DATE          NOT NULL,                 -- 生效起（含）
    effective_to    DATE,                                   -- 生效止（含），NULL = 至今
    remark          VARCHAR(200),
    created_at      TIMESTAMPTZ   NOT NULL DEFAULT CURRENT_TIMESTAMP,
    updated_at      TIMESTAMPTZ   NOT NULL DEFAULT CURRENT_TIMESTAMP,
    create_by       UUID,
    update_by       UUID,
    deleted_at      TIMESTAMPTZ,
    CONSTRAINT ck_biz_emp_salary_amount CHECK (amount >= 0),
    CONSTRAINT ck_biz_emp_salary_range  CHECK (effective_to IS NULL OR effective_to >= effective_from)
);

CREATE UNIQUE INDEX IF NOT EXISTS uk_biz_emp_salary
    ON biz_employee_salary (employee_id, item_code, effective_from) WHERE deleted_at IS NULL;
CREATE INDEX IF NOT EXISTS idx_biz_emp_salary_emp ON biz_employee_salary (employee_id, effective_from DESC) WHERE deleted_at IS NULL;

-- ----------------------------------------------------------------------------
-- ④ biz_attendance_period —— 月度考勤批次（一公司一月一批）
-- ----------------------------------------------------------------------------
CREATE TABLE IF NOT EXISTS biz_attendance_period (
    id                    UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    org_id                UUID NOT NULL,
    period_code           VARCHAR(7)   NOT NULL,            -- 考勤月 yyyy-MM（与 biz_accounting_period.period_code 同口径）
    period_start          DATE         NOT NULL,            -- 起（含）：支持「26 日~次月 25 日」的计薪周期
    period_end            DATE         NOT NULL,
    status                VARCHAR(16)  NOT NULL DEFAULT 'DRAFT',
    employee_count        INT          NOT NULL DEFAULT 0,  -- 汇总快照（导入/修改后重算）
    should_days_total     NUMERIC(12,2) NOT NULL DEFAULT 0,
    actual_days_total     NUMERIC(12,2) NOT NULL DEFAULT 0,
    ot_hours_total        NUMERIC(12,2) NOT NULL DEFAULT 0,
    import_task_id        UUID,                             -- 最近一次导入的 sys_import_task.id
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
    -- 月度正则必须写 (0[1-9]|1[0-2])：宽正则 ^[0-9]{4}-[0-9]{2}$ 会放过 2026-13
    CONSTRAINT ck_biz_att_period_code   CHECK (period_code ~ '^[0-9]{4}-(0[1-9]|1[0-2])$'),
    CONSTRAINT ck_biz_att_period_range  CHECK (period_end >= period_start),
    CONSTRAINT ck_biz_att_period_status CHECK (status IN ('DRAFT','SUBMITTED','LOCKED')),
    CONSTRAINT ck_biz_att_period_counts CHECK (employee_count >= 0 AND should_days_total >= 0
                                           AND actual_days_total >= 0 AND ot_hours_total >= 0)
);

CREATE UNIQUE INDEX IF NOT EXISTS uk_biz_att_period ON biz_attendance_period (org_id, period_code) WHERE deleted_at IS NULL;

-- ----------------------------------------------------------------------------
-- ⑤ biz_attendance_record —— 考勤记录（一人一月一行，工号/姓名为考勤口径快照）
-- ----------------------------------------------------------------------------
CREATE TABLE IF NOT EXISTS biz_attendance_record (
    id                  UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    org_id              UUID NOT NULL,
    period_id           UUID NOT NULL REFERENCES biz_attendance_period(id) ON DELETE CASCADE,
    employee_id         UUID NOT NULL REFERENCES biz_employee(id) ON DELETE RESTRICT,
    employee_no         VARCHAR(32)  NOT NULL,              -- 工号快照（考勤口径的历史留痕）
    employee_name       VARCHAR(50)  NOT NULL,              -- 姓名快照
    dept_id             UUID,                               -- 归属部门（凭证 aux_dept_id 用）
    should_days         NUMERIC(6,2) NOT NULL DEFAULT 0,    -- 应出勤天数（口径客户定：自然月/21.75/排班）
    actual_days         NUMERIC(6,2) NOT NULL DEFAULT 0,    -- 实际出勤
    leave_paid_days     NUMERIC(6,2) NOT NULL DEFAULT 0,    -- 带薪假（年假/婚丧/产假）
    leave_unpaid_days   NUMERIC(6,2) NOT NULL DEFAULT 0,    -- 无薪假（事假）→ 薪资扣款依据
    sick_days           NUMERIC(6,2) NOT NULL DEFAULT 0,    -- 病假（按比例发，比例在薪资项公式里配）
    absent_days         NUMERIC(6,2) NOT NULL DEFAULT 0,    -- 旷工
    ot_hours_workday    NUMERIC(8,2) NOT NULL DEFAULT 0,    -- 平时加班小时
    ot_hours_weekend    NUMERIC(8,2) NOT NULL DEFAULT 0,    -- 休息日加班小时
    ot_hours_holiday    NUMERIC(8,2) NOT NULL DEFAULT 0,    -- 法定节假日加班小时
    late_count          INT          NOT NULL DEFAULT 0,
    early_leave_count   INT          NOT NULL DEFAULT 0,
    source              VARCHAR(16)  NOT NULL DEFAULT 'IMPORT',
    remark              TEXT,
    created_at          TIMESTAMPTZ  NOT NULL DEFAULT CURRENT_TIMESTAMP,
    updated_at          TIMESTAMPTZ  NOT NULL DEFAULT CURRENT_TIMESTAMP,
    create_by           UUID,
    update_by           UUID,
    deleted_at          TIMESTAMPTZ,
    -- 刻意**没有**「应出勤 = 实出勤 + 各类假 + 旷工」的守恒 CHECK：应出勤口径客户之间不同
    -- （自然月 30/31、固定 21.75、排班制），硬 CHECK 会把合法数据挡在门外；
    -- 改为导入校验器出行级警告 + 巡检断言 W1（§4.9）。
    CONSTRAINT ck_biz_att_rec_nonneg CHECK (should_days >= 0 AND actual_days >= 0
        AND leave_paid_days >= 0 AND leave_unpaid_days >= 0 AND sick_days >= 0 AND absent_days >= 0
        AND ot_hours_workday >= 0 AND ot_hours_weekend >= 0 AND ot_hours_holiday >= 0
        AND late_count >= 0 AND early_leave_count >= 0),
    CONSTRAINT ck_biz_att_rec_source CHECK (source IN ('IMPORT','MANUAL','LEAVE_SYNC'))
);

CREATE UNIQUE INDEX IF NOT EXISTS uk_biz_att_rec ON biz_attendance_record (period_id, employee_id) WHERE deleted_at IS NULL;
CREATE INDEX        IF NOT EXISTS idx_biz_att_rec_emp ON biz_attendance_record (employee_id, period_id);

-- ----------------------------------------------------------------------------
-- ⑥ biz_leave_request —— 请假单（走审批流）
-- ----------------------------------------------------------------------------
CREATE TABLE IF NOT EXISTS biz_leave_request (
    id                    UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    org_id                UUID NOT NULL,
    leave_no              VARCHAR(32) NOT NULL,             -- LV-yyyyMMdd-000001
    employee_id           UUID NOT NULL REFERENCES biz_employee(id) ON DELETE RESTRICT,
    employee_no           VARCHAR(32) NOT NULL,
    employee_name         VARCHAR(50) NOT NULL,
    dept_id               UUID,
    leave_type            VARCHAR(16) NOT NULL,             -- 与字典 hr_leave_type 一一对应
    start_time            TIMESTAMPTZ NOT NULL,             -- 精确到时（半天假用 12:00 切）
    end_time              TIMESTAMPTZ NOT NULL,
    duration_days         NUMERIC(6,2) NOT NULL,            -- 请假天数（0.5 天粒度）
    paid                  SMALLINT    NOT NULL DEFAULT 1,   -- 1 带薪 / 0 无薪（薪资扣款只看这一列）
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

CREATE UNIQUE INDEX IF NOT EXISTS uk_biz_leave_no ON biz_leave_request (org_id, leave_no) WHERE deleted_at IS NULL;
CREATE INDEX        IF NOT EXISTS idx_biz_leave_emp  ON biz_leave_request (employee_id, start_time DESC) WHERE deleted_at IS NULL;
CREATE INDEX        IF NOT EXISTS idx_biz_leave_stat ON biz_leave_request (org_id, status) WHERE deleted_at IS NULL;

-- ----------------------------------------------------------------------------
-- ⑦ biz_salary_item —— 薪资项定义 + 公式（计算顺序 sort_order 公司内唯一）
-- ----------------------------------------------------------------------------
CREATE TABLE IF NOT EXISTS biz_salary_item (
    id                    UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    org_id                UUID NOT NULL,
    item_code             VARCHAR(32) NOT NULL,             -- BASE / POST / PERF / OT / ATT_DEDUCT / SI_PERSONAL / IIT / GROSS / NET …
    item_name             VARCHAR(50) NOT NULL,
    item_type             VARCHAR(16) NOT NULL,             -- EARNING/PRE_DEDUCT/WITHHOLD/EMPLOYER/SUMMARY（语义见 §4.4.1）
    calc_type             VARCHAR(16) NOT NULL,             -- FIXED 取标准 / FORMULA 公式 / INPUT 手工录
    source                VARCHAR(24) NOT NULL DEFAULT 'INPUT',
    formula               VARCHAR(500),                     -- calc_type=FORMULA 时必填（DSL 见 §4.4.2）
    sort_order            INT         NOT NULL DEFAULT 10,  -- 计算顺序（拓扑外的稳定排序，公司内唯一）
    debit_account_code    VARCHAR(30),                      -- 借方科目（EARNING/PRE_DEDUCT/EMPLOYER 必填）
    credit_account_code   VARCHAR(30),                      -- 贷方科目（EARNING/PRE_DEDUCT/EMPLOYER/WITHHOLD 必填）
    is_taxable            SMALLINT    NOT NULL DEFAULT 1,   -- 是否计入个税应税收入（二期自动个税用）
    is_active             SMALLINT    NOT NULL DEFAULT 1,
    is_system             SMALLINT    NOT NULL DEFAULT 0,   -- 系统项（GROSS/NET）不许删
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
    -- 科目要求按类型分开：SUMMARY 不要科目（对账时由具体项汇总），WITHHOLD 只需贷方
    CONSTRAINT ck_biz_sal_item_account CHECK (
        (item_type IN ('EARNING','PRE_DEDUCT','EMPLOYER') AND debit_account_code IS NOT NULL AND credit_account_code IS NOT NULL)
        OR (item_type = 'WITHHOLD' AND credit_account_code IS NOT NULL)
        OR (item_type = 'SUMMARY' AND formula IS NOT NULL)
    )
);

CREATE UNIQUE INDEX IF NOT EXISTS uk_biz_sal_item_code ON biz_salary_item (org_id, item_code) WHERE deleted_at IS NULL;
CREATE UNIQUE INDEX IF NOT EXISTS uk_biz_sal_item_sort ON biz_salary_item (org_id, sort_order) WHERE deleted_at IS NULL;

-- ----------------------------------------------------------------------------
-- ⑧ biz_salary_sheet —— 薪资单（一头；三条恒等式写进库）
-- ----------------------------------------------------------------------------
CREATE TABLE IF NOT EXISTS biz_salary_sheet (
    id                    UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    org_id                UUID NOT NULL,
    sheet_no              VARCHAR(32) NOT NULL,             -- SAL-yyyyMM-000001
    period_code           VARCHAR(7)  NOT NULL,             -- 薪资期间 yyyy-MM
    period_start          DATE        NOT NULL,
    period_end            DATE        NOT NULL,
    status                VARCHAR(16) NOT NULL DEFAULT 'DRAFT',
    attendance_period_id  UUID REFERENCES biz_attendance_period(id) ON DELETE RESTRICT,  -- 数据来源批次
    employee_count        INT           NOT NULL DEFAULT 0,
    earning_total         NUMERIC(20,4) NOT NULL DEFAULT 0, -- Σ EARNING（含正向与负数补发）
    pre_deduct_total      NUMERIC(20,4) NOT NULL DEFAULT 0, -- Σ PRE_DEDUCT（考勤扣款）
    gross_total           NUMERIC(20,4) NOT NULL DEFAULT 0, -- 应发 = earning - pre_deduct
    withhold_total        NUMERIC(20,4) NOT NULL DEFAULT 0, -- 代扣合计 = 社保+公积金个人+个税+其他
    net_total             NUMERIC(20,4) NOT NULL DEFAULT 0, -- 实发 = 应发 - 代扣
    si_company_total      NUMERIC(20,4) NOT NULL DEFAULT 0, -- 单位社保（进费用，不进应发）
    hf_company_total      NUMERIC(20,4) NOT NULL DEFAULT 0, -- 单位公积金
    accrual_voucher_id    UUID,                             -- 计提凭证
    accrual_voucher_no    VARCHAR(32),
    payment_voucher_id    UUID,                             -- 发放凭证
    payment_voucher_no    VARCHAR(32),
    reversal_voucher_id   UUID,                             -- 红冲凭证（POSTED → REVERSED 时写）
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
    -- 三个恒等式写进库：任何绕过引擎的写路径都写不进「算错」的薪资单
    CONSTRAINT ck_biz_sal_sheet_idem   CHECK (gross_total = net_total + withhold_total),
    CONSTRAINT ck_biz_sal_sheet_gross  CHECK (gross_total = earning_total - pre_deduct_total),
    CONSTRAINT ck_biz_sal_sheet_nonneg CHECK (earning_total >= 0 AND pre_deduct_total >= 0
                                            AND gross_total >= 0 AND withhold_total >= 0 AND net_total >= 0
                                            AND si_company_total >= 0 AND hf_company_total >= 0)
);

CREATE UNIQUE INDEX IF NOT EXISTS uk_biz_sal_sheet_no     ON biz_salary_sheet (org_id, sheet_no) WHERE deleted_at IS NULL;
CREATE UNIQUE INDEX IF NOT EXISTS uk_biz_sal_sheet_period ON biz_salary_sheet (org_id, period_code) WHERE deleted_at IS NULL;

-- ----------------------------------------------------------------------------
-- ⑨ biz_salary_detail —— 薪资明细（一人一行汇总；无 deleted_at，随单头级联）
-- ----------------------------------------------------------------------------
CREATE TABLE IF NOT EXISTS biz_salary_detail (
    id                  UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    org_id              UUID NOT NULL,
    sheet_id            UUID NOT NULL REFERENCES biz_salary_sheet(id) ON DELETE CASCADE,
    employee_id         UUID NOT NULL REFERENCES biz_employee(id) ON DELETE RESTRICT,
    employee_no         VARCHAR(32) NOT NULL,               -- 快照：工号（审计口径）
    employee_name       VARCHAR(50) NOT NULL,               -- 快照：姓名
    dept_id             UUID,                               -- 快照：部门（凭证 aux_dept_id / 部门汇总）
    dept_name           VARCHAR(100),                       -- 快照：部门名（部门改名后工资表不漂移）
    user_id             UUID,                               -- ★ 数据权限 SELF 锚点（由 employee.user_id 派生，可空）
    position_code       VARCHAR(32),                        -- 快照：岗位
    job_level           VARCHAR(20),                        -- 快照：职级
    should_days         NUMERIC(6,2)  NOT NULL DEFAULT 0,   -- 考勤快照
    actual_days         NUMERIC(6,2)  NOT NULL DEFAULT 0,
    leave_unpaid_days   NUMERIC(6,2)  NOT NULL DEFAULT 0,
    ot_hours            NUMERIC(8,2)  NOT NULL DEFAULT 0,
    earning_amount      NUMERIC(18,4) NOT NULL DEFAULT 0,   -- Σ EARNING
    pre_deduct_amount   NUMERIC(18,4) NOT NULL DEFAULT 0,   -- Σ PRE_DEDUCT
    gross_amount        NUMERIC(18,4) NOT NULL DEFAULT 0,   -- 应发
    social_personal     NUMERIC(18,4) NOT NULL DEFAULT 0,   -- 社保个人
    fund_personal       NUMERIC(18,4) NOT NULL DEFAULT 0,   -- 公积金个人
    iit_amount          NUMERIC(18,4) NOT NULL DEFAULT 0,   -- 个税
    other_withhold      NUMERIC(18,4) NOT NULL DEFAULT 0,   -- 其他代扣
    withhold_amount     NUMERIC(18,4) NOT NULL DEFAULT 0,   -- 代扣合计
    net_amount          NUMERIC(18,4) NOT NULL DEFAULT 0,   -- 实发
    si_company          NUMERIC(18,4) NOT NULL DEFAULT 0,   -- 单位社保（费用侧）
    hf_company          NUMERIC(18,4) NOT NULL DEFAULT 0,   -- 单位公积金（费用侧）
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

-- 明细行没有 deleted_at（随单头级联）⇒ 唯一键天然是「全量唯一」，不需要软删谓词
CREATE UNIQUE INDEX IF NOT EXISTS uk_biz_sal_det ON biz_salary_detail (sheet_id, employee_id);
CREATE INDEX        IF NOT EXISTS idx_biz_sal_det_emp  ON biz_salary_detail (employee_id, sheet_id);
CREATE INDEX        IF NOT EXISTS idx_biz_sal_det_user ON biz_salary_detail (user_id) WHERE user_id IS NOT NULL;
CREATE INDEX        IF NOT EXISTS idx_biz_sal_det_dept ON biz_salary_detail (org_id, dept_id);

-- ----------------------------------------------------------------------------
-- ⑩ biz_salary_detail_item —— 薪资明细项目（公式解释层）
-- ----------------------------------------------------------------------------
CREATE TABLE IF NOT EXISTS biz_salary_detail_item (
    id             UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    org_id         UUID NOT NULL,
    sheet_id       UUID NOT NULL REFERENCES biz_salary_sheet(id) ON DELETE CASCADE,   -- 冗余：按单查项免 join
    detail_id      UUID NOT NULL REFERENCES biz_salary_detail(id) ON DELETE CASCADE,
    item_id        UUID REFERENCES biz_salary_item(id) ON DELETE SET NULL,            -- 薪资项被删后项快照仍在
    item_code      VARCHAR(32) NOT NULL,
    item_name      VARCHAR(50) NOT NULL,
    item_type      VARCHAR(16) NOT NULL,
    calc_type      VARCHAR(16) NOT NULL,
    amount         NUMERIC(18,4) NOT NULL DEFAULT 0,        -- 允许负数（补发/冲回，见 §4.5.3）
    expr_snapshot  VARCHAR(500),                            -- 计算时公式原文（快照：改公式不改历史）
    calc_note      VARCHAR(500),                            -- 代入值说明，如 "8000/21.75*2 = 735.63"
    line_no        INT NOT NULL DEFAULT 1,
    created_at     TIMESTAMPTZ NOT NULL DEFAULT CURRENT_TIMESTAMP,
    CONSTRAINT ck_biz_sal_ditem_type CHECK (item_type IN ('EARNING','PRE_DEDUCT','WITHHOLD','EMPLOYER','SUMMARY')),
    CONSTRAINT ck_biz_sal_ditem_calc CHECK (calc_type IN ('FIXED','FORMULA','INPUT')),
    CONSTRAINT uk_biz_sal_ditem UNIQUE (detail_id, item_code)
);

CREATE INDEX IF NOT EXISTS idx_biz_sal_ditem_sheet ON biz_salary_detail_item (sheet_id, item_code);
CREATE INDEX IF NOT EXISTS idx_biz_sal_ditem_item  ON biz_salary_detail_item (item_id);

-- ----------------------------------------------------------------------------
-- 注释（幂等：COMMENT ON 是覆盖写，重复执行同值）
-- ----------------------------------------------------------------------------
COMMENT ON TABLE  biz_employee IS '员工主档（人事维度）：与登录账号 sys_user 是 1:0..1 —— 车间工人无账号，外包/机器人有账号非员工';
COMMENT ON COLUMN biz_employee.user_id  IS '关联登录账号 sys_user.id（可空、唯一）。放员工侧而非 sys_user 侧的理由见 ARCH-9-2 §4.1.1';
COMMENT ON COLUMN biz_employee.id_card  IS '身份证号。存明文（导入查重/社保申报需要），出参按 sys_field_mask(mask_type=2) 脱敏，明文需 business:hr:employee:sensitive';

COMMENT ON TABLE  biz_employee_contract IS '劳动合同台账：1:N 版本化台账，附件走 biz_doc_attachment(doc_type=EMPLOYEE_CONTRACT)';
COMMENT ON COLUMN biz_employee_contract.contract_no IS '合同编号：人工填，不取号（必须与纸质一致），公司内唯一';

COMMENT ON TABLE  biz_employee_salary IS '员工薪资标准（版本化）：试算按 effective_from/effective_to 取当期标准，区间不许重叠（巡检 A3）';

COMMENT ON TABLE  biz_attendance_period IS '月度考勤批次：一公司一月一批，锁定后薪资试算才可读（状态机 DRAFT→SUBMITTED→LOCKED）';
COMMENT ON COLUMN biz_attendance_period.period_code IS '考勤月 yyyy-MM，与 biz_accounting_period.period_code 同口径';

COMMENT ON TABLE  biz_attendance_record IS '考勤记录：一人一月一行；employee_no/employee_name 为考勤口径快照（AGENTS.md 约定 13 审计类）';
COMMENT ON COLUMN biz_attendance_record.leave_unpaid_days IS '无薪假天数：薪资扣款（ATT_DEDUCT）的唯一输入，不在库里做应出勤守恒（口径随客户）';

COMMENT ON TABLE  biz_leave_request IS '请假单：走审批流（hr_leave），审批通过后回写考勤批次';
COMMENT ON COLUMN biz_leave_request.paid IS '1 带薪 / 0 无薪：薪资扣款只看这一列（请假类型与带薪与否解耦）';

COMMENT ON TABLE  biz_salary_item IS '薪资项定义 + 公式：sort_order 决定计算顺序（公司内唯一），科目按 item_type 分别必填';
COMMENT ON COLUMN biz_salary_item.sort_order IS '计算顺序：拓扑之外还要一个稳定排序，重复会让同一份数据算出两个结果';
COMMENT ON COLUMN biz_salary_item.is_system IS '系统项（GROSS/NET）不许删；客户端删除接口按此列拦';

COMMENT ON TABLE  biz_salary_sheet IS '薪资单（一头）：三条恒等式写进 CHECK（净发=应发-代扣、应发=收入-税前扣、各项非负）';
COMMENT ON COLUMN biz_salary_sheet.accrual_voucher_id IS '计提凭证：借 费用/贷 2211 应付职工薪酬（source_type=PAYROLL_ACCRUAL）';
COMMENT ON COLUMN biz_salary_sheet.payment_voucher_id IS '发放凭证：借 2211/贷 1002 银行存款（source_type=PAYROLL_PAYMENT）';

COMMENT ON TABLE  biz_salary_detail IS '薪资明细：一人一行汇总，快照工号/姓名/部门名（改主数据不漂移工资表）；无 deleted_at，随单头级联';
COMMENT ON COLUMN biz_salary_detail.user_id IS '数据权限 SELF 锚点：由 employee.user_id 派生（可空），SELF 角色只能看自己这一行';

COMMENT ON TABLE  biz_salary_detail_item IS '薪资明细项目（公式解释层）：expr_snapshot 存计算时公式原文（改公式不改历史），amount 允许负数（补发/冲回）';

-- ----------------------------------------------------------------------------
-- 自检（写进迁移，部署即校验；失败即整笔回滚 ⇒ 不存在「半建好的表」）
--
--   A. 形态：10 张表在 / 12 条唯一索引带软删谓词 / 49 条 CHECK / 12 条外键 /
--            关键金额列精度；明细两表**没有** deleted_at（唯一键设计前提）。
--   B. 行为：探针行真插，验「该拒的拒、该放的放」，含级联删除；探针按 probe 键
--            清理并断言 0 残留（迁移重复执行不攒垃圾行）。
-- ----------------------------------------------------------------------------
DO $$
DECLARE
    v_org   UUID := '00000000-0000-4000-8000-0000000d0383';
    v_dept  UUID := '00000000-0000-4000-8000-0000000d0384';
    v_user  UUID := '00000000-0000-4000-8000-0000000d0385';
    v_tab   TEXT;
    v_idx   TEXT;
    v_def   TEXT;
    v_expect INT;
    v_n     INT;
    v_hit   BOOLEAN;
    v_emp   UUID;
    v_emp2  UUID;
    v_per   UUID;
    v_sheet UUID;
    v_det   UUID;
BEGIN
    -- ===== A1. 10 张表全部在位 =====
    FOREACH v_tab IN ARRAY ARRAY[
        'biz_employee', 'biz_employee_contract', 'biz_employee_salary',
        'biz_attendance_period', 'biz_attendance_record', 'biz_leave_request',
        'biz_salary_item', 'biz_salary_sheet', 'biz_salary_detail', 'biz_salary_detail_item'
    ] LOOP
        IF to_regclass('public.' || v_tab) IS NULL THEN
            RAISE EXCEPTION 'V0383 自检失败：% 未建出来', v_tab;
        END IF;
    END LOOP;

    -- ===== A2. 12 条唯一索引必须是「UNIQUE + 带 deleted_at IS NULL 的部分索引」 =====
    --      漏谓词不会报错，只会让「软删后同工号/同期间重建」变成永不可能（只能改库）。
    FOREACH v_idx IN ARRAY ARRAY[
        'uk_biz_employee_no', 'uk_biz_employee_user', 'uk_biz_employee_idcard',
        'uk_biz_emp_contract_no', 'uk_biz_emp_salary', 'uk_biz_att_period',
        'uk_biz_att_rec', 'uk_biz_leave_no', 'uk_biz_sal_item_code',
        'uk_biz_sal_item_sort', 'uk_biz_sal_sheet_no', 'uk_biz_sal_sheet_period'
    ] LOOP
        SELECT indexdef INTO v_def FROM pg_indexes
         WHERE schemaname = 'public' AND indexname = v_idx;
        IF v_def IS NULL THEN
            RAISE EXCEPTION 'V0383 自检失败：唯一索引 % 不存在', v_idx;
        END IF;
        IF v_def NOT LIKE 'CREATE UNIQUE%' THEN
            RAISE EXCEPTION 'V0383 自检失败：% 不是 UNIQUE 索引：%', v_idx, v_def;
        END IF;
        IF replace(v_def, ' ', '') NOT LIKE '%WHERE%deleted_atISNULL%' THEN
            RAISE EXCEPTION 'V0383 自检失败：% 缺软删条件（软删后无法重建同业务键）：%', v_idx, v_def;
        END IF;
    END LOOP;

    -- 明细两表是级联口径，**必须没有** deleted_at（有的话唯一键就得带谓词，两套口径会打架）
    FOREACH v_tab IN ARRAY ARRAY['biz_salary_detail', 'biz_salary_detail_item'] LOOP
        SELECT count(*) INTO v_n FROM information_schema.columns
         WHERE table_schema = 'public' AND table_name = v_tab AND column_name = 'deleted_at';
        IF v_n <> 0 THEN
            RAISE EXCEPTION 'V0383 自检失败：% 不该有 deleted_at（随单头级联，留痕靠 detail_item.expr_snapshot + biz_status_log）', v_tab;
        END IF;
    END LOOP;

    -- ===== A3. CHECK 约束逐表计数（少一条 = 一条脏数据入口） =====
    FOR v_tab, v_expect IN
        SELECT * FROM (VALUES
            ('biz_employee',           10),
            ('biz_employee_contract',   7),
            ('biz_employee_salary',     2),
            ('biz_attendance_period',   4),
            ('biz_attendance_record',   2),
            ('biz_leave_request',       6),
            ('biz_salary_item',         6),
            ('biz_salary_sheet',        6),
            ('biz_salary_detail',       4),
            ('biz_salary_detail_item',  2)
        ) AS t(tab, expected)
    LOOP
        SELECT count(*) INTO v_n FROM pg_constraint
         WHERE conrelid = ('public.' || v_tab)::regclass AND contype = 'c';
        IF v_n <> v_expect THEN
            RAISE EXCEPTION 'V0383 自检失败：% 的 CHECK 约束应为 % 条，实际 % 条', v_tab, v_expect, v_n;
        END IF;
    END LOOP;

    -- ===== A4. 外键 12 条（级联方向错 = 删单头留孤儿行） =====
    SELECT count(*) INTO v_n FROM pg_constraint
     WHERE contype = 'f' AND connamespace = 'public'::regnamespace
       AND conrelid IN ('public.biz_employee_contract'::regclass, 'public.biz_employee_salary'::regclass,
                        'public.biz_attendance_record'::regclass, 'public.biz_leave_request'::regclass,
                        'public.biz_salary_sheet'::regclass, 'public.biz_salary_detail'::regclass,
                        'public.biz_salary_detail_item'::regclass);
    IF v_n <> 12 THEN
        RAISE EXCEPTION 'V0383 自检失败：10 张表的外键应为 12 条，实际 % 条', v_n;
    END IF;

    -- 明细必须随单头级联删（细节：detail→sheet CASCADE、ditem→sheet/detail CASCADE）
    SELECT confdeltype INTO v_def FROM pg_constraint
     WHERE contype = 'f' AND conrelid = 'public.biz_salary_detail'::regclass
       AND confrelid = 'public.biz_salary_sheet'::regclass;
    IF v_def IS DISTINCT FROM 'c' THEN
        RAISE EXCEPTION 'V0383 自检失败：biz_salary_detail.sheet_id 不是 ON DELETE CASCADE（删单头会留孤儿明细）';
    END IF;

    -- ===== A5. 关键金额列精度（写窄 = 第 3/4 位小数被四舍五入，工资表对不平） =====
    FOREACH v_def IN ARRAY ARRAY[
        'biz_employee_salary|amount|18,4',
        'biz_salary_sheet|earning_total|20,4',
        'biz_salary_sheet|net_total|20,4',
        'biz_salary_detail|gross_amount|18,4',
        'biz_salary_detail|net_amount|18,4',
        'biz_salary_detail_item|amount|18,4'
    ] LOOP
        SELECT numeric_precision || ',' || numeric_scale INTO v_idx
          FROM information_schema.columns
         WHERE table_schema = 'public'
           AND table_name  = split_part(v_def, '|', 1)
           AND column_name = split_part(v_def, '|', 2);
        IF v_idx IS DISTINCT FROM split_part(v_def, '|', 3) THEN
            RAISE EXCEPTION 'V0383 自检失败：%.% 的金额精度应为 NUMERIC(%)，实际 NUMERIC(%)',
                split_part(v_def, '|', 1), split_part(v_def, '|', 2),
                split_part(v_def, '|', 3), coalesce(v_idx, '缺列');
        END IF;
    END LOOP;

    -- ===== B. 行为（探针行，断言后按 probe 键清理） =====
    -- B1 「离职语义双向」：RESIGNED 无离职日必须被拒
    v_hit := false;
    BEGIN
        INSERT INTO biz_employee (org_id, emp_no, name, dept_id, hire_date, status)
        VALUES (v_org, 'SELFCHECK-0383-R', '自检离职', v_dept, DATE '2026-01-01', 'RESIGNED');
        v_hit := true;
    EXCEPTION WHEN check_violation THEN
        NULL;   -- 期望路径
    END;
    IF v_hit THEN
        RAISE EXCEPTION 'V0383 自检失败：status=RESIGNED 且 leave_date 为空竟然写入成功（ck_biz_employee_leave 未生效）';
    END IF;

    -- B2 正常员工可写；同公司同工号第二条活行必须被唯一键拦下
    INSERT INTO biz_employee (org_id, emp_no, name, dept_id, hire_date, user_id)
    VALUES (v_org, 'SELFCHECK-0383', '自检员工', v_dept, DATE '2026-01-01', v_user)
    RETURNING id INTO v_emp;

    v_hit := false;
    BEGIN
        INSERT INTO biz_employee (org_id, emp_no, name, dept_id, hire_date)
        VALUES (v_org, 'SELFCHECK-0383', '自检员工2', v_dept, DATE '2026-01-01');
        v_hit := true;
    EXCEPTION WHEN unique_violation THEN
        NULL;
    END;
    IF v_hit THEN
        RAISE EXCEPTION 'V0383 自检失败：同公司同工号写入第二条活行竟然成功（uk_biz_employee_no 未生效）';
    END IF;

    -- B3 软删行不占唯一键：软删后同工号可重建（这正是「唯一键必须带软删条件」的意义）
    UPDATE biz_employee SET deleted_at = now() WHERE id = v_emp;
    INSERT INTO biz_employee (org_id, emp_no, name, dept_id, hire_date, id_card, user_id)
    VALUES (v_org, 'SELFCHECK-0383', '自检员工', v_dept, DATE '2026-01-01', '110101199001010011', v_user)
    RETURNING id INTO v_emp;

    -- B4 user_id 唯一（非空时）：同一账号不许挂两份员工档（巡检 A2 的库级兜底）
    v_hit := false;
    BEGIN
        INSERT INTO biz_employee (org_id, emp_no, name, dept_id, hire_date, user_id)
        VALUES (v_org, 'SELFCHECK-0383-U', '自检员工U', v_dept, DATE '2026-01-01', v_user);
        v_hit := true;
    EXCEPTION WHEN unique_violation THEN
        NULL;
    END;
    IF v_hit THEN
        RAISE EXCEPTION 'V0383 自检失败：同一 user_id 挂两份员工档竟然成功（uk_biz_employee_user 未生效）';
    END IF;

    -- B5 合同：无固定期限合同不许有 end_date
    v_hit := false;
    BEGIN
        INSERT INTO biz_employee_contract (org_id, employee_id, contract_no, contract_type,
                                           sign_date, start_date, end_date)
        VALUES (v_org, v_emp, 'SELFCHECK-HT-0383', 'UNFIXED_TERM',
                DATE '2026-01-01', DATE '2026-01-01', DATE '2028-01-01');
        v_hit := true;
    EXCEPTION WHEN check_violation THEN
        NULL;
    END;
    IF v_hit THEN
        RAISE EXCEPTION 'V0383 自检失败：UNFIXED_TERM 带 end_date 竟然写入成功（ck_biz_emp_contract_unfixed 未生效）';
    END IF;

    -- B6 考勤期间：2026-13 必须被正则拦下（宽正则的经典坑）
    v_hit := false;
    BEGIN
        INSERT INTO biz_attendance_period (org_id, period_code, period_start, period_end)
        VALUES (v_org, '2026-13', DATE '2026-12-26', DATE '2027-01-25');
        v_hit := true;
    EXCEPTION WHEN check_violation THEN
        NULL;
    END;
    IF v_hit THEN
        RAISE EXCEPTION 'V0383 自检失败：period_code=2026-13 竟然写入成功（ck_biz_att_period_code 未生效）';
    END IF;

    INSERT INTO biz_attendance_period (org_id, period_code, period_start, period_end)
    VALUES (v_org, '2026-11', DATE '2026-10-26', DATE '2026-11-25')
    RETURNING id INTO v_per;

    -- B7 考勤记录：一人一月一行（同期间同员工第二条必须被拦）
    INSERT INTO biz_attendance_record (org_id, period_id, employee_id, employee_no, employee_name)
    VALUES (v_org, v_per, v_emp, 'SELFCHECK-0383', '自检员工');
    v_hit := false;
    BEGIN
        INSERT INTO biz_attendance_record (org_id, period_id, employee_id, employee_no, employee_name)
        VALUES (v_org, v_per, v_emp, 'SELFCHECK-0383', '自检员工');
        v_hit := true;
    EXCEPTION WHEN unique_violation THEN
        NULL;
    END;
    IF v_hit THEN
        RAISE EXCEPTION 'V0383 自检失败：同期间同员工写入第二条考勤竟然成功（uk_biz_att_rec 未生效）';
    END IF;

    -- B8 请假单：end_time 必须晚于 start_time
    v_hit := false;
    BEGIN
        INSERT INTO biz_leave_request (org_id, leave_no, employee_id, employee_no, employee_name,
                                       leave_type, start_time, end_time, duration_days)
        VALUES (v_org, 'SELFCHECK-LV-0383', v_emp, 'SELFCHECK-0383', '自检员工', 'ANNUAL',
                TIMESTAMPTZ '2026-11-10 09:00+08', TIMESTAMPTZ '2026-11-10 09:00+08', 1);
        v_hit := true;
    EXCEPTION WHEN check_violation THEN
        NULL;
    END;
    IF v_hit THEN
        RAISE EXCEPTION 'V0383 自检失败：请假 end_time = start_time 竟然写入成功（ck_biz_leave_range 未生效）';
    END IF;

    -- B9 薪资项：FORMULA 无公式必须被拒；EARNING 缺科目必须被拒
    v_hit := false;
    BEGIN
        INSERT INTO biz_salary_item (org_id, item_code, item_name, item_type, calc_type, sort_order)
        VALUES (v_org, 'SELFCHECK_F', '自检公式项', 'EARNING', 'FORMULA', 990001);
        v_hit := true;
    EXCEPTION WHEN check_violation THEN
        NULL;
    END;
    IF v_hit THEN
        RAISE EXCEPTION 'V0383 自检失败：FORMULA 项无 formula 竟然写入成功（ck_biz_sal_item_formula / ck_biz_sal_item_account 未生效）';
    END IF;

    INSERT INTO biz_salary_item (org_id, item_code, item_name, item_type, calc_type,
                                 sort_order, debit_account_code, credit_account_code)
    VALUES (v_org, 'SELFCHECK_BASE', '自检基本工资', 'EARNING', 'INPUT',
            990002, '5602.10', '2211.01');
    INSERT INTO biz_salary_item (org_id, item_code, item_name, item_type, calc_type,
                                 sort_order, debit_account_code, credit_account_code)
    VALUES (v_org, 'SELFCHECK_OTHER', '自检其他', 'EARNING', 'INPUT',
            990003, '5602.10', '2211.01');

    -- B10 薪资单：三条恒等式。gross ≠ net + withhold 必须被拒
    INSERT INTO biz_salary_sheet (org_id, sheet_no, period_code, period_start, period_end)
    VALUES (v_org, 'SELFCHECK-SAL-0383', '2026-11', DATE '2026-11-01', DATE '2026-11-30')
    RETURNING id INTO v_sheet;

    v_hit := false;
    BEGIN
        UPDATE biz_salary_sheet
           SET earning_total = 10000, pre_deduct_total = 0, gross_total = 10000,
               withhold_total = 1500, net_total = 9000        -- 10000 ≠ 9000 + 1500
         WHERE id = v_sheet;
        v_hit := true;
    EXCEPTION WHEN check_violation THEN
        NULL;
    END;
    IF v_hit THEN
        RAISE EXCEPTION 'V0383 自检失败：应发 ≠ 实发 + 代扣 竟然写入成功（ck_biz_sal_sheet_idem 未生效）';
    END IF;

    -- 正确的一组必须放行（防止 CHECK 写反了把合法数据也拦掉）
    UPDATE biz_salary_sheet
       SET earning_total = 10000, pre_deduct_total = 0, gross_total = 10000,
           withhold_total = 1500, net_total = 8500, employee_count = 1
     WHERE id = v_sheet;

    -- B11 同公司同期间第二张薪资单必须被唯一键拦下
    v_hit := false;
    BEGIN
        INSERT INTO biz_salary_sheet (org_id, sheet_no, period_code, period_start, period_end)
        VALUES (v_org, 'SELFCHECK-SAL-0383B', '2026-11', DATE '2026-11-01', DATE '2026-11-30');
        v_hit := true;
    EXCEPTION WHEN unique_violation THEN
        NULL;
    END;
    IF v_hit THEN
        RAISE EXCEPTION 'V0383 自检失败：同公司同期间写入第二张薪资单竟然成功（uk_biz_sal_sheet_period 未生效）';
    END IF;

    -- B12 薪资明细：三方恒等式 + 一项一码唯一 + 级联删除
    INSERT INTO biz_salary_detail (org_id, sheet_id, employee_id, employee_no, employee_name,
                                   earning_amount, gross_amount, withhold_amount, net_amount,
                                   social_personal, fund_personal, iit_amount, other_withhold)
    VALUES (v_org, v_sheet, v_emp, 'SELFCHECK-0383', '自检员工',
            10000, 10000, 1500, 8500, 1000, 500, 0, 0)
    RETURNING id INTO v_det;

    v_hit := false;
    BEGIN
        INSERT INTO biz_salary_detail (org_id, sheet_id, employee_id, employee_no, employee_name,
                                       earning_amount, gross_amount, withhold_amount, net_amount,
                                       social_personal, fund_personal, iit_amount, other_withhold)
        VALUES (v_org, v_sheet, v_emp, 'SELFCHECK-0383', '自检员工',
                10000, 10000, 1500, 8000, 1000, 500, 0, 0);   -- 应发 ≠ 实发 + 代扣
        v_hit := true;
    EXCEPTION WHEN check_violation THEN
        NULL;
    WHEN unique_violation THEN
        RAISE EXCEPTION 'V0383 自检失败：同单同员工写入第二条明细竟然成功（uk_biz_sal_det 未生效）';
    END;
    IF v_hit THEN
        RAISE EXCEPTION 'V0383 自检失败：明细 应发 ≠ 实发 + 代扣 竟然写入成功（ck_biz_sal_det_net 未生效）';
    END IF;

    INSERT INTO biz_salary_detail_item (org_id, sheet_id, detail_id, item_code, item_name,
                                        item_type, calc_type, amount)
    VALUES (v_org, v_sheet, v_det, 'SELFCHECK_BASE', '自检基本工资', 'EARNING', 'INPUT', 10000);
    v_hit := false;
    BEGIN
        INSERT INTO biz_salary_detail_item (org_id, sheet_id, detail_id, item_code, item_name,
                                            item_type, calc_type, amount)
        VALUES (v_org, v_sheet, v_det, 'SELFCHECK_BASE', '自检基本工资', 'EARNING', 'INPUT', 10000);
        v_hit := true;
    EXCEPTION WHEN unique_violation THEN
        NULL;
    END;
    IF v_hit THEN
        RAISE EXCEPTION 'V0383 自检失败：同明细同 item_code 写入第二条项目竟然成功（uk_biz_sal_ditem 未生效）';
    END IF;

    -- 删单头 ⇒ 明细与明细项目必须一起消失（ON DELETE CASCADE 生效）
    DELETE FROM biz_salary_sheet WHERE id = v_sheet;
    SELECT count(*) INTO v_n FROM biz_salary_detail      WHERE sheet_id = v_sheet;
    SELECT v_n + count(*) INTO v_n FROM biz_salary_detail_item WHERE sheet_id = v_sheet;
    IF v_n <> 0 THEN
        RAISE EXCEPTION 'V0383 自检失败：删薪资单后明细/明细项目残留 % 行（级联未生效，会留孤儿行）', v_n;
    END IF;

    -- ===== 清理探针（按 probe 键，重复执行零残留） =====
    DELETE FROM biz_leave_request      WHERE org_id = v_org;
    DELETE FROM biz_attendance_record  WHERE org_id = v_org;
    DELETE FROM biz_attendance_period  WHERE org_id = v_org;
    DELETE FROM biz_employee_salary    WHERE org_id = v_org;
    DELETE FROM biz_employee_contract  WHERE org_id = v_org;
    DELETE FROM biz_salary_item        WHERE org_id = v_org;
    DELETE FROM biz_employee           WHERE org_id = v_org;

    SELECT count(*) INTO v_n FROM biz_employee           WHERE org_id = v_org;
    SELECT v_n + count(*) INTO v_n FROM biz_employee_contract WHERE org_id = v_org;
    SELECT v_n + count(*) INTO v_n FROM biz_employee_salary   WHERE org_id = v_org;
    SELECT v_n + count(*) INTO v_n FROM biz_attendance_period WHERE org_id = v_org;
    SELECT v_n + count(*) INTO v_n FROM biz_attendance_record WHERE org_id = v_org;
    SELECT v_n + count(*) INTO v_n FROM biz_leave_request     WHERE org_id = v_org;
    SELECT v_n + count(*) INTO v_n FROM biz_salary_item       WHERE org_id = v_org;
    SELECT v_n + count(*) INTO v_n FROM biz_salary_sheet      WHERE org_id = v_org;
    IF v_n <> 0 THEN
        RAISE EXCEPTION 'V0383 自检失败：探针行未清理干净（残留 % 行）', v_n;
    END IF;

    RAISE NOTICE 'V0383 完成：10 张 HR 表 + 27 条索引建成；形态断言（10 表/12 唯一键带软删/49 CHECK/12 FK/精度）与行为断言（11 条）全过，探针已清理';
END $$;

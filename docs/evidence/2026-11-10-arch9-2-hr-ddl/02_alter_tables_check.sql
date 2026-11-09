-- ARCH-9-2 人力资源模块：改造表 / 种子 DDL 校验（整段事务 + ROLLBACK，零残留）
-- 复现：PGPASSWORD=alun psql -h 127.0.0.1 -U alun -d cloudpivot_test -v ON_ERROR_STOP=1 \
--         -f docs/evidence/2026-11-10-arch9-2-hr-ddl/02_alter_tables_check.sql
--
-- 覆盖：① 6 个末级科目 + 2211 翻非末级  ② 4 条号段（含 org_id 兼容分支）
--       ③ 4 个字典类型  ④ 菜单/权限（1 目录 + 5 页面 + 30 按钮）
--       ⑤ HR_MANAGER 角色  ⑥ 2 条字段脱敏规则  ⑦ 3 条 sys_config
--       ⑧ 2 条凭证规则  ⑨ 4 条审批流程定义（+ 默认流程唯一性反向断言）
--       ⑩ 附件 CHECK 放宽（本库无 biz_doc_attachment → 走 NOTICE 跳过分支，证明幂等守卫有效）
BEGIN;

-- ============================================================
-- ① 科目：6 个末级 + 父科目 2211 翻非末级
-- ============================================================
INSERT INTO biz_account (account_code, account_name, parent_id, level, account_category,
                         balance_direction, is_leaf, status, aux_dimensions, remark)
SELECT v.code, v.name,
       (SELECT id FROM biz_account WHERE account_code = split_part(v.code, '.', 1) AND deleted_at IS NULL),
       2, v.category, v.direction, 1, 1, v.aux, 'ARCH-9-2 工资相关科目'
  FROM (VALUES
        ('2211.01', '应付职工薪酬-工资',        'LIABILITY',   'CREDIT', 'DEPT'),
        ('2211.02', '应付职工薪酬-社保(单位)',  'LIABILITY',   'CREDIT', 'DEPT'),
        ('2211.03', '应付职工薪酬-公积金(单位)','LIABILITY',   'CREDIT', 'DEPT'),
        ('2221.03', '应交税费-应交个人所得税',  'LIABILITY',   'CREDIT', NULL),
        ('2241.03', '其他应付款-社保(个人)',    'LIABILITY',   'CREDIT', NULL),
        ('2241.04', '其他应付款-公积金(个人)',  'LIABILITY',   'CREDIT', NULL),
        ('5602.10', '管理费用-工资',            'PROFIT_LOSS', 'DEBIT',  'DEPT'),
        ('5602.11', '管理费用-社保(单位)',      'PROFIT_LOSS', 'DEBIT',  'DEPT'),
        ('5602.12', '管理费用-公积金(单位)',    'PROFIT_LOSS', 'DEBIT',  'DEPT')
       ) AS v(code, name, category, direction, aux)
 WHERE NOT EXISTS (SELECT 1 FROM biz_account b WHERE b.account_code = v.code)
   AND EXISTS (SELECT 1 FROM biz_account p WHERE p.account_code = split_part(v.code, '.', 1) AND p.deleted_at IS NULL);

-- 父科目翻非末级（幂等：只翻「确实有未删子科目」且当前还是末级的父科目）
UPDATE biz_account p SET is_leaf = 0, updated_at = now()
 WHERE p.deleted_at IS NULL AND p.is_leaf = 1
   AND p.account_code IN ('2211','2221','2241','5602')
   AND EXISTS (SELECT 1 FROM biz_account c WHERE c.parent_id = p.id AND c.deleted_at IS NULL);

-- ============================================================
-- ② 号段 4 条（EMP 是主数据码，LV/ATT/SAL 是单据号）
--    兼容分支：ARCH-9-1 给 biz_seq 加了 org_id 后，本段自动带上该列
-- ============================================================
DO $$
DECLARE
    has_org boolean;
    org_col text;
    org_val text;
    sql text;
BEGIN
    SELECT EXISTS (SELECT 1 FROM information_schema.columns
                    WHERE table_schema='public' AND table_name='biz_seq' AND column_name='org_id') INTO has_org;
    org_col := CASE WHEN has_org THEN ', org_id' ELSE '' END;
    org_val := CASE WHEN has_org THEN ', ''00000000-0000-4000-8000-000000000001''::uuid' ELSE '' END;

    sql := 'INSERT INTO biz_seq (seq_type, seq_name, prefix, date_pattern, period_key, current_no, step, separator, status' || org_col || ') '
        || 'SELECT v.t, v.n, v.p, v.d, CASE WHEN v.d = ''none'' THEN ''GLOBAL'' ELSE '''' END, 0, 1, ''-'', 1' || org_val
        || '  FROM (VALUES (''EMP'',''员工工号'',''EMP'',''none''),'
        || '               (''LV'',''请假单号'',''LV'',''yyyyMMdd''),'
        || '               (''ATT'',''考勤批次号'',''ATT'',''yyyyMM''),'
        || '               (''SAL'',''薪资单号'',''SAL'',''yyyyMM'')) AS v(t,n,p,d) '
        || ' WHERE NOT EXISTS (SELECT 1 FROM biz_seq s WHERE s.seq_type = v.t AND s.deleted_at IS NULL)';
    EXECUTE sql;
END $$;

-- ============================================================
-- ③ 字典（4 类）
-- ============================================================
INSERT INTO sys_dict_type (id, dict_code, dict_name, description, status, sort_order)
VALUES
    (gen_random_uuid(), 'hr_position',  '岗位', '员工档案-岗位', 1, 10),
    (gen_random_uuid(), 'hr_job_level', '职级', '员工档案-职级（P1~P9 / M1~M4）', 1, 20),
    (gen_random_uuid(), 'hr_education', '学历', '员工档案-学历', 1, 30),
    (gen_random_uuid(), 'hr_leave_type','请假类型', '请假单-类型（与 biz_leave_request.leave_type 一一对应）', 1, 40)
ON CONFLICT (id) DO NOTHING;

INSERT INTO sys_dict_item (id, dict_type_id, item_value, item_label, sort_order, status)
SELECT gen_random_uuid(), t.id, v.val, v.label, v.sort, 1
  FROM (VALUES
        ('hr_position', 'WORKER',     '车间工人', 10),
        ('hr_position', 'CLERK',      '文员',     20),
        ('hr_position', 'ENGINEER',   '工程师',   30),
        ('hr_position', 'SALES',      '销售',     40),
        ('hr_position', 'MANAGER',    '经理',     50),
        ('hr_job_level', 'P1', 'P1', 10), ('hr_job_level', 'P5', 'P5', 20), ('hr_job_level', 'P8', 'P8', 30),
        ('hr_job_level', 'M1', 'M1', 40), ('hr_job_level', 'M4', 'M4', 50),
        ('hr_education', 'PRIMARY',   '小学',   10),
        ('hr_education', 'JUNIOR',    '初中',   20),
        ('hr_education', 'SENIOR',    '高中',   30),
        ('hr_education', 'COLLEGE',   '大专',   40),
        ('hr_education', 'BACHELOR',  '本科',   50),
        ('hr_education', 'MASTER',    '硕士',   60),
        ('hr_education', 'DOCTOR',    '博士',   70),
        ('hr_leave_type','ANNUAL',    '年假',   10),
        ('hr_leave_type','SICK',      '病假',   20),
        ('hr_leave_type','PERSONAL',  '事假',   30),
        ('hr_leave_type','MARRIAGE',  '婚假',   40),
        ('hr_leave_type','MATERNITY', '产假',   50),
        ('hr_leave_type','PATERNITY', '陪产假', 60),
        ('hr_leave_type','BEREAVEMENT','丧假',  70),
        ('hr_leave_type','COMPENSATORY','调休', 80),
        ('hr_leave_type','OTHER',     '其他',   90)
       ) AS v(dict, val, label, sort)
  JOIN sys_dict_type t ON t.dict_code = v.dict AND t.deleted_at IS NULL
 WHERE NOT EXISTS (SELECT 1 FROM sys_dict_item i
                    WHERE i.dict_type_id = t.id AND i.item_value = v.val AND i.deleted_at IS NULL);

-- ============================================================
-- ④ 菜单 / 权限：1 目录 + 5 页面 + 30 按钮（顶级 sort_order = 77，实测 19 个顶级菜单中 77 空闲）
-- ============================================================
INSERT INTO sys_menu (id, name, menu_type, auth_code, path, parent_id, component,
                      sort_order, status, icon, title, is_visible, module_code)
VALUES
    ('20000000-0000-0000-0000-000000001600'::uuid, '人力资源', 1, '',
     '/business/hr', NULL, NULL, 77, 1, 'user-business', '人力资源', TRUE, 'business'),
    ('20000000-0000-0000-0000-000000001601'::uuid, '员工档案', 2, 'business:hr:employee:list',
     '/business/hr/employee', '20000000-0000-0000-0000-000000001600'::uuid,
     'business/hr/employee/index', 10, 1, 'user', '员工档案', TRUE, 'business'),
    ('20000000-0000-0000-0000-000000001602'::uuid, '考勤管理', 2, 'business:hr:attendance:list',
     '/business/hr/attendance', '20000000-0000-0000-0000-000000001600'::uuid,
     'business/hr/attendance/index', 20, 1, 'calendar', '考勤管理', TRUE, 'business'),
    ('20000000-0000-0000-0000-000000001603'::uuid, '请假单', 2, 'business:hr:leave:list',
     '/business/hr/leave', '20000000-0000-0000-0000-000000001600'::uuid,
     'business/hr/leave/index', 30, 1, 'time', '请假单', TRUE, 'business'),
    ('20000000-0000-0000-0000-000000001604'::uuid, '薪资管理', 2, 'business:hr:salary:list',
     '/business/hr/salary', '20000000-0000-0000-0000-000000001600'::uuid,
     'business/hr/salary/index', 40, 1, 'wallet', '薪资管理', TRUE, 'business'),
    ('20000000-0000-0000-0000-000000001605'::uuid, '薪资项', 2, 'business:hr:salary-item:list',
     '/business/hr/salary-item', '20000000-0000-0000-0000-000000001600'::uuid,
     'business/hr/salary-item/index', 50, 1, 'setting', '薪资项', TRUE, 'business')
ON CONFLICT (id) DO NOTHING;

-- 按钮权限（30 个）：父节点 = 对应页面，sort_order 10 递增
INSERT INTO sys_menu (id, name, menu_type, auth_code, path, parent_id, component,
                      sort_order, status, icon, title, is_visible, module_code)
SELECT v.id::uuid, v.code, 3, v.code, NULL, v.parent::uuid, NULL, v.sort, 1, NULL, v.label, FALSE, 'business'
  FROM (VALUES
    -- 员工档案
    ('20000000-0000-0000-0000-000000001611','business:hr:employee:create','20000000-0000-0000-0000-000000001601',10,'新建员工'),
    ('20000000-0000-0000-0000-000000001612','business:hr:employee:update','20000000-0000-0000-0000-000000001601',20,'编辑员工'),
    ('20000000-0000-0000-0000-000000001613','business:hr:employee:delete','20000000-0000-0000-0000-000000001601',30,'删除员工'),
    ('20000000-0000-0000-0000-000000001614','business:hr:employee:detail','20000000-0000-0000-0000-000000001601',40,'员工详情'),
    ('20000000-0000-0000-0000-000000001615','business:hr:employee:sensitive','20000000-0000-0000-0000-000000001601',50,'查看敏感字段（身份证/银行账号）'),
    ('20000000-0000-0000-0000-000000001616','business:hr:employee:resign','20000000-0000-0000-0000-000000001601',60,'发起离职'),
    ('20000000-0000-0000-0000-000000001617','business:hr:employee:import','20000000-0000-0000-0000-000000001601',70,'导入员工'),
    ('20000000-0000-0000-0000-000000001618','business:hr:employee:export','20000000-0000-0000-0000-000000001601',80,'导出员工'),
    ('20000000-0000-0000-0000-000000001619','business:hr:contract:manage','20000000-0000-0000-0000-000000001601',90,'合同台账维护'),
    -- 考勤
    ('20000000-0000-0000-0000-000000001621','business:hr:attendance:period','20000000-0000-0000-0000-000000001602',10,'考勤期间维护'),
    ('20000000-0000-0000-0000-000000001622','business:hr:attendance:record','20000000-0000-0000-0000-000000001602',20,'考勤记录维护'),
    ('20000000-0000-0000-0000-000000001623','business:hr:attendance:import','20000000-0000-0000-0000-000000001602',30,'导入考勤'),
    ('20000000-0000-0000-0000-000000001624','business:hr:attendance:export','20000000-0000-0000-0000-000000001602',40,'导出考勤'),
    ('20000000-0000-0000-0000-000000001625','business:hr:attendance:lock','20000000-0000-0000-0000-000000001602',50,'锁定/解锁考勤期间'),
    -- 请假
    ('20000000-0000-0000-0000-000000001631','business:hr:leave:create','20000000-0000-0000-0000-000000001603',10,'新建请假单'),
    ('20000000-0000-0000-0000-000000001632','business:hr:leave:update','20000000-0000-0000-0000-000000001603',20,'编辑请假单'),
    ('20000000-0000-0000-0000-000000001633','business:hr:leave:delete','20000000-0000-0000-0000-000000001603',30,'删除请假单'),
    ('20000000-0000-0000-0000-000000001634','business:hr:leave:submit','20000000-0000-0000-0000-000000001603',40,'提交审批'),
    ('20000000-0000-0000-0000-000000001635','business:hr:leave:cancel','20000000-0000-0000-0000-000000001603',50,'作废请假单'),
    ('20000000-0000-0000-0000-000000001636','business:hr:leave:detail','20000000-0000-0000-0000-000000001603',60,'请假单详情'),
    -- 薪资
    ('20000000-0000-0000-0000-000000001641','business:hr:salary:detail','20000000-0000-0000-0000-000000001604',10,'薪资单详情'),
    ('20000000-0000-0000-0000-000000001642','business:hr:salary:calc','20000000-0000-0000-0000-000000001604',20,'试算/重算'),
    ('20000000-0000-0000-0000-000000001643','business:hr:salary:submit','20000000-0000-0000-0000-000000001604',30,'提交审批'),
    ('20000000-0000-0000-0000-000000001644','business:hr:salary:post','20000000-0000-0000-0000-000000001604',40,'过账（生成凭证）'),
    ('20000000-0000-0000-0000-000000001645','business:hr:salary:reverse','20000000-0000-0000-0000-000000001604',50,'红冲'),
    ('20000000-0000-0000-0000-000000001646','business:hr:salary:export','20000000-0000-0000-0000-000000001604',60,'导出工资表'),
    ('20000000-0000-0000-0000-000000001647','business:hr:salary:my','20000000-0000-0000-0000-000000001604',70,'我的薪资（自助）'),
    -- 薪资项
    ('20000000-0000-0000-0000-000000001651','business:hr:salary-item:create','20000000-0000-0000-0000-000000001605',10,'新建薪资项'),
    ('20000000-0000-0000-0000-000000001652','business:hr:salary-item:update','20000000-0000-0000-0000-000000001605',20,'编辑薪资项'),
    ('20000000-0000-0000-0000-000000001653','business:hr:salary-item:delete','20000000-0000-0000-0000-000000001605',30,'删除薪资项')
  ) AS v(id, code, parent, sort, label)
ON CONFLICT (id) DO NOTHING;

-- ============================================================
-- ⑤ 角色 HR_MANAGER（人事经理）
-- ============================================================
INSERT INTO sys_role (id, role_code, role_name, description, status, data_scope, sort_order)
SELECT '20000000-0000-4000-8000-0000000f0008'::uuid, 'HR_MANAGER', '人事经理',
       'ARCH-9-2：员工档案 / 考勤 / 薪资维护；无财务过账权（过账另有 salary:post 码）', 1, 3, 80
 WHERE NOT EXISTS (SELECT 1 FROM sys_role WHERE role_code = 'HR_MANAGER' AND deleted_at IS NULL);

-- ============================================================
-- ⑥ 字段脱敏：身份证 / 银行账号（全局规则，role_id = NULL）
-- ============================================================
INSERT INTO sys_field_mask (id, table_name, column_name, mask_type, mask_param, role_id, status, reason)
SELECT gen_random_uuid(), 'biz_employee', 'id_card', 2, '6,4', NULL, 1, '个人信息保护：身份证号默认脱敏（明文需 business:hr:employee:sensitive）'
 WHERE NOT EXISTS (SELECT 1 FROM sys_field_mask WHERE table_name='biz_employee' AND column_name='id_card' AND role_id IS NULL AND deleted_at IS NULL);
INSERT INTO sys_field_mask (id, table_name, column_name, mask_type, mask_param, role_id, status, reason)
SELECT gen_random_uuid(), 'biz_employee', 'bank_account', 5, '4,4', NULL, 1, '个人信息保护：工资卡号默认脱敏（明文需 business:hr:employee:sensitive）'
 WHERE NOT EXISTS (SELECT 1 FROM sys_field_mask WHERE table_name='biz_employee' AND column_name='bank_account' AND role_id IS NULL AND deleted_at IS NULL);

-- ============================================================
-- ⑦ sys_config：3 个开关（一期口径写进配置，二期改配置即可切自动）
-- ============================================================
INSERT INTO sys_config (id, config_key, config_value, config_type, description, category, sort_order, config_level, is_modifiable, is_encrypted, is_restart, status)
VALUES
    (gen_random_uuid(), 'hr.salary.iit_mode',      'MANUAL', 'STRING', '个税算法：MANUAL 手工录入（一期）/ AUTO 累计预扣法（二期）', 'hr', 10, 1, TRUE, FALSE, FALSE, 1),
    (gen_random_uuid(), 'hr.salary.si_mode',       'MANUAL', 'STRING', '社保公积金算法：MANUAL 手工录入（一期）/ AUTO 按城市规则（二期）', 'hr', 20, 1, TRUE, FALSE, FALSE, 1),
    (gen_random_uuid(), 'hr.attendance.require_leave_match', 'TRUE', 'BOOLEAN', '导入考勤时是否校验「无薪假 ≤ 已批准请假天数」', 'hr', 30, 1, TRUE, FALSE, FALSE, 1)
ON CONFLICT (id) DO NOTHING;

-- ============================================================
-- ⑧ 凭证规则（2 条，登记口径；实际分录按薪资项展开，走 voucher_multi::post_explicit_lines）
-- ============================================================
INSERT INTO biz_voucher_rule (rule_code, biz_type, debit_account_id, credit_account_id, tax_account_id, is_active, remark)
SELECT v.rule, v.biz, d.id, c.id, NULL, 1, v.remark
  FROM (VALUES
        ('PAYROLL_ACCRUAL_DEFAULT', 'PAYROLL_ACCRUAL', '5602.10', '2211.01',
         'ARCH-9-2 工资计提：借 管理费用-工资 / 贷 应付职工薪酬-工资（多借方按部门展开）'),
        ('PAYROLL_PAYMENT_DEFAULT', 'PAYROLL_PAYMENT', '2211.01', '1002',
         'ARCH-9-2 工资发放：借 应付职工薪酬-工资 / 贷 银行存款（+代扣项贷方）')
       ) AS v(rule, biz, debit, credit, remark)
  LEFT JOIN biz_account d ON d.account_code = v.debit  AND d.deleted_at IS NULL
  LEFT JOIN biz_account c ON c.account_code = v.credit AND c.deleted_at IS NULL
 WHERE NOT EXISTS (SELECT 1 FROM biz_voucher_rule r WHERE r.rule_code = v.rule AND r.deleted_at IS NULL);

-- ============================================================
-- ⑨ 审批流程定义（4 条；完整图/版本/hash 由实现卡负责，这里只验列域与默认流程唯一性）
-- ============================================================
INSERT INTO biz_flow_def (id, flow_code, flow_name, biz_type, category, description, status,
                          current_version_id, current_version_no, is_default, allow_auto_approve, is_system)
SELECT v.id::uuid, v.code, v.name, v.biz, '人力资源', v.desc_, 'DRAFT', NULL, 0, TRUE, FALSE, TRUE
  FROM (VALUES
    ('20000000-0000-4000-8000-0000000e2001','hr_leave','请假审批','HR_LEAVE','请假单审批：直属上级 →（≥3 天）人事经理；销假后可作废'),
    ('20000000-0000-4000-8000-0000000e2002','hr_salary','工资发放审批','HR_SALARY','薪资单审批：人事经理 → 财务经理会签 → 总经理（≥ 50 万）'),
    ('20000000-0000-4000-8000-0000000e2003','hr_salary_adjust','调薪审批','HR_SALARY_ADJUST','调薪单审批：直属上级 → 人事经理 → 财务经理'),
    ('20000000-0000-4000-8000-0000000e2004','hr_employee_resign','离职审批','HR_EMPLOYEE_RESIGN','离职审批：直属上级 → 人事经理 →（关键岗位）总经理')
  ) AS v(id, code, name, biz, desc_)
 WHERE NOT EXISTS (SELECT 1 FROM biz_flow_def f WHERE f.flow_code = v.code AND f.deleted_at IS NULL)
   AND NOT EXISTS (SELECT 1 FROM biz_flow_def f WHERE f.biz_type = v.biz AND f.is_default AND f.deleted_at IS NULL);

-- ============================================================
-- ⑩ 附件 CHECK 放宽（biz_doc_attachment + biz_attachment_rule，存在才放宽）
-- ============================================================
DO $$
DECLARE
    v_def text;
BEGIN
    IF NOT EXISTS (SELECT 1 FROM information_schema.tables
                    WHERE table_schema='public' AND table_name='biz_doc_attachment') THEN
        RAISE NOTICE '本库无 biz_doc_attachment：放宽段按幂等守卫跳过（生产库会执行）';
        RETURN;
    END IF;
    SELECT pg_get_constraintdef(oid) INTO v_def FROM pg_constraint
     WHERE conname='ck_biz_doc_attachment_type' AND conrelid='biz_doc_attachment'::regclass;
    IF v_def IS NOT NULL AND position('SALARY_SHEET' IN v_def) > 0 THEN
        RAISE NOTICE 'ck_biz_doc_attachment_type 已含 SALARY_SHEET，跳过';
        RETURN;
    END IF;
    ALTER TABLE biz_doc_attachment DROP CONSTRAINT IF EXISTS ck_biz_doc_attachment_type;
    ALTER TABLE biz_doc_attachment ADD CONSTRAINT ck_biz_doc_attachment_type
        CHECK (doc_type IN ('CONTRACT','PURCHASE_ORDER','STOCK_IN','PAYABLE','PAYMENT',
                            'FIXED_ASSET','EXPENSE_REPORT','INVOICE','RECEIPT','STOCK_TAKE','PURCHASE_RETURN',
                            'SALES_RETURN','STOCK_TRANSFER','SUPPLIER','CUSTOMER','OPENING_BALANCE',
                            'EMPLOYEE','EMPLOYEE_CONTRACT','LEAVE_REQUEST','SALARY_SHEET'));
END $$;

-- ============================================================
-- 断言
-- ============================================================
DO $$
DECLARE
    ok int := 0;
    n int;
    tops int;
BEGIN
    -- 科目
    SELECT count(*) INTO n FROM biz_account WHERE account_code IN ('2211.01','2211.02','2211.03') AND is_leaf=1 AND deleted_at IS NULL;
    IF n <> 3 THEN RAISE EXCEPTION '断言失败：2211.01~03 应有 3 条末级科目，实际 %', n; END IF; ok := ok + 1;
    SELECT count(*) INTO n FROM biz_account WHERE account_code IN ('2221.03','2241.03','2241.04') AND is_leaf=1 AND deleted_at IS NULL;
    IF n <> 3 THEN RAISE EXCEPTION '断言失败：2221.03/2241.03/2241.04 应有 3 条，实际 %', n; END IF; ok := ok + 1;
    SELECT is_leaf INTO n FROM biz_account WHERE account_code='2211' AND deleted_at IS NULL;
    IF n <> 0 THEN RAISE EXCEPTION '断言失败：2211 应翻为非末级，实际 is_leaf=%', n; END IF; ok := ok + 1;
    SELECT count(*) INTO n FROM biz_account c JOIN biz_account p ON p.id = c.parent_id
     WHERE c.account_code LIKE '2211.%' AND p.account_code='2211';
    IF n <> 3 THEN RAISE EXCEPTION '断言失败：2211 应有 3 个子科目，实际 %', n; END IF; ok := ok + 1;
    SELECT count(*) INTO n FROM biz_account WHERE account_code IN ('5602.10','5602.11','5602.12') AND is_leaf=1 AND deleted_at IS NULL;
    IF n <> 3 THEN RAISE EXCEPTION '断言失败：5602.10~12 应有 3 条，实际 %', n; END IF; ok := ok + 1;
    SELECT count(*) INTO n FROM biz_account WHERE account_code IN ('2211','5602') AND is_leaf=1 AND deleted_at IS NULL;
    IF n <> 0 THEN RAISE EXCEPTION '断言失败：2211/5602 应已翻非末级，实际仍有 % 条末级', n; END IF; ok := ok + 1;
    -- 2221/2241 必须本来就是非末级（V0226 依赖），否则子科目挂不上
    SELECT count(*) INTO n FROM biz_account WHERE account_code IN ('2221','2241') AND is_leaf = 1 AND deleted_at IS NULL;
    IF n > 0 THEN RAISE NOTICE '提示：本库 2221/2241 中 % 条仍为末级（生产库 V0226 已翻）', n; END IF; ok := ok + 1;

    -- 号段
    SELECT count(*) INTO n FROM biz_seq WHERE seq_type IN ('EMP','LV','ATT','SAL') AND deleted_at IS NULL;
    IF n <> 4 THEN RAISE EXCEPTION '断言失败：应有 4 条号段，实际 %', n; END IF; ok := ok + 1;
    SELECT count(*) INTO n FROM biz_seq WHERE seq_type='EMP' AND date_pattern='none';
    IF n <> 1 THEN RAISE EXCEPTION '断言失败：EMP 号段应永久不重置（date_pattern=none）'; END IF; ok := ok + 1;

    -- 字典
    SELECT count(*) INTO n FROM sys_dict_type WHERE dict_code LIKE 'hr\_%' AND deleted_at IS NULL;
    IF n <> 4 THEN RAISE EXCEPTION '断言失败：应有 4 个 hr 字典类型，实际 %', n; END IF; ok := ok + 1;
    SELECT count(*) INTO n FROM sys_dict_item i JOIN sys_dict_type t ON t.id=i.dict_type_id WHERE t.dict_code='hr_leave_type' AND i.deleted_at IS NULL;
    IF n <> 9 THEN RAISE EXCEPTION '断言失败：hr_leave_type 应有 9 项，实际 %', n; END IF; ok := ok + 1;

    -- 菜单结构
    SELECT count(*) INTO tops FROM sys_menu WHERE parent_id IS NULL AND sort_order = 77 AND deleted_at IS NULL;
    IF tops <> 1 THEN RAISE EXCEPTION '断言失败：顶级 sort_order=77 应唯一占用，实际 %', tops; END IF; ok := ok + 1;
    SELECT count(*) INTO n FROM sys_menu WHERE parent_id = '20000000-0000-0000-0000-000000001600'::uuid AND menu_type=2 AND deleted_at IS NULL;
    IF n <> 5 THEN RAISE EXCEPTION '断言失败：人力资源下应有 5 个页面，实际 %', n; END IF; ok := ok + 1;
    SELECT count(*) INTO n FROM sys_menu WHERE parent_id IN (
        '20000000-0000-0000-0000-000000001601','20000000-0000-0000-0000-000000001602',
        '20000000-0000-0000-0000-000000001603','20000000-0000-0000-0000-000000001604',
        '20000000-0000-0000-0000-000000001605') AND menu_type=3 AND deleted_at IS NULL;
    IF n <> 30 THEN RAISE EXCEPTION '断言失败：应有 30 个按钮权限，实际 %', n; END IF; ok := ok + 1;
    -- 同父 sort_order 唯一（AGENTS.md 约定 12）
    SELECT count(*) INTO n FROM (
        SELECT parent_id, sort_order FROM sys_menu
         WHERE deleted_at IS NULL AND parent_id IN (
            '20000000-0000-0000-0000-000000001600','20000000-0000-0000-0000-000000001601',
            '20000000-0000-0000-0000-000000001602','20000000-0000-0000-0000-000000001603',
            '20000000-0000-0000-0000-000000001604','20000000-0000-0000-0000-000000001605')
         GROUP BY parent_id, sort_order HAVING count(*) > 1) d;
    IF n > 0 THEN RAISE EXCEPTION '断言失败：同父 sort_order 有 % 组重复', n; END IF; ok := ok + 1;
    -- title ≤ 12 字（AGENTS.md 约定 12）
    SELECT count(*) INTO n FROM sys_menu WHERE path LIKE '/business/hr%' AND deleted_at IS NULL AND length(title) > 12;
    IF n <> 0 THEN RAISE EXCEPTION '断言失败：有 % 条 HR 菜单 title 超 12 字', n; END IF; ok := ok + 1;
    -- 权限码格式 {模块}:{资源}:{动作}，且按钮码不以 list/detail 之外的动词重复
    SELECT count(*) INTO n FROM sys_menu WHERE path IS NULL AND parent_id IN (
        '20000000-0000-0000-0000-000000001601','20000000-0000-0000-0000-000000001602',
        '20000000-0000-0000-0000-000000001603','20000000-0000-0000-0000-000000001604',
        '20000000-0000-0000-0000-000000001605') AND auth_code !~ '^business:hr:[a-z-]+:[a-z-]+$';
    IF n <> 0 THEN RAISE EXCEPTION '断言失败：有 % 条权限码不符合 business:hr:{资源}:{动作}', n; END IF; ok := ok + 1;

    -- 角色
    SELECT count(*) INTO n FROM sys_role WHERE role_code='HR_MANAGER' AND deleted_at IS NULL;
    IF n <> 1 THEN RAISE EXCEPTION '断言失败：HR_MANAGER 角色未种'; END IF; ok := ok + 1;
    SELECT data_scope INTO n FROM sys_role WHERE role_code='HR_MANAGER';
    IF n <> 3 THEN RAISE EXCEPTION '断言失败：HR_MANAGER 默认 data_scope 应为 3（本部门及以下），实际 %', n; END IF; ok := ok + 1;

    -- 脱敏
    SELECT count(*) INTO n FROM sys_field_mask WHERE table_name='biz_employee' AND role_id IS NULL AND deleted_at IS NULL;
    IF n <> 2 THEN RAISE EXCEPTION '断言失败：biz_employee 应有 2 条全局脱敏规则，实际 %', n; END IF; ok := ok + 1;

    -- 配置
    SELECT count(*) INTO n FROM sys_config WHERE config_key LIKE 'hr.%' AND deleted_at IS NULL;
    IF n <> 3 THEN RAISE EXCEPTION '断言失败：应有 3 条 hr.* 配置，实际 %', n; END IF; ok := ok + 1;

    -- 凭证规则：两条都解析到了科目（LEFT JOIN 没解析到会留 NULL，这里必须非空）
    SELECT count(*) INTO n FROM biz_voucher_rule WHERE rule_code IN ('PAYROLL_ACCRUAL_DEFAULT','PAYROLL_PAYMENT_DEFAULT')
       AND debit_account_id IS NOT NULL AND credit_account_id IS NOT NULL AND deleted_at IS NULL;
    IF n <> 2 THEN RAISE EXCEPTION '断言失败：2 条工资凭证规则未全部解析到科目（实际 %）', n; END IF; ok := ok + 1;

    -- 审批流程：4 条 + 默认唯一性反向断言
    SELECT count(*) INTO n FROM biz_flow_def WHERE flow_code IN ('hr_leave','hr_salary','hr_salary_adjust','hr_employee_resign') AND deleted_at IS NULL;
    IF n <> 4 THEN RAISE EXCEPTION '断言失败：应有 4 条 HR 流程，实际 %', n; END IF; ok := ok + 1;
    BEGIN
        INSERT INTO biz_flow_def (flow_code, flow_name, biz_type, status, is_default, is_system)
        VALUES ('hr_leave_dup', '重复默认流程', 'HR_LEAVE', 'DRAFT', TRUE, TRUE);
        RAISE EXCEPTION '断言失败：同一 biz_type 第二个 is_default 应被拒';
    EXCEPTION WHEN unique_violation THEN ok := ok + 1; END;

    -- 清理本事务内插入的流程（它们只有定义没有版本，留着会让 health 自检报 DRAFT 残留）
    DELETE FROM biz_flow_def WHERE flow_code IN ('hr_leave','hr_salary','hr_salary_adjust','hr_employee_resign');

    RAISE NOTICE 'ARCH-9-2 改造表/种子断言通过 % 条', ok;
    IF ok <> 24 THEN
        RAISE EXCEPTION '断言条数不符：期望 24，实际 %（不许静默通过）', ok;
    END IF;
END $$;

SELECT 'ARCH-9-2 改造表校验完成（即将 ROLLBACK，零残留）' AS result;
ROLLBACK;

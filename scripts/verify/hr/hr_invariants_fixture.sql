-- ============================================================================
-- scripts/verify/hr/hr_invariants_fixture.sql —— HR 巡检的「全绿基线」夹具
--
-- 卡：t_914199ef（HR-45）。用途：**只给 hr_invariants_selftest.sh 用**。
--
-- # 它造什么（一条能同时满足 10 条断言的完整链路）
--   员工（入职 2 年，ACTIVE，挂 HUMAN 账号）
--     ├─ 劳动合同（起 -1 年 / 止 +2 年，ACTIVE ⇒ A4 绿、A5 绿）
--     ├─ 薪资标准（BASE 一行 ⇒ A3 绿；给两段不重叠区间更贴真实，这里只放一段）
--     ├─ 考勤期（本月，OPEN）+ 考勤行（应=实=21.75 ⇒ W1 绿）
--     ├─ 请假单（SUBMITTED ↔ 在跑实例 ⇒ A10 绿）
--     └─ 工资单（POSTED）
--          ├─ 明细 1 行（g=9500 / withhold=1500 / net=8000）
--          ├─ 明细项 6 行（EARNING 10000 / PRE_DEDUCT 500 / WITHHOLD 800+400+200+100 ⇒ A7 绿）
--          ├─ 计提凭证（PAYROLL_ACCRUAL，借贷 10000，期间已存在 ⇒ A8/A9 绿）
--          └─ 发放凭证（PAYROLL_PAYMENT，借贷 8000）
--   单头 4 个汇总列与明细恒等 ⇒ A6 绿；账号部门 = 人事部门 ⇒ W2 绿。
--
-- # 使用约束（很重要）
--   · **必须在一个事务里跑完并 ROLLBACK**（selftest 就是这么用的）。本文件不做清理：
--     夹具的 org/emp_no 都是 HR45 专用值，跑在演练库上不会碰到真实数据，
--     但直接 COMMIT 会往库里留下一条员工 + 一张工资单。
--   · 幂等：所有行都是固定 UUID + `ON CONFLICT DO NOTHING`，同一事务里重复执行不报错。
--   · 依赖迁移跑到 V0398（科目 / 薪资项 / 流程种子必须在）。
-- ============================================================================

-- 固定 UUID 段（…c45xx 专供本夹具，别和种子段 e0xx / f0xx / d1xx 撞）
--   设备/组织：00000000-0000-4000-8000-0000000c4500
--   员工：    …c4501   合同：…c4502   薪资标准：…c4503
--   考勤期：  …c4504   考勤行：…c4505   请假单：…c4506   实例：…c4507
--   工资单：  …c4508   明细：…c4509   明细项：…c4510 ~ …c4515
--   计提凭证：…c4516 （分录 …c4520/…c4521）  发放凭证：…c4517（分录 …c4522/…c4523）
--   账号：    …c4518   部门：…c4519

DO $$
DECLARE
    v_org   constant uuid := '00000000-0000-4000-8000-0000000c4500';
    v_dept  constant uuid := '00000000-0000-4000-8000-0000000c4519';
    v_user  constant uuid := '00000000-0000-4000-8000-0000000c4518';
    v_emp   constant uuid := '00000000-0000-4000-8000-0000000c4501';
    v_sheet constant uuid := '00000000-0000-4000-8000-0000000c4508';
    v_inst  constant uuid := '00000000-0000-4000-8000-0000000c4507';
    v_period text;
BEGIN
    -- 期间取「本月」，这样 A4（合同覆盖今天）与 A9（期间存在）都成立
    v_period := to_char(CURRENT_DATE, 'YYYY-MM');

    -- ---------- 0) 前置：科目与薪资项必须在（否则夹具自身就是脏的） ----------
    IF to_regclass('public.biz_salary_item') IS NULL
       OR to_regclass('public.biz_voucher') IS NULL
       OR to_regclass('public.biz_flow_def') IS NULL THEN
        RAISE EXCEPTION 'HR45 夹具前置失败：HR 表 / 凭证表 / 流程表 不存在（先跑到 V0398）';
    END IF;
    IF NOT EXISTS (SELECT 1 FROM biz_account
                    WHERE account_code = '5602.10' AND deleted_at IS NULL) THEN
        RAISE EXCEPTION 'HR45 夹具前置失败：科目 5602.10 不存在（V0384 没种）';
    END IF;
    IF NOT EXISTS (SELECT 1 FROM biz_flow_def
                    WHERE status = 'PUBLISHED' AND deleted_at IS NULL) THEN
        RAISE EXCEPTION 'HR45 夹具前置失败：没有任何已发布流程（V0393~V0395 没种）';
    END IF;

    -- ---------- 1) 账号 + 员工档 ----------
    INSERT INTO sys_user (id, username, password, real_name, dept_id, status, user_type)
    VALUES (v_user, 'hr45_fixture', 'x', '巡检夹具', v_dept, 1, 'HUMAN')
    ON CONFLICT (id) DO NOTHING;

    INSERT INTO biz_employee (id, org_id, emp_no, name, dept_id, hire_date, status,
                              employment_type, mobile, user_id)
    VALUES (v_emp, v_org, 'HR45-001', '巡检夹具', v_dept, CURRENT_DATE - INTERVAL '2 year',
            'ACTIVE', 'FULL_TIME', '13900000001', v_user)
    ON CONFLICT (id) DO NOTHING;

    -- ---------- 2) 劳动合同（覆盖今天 ⇒ A4 绿；止于 +2 年 ⇒ 以后注入 A5 才有空间） ----------
    INSERT INTO biz_employee_contract (id, org_id, employee_id, contract_no, contract_type,
                                       sign_date, start_date, end_date, status)
    VALUES ('00000000-0000-4000-8000-0000000c4502', v_org, v_emp, 'HT-HR45-001', 'FIXED_TERM',
            CURRENT_DATE - INTERVAL '1 year', CURRENT_DATE - INTERVAL '1 year',
            CURRENT_DATE + INTERVAL '2 year', 'ACTIVE')
    ON CONFLICT (id) DO NOTHING;

    -- ---------- 3) 薪资标准（两段**不重叠**区间 ⇒ A3 绿，且覆盖本月） ----------
    INSERT INTO biz_employee_salary (id, org_id, employee_id, item_code, amount,
                                     effective_from, effective_to)
    VALUES ('00000000-0000-4000-8000-0000000c4503', v_org, v_emp, 'BASE', 10000,
            CURRENT_DATE - INTERVAL '1 year', CURRENT_DATE - INTERVAL '1 day')
    ON CONFLICT (id) DO NOTHING;
    INSERT INTO biz_employee_salary (id, org_id, employee_id, item_code, amount, effective_from)
    VALUES ('00000000-0000-4000-8000-0000000c4524', v_org, v_emp, 'BASE', 11000, CURRENT_DATE)
    ON CONFLICT (id) DO NOTHING;

    -- ---------- 4) 考勤期 + 考勤行（应=实 ⇒ W1 绿） ----------
    -- 先建 DRAFT 期 → 灌考勤行 → 再锁：V0392 的触发器 fn_biz_att_record_lock_guard
    -- 禁止往 LOCKED 期间插记录（这条守卫本身就是设计要的，夹具必须顺着它走）
    INSERT INTO biz_attendance_period (id, org_id, period_code, period_start, period_end,
                                       status, employee_count, should_days_total, actual_days_total)
    VALUES ('00000000-0000-4000-8000-0000000c4504', v_org, v_period,
            date_trunc('month', CURRENT_DATE)::date,
            (date_trunc('month', CURRENT_DATE) + INTERVAL '1 month - 1 day')::date,
            'DRAFT', 1, 21.75, 21.75)
    ON CONFLICT (id) DO NOTHING;

    INSERT INTO biz_attendance_record (id, org_id, period_id, employee_id, employee_no,
                                       employee_name, dept_id, should_days, actual_days, source)
    VALUES ('00000000-0000-4000-8000-0000000c4505', v_org,
            '00000000-0000-4000-8000-0000000c4504', v_emp, 'HR45-001', '巡检夹具', v_dept,
            21.75, 21.75, 'IMPORT')
    ON CONFLICT (id) DO NOTHING;

    -- 灌完再锁（过账的业务前置：考勤期间 LOCKED）
    UPDATE biz_attendance_period
       SET status = 'LOCKED', locked_at = now()
     WHERE id = '00000000-0000-4000-8000-0000000c4504' AND status <> 'LOCKED';

    -- ---------- 5) 审批实例（hr_leave，PENDING）+ 请假单（SUBMITTED ⇒ A10 绿） ----------
    INSERT INTO biz_flow_instance (id, instance_no, def_id, version_id, version_no,
                                   flow_code, flow_name, biz_type, biz_id, biz_no, title,
                                   status, initiator_id, initiator_name, initiator_dept_id)
    SELECT v_inst, 'FLW-HR45-FIXTURE', d.id, d.current_version_id, d.current_version_no,
           d.flow_code, d.flow_name, d.biz_type,
           '00000000-0000-4000-8000-0000000c4506'::uuid, 'LV-HR45-0001',
           'HR45 巡检夹具请假单', 'PENDING', v_user, '巡检夹具', v_dept
      FROM biz_flow_def d
     WHERE d.status = 'PUBLISHED' AND d.deleted_at IS NULL
       AND d.current_version_id IS NOT NULL
     ORDER BY (d.biz_type = 'HR_LEAVE') DESC, d.flow_code
     LIMIT 1
    ON CONFLICT (id) DO NOTHING;

    IF NOT EXISTS (SELECT 1 FROM biz_flow_instance WHERE id = v_inst) THEN
        RAISE EXCEPTION 'HR45 夹具失败：审批实例没建出来（没有可用的已发布流程）';
    END IF;

    INSERT INTO biz_leave_request (id, org_id, leave_no, employee_id, employee_no, employee_name,
                                   dept_id, leave_type, start_time, end_time, duration_days,
                                   paid, status, reason, approval_instance_id)
    VALUES ('00000000-0000-4000-8000-0000000c4506', v_org, 'LV-HR45-0001', v_emp, 'HR45-001',
            '巡检夹具', v_dept, 'ANNUAL',
            date_trunc('month', CURRENT_DATE) + INTERVAL '9 day',
            date_trunc('month', CURRENT_DATE) + INTERVAL '10 day',
            1, 1, 'SUBMITTED', 'HR45 巡检夹具', v_inst)
    ON CONFLICT (id) DO NOTHING;

    -- ---------- 6) 工资单（POSTED）+ 明细 + 明细项 ----------
    -- 单头恒等式（库层 CHECK 已强制）：gross = earning - pre_deduct；gross = net + withhold
    INSERT INTO biz_salary_sheet (id, org_id, sheet_no, period_code, period_start, period_end,
                                  status, attendance_period_id, employee_count,
                                  earning_total, pre_deduct_total, gross_total,
                                  withhold_total, net_total, si_company_total, hf_company_total,
                                  posted_at)
    VALUES (v_sheet, v_org, 'SAL-HR45-0001', v_period,
            date_trunc('month', CURRENT_DATE)::date,
            (date_trunc('month', CURRENT_DATE) + INTERVAL '1 month - 1 day')::date,
            'POSTED', '00000000-0000-4000-8000-0000000c4504', 1,
            10000, 500, 9500, 1500, 8000, 1600, 400, now())
    ON CONFLICT (id) DO NOTHING;

    INSERT INTO biz_salary_detail (id, org_id, sheet_id, employee_id, employee_no, employee_name,
                                   dept_id, user_id, should_days, actual_days,
                                   earning_amount, pre_deduct_amount, gross_amount,
                                   social_personal, fund_personal, iit_amount, other_withhold,
                                   withhold_amount, net_amount, si_company, hf_company)
    VALUES ('00000000-0000-4000-8000-0000000c4509', v_org, v_sheet, v_emp, 'HR45-001', '巡检夹具',
            v_dept, v_user, 21.75, 21.75,
            10000, 500, 9500, 800, 400, 200, 100, 1500, 8000, 1600, 400)
    ON CONFLICT (id) DO NOTHING;

    -- 明细项 6 行：Σ EARNING(10000) − Σ PRE_DEDUCT(500) = 9500 ⇒ A7 绿
    --              Σ WITHHOLD(800+400+200+100) = 1500 ⇒ A7 绿
    INSERT INTO biz_salary_detail_item (id, org_id, sheet_id, detail_id, item_id, item_code,
                                        item_name, item_type, calc_type, amount, line_no)
    SELECT v.id, v_org, v_sheet, '00000000-0000-4000-8000-0000000c4509'::uuid,
           si.id, v.code, v.name, v.type, v.calc, v.amount, v.line
      FROM (VALUES
            ('00000000-0000-4000-8000-0000000c4510'::uuid, 'BASE',           '基本工资',     'EARNING',    'FIXED',   10000, 1),
            ('00000000-0000-4000-8000-0000000c4511'::uuid, 'ATT_DEDUCT',     '考勤扣款',     'PRE_DEDUCT', 'FORMULA',   500, 2),
            ('00000000-0000-4000-8000-0000000c4512'::uuid, 'SI_PERSONAL',    '社保(个人)',   'WITHHOLD',   'INPUT',     800, 3),
            ('00000000-0000-4000-8000-0000000c4513'::uuid, 'HF_PERSONAL',    '公积金(个人)', 'WITHHOLD',   'INPUT',     400, 4),
            ('00000000-0000-4000-8000-0000000c4514'::uuid, 'IIT',            '个人所得税',   'WITHHOLD',   'INPUT',     200, 5),
            ('00000000-0000-4000-8000-0000000c4515'::uuid, 'OTHER_WITHHOLD', '其他代扣',     'WITHHOLD',   'INPUT',     100, 6)
           ) AS v(id, code, name, type, calc, amount, line)
      LEFT JOIN biz_salary_item si ON si.item_code = v.code AND si.deleted_at IS NULL
    ON CONFLICT (id) DO NOTHING;

    -- ---------- 7) 两张凭证（借贷平衡、来源类型正确、期间存在 ⇒ A8/A9 绿） ----------
    INSERT INTO biz_voucher (id, voucher_no, period, voucher_date, voucher_type, source_type,
                             source_id, source_no, summary, total_debit, total_credit,
                             entry_count, status)
    VALUES ('00000000-0000-4000-8000-0000000c4516', '记-HR45-ACC', v_period, CURRENT_DATE,
            'AUTO', 'PAYROLL_ACCRUAL', v_sheet, 'SAL-HR45-0001', '工资计提（巡检夹具）',
            10000, 10000, 2, 'POSTED'),
           ('00000000-0000-4000-8000-0000000c4517', '记-HR45-PAY', v_period, CURRENT_DATE,
            'AUTO', 'PAYROLL_PAYMENT', v_sheet, 'SAL-HR45-0001', '工资发放（巡检夹具）',
            8000, 8000, 2, 'POSTED')
    ON CONFLICT (id) DO NOTHING;

    INSERT INTO biz_voucher_entry (id, voucher_id, line_no, summary, account_id,
                                   account_code, account_name, debit_amount, credit_amount)
    SELECT v.id, v.voucher, v.line, '巡检夹具', a.id, a.account_code, a.account_name,
           v.dr, v.cr
      FROM (VALUES
            ('00000000-0000-4000-8000-0000000c4520'::uuid, '00000000-0000-4000-8000-0000000c4516'::uuid, 1, '5602.10', 10000::numeric,     0::numeric),
            ('00000000-0000-4000-8000-0000000c4521'::uuid, '00000000-0000-4000-8000-0000000c4516'::uuid, 2, '2211.01',     0::numeric, 10000::numeric),
            ('00000000-0000-4000-8000-0000000c4522'::uuid, '00000000-0000-4000-8000-0000000c4517'::uuid, 1, '2211.01',  8000::numeric,     0::numeric),
            ('00000000-0000-4000-8000-0000000c4523'::uuid, '00000000-0000-4000-8000-0000000c4517'::uuid, 2, '1002',        0::numeric,  8000::numeric)
           ) AS v(id, voucher, line, code, dr, cr)
      JOIN biz_account a ON a.account_code = v.code AND a.deleted_at IS NULL
    ON CONFLICT (id) DO NOTHING;

    -- 单头挂两张凭证（POSTED 单据的必然形态）
    UPDATE biz_salary_sheet
       SET accrual_voucher_id = '00000000-0000-4000-8000-0000000c4516',
           accrual_voucher_no = '记-HR45-ACC',
           payment_voucher_id = '00000000-0000-4000-8000-0000000c4517',
           payment_voucher_no = '记-HR45-PAY'
     WHERE id = v_sheet;

    -- ---------- 8) 单据 ↔ 凭证的来源回指也要对（A8 的进阶形态） ----------
    UPDATE biz_voucher
       SET source_id = v_sheet, source_no = 'SAL-HR45-0001'
     WHERE id IN ('00000000-0000-4000-8000-0000000c4516', '00000000-0000-4000-8000-0000000c4517');

    RAISE NOTICE 'HR45 夹具就绪：员工 1 / 合同 1 / 标准 2 / 考勤 1 / 请假 1 / 工资单 1（POSTED）+ 凭证 2';
END $$;

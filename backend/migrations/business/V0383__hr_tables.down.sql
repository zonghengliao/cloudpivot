-- ============================================================================
-- V0383 down：拆除人力资源底座 10 张表（biz_employee 家族 + 考勤 + 请假 + 薪资）
--
-- 卡：t_65a3dc14（HR-01）—— 回滚方案见设计 §7.3 第 1 行
--
-- # 顺序（不能反）
--   ① 数据护栏：**有业务数据就拒绝回滚**（设计 §7.3 定稿口径）
--        · biz_employee 还有未软删的员工 ⇒ 拒绝（员工档是人事主数据，删了工资/考勤/合同的
--          历史就再也解释不清「这行是谁」）；
--        · biz_salary_sheet 存在 POSTED / REVERSED 的薪资单 ⇒ 拒绝（凭证已经进了账，
--          拆表等于把凭证变成孤儿：借方 5602.* / 贷方 2211.01 平衡不了）；
--        · 已锁定的考勤批次（status = 'LOCKED'）⇒ 拒绝（薪资试算的输入，删了当月无法复算）。
--      护栏只在表存在时生效（重复执行第二遍表已没了，直接跳过）。
--   ② 按 FK 逆序 DROP：明细项目 → 明细 → 薪资单 → 请假 → 考勤记录 → 考勤批次
--                       → 薪资标准 → 劳动合同 → 薪资项 → 员工主档
--   ③ 结尾断言：10 张表确实全没了（否则迁移器会记一个「成功」但库里还留着半张表）。
--
-- # 幂等
--   DROP TABLE IF EXISTS ⇒ 第二遍只打 NOTICE，不报错（重放 / 演习库补跑安全）。
--
-- # 已知边界（写在这里免得下一个人踩）
--   · 本迁移只删**本迁移建的表**：科目 / 号段 / 字典 / 菜单 / 角色 / 脱敏 / 配置 /
--     附件 CHECK 放宽 / 审批流 / 薪资项种子各有自己的迁移（V0384 起），
--     要整体回滚请按版本号**严格逆序**逐个 down，本文件不替它们删数据。
--   · 科目 2211.01 / 5602.* 若已被凭证引用，down 之后凭证仍在（本迁移不删凭证）——
--     这是刻意的：删账不是回滚，是错账，必须走红冲。
-- ============================================================================

-- ---------- ① 数据护栏 ----------
DO $$
DECLARE
    v_n  BIGINT;
    v_bad TEXT;
BEGIN
    -- A. 未删员工
    IF to_regclass('public.biz_employee') IS NOT NULL THEN
        SELECT count(*) INTO v_n FROM biz_employee WHERE deleted_at IS NULL;
        IF v_n > 0 THEN
            RAISE EXCEPTION 'V0383 down 拒绝执行：biz_employee 还有 % 名未删员工（人事主数据）。请先在员工档案页把数据导出/清理，或保留表只回滚种子迁移', v_n;
        END IF;
    END IF;

    -- B. 已过账 / 已红冲的薪资单（凭证已进账，拆表会让分录变成孤儿）
    IF to_regclass('public.biz_salary_sheet') IS NOT NULL THEN
        SELECT string_agg(status || '×' || n, ' / ') INTO v_bad
          FROM (SELECT status, count(*) AS n FROM biz_salary_sheet
                 WHERE deleted_at IS NULL AND status IN ('POSTED','REVERSED')
                 GROUP BY status) s;
        IF v_bad IS NOT NULL THEN
            RAISE EXCEPTION 'V0383 down 拒绝执行：存在已过账/已红冲的薪资单（%）。凭证已进账，请先红冲并作废薪资单再回滚', v_bad;
        END IF;
    END IF;

    -- C. 已锁定的考勤批次（薪资试算的输入）
    IF to_regclass('public.biz_attendance_period') IS NOT NULL THEN
        SELECT count(*) INTO v_n FROM biz_attendance_period
         WHERE deleted_at IS NULL AND status = 'LOCKED';
        IF v_n > 0 THEN
            RAISE EXCEPTION 'V0383 down 拒绝执行：有 % 个月度考勤批次已 LOCKED（薪资试算输入）。请先解锁或确认这些批次不再需要再回滚', v_n;
        END IF;
    END IF;

    RAISE NOTICE 'V0383 down 护栏通过：无未删员工 / 无已过账薪资单 / 无锁定考勤批次';
END $$;

-- ---------- ② 按 FK 逆序拆表 ----------
DROP TABLE IF EXISTS biz_salary_detail_item;
DROP TABLE IF EXISTS biz_salary_detail;
DROP TABLE IF EXISTS biz_salary_sheet;
DROP TABLE IF EXISTS biz_leave_request;
DROP TABLE IF EXISTS biz_attendance_record;
DROP TABLE IF EXISTS biz_attendance_period;
DROP TABLE IF EXISTS biz_employee_salary;
DROP TABLE IF EXISTS biz_employee_contract;
DROP TABLE IF EXISTS biz_salary_item;
DROP TABLE IF EXISTS biz_employee;

-- ---------- ③ 反向自检：零残留 ----------
DO $$
DECLARE
    v_left TEXT;
    v_n    BIGINT;
BEGIN
    SELECT string_agg(t, ', ' ORDER BY t) INTO v_left
      FROM unnest(ARRAY['biz_employee','biz_employee_contract','biz_employee_salary',
                        'biz_attendance_period','biz_attendance_record','biz_leave_request',
                        'biz_salary_item','biz_salary_sheet','biz_salary_detail',
                        'biz_salary_detail_item']) AS t
     WHERE to_regclass('public.' || t) IS NOT NULL;
    IF v_left IS NOT NULL THEN
        RAISE EXCEPTION 'V0383 down 自检失败：仍有表未删掉（%）', v_left;
    END IF;

    -- 索引随表一起消失，但残留的同名索引会让下次建表 IF NOT EXISTS 静默复用错定义 ⇒ 查一遍
    SELECT count(*) INTO v_n FROM pg_indexes
     WHERE schemaname = 'public'
       AND indexname IN ('uk_biz_employee_no','uk_biz_employee_user','uk_biz_employee_idcard',
                         'uk_biz_emp_contract_no','uk_biz_emp_salary','uk_biz_att_period',
                         'uk_biz_att_rec','uk_biz_leave_no','uk_biz_sal_item_code','uk_biz_sal_item_sort',
                         'uk_biz_sal_sheet_no','uk_biz_sal_sheet_period','uk_biz_sal_det','uk_biz_sal_ditem');
    IF v_n > 0 THEN
        RAISE EXCEPTION 'V0383 down 自检失败：仍有 % 条 HR 唯一索引残留', v_n;
    END IF;

    RAISE NOTICE 'V0383 down 完成：10 张 HR 表及其索引、注释已全部删除（零残留）';
END $$;

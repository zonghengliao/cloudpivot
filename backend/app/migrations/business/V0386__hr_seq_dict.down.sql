-- ============================================================================
-- V0386 down：撤销「4 条 HR 号段 + 4 类字典 + 26 项字典值」
--
-- 只回滚本迁移种下的东西：**不删业务单据、不动任何单据号、不改表结构**。
--   ① 数据护栏：已有业务数据在用这些号段 / 字典值 ⇒ 拒绝回滚
--      （删号段 ⇒ 建单取号直接失败；删字典项 ⇒ 界面下拉缺项、存不进岗位/学历/假别）；
--   ② 删本迁移种的 26 个字典项（按 dict_type_id + item_value 清单，不误伤客户自加的项）；
--   ③ 字典类型仅在某类下**已无任何未删项**时才删（否则留壳并 NOTICE 点名）；
--   ④ 删 4 条号段，且**只删 remark 归属本迁移的行**（客户自建同 seq_type 的行一律不动）；
--   ⑤ 反向自检：本迁移种的项 / 类型 / 号段全部不在位（归属为非本迁移的只 NOTICE）。
--
-- # 为什么护栏要按「表里有未删行」判，而不是按「有没有引用到具体值」
--   本仓单据号/工号都是取号器发出去的（`erp::seq` / 单据号生成器），删掉号段后
--   老单据里的号**不会**失效（号是字符串快照），但**新建**会立刻失败：
--   `load_rule` 找不到规则 ⇒ 报「没有可用取号规则（seq_type='EMP'）」。
--   这就是 §7.3「有业务数据就拒绝回滚」要挡的死路：回滚后系统看着正常、一建单就 500。
--
-- # 幂等
--   DELETE 都带条件（行已删 ⇒ 第二遍 0 行）；护栏读的表一直存在，第二遍自然不会命中。
-- ============================================================================

-- ---------- ① 数据护栏：有业务数据在用 ⇒ 拒绝回滚 ----------
-- 说明：表不存在（极端精简库 / 只装了部分模块）⇒ 该条跳过，不把「没这张表」报成数据冲突。
DO $$
DECLARE
    v_blocked text;
BEGIN
    -- EMP：员工档案有未删行 ⇒ 删号段后「新建员工」取不到工号
    IF to_regclass('public.biz_employee') IS NOT NULL THEN
        SELECT string_agg(format('%s×%s', 'biz_employee', n), ' / ')
          INTO v_blocked FROM (SELECT count(*) AS n FROM biz_employee WHERE deleted_at IS NULL) s
         WHERE n > 0;
        IF v_blocked IS NOT NULL THEN
            RAISE EXCEPTION 'V0386 down 拒绝执行：% 条未删员工在用 EMP 工号（号段一删，新建员工取号即失败）。'
                '请先确认要回滚人力资源模块、清理员工档案，或改为只回滚后续 HR 迁移', v_blocked;
        END IF;
    END IF;

    -- LV：请假单有未删行 ⇒ 删号段后「新建请假单」取不到单号
    IF to_regclass('public.biz_leave_request') IS NOT NULL THEN
        SELECT string_agg(format('%s×%s', 'biz_leave_request', n), ' / ')
          INTO v_blocked FROM (SELECT count(*) AS n FROM biz_leave_request WHERE deleted_at IS NULL) s
         WHERE n > 0;
        IF v_blocked IS NOT NULL THEN
            RAISE EXCEPTION 'V0386 down 拒绝执行：% 条未删请假单在用 LV 号段 / hr_leave_type 字典。'
                '请先清理请假单再回滚', v_blocked;
        END IF;
    END IF;

    -- ATT：考勤期间有未删行 ⇒ 删号段后「新建考勤批次」取不到批次号
    IF to_regclass('public.biz_attendance_period') IS NOT NULL THEN
        SELECT string_agg(format('%s×%s', 'biz_attendance_period', n), ' / ')
          INTO v_blocked
          FROM (SELECT count(*) AS n FROM biz_attendance_period WHERE deleted_at IS NULL) s
         WHERE n > 0;
        IF v_blocked IS NOT NULL THEN
            RAISE EXCEPTION 'V0386 down 拒绝执行：% 条未删考勤期间在用 ATT 号段 / hr_position 等字典。'
                '请先清理考勤数据再回滚', v_blocked;
        END IF;
    END IF;

    -- SAL：薪资单有未删行 ⇒ 删号段后「生成本月薪资单」取不到单号
    IF to_regclass('public.biz_salary_sheet') IS NOT NULL THEN
        SELECT string_agg(format('%s×%s', 'biz_salary_sheet', n), ' / ')
          INTO v_blocked FROM (SELECT count(*) AS n FROM biz_salary_sheet WHERE deleted_at IS NULL) s
         WHERE n > 0;
        IF v_blocked IS NOT NULL THEN
            RAISE EXCEPTION 'V0386 down 拒绝执行：% 条未删薪资单在用 SAL 号段。请先清理薪资单再回滚', v_blocked;
        END IF;
    END IF;

    RAISE NOTICE 'V0386 down：4 张业务表（员工/请假/考勤期间/薪资单）均无未删行 —— 允许回滚号段与字典';
END $$;


-- ---------- ② 删 26 个字典项（只删本迁移种下的值，客户自加项保留）----------
DELETE FROM sys_dict_item i
 USING sys_dict_type t
 WHERE i.dict_type_id = t.id
   AND t.id IN ('20000000-0000-4000-8000-00000000d101'::uuid,
                '20000000-0000-4000-8000-00000000d102'::uuid,
                '20000000-0000-4000-8000-00000000d103'::uuid,
                '20000000-0000-4000-8000-00000000d104'::uuid)
   AND (t.dict_code, i.item_value) IN (
        ('hr_position',   'WORKER'), ('hr_position',   'CLERK'),   ('hr_position',   'ENGINEER'),
        ('hr_position',   'SALES'),  ('hr_position',   'MANAGER'),
        ('hr_job_level',  'P1'),     ('hr_job_level',  'P5'),      ('hr_job_level',  'P8'),
        ('hr_job_level',  'M1'),     ('hr_job_level',  'M4'),
        ('hr_education',  'PRIMARY'),('hr_education',  'JUNIOR'),  ('hr_education',  'SENIOR'),
        ('hr_education',  'COLLEGE'),('hr_education',  'BACHELOR'),('hr_education',  'MASTER'),
        ('hr_education',  'DOCTOR'),
        ('hr_leave_type', 'ANNUAL'), ('hr_leave_type', 'SICK'),    ('hr_leave_type', 'PERSONAL'),
        ('hr_leave_type', 'MARRIAGE'),('hr_leave_type','MATERNITY'),('hr_leave_type','PATERNITY'),
        ('hr_leave_type', 'BEREAVEMENT'),('hr_leave_type','COMPENSATORY'),('hr_leave_type','OTHER')
       );


-- ---------- ③ 删字典类型（仅当该类下已无任何未删项，避免留空壳/误删客户新增项）----------
DO $$
DECLARE
    v_kept text;
BEGIN
    SELECT string_agg(format('%s(剩 %s 项)', t.dict_code, c.n), ' / ')
      INTO v_kept
      FROM sys_dict_type t
      JOIN (SELECT dict_type_id, count(*) AS n
              FROM sys_dict_item
             WHERE deleted_at IS NULL
             GROUP BY dict_type_id) c ON c.dict_type_id = t.id
     WHERE t.id IN ('20000000-0000-4000-8000-00000000d101'::uuid,
                    '20000000-0000-4000-8000-00000000d102'::uuid,
                    '20000000-0000-4000-8000-00000000d103'::uuid,
                    '20000000-0000-4000-8000-00000000d104'::uuid);

    IF v_kept IS NOT NULL THEN
        RAISE NOTICE 'V0386 down：以下字典类型被客户加过项，保留类型不删 —— %', v_kept;
    END IF;

    DELETE FROM sys_dict_type t
     WHERE t.id IN ('20000000-0000-4000-8000-00000000d101'::uuid,
                    '20000000-0000-4000-8000-00000000d102'::uuid,
                    '20000000-0000-4000-8000-00000000d103'::uuid,
                    '20000000-0000-4000-8000-00000000d104'::uuid)
       AND NOT EXISTS (SELECT 1 FROM sys_dict_item i
                        WHERE i.dict_type_id = t.id AND i.deleted_at IS NULL);
END $$;


-- ---------- ④ 删 4 条号段（只删 remark 归属本迁移的行）----------
DO $$
DECLARE
    v_kept text;
BEGIN
    SELECT string_agg(format('%s(remark=%s)', s.seq_type, coalesce(s.remark, '<空>')), ' / ')
      INTO v_kept
      FROM biz_seq s
     WHERE s.seq_type IN ('EMP', 'LV', 'ATT', 'SAL')
       AND s.deleted_at IS NULL
       AND coalesce(s.remark, '') NOT LIKE 'ARCH-9-2 HR 号段（V0386）%';

    IF v_kept IS NOT NULL THEN
        RAISE NOTICE 'V0386 down：以下 HR 号段不是本迁移种下的（客户自建 / 别处种），保留不删 —— %', v_kept;
    END IF;

    DELETE FROM biz_seq s
     WHERE s.seq_type IN ('EMP', 'LV', 'ATT', 'SAL')
       AND coalesce(s.remark, '') LIKE 'ARCH-9-2 HR 号段（V0386）%';
END $$;


-- ---------- ⑤ 反向自检：本迁移种的东西不能残留 ----------
DO $$
DECLARE
    v_n integer;
BEGIN
    SELECT count(*) INTO v_n FROM biz_seq
     WHERE seq_type IN ('EMP', 'LV', 'ATT', 'SAL')
       AND deleted_at IS NULL
       AND coalesce(remark, '') LIKE 'ARCH-9-2 HR 号段（V0386）%';
    IF v_n <> 0 THEN
        RAISE EXCEPTION 'V0386 down 自检失败：本迁移种的 HR 号段仍有 % 条残留', v_n;
    END IF;

    SELECT count(*) INTO v_n FROM sys_dict_item i
      JOIN sys_dict_type t ON t.id = i.dict_type_id
     WHERE i.deleted_at IS NULL
       AND t.id IN ('20000000-0000-4000-8000-00000000d101'::uuid,
                    '20000000-0000-4000-8000-00000000d102'::uuid,
                    '20000000-0000-4000-8000-00000000d103'::uuid,
                    '20000000-0000-4000-8000-00000000d104'::uuid)
       AND (t.dict_code, i.item_value) IN (
            ('hr_position',   'WORKER'), ('hr_position',   'CLERK'),   ('hr_position',   'ENGINEER'),
            ('hr_position',   'SALES'),  ('hr_position',   'MANAGER'),
            ('hr_job_level',  'P1'),     ('hr_job_level',  'P5'),      ('hr_job_level',  'P8'),
            ('hr_job_level',  'M1'),     ('hr_job_level',  'M4'),
            ('hr_education',  'PRIMARY'),('hr_education',  'JUNIOR'),  ('hr_education',  'SENIOR'),
            ('hr_education',  'COLLEGE'),('hr_education',  'BACHELOR'),('hr_education',  'MASTER'),
            ('hr_education',  'DOCTOR'),
            ('hr_leave_type', 'ANNUAL'), ('hr_leave_type', 'SICK'),    ('hr_leave_type', 'PERSONAL'),
            ('hr_leave_type', 'MARRIAGE'),('hr_leave_type','MATERNITY'),('hr_leave_type','PATERNITY'),
            ('hr_leave_type', 'BEREAVEMENT'),('hr_leave_type','COMPENSATORY'),('hr_leave_type','OTHER')
           );
    IF v_n <> 0 THEN
        RAISE EXCEPTION 'V0386 down 自检失败：本迁移种的字典项仍有 % 条残留', v_n;
    END IF;

    RAISE NOTICE 'V0386 down 完成：HR 号段 4 条 + 字典 4 类 + 26 项已撤销，零残留';
END $$;

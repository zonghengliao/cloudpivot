-- ============================================================================
-- V0386：人力资源模块底座 · 4 条号段 + 4 类字典 + 26 项字典值（ARCH-9-2 §3.3 ②③）
--
-- 卡：t_7799d567（HR-03【HR】迁移 V0380）
-- 依据：docs/architecture/ARCH-9-人力资源模块设计.md
--       §2 D-15（编号口径：工号 EMP-0001 永久不重置；单据号 LV/ATT/SAL 按日/按月滚动）、
--       §3.3 ②（biz_seq 4 条号段 SQL）、§3.3 ③（4 类字典 26 项）、
--       §3.5（biz_leave_request.leave_type 与字典 hr_leave_type 一一对应）、
--       §7.1（迁移清单）、§7.3（回滚方案）、§8.1（拆卡清单 HR-03）。
--       字典值与真实 DDL 证据脚本 03 段逐字一致：
--       docs/evidence/2026-11-10-arch9-2-hr-ddl/02_alter_tables_check.sql（24 条断言 RC=0）
--
-- # 号段为什么是 V0386，而不是卡面 / 设计稿 §7.1 写的 V0380 / V0385
--   本仓迁移版本号**跨目录全局唯一**（nik loader「版本号跨目录全局唯一」校验，
--   重号 ⇒ 服务启动直接失败，AGENTS.md 约定 6：动手前先 ls 核对号段）。已发生的两次实占：
--     ① 成本核算（已提交）：V0378__cost_menu_and_perm / V0379__cost_seq_and_dict /
--        V0380__cost_lot_backfill / V0381__cost_flow_def / V0382__stock_in_production_sourceless
--        ⇒ HR 全系列整体后移 5 号（HR-01 落地 V0383，HR-02 落地 V0384，见两卡头注释）；
--     ② 成本灰度开关（COST-B4-4）：V0385__cost_module_switch（t_e757c1e7，已落入工作区）
--        ⇒ HR-03 及其后的 HR 系列再后移 1 号：本卡 V0386。
--   映射（本次定稿，同步回填设计文档 §7.1/§8.1 与相关卡 comment）：
--     HR-03 V0386 hr_seq_dict（本卡）        HR-04 V0387 hr_menu_perms
--     HR-05 V0388 hr_mask_config + V0389 hr_attachment_doc_types
--     批3   V0390 hr_leave_flow / V0391 hr_salary_flow / V0392 hr_adjust_resign_flow
--     批5   V0393 hr_salary_items_seed / V0394 hr_selfcheck
--
-- # 范围（只写 3 张既有表，不建表、不改表结构、不动业务数据）
--   biz_seq           +4 行（EMP / LV / ATT / SAL）
--   sys_dict_type     +4 行（hr_position / hr_job_level / hr_education / hr_leave_type）
--   sys_dict_item     +26 行（5 / 5 / 7 / 9）
--   不出现 CREATE / ALTER / DROP / TRUNCATE，不与任何存量表改造卡撞车。
--
-- # 幂等（重复执行结果不变）
--   号段：`WHERE NOT EXISTS (seq_type = v.t AND deleted_at IS NULL)`；
--   字典类型：固定 UUID（20000000-0000-4000-8000-00000000d1xx）+ `ON CONFLICT (id) DO NOTHING`；
--   字典项：`WHERE NOT EXISTS (dict_type_id, item_value AND deleted_at IS NULL)`。
--   末尾自检是纯读（不插探针行）⇒ 第二遍执行零残留、零副作用。
--
-- # 为什么 pad_width 要显式写
--   `biz_seq.pad_width`（V0337 加列，DEFAULT 4）只有**主数据码**取号器
--   `erp::seq::next_master_code*` 会用（`format_master_code`）。EMP 是主数据码 ⇒ 4 位（EMP-0001）；
--   LV/ATT/SAL 是**单据号**，生成器不走 pad_width（同 PO/IN：库里 pad_width=4 但单号是 6 位），
--   这里按设计写 6，使「列里的值」与「实际单号形态」一致，避免二开时误读。
--   老库若还没有 pad_width 列（V0337 之前建的库），DO 块自动降级为不带该列并 NOTICE。
--
-- # 自检为什么写进迁移（不是可选项）
--   本仓迁移「文件内不写 BEGIN/COMMIT、由迁移器开事务」：自检失败 ⇒ 整笔回滚 ⇒ 不存在
--   「种了一半」。要拦的是**不报错但会算错**的静默错误：
--     · 号段少一条 ⇒ HR-14 工号取号 / LV/ATT/SAL 建单时 `load_rule` 抛「没有可用取号规则」；
--     · EMP 写成按日滚动（period_key <> GLOBAL）⇒ 工号每天从 0001 重发，必然撞 uk_biz_employee_no；
--     · 字典项少一条（如漏 COMPENSATORY 调休）⇒ 前端 DictSelect 里没有该项，请假单存不进去
--       （`ck_biz_leave_type` 放行但界面选不到 ⇒ 用户以为系统丢了功能）；
--     · hr_leave_type 与 `ck_biz_leave_type` 值集不一致 ⇒ 字典里能选、落库被 CHECK 拒（或反之）。
-- ============================================================================

-- ---------- ① 4 条号段（EMP 主数据码不重置；LV/ATT/SAL 单据号滚动）----------
--
-- 语义（§2 D-15，代码侧见 erp/seq.rs 模块文档「与单据号的区别」）：
--   EMP  date_pattern='none'   ⇒ period_key 固定 'GLOBAL'，流水永不归零（工号是人事档案码）
--   LV   date_pattern='yyyyMMdd' ⇒ 按日归零（LV-20261111-000001）
--   ATT  date_pattern='yyyyMM'   ⇒ 按月归零（ATT-202611-000001）
--   SAL  date_pattern='yyyyMM'   ⇒ 按月归零（SAL-202611-000001）
--
-- remark 是本迁移的归属标记：down 只删「remark 归属本迁移」的行，
-- 客户自己建的 / 别处种的同 seq_type 行一律不动（见 V0386__hr_seq_dict.down.sql）。
DO $$
DECLARE
    v_has_org   boolean;
    v_has_pad   boolean;
    v_org_col   text;
    v_org_val   text;
    v_pad_col   text;
    v_pad_val   text;
    v_sql       text;
BEGIN
    SELECT EXISTS (SELECT 1 FROM information_schema.columns
                    WHERE table_schema = 'public' AND table_name = 'biz_seq'
                      AND column_name = 'org_id') INTO v_has_org;
    SELECT EXISTS (SELECT 1 FROM information_schema.columns
                    WHERE table_schema = 'public' AND table_name = 'biz_seq'
                      AND column_name = 'pad_width') INTO v_has_pad;

    IF NOT v_has_pad THEN
        RAISE NOTICE 'V0386 提示：biz_seq 没有 pad_width 列（V0337 未应用？）—— 号段降级为不带位数，'
            'EMP 取号将按列默认/兜底位数补零';
    END IF;

    v_org_col := CASE WHEN v_has_org THEN ', org_id' ELSE '' END;
    -- ARCH-9-1 之后 biz_seq 的唯一键变成 (org_id, seq_type)：默认公司常量见 V0383 的 org_id 约定
    v_org_val := CASE WHEN v_has_org
                      THEN ', ''00000000-0000-4000-8000-000000000001''::uuid' ELSE '' END;
    v_pad_col := CASE WHEN v_has_pad THEN ', pad_width' ELSE '' END;
    v_pad_val := CASE WHEN v_has_pad THEN ', v.pw' ELSE '' END;

    v_sql :=
        'INSERT INTO biz_seq (seq_type, seq_name, prefix, date_pattern, period_key, '
        || 'current_no, step, separator, status, remark' || v_pad_col || v_org_col || ') '
        || 'SELECT v.t, v.n, v.p, v.d, v.pk, 0, 1, ''-'', 1, v.rm' || v_pad_val || v_org_val
        || '  FROM (VALUES'
        || '    (''EMP'', ''员工工号'',   ''EMP'', ''none'',     ''GLOBAL'', 4, '
        || '''ARCH-9-2 HR 号段（V0386）：工号是主数据码，永久不重置''),'
        || '    (''LV'',  ''请假单号'',   ''LV'',  ''yyyyMMdd'', '''',       6, '
        || '''ARCH-9-2 HR 号段（V0386）：按日滚动 LV-yyyyMMdd-000001''),'
        || '    (''ATT'', ''考勤批次号'', ''ATT'', ''yyyyMM'',   '''',       6, '
        || '''ARCH-9-2 HR 号段（V0386）：按月滚动 ATT-yyyyMM-000001''),'
        || '    (''SAL'', ''薪资单号'',   ''SAL'', ''yyyyMM'',   '''',       6, '
        || '''ARCH-9-2 HR 号段（V0386）：按月滚动 SAL-yyyyMM-000001'')'
        || '  ) AS v(t, n, p, d, pk, pw, rm) '
        || ' WHERE NOT EXISTS (SELECT 1 FROM biz_seq s '
        || '                      WHERE s.seq_type = v.t AND s.deleted_at IS NULL)';

    EXECUTE v_sql;
END $$;


-- ---------- ② 字典类型 4 类（固定 UUID ⇒ down 能精确定位、重复执行不变）----------
-- 固定段沿用本仓既有习惯（20000000-0000-4000-8000-…）：
--   d101 hr_position / d102 hr_job_level / d103 hr_education / d104 hr_leave_type
INSERT INTO sys_dict_type (id, dict_code, dict_name, description, status, sort_order, remark)
VALUES
    ('20000000-0000-4000-8000-00000000d101'::uuid, 'hr_position',  '岗位',
     '员工档案-岗位（biz_employee.position_code）', 1, 10, 'ARCH-9-2 HR 字典（V0386）'),
    ('20000000-0000-4000-8000-00000000d102'::uuid, 'hr_job_level', '职级',
     '员工档案-职级（biz_employee.job_level，P1~P9 / M1~M4）', 1, 20, 'ARCH-9-2 HR 字典（V0386）'),
    ('20000000-0000-4000-8000-00000000d103'::uuid, 'hr_education', '学历',
     '员工档案-学历（biz_employee.education）', 1, 30, 'ARCH-9-2 HR 字典（V0386）'),
    ('20000000-0000-4000-8000-00000000d104'::uuid, 'hr_leave_type', '请假类型',
     '请假单-类型（与 biz_leave_request.leave_type 的 CHECK 一一对应）', 1, 40, 'ARCH-9-2 HR 字典（V0386）')
ON CONFLICT (id) DO NOTHING;


-- ---------- ③ 字典项 26 项（5 岗位 / 5 职级 / 7 学历 / 9 请假类型）----------
-- 判定幂等用 (dict_type_id, item_value, deleted_at IS NULL)，不是主键：
-- 客户若曾手工建过同值项（软删状态），这里会再种一条有效的，语义仍成立。
INSERT INTO sys_dict_item (id, dict_type_id, item_value, item_label, sort_order, status)
SELECT gen_random_uuid(), t.id, v.val, v.label, v.sort, 1
  FROM (VALUES
        -- 岗位 5 项
        ('hr_position',  'WORKER',      '车间工人', 10),
        ('hr_position',  'CLERK',       '文员',     20),
        ('hr_position',  'ENGINEER',    '工程师',   30),
        ('hr_position',  'SALES',       '销售',     40),
        ('hr_position',  'MANAGER',     '经理',     50),
        -- 职级 5 项
        ('hr_job_level', 'P1',          'P1',       10),
        ('hr_job_level', 'P5',          'P5',       20),
        ('hr_job_level', 'P8',          'P8',       30),
        ('hr_job_level', 'M1',          'M1',       40),
        ('hr_job_level', 'M4',          'M4',       50),
        -- 学历 7 项（小学 … 博士）
        ('hr_education', 'PRIMARY',     '小学',     10),
        ('hr_education', 'JUNIOR',      '初中',     20),
        ('hr_education', 'SENIOR',      '高中',     30),
        ('hr_education', 'COLLEGE',     '大专',     40),
        ('hr_education', 'BACHELOR',    '本科',     50),
        ('hr_education', 'MASTER',      '硕士',     60),
        ('hr_education', 'DOCTOR',      '博士',     70),
        -- 请假类型 9 项（值集 = ck_biz_leave_type，逐字一致）
        ('hr_leave_type', 'ANNUAL',      '年假',     10),
        ('hr_leave_type', 'SICK',        '病假',     20),
        ('hr_leave_type', 'PERSONAL',    '事假',     30),
        ('hr_leave_type', 'MARRIAGE',    '婚假',     40),
        ('hr_leave_type', 'MATERNITY',   '产假',     50),
        ('hr_leave_type', 'PATERNITY',   '陪产假',   60),
        ('hr_leave_type', 'BEREAVEMENT', '丧假',     70),
        ('hr_leave_type', 'COMPENSATORY', '调休',     80),
        ('hr_leave_type', 'OTHER',       '其他',     90)
       ) AS v(dict, val, label, sort)
  JOIN sys_dict_type t ON t.id IN ('20000000-0000-4000-8000-00000000d101'::uuid,
                                   '20000000-0000-4000-8000-00000000d102'::uuid,
                                   '20000000-0000-4000-8000-00000000d103'::uuid,
                                   '20000000-0000-4000-8000-00000000d104'::uuid)
                      AND t.dict_code = v.dict
                      AND t.deleted_at IS NULL
 WHERE NOT EXISTS (SELECT 1 FROM sys_dict_item i
                    WHERE i.dict_type_id = t.id
                      AND i.item_value = v.val
                      AND i.deleted_at IS NULL);


-- ---------- ④ 自检（纯读；任一不成立 ⇒ RAISE EXCEPTION ⇒ 整笔迁移回滚）----------
DO $$
DECLARE
    v_n          integer;
    v_bad        text;
    v_has_pad    boolean;
    v_ck_values  text[];
    v_dict_values text[];
BEGIN
    -- ① 4 条号段都在，且 prefix / date_pattern / period_key 与设计口径一致
    SELECT count(*) INTO v_n
      FROM biz_seq
     WHERE seq_type IN ('EMP', 'LV', 'ATT', 'SAL') AND deleted_at IS NULL;
    IF v_n <> 4 THEN
        RAISE EXCEPTION 'V0386 自检失败：HR 号段只有 % 条（应为 4：EMP/LV/ATT/SAL）—— '
            'HR 建单取号会直接报「没有可用取号规则」', v_n;
    END IF;

    SELECT string_agg(format('%s(prefix=%s,date=%s,period=%s,status=%s)',
                             s.seq_type, s.prefix, s.date_pattern, s.period_key, s.status),
                      ' / ')
      INTO v_bad
      FROM biz_seq s
      JOIN (VALUES ('EMP', 'EMP', 'none',     'GLOBAL', 1),
                   ('LV',  'LV',  'yyyyMMdd', '',       1),
                   ('ATT', 'ATT', 'yyyyMM',   '',       1),
                   ('SAL', 'SAL', 'yyyyMM',   '',       1)
           ) AS d(t, prefix, date_pattern, period_key, status) ON d.t = s.seq_type
     WHERE s.deleted_at IS NULL
       AND (s.prefix <> d.prefix
            OR s.date_pattern <> d.date_pattern
            OR coalesce(s.period_key, '') <> d.period_key
            OR s.status <> d.status);
    IF v_bad IS NOT NULL THEN
        RAISE EXCEPTION 'V0386 自检失败：HR 号段配置与设计不符 —— %。'
            'EMP 尤其不能按日/月滚动（period_key 必须 GLOBAL，否则工号每天从 0001 重发、'
            '撞 uk_biz_employee_no）；LV/ATT/SAL 的日期段决定单号形态。'
            '若是客户自行改过，请人工确认后调整本迁移的断言口径', v_bad;
    END IF;

    -- ② 位数：EMP 4 位（EMP-0001）；LV/ATT/SAL 6 位（与单据号实际形态一致）
    --    pad_width 列不存在（V0337 之前的库）⇒ 整段跳过，只提示
    SELECT EXISTS (SELECT 1 FROM information_schema.columns
                    WHERE table_schema = 'public' AND table_name = 'biz_seq'
                      AND column_name = 'pad_width') INTO v_has_pad;
    IF v_has_pad THEN
        SELECT string_agg(format('%s(pad_width=%s)', s.seq_type, s.pad_width), ' / ')
          INTO v_bad
          FROM biz_seq s
          JOIN (VALUES ('EMP', 4), ('LV', 6), ('ATT', 6), ('SAL', 6)) AS d(t, pw)
            ON d.t = s.seq_type
         WHERE s.deleted_at IS NULL AND s.pad_width <> d.pw;
        IF v_bad IS NOT NULL THEN
            RAISE EXCEPTION 'V0386 自检失败：HR 号段补零位数不对 —— %（应为 EMP=4 / LV=6 / ATT=6 / SAL=6）',
                v_bad;
        END IF;
    ELSE
        RAISE NOTICE 'V0386 提示：biz_seq 无 pad_width 列 —— 跳过位数断言（EMP 取号将用兜底位数）';
    END IF;

    -- ③ 4 类字典都在（固定 UUID + dict_code 双条件，防被客户改成同码不同 id）
    SELECT count(*) INTO v_n
      FROM sys_dict_type
     WHERE deleted_at IS NULL
       AND id IN ('20000000-0000-4000-8000-00000000d101'::uuid,
                  '20000000-0000-4000-8000-00000000d102'::uuid,
                  '20000000-0000-4000-8000-00000000d103'::uuid,
                  '20000000-0000-4000-8000-00000000d104'::uuid);
    IF v_n <> 4 THEN
        RAISE EXCEPTION 'V0386 自检失败：HR 字典类型只有 % 类（应为 4：position/job_level/'
            'education/leave_type）—— 前端 DictSelect 会拿不到选项、员工档案存不进岗位/学历', v_n;
    END IF;

    -- ④ 字典项数 5 / 5 / 7 / 9 = 26
    SELECT string_agg(format('%s=%s(应 %s)', d.dict_code, coalesce(c.n, 0), d.expect_n), ' / ')
      INTO v_bad
      FROM (VALUES ('hr_position', 5), ('hr_job_level', 5),
                   ('hr_education', 7), ('hr_leave_type', 9)) AS d(dict_code, expect_n)
      LEFT JOIN (
            SELECT t.dict_code, count(*) AS n
              FROM sys_dict_item i
              JOIN sys_dict_type t ON t.id = i.dict_type_id
             WHERE i.deleted_at IS NULL AND t.deleted_at IS NULL
               AND t.id IN ('20000000-0000-4000-8000-00000000d101'::uuid,
                            '20000000-0000-4000-8000-00000000d102'::uuid,
                            '20000000-0000-4000-8000-00000000d103'::uuid,
                            '20000000-0000-4000-8000-00000000d104'::uuid)
             GROUP BY t.dict_code
      ) c ON c.dict_code = d.dict_code
     WHERE coalesce(c.n, 0) <> d.expect_n;
    IF v_bad IS NOT NULL THEN
        RAISE EXCEPTION 'V0386 自检失败：字典项数不对 —— %。缺项会让界面选不到值（如漏 COMPENSATORY '
            '「调休」⇒ 请假单存不进去）', v_bad;
    END IF;

    -- ⑤ 字典项无重复值（同类型同名值两条 ⇒ DictSelect 出现重复选项）
    SELECT string_agg(format('%s:%s×%s', t.dict_code, i.item_value, i.n), ' / ')
      INTO v_bad
      FROM (SELECT dict_type_id, item_value, count(*) AS n
              FROM sys_dict_item
             WHERE deleted_at IS NULL
             GROUP BY dict_type_id, item_value
            HAVING count(*) > 1) i
      JOIN sys_dict_type t ON t.id = i.dict_type_id
     WHERE t.id IN ('20000000-0000-4000-8000-00000000d101'::uuid,
                    '20000000-0000-4000-8000-00000000d102'::uuid,
                    '20000000-0000-4000-8000-00000000d103'::uuid,
                    '20000000-0000-4000-8000-00000000d104'::uuid);
    IF v_bad IS NOT NULL THEN
        RAISE EXCEPTION 'V0386 自检失败：字典项重复 —— %（界面会出现重复选项）', v_bad;
    END IF;

    -- ⑥ hr_leave_type 与 biz_leave_request.leave_type 的 CHECK 值集**互为子集**
    --    （§3.3 ③「与 biz_leave_request.leave_type 的 CHECK 一一对应」：
    --      字典里有而 CHECK 没有 ⇒ 界面能选、落库被拒；CHECK 有而字典没有 ⇒ 数据能存、界面显示裸值）
    --    表不在（本迁移只依赖 V0383，正常一定在；极端精简库跳过）⇒ 只提示
    IF to_regclass('public.biz_leave_request') IS NULL THEN
        RAISE NOTICE 'V0386 提示：biz_leave_request 不存在 —— 跳过请假类型字典与 CHECK 的对齐断言';
    ELSE
        SELECT array_agg(DISTINCT m[1]) INTO v_ck_values
          FROM pg_constraint c,
               LATERAL regexp_matches(pg_get_constraintdef(c.oid), '''([A-Z_]+)''', 'g') AS m
         WHERE c.conrelid = 'public.biz_leave_request'::regclass
           AND c.conname = 'ck_biz_leave_type';
        IF v_ck_values IS NULL THEN
            RAISE EXCEPTION 'V0386 自检失败：biz_leave_request 上找不到 ck_biz_leave_type 约束 —— '
                '请假类型的 CHECK 契约没了，字典与落库口径无法互校（V0383 应建过该约束）';
        END IF;

        SELECT array_agg(i.item_value ORDER BY i.item_value) INTO v_dict_values
          FROM sys_dict_item i
         WHERE i.deleted_at IS NULL
           AND i.dict_type_id = '20000000-0000-4000-8000-00000000d104'::uuid;

        SELECT string_agg(x, ', ') INTO v_bad
          FROM (SELECT unnest(v_dict_values) AS x
                EXCEPT
                SELECT unnest(v_ck_values)) s;
        IF v_bad IS NOT NULL THEN
            RAISE EXCEPTION 'V0386 自检失败：字典 hr_leave_type 有 CHECK 不允许的值 —— %。'
                '界面能选、落库会被 ck_biz_leave_type 拒', v_bad;
        END IF;

        SELECT string_agg(x, ', ') INTO v_bad
          FROM (SELECT unnest(v_ck_values) AS x
                EXCEPT
                SELECT unnest(v_dict_values)) s;
        IF v_bad IS NOT NULL THEN
            RAISE EXCEPTION 'V0386 自检失败：ck_biz_leave_type 允许的值字典里没有 —— %。'
                '这类假别落库后界面只能显示裸英文（AGENTS.md 约定：字典驱动的下拉不许硬编码）', v_bad;
        END IF;
    END IF;

    RAISE NOTICE 'V0386 自检通过：4 条号段（EMP 4 位不重置 / LV·ATT·SAL 6 位滚动）+ '
        '4 类字典 + 26 项字典值（5/5/7/9）+ 请假类型与 ck_biz_leave_type 双向一致';
    RAISE NOTICE 'V0386 完成：biz_seq +4 / sys_dict_type +4 / sys_dict_item +26（ARCH-9-2 §3.3 ②③）';
END $$;

-- ============================================================================
-- V0398：人力资源模块 · 收口自检（ARCH-9-2 §7.1「hr_selfcheck」/ §8.1 HR-45）
--
-- 卡：t_914199ef（HR-45【HR】迁移 V0388 收口自检 + scripts/verify/hr_invariants.sql）
-- 依据：docs/architecture/ARCH-9-人力资源模块设计.md §7.1（迁移文件清单）、
--       §7.5（DDL 真库跑通）、§8.1（卡清单）
--
-- # 号段为什么是 V0398，而不是卡面写的 V0388 / 设计 §8.1 写的 V0397
--   设计稿给人力资源预留 V0378~V0388，历经三次整体后移（V0385 成本灰度开关、
--   V0390~V0392 成本菜单/结算/考勤锁守卫相继被实占），本仓迁移号**跨目录全局唯一**，
--   重号 ⇒ 服务启动直接失败（AGENTS.md 约定 6）。落到本卡时已占用的号：
--     V0393 hr_leave_flow（HR-26）/ V0394 hr_salary_flow（HR-38）/
--     V0395 hr_adjust_resign_flow（HR-38）/ V0396 hr_salary_items_seed（HR-35）/
--     V0397 hr_salary_period_after_reverse（**HR-40 红冲**，设计 §8.1 原写本卡用 V0397，
--     但 HR-40 先落地并实占 —— 先落地者定号）
--   ⇒ 收口自检取 **V0398**。设计文档 §7.1 / §8.1 已回填本行的实际号。
--
-- # 本迁移做什么（只有一个 DO 块，纯读断言，零写操作）
--   把「人力资源底座是否种齐」变成一个**原子承诺**：前面 12 条 HR 迁移里任何一条
--   在某类库（空库 / 老库 / 克隆库）上静默没种进去，本迁移立刻 RAISE EXCEPTION ⇒
--   整笔回滚 ⇒ 本次升级失败并指出缺哪一项，而不是「表在、科目缺、过账时才发现
--   借方科目解析为空」。
--
--   为什么不靠「前面的迁移自己断言」：本仓迁移是「文件内不写 BEGIN/COMMIT、由迁移器
--   开事务」，每条迁移只看得见自己种的东西。跨迁移的完整性（科目 ↔ 薪资项 ↔ 凭证规则
--   ↔ 菜单权限）只有收口点能验，照 V0378/V0226 末尾 `DO $$ … RAISE EXCEPTION` 的既有做法。
--
--   8 组断言（缺一即红，全部列出缺什么，不报「第一处」就停）：
--     ① 10 张业务表（V0383）
--     ② 9 个末级科目 + 4 个父科目翻非末级 + 2 条工资凭证规则（V0384）
--     ③ 4 条号段 + 4 类字典 + 26 项字典值（V0386）
--     ④ 36 条菜单（1 目录 + 5 页面 + 30 按钮）+ HR_MANAGER 角色与授权（V0387）
--     ⑤ 2 条字段脱敏 + 3 条 sys_config（V0388）
--     ⑥ 4 套审批流程（V0393/V0394/V0395）
--     ⑦ 17 个薪资项（V0396）
--     ⑧ 汇总：打印通过条数
--
-- # 幂等（重复应用结果不变）
--   纯 `DO` 块 + 只读 SELECT / to_regclass，无 INSERT/UPDATE/DELETE/DDL，
--   重复执行结果恒等；不需要 `.down.sql` 做反向，见 V0398__hr_selfcheck.down.sql。
--
-- # 为什么「流程」与「薪资项」按 biz_type / item_code 查，而不按固定 id 查
--   V0393/V0394/V0395 的种子带**客户配置保留**守卫：库里若已有同 `flow_code` 或同
--   `biz_type` 的默认流程，种子主动跳过（保留客户自建流程，设计 §7.1 的幂等策略）；
--   V0396 同理（客户已占用同 item_code 则不重复种）。
--   若这里按固定 id 断言，那些**合法跳过**的库会被本自检判红 ⇒ 升级失败。
--   故断言口径 = 「这个 biz_type 有一个未删、已发布、默认的流程」与「这个 item_code
--   有一行未删的薪资项」—— 既容得下客户自建，也拦得住「压根没种」。
--
-- # 与 scripts/verify/hr_invariants.sql 的分工（别混）
--   本迁移 = **一次性**（应用时跑一次，缺种子就拦升级）——静态种子完整性。
--   巡检脚本 = **可反复跑**（生产/演练库随时跑，查数据一致性 A1~A10）——单据级不变式。
--   两者互补：本迁移管「配齐」，巡检管「算对」。
-- ============================================================================

DO $$
DECLARE
    v_missing   text[] := ARRAY[]::text[];
    v_n         integer;
    v_bad       text;

    -- ① 10 张表（V0383）
    c_tables    constant text[] := ARRAY[
        'biz_employee', 'biz_employee_contract', 'biz_employee_salary',
        'biz_attendance_period', 'biz_attendance_record', 'biz_leave_request',
        'biz_salary_item', 'biz_salary_sheet', 'biz_salary_detail',
        'biz_salary_detail_item'];

    -- ② 9 个末级科目 + 其应有父科目（V0384）
    --    两个平行数组（PL/pgSQL 的 `text[][2]` 声明在部分 PG 版本上不可用，别写二维）
    c_acc_code  constant text[] := ARRAY[
        '2211.01', '2211.02', '2211.03', '2221.03', '2241.03',
        '2241.04', '5602.10', '5602.11', '5602.12'];
    c_acc_parent constant text[] := ARRAY[
        '2211', '2211', '2211', '2221', '2241',
        '2241', '5602', '5602', '5602'];
    c_parents   constant text[] := ARRAY['2211', '2221', '2241', '5602'];
    c_rules     constant text[] := ARRAY['PAYROLL_ACCRUAL_DEFAULT', 'PAYROLL_PAYMENT_DEFAULT'];

    -- ③ 号段（V0386）：seq_type → (prefix, date_pattern, period_key, pad_width)
    c_seqs      constant text[] := ARRAY['EMP', 'LV', 'ATT', 'SAL'];
    -- ③ 字典（V0386）：dict_code → 期望项数（5+5+7+9 = 26）
    c_dicts     constant text[] := ARRAY['hr_position', 'hr_job_level', 'hr_education', 'hr_leave_type'];
    c_dict_cnt  constant integer[] := ARRAY[5, 5, 7, 9];

    -- ④ 菜单（V0387）：1 目录 + 5 页面（固定 id）+ 30 按钮 = 36
    c_menu_dir  constant uuid := '20000000-0000-4000-8000-00000000e000';
    c_menu_pages constant uuid[] := ARRAY[
        '20000000-0000-4000-8000-00000000e001', '20000000-0000-4000-8000-00000000e002',
        '20000000-0000-4000-8000-00000000e003', '20000000-0000-4000-8000-00000000e004',
        '20000000-0000-4000-8000-00000000e005']::uuid[];
    c_page_paths constant text[] := ARRAY[
        '/business/hr', '/business/hr/employee', '/business/hr/attendance',
        '/business/hr/leave', '/business/hr/salary', '/business/hr/salary-item'];

    -- ⑥ 4 套流程（V0393/V0394/V0395）：biz_type（flow_code 见括号）
    --    hr_leave(hr_leave) / hr_salary(hr_salary) /
    --    hr_salary_adjust(hr_salary_adjust) / hr_employee_resign(hr_employee_resign)
    c_biz_types constant text[] := ARRAY[
        'HR_LEAVE', 'HR_SALARY', 'HR_SALARY_ADJUST', 'HR_EMPLOYEE_RESIGN'];

    -- ⑦ 17 个薪资项（V0396）
    c_items     constant text[] := ARRAY[
        'BASE', 'POST', 'MEAL', 'PERF', 'OT_WD', 'OT_WE', 'OT_HO', 'ATT_BONUS',
        'ATT_DEDUCT', 'SI_PERSONAL', 'HF_PERSONAL', 'IIT', 'OTHER_WITHHOLD',
        'SI_COMPANY', 'HF_COMPANY', 'GROSS', 'NET'];

    -- ⑤ 脱敏列 / 配置键（V0388）
    c_mask_cols constant text[] := ARRAY['id_card', 'bank_account'];
    c_configs   constant text[] := ARRAY[
        'hr.salary.iit_mode', 'hr.salary.si_mode', 'hr.attendance.require_leave_match'];

    v_i         integer;
BEGIN
    -- ========================================================================
    -- ① 10 张业务表
    -- ========================================================================
    FOREACH v_bad IN ARRAY c_tables LOOP
        IF to_regclass('public.' || v_bad) IS NULL THEN
            v_missing := v_missing || format('表 %s', v_bad);
        END IF;
    END LOOP;

    -- 表在但**列被削**（克隆库升级时最阴的一种）也拦：抽查每张表的关键列
    IF to_regclass('public.biz_salary_sheet') IS NOT NULL THEN
        SELECT count(*) INTO v_n
          FROM information_schema.columns
         WHERE table_schema = 'public' AND table_name = 'biz_salary_sheet'
           AND column_name IN ('gross_total', 'net_total', 'accrual_voucher_id',
                               'payment_voucher_id', 'reversal_voucher_id', 'status');
        IF v_n <> 6 THEN
            v_missing := v_missing || format('biz_salary_sheet 关键列（%s/6）', v_n);
        END IF;
    END IF;
    IF to_regclass('public.biz_salary_detail_item') IS NOT NULL THEN
        SELECT count(*) INTO v_n
          FROM information_schema.columns
         WHERE table_schema = 'public' AND table_name = 'biz_salary_detail_item'
           AND column_name IN ('item_type', 'amount', 'detail_id', 'item_code');
        IF v_n <> 4 THEN
            v_missing := v_missing || format('biz_salary_detail_item 关键列（%s/4）', v_n);
        END IF;
    END IF;

    -- ========================================================================
    -- ② 科目（9 末级 + 4 父翻非末级）与 2 条工资凭证规则
    -- ========================================================================
    FOR v_i IN 1 .. array_length(c_acc_code, 1) LOOP
        -- 末级科目必须在、且 is_leaf=1、且父级指向预期父科目
        IF NOT EXISTS (
            SELECT 1 FROM biz_account a
              JOIN biz_account p ON p.id = a.parent_id AND p.deleted_at IS NULL
             WHERE a.account_code = c_acc_code[v_i]
               AND a.deleted_at IS NULL
               AND a.is_leaf = 1
               AND p.account_code = c_acc_parent[v_i]
        ) THEN
            v_missing := v_missing || format('科目 %s（末级/父级=%s）',
                                             c_acc_code[v_i], c_acc_parent[v_i]);
        END IF;
    END LOOP;

    -- 9 个末级科目**不许**再挂子科目：挂着子级还标 is_leaf=1 ⇒ 分录既能记到父级又能记到子级，
    -- 末级口径分叉（试算平衡表按末级汇总时会漏/重）。造错自检里专门有一条打这个。
    FOR v_i IN 1 .. array_length(c_acc_code, 1) LOOP
        IF EXISTS (
            SELECT 1
              FROM biz_account a
              JOIN biz_account c ON c.parent_id = a.id AND c.deleted_at IS NULL
             WHERE a.account_code = c_acc_code[v_i] AND a.deleted_at IS NULL
        ) THEN
            v_missing := v_missing || format('科目 %s 下挂了子科目（末级不许有子级）', c_acc_code[v_i]);
        END IF;
    END LOOP;

    -- 父科目只要挂了子科目就**不许**还是末级（否则分录能直接记到父级，末级口径分叉）
    FOREACH v_bad IN ARRAY c_parents LOOP
        IF EXISTS (
            SELECT 1 FROM biz_account p
             WHERE p.account_code = v_bad AND p.deleted_at IS NULL AND p.is_leaf = 1
               AND EXISTS (SELECT 1 FROM biz_account c
                            WHERE c.parent_id = p.id AND c.deleted_at IS NULL)
        ) THEN
            v_missing := v_missing || format('父科目 %s 仍 is_leaf=1（应翻非末级）', v_bad);
        END IF;
    END LOOP;

    IF to_regclass('public.biz_voucher_rule') IS NOT NULL THEN
        FOREACH v_bad IN ARRAY c_rules LOOP
            IF NOT EXISTS (
                SELECT 1 FROM biz_voucher_rule r
                 WHERE r.rule_code = v_bad AND r.deleted_at IS NULL
                   AND r.debit_account_id IS NOT NULL AND r.credit_account_id IS NOT NULL
            ) THEN
                v_missing := v_missing || format('凭证规则 %s（缺行或科目未解析）', v_bad);
            END IF;
        END LOOP;
    ELSE
        v_missing := v_missing || '表 biz_voucher_rule'::text;
    END IF;

    -- ========================================================================
    -- ③ 4 条号段 + 4 类字典 + 26 项字典值
    --     号段按 seq_type 查（不按 org_id：ARCH-9-1 后唯一键是 (org_id, seq_type)，
    --     断言口径是「这个公司至少有一条可用号段」）
    -- ========================================================================
    FOREACH v_bad IN ARRAY c_seqs LOOP
        SELECT count(*) INTO v_n FROM biz_seq
         WHERE seq_type = v_bad AND deleted_at IS NULL AND status = 1;
        IF v_n = 0 THEN
            v_missing := v_missing || format('号段 %s', v_bad);
        END IF;
    END LOOP;

    -- 号段位数口径（EMP 4 位、其余 6 位）+ EMP 是 GLOBAL 不重置（HR-03 卡面口径）
    IF EXISTS (SELECT 1 FROM information_schema.columns
                WHERE table_schema = 'public' AND table_name = 'biz_seq'
                  AND column_name = 'pad_width') THEN
        IF NOT EXISTS (SELECT 1 FROM biz_seq
                        WHERE seq_type = 'EMP' AND deleted_at IS NULL
                          AND pad_width = 4 AND period_key = 'GLOBAL') THEN
            v_missing := v_missing || '号段 EMP 口径（pad_width=4 / period_key=GLOBAL）'::text;
        END IF;
        SELECT count(*) INTO v_n FROM biz_seq
         WHERE seq_type IN ('LV', 'ATT', 'SAL') AND deleted_at IS NULL AND pad_width = 6;
        IF v_n < 3 THEN
            v_missing := v_missing || format('号段 LV/ATT/SAL pad_width=6（实际 %s/3）', v_n);
        END IF;
    END IF;

    FOR v_i IN 1 .. array_length(c_dicts, 1) LOOP
        SELECT count(*) INTO v_n
          FROM sys_dict_item i
          JOIN sys_dict_type t ON t.id = i.dict_type_id
         WHERE t.dict_code = c_dicts[v_i] AND t.deleted_at IS NULL
           AND i.deleted_at IS NULL AND i.status = 1;
        IF v_n <> c_dict_cnt[v_i] THEN
            v_missing := v_missing || format('字典 %s（%s 项，期望 %s）',
                                             c_dicts[v_i], v_n, c_dict_cnt[v_i]);
        END IF;
    END LOOP;

    -- ========================================================================
    -- ④ 36 条菜单 + 权限码唯一 + HR_MANAGER 角色与授权
    -- ========================================================================
    -- 目录
    IF NOT EXISTS (SELECT 1 FROM sys_menu
                    WHERE id = c_menu_dir AND deleted_at IS NULL AND menu_type = 1) THEN
        v_missing := v_missing || '菜单 目录 /business/hr（menu_type=1）'::text;
    END IF;

    -- 5 个页面：按固定 id 查，并校验 path（path 是前端路由的真身）
    FOR v_i IN 1 .. array_length(c_menu_pages, 1) LOOP
        IF NOT EXISTS (SELECT 1 FROM sys_menu
                        WHERE id = c_menu_pages[v_i] AND deleted_at IS NULL
                          AND menu_type = 2
                          AND path = c_page_paths[v_i + 1]) THEN
            v_missing := v_missing || format('菜单 页面 %s', c_page_paths[v_i + 1]);
        END IF;
    END LOOP;

    -- 30 个按钮权限点：type=3 / 不可见 / name = auth_code / 前缀 /business/hr:
    SELECT count(*) INTO v_n FROM sys_menu
     WHERE deleted_at IS NULL AND menu_type = 3
       AND auth_code LIKE 'business:hr:%'
       AND path IS NULL AND name = auth_code AND is_visible IS FALSE;
    IF v_n <> 30 THEN
        v_missing := v_missing || format('按钮权限点 business:hr:*（%s 个，期望 30）', v_n);
    END IF;

    -- 36 = 1 目录 + 5 页面 + 30 按钮（本模块自有 id 段：…e000~e005 与 20000000-…-e1xx/fxxx）
    SELECT count(*) INTO v_n FROM sys_menu
     WHERE deleted_at IS NULL
       AND (id = c_menu_dir OR id = ANY (c_menu_pages)
            OR (menu_type = 3 AND auth_code LIKE 'business:hr:%'));
    IF v_n <> 36 THEN
        v_missing := v_missing || format('人力资源菜单合计（%s 条，期望 36）', v_n);
    END IF;

    -- 权限码不许分叉：同一 auth_code 若挂到多个未删菜单，授权树会分叉（AGENTS 约定 7）
    SELECT string_agg(m.auth_code || '×' || m.dup, ' / ') INTO v_bad
      FROM (SELECT auth_code, count(*) AS dup FROM sys_menu
             WHERE deleted_at IS NULL AND auth_code LIKE 'business:hr:%' AND auth_code <> ''
             GROUP BY auth_code HAVING count(*) > 1) m;
    IF v_bad IS NOT NULL THEN
        v_missing := v_missing || format('人力资源权限码重复（%s）', v_bad);
    END IF;

    -- 角色 + 授权：HR_MANAGER 必须在，且至少授了目录与 5 个页面（36 条授权的下限）
    IF NOT EXISTS (SELECT 1 FROM sys_role
                    WHERE role_code = 'HR_MANAGER' AND deleted_at IS NULL) THEN
        v_missing := v_missing || '角色 HR_MANAGER'::text;
    ELSE
        SELECT count(*) INTO v_n
          FROM sys_role_menu rm
          JOIN sys_role r ON r.id = rm.role_id AND r.role_code = 'HR_MANAGER' AND r.deleted_at IS NULL
          JOIN sys_menu m ON m.id = rm.menu_id AND m.deleted_at IS NULL
         WHERE m.id = c_menu_dir OR m.id = ANY (c_menu_pages);
        IF v_n < 6 THEN
            v_missing := v_missing || format('HR_MANAGER 授权（目录+5 页面 %s/6）', v_n);
        END IF;
    END IF;

    -- ========================================================================
    -- ⑤ 2 条脱敏 + 3 条配置（V0388）
    -- ========================================================================
    FOREACH v_bad IN ARRAY c_mask_cols LOOP
        IF NOT EXISTS (SELECT 1 FROM sys_field_mask
                        WHERE table_name = 'biz_employee' AND column_name = v_bad
                          AND role_id IS NULL AND deleted_at IS NULL AND status = 1) THEN
            v_missing := v_missing || format('脱敏 biz_employee.%s', v_bad);
        END IF;
    END LOOP;

    FOREACH v_bad IN ARRAY c_configs LOOP
        SELECT count(*) INTO v_n FROM sys_config
         WHERE config_key = v_bad AND deleted_at IS NULL;
        IF v_n <> 1 THEN
            v_missing := v_missing || format('配置 %s（%s 行，期望 1）', v_bad, v_n);
        END IF;
    END LOOP;

    -- ========================================================================
    -- ⑥ 4 套审批流程（已发布 + 默认 + 当前版本 + 图快照）
    -- ========================================================================
    FOREACH v_bad IN ARRAY c_biz_types LOOP
        IF NOT EXISTS (
            SELECT 1
              FROM biz_flow_def d
              JOIN biz_flow_def_version v ON v.id = d.current_version_id
             WHERE d.biz_type = v_bad AND d.deleted_at IS NULL
               AND d.status = 'PUBLISHED' AND d.is_default
               AND v.version_status = 'PUBLISHED' AND v.is_current
               AND v.graph_hash IS NOT NULL AND v.graph_snapshot IS NOT NULL
        ) THEN
            v_missing := v_missing || format('流程 biz_type=%s（已发布/默认/当前版本）', v_bad);
        END IF;
    END LOOP;

    -- ========================================================================
    -- ⑦ 17 个薪资项
    -- ========================================================================
    FOREACH v_bad IN ARRAY c_items LOOP
        IF NOT EXISTS (SELECT 1 FROM biz_salary_item
                        WHERE item_code = v_bad AND deleted_at IS NULL AND is_active = 1) THEN
            v_missing := v_missing || format('薪资项 %s', v_bad);
        END IF;
    END LOOP;

    -- 系统项不许被标成可删（GROSS/NET 是引擎写入的汇总行，界面删掉就再也算不出应发/实发）
    SELECT string_agg(item_code || '(is_system=' || is_system || ')', ' / ') INTO v_bad
      FROM biz_salary_item
     WHERE item_code IN ('GROSS', 'NET') AND deleted_at IS NULL AND is_system <> 1;
    IF v_bad IS NOT NULL THEN
        v_missing := v_missing || format('系统薪资项 is_system 非 1（%s）', v_bad);
    END IF;

    -- ========================================================================
    -- ⑧ 汇总：有任何一项缺失 ⇒ 一次性全部列出并回滚
    -- ========================================================================
    IF array_length(v_missing, 1) IS NOT NULL THEN
        RAISE EXCEPTION 'V0398 人力资源收口自检失败：% 项未种齐 → %',
            array_length(v_missing, 1), array_to_string(v_missing, '；')
            USING HINT = '本迁移是纯只读断言，回滚不影响数据。缺哪项就重放对应迁移：'
                         '表=V0383 / 科目与规则=V0384 / 号段与字典=V0386 / 菜单与角色=V0387 / '
                         '脱敏与配置=V0388 / 流程=V0393~V0395 / 薪资项=V0396。'
                         '若某项是被客户主动删掉的（例如流程改成了客户自建默认流），'
                         '先把 biz_type 的默认发布流补回来再升级。';
    END IF;

    RAISE NOTICE 'V0398 人力资源收口自检通过：10 表 / 9 科目+4 父级+2 规则 / 4 号段+4 字典 26 项 / '
                 '36 菜单+HR_MANAGER / 2 脱敏+3 配置 / 4 流程 / 17 薪资项 —— 8 组全部在位';
END $$;

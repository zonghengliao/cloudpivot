-- ======================================================
-- V0190：年结（ERP-R4）—— 期初来源列 + YEAR_END 凭证类型 + 权限节点
--
-- 依据：kanban 卡 t_0daa0ca0 §三 / §四 / §五
--
-- # 一句话
--
-- 年结把「本年的损益类结平 → 本年利润转利润分配 → 下年期初」一次做完。
-- 这需要三处结构性支撑：
--   · `voucher_type='YEAR_END'` 的结转凭证（一个会计年度一张）
--   · `biz_opening_balance.source` —— 区分「财务手工录的期初」与「年结自动生成的期初」
--   · `business:erp:yearend:{read,close,reopen}` 三个权限码
--
-- # 为什么年结只写期初台账、不生成 2027-01 的 OPENING 凭证
--
-- 报表的「期初」列 = 期间开始日之前的历史累计 + 本期间自己的 OPENING 凭证。
-- 2027 年的历史累计里已经有 2026 年的结转凭证（它等于 2026 年期末余额），
-- 再生成一张 2027-01 的 OPENING 凭证就是**双计**（银行存款、存货全部翻倍）。
-- 所以年结把下年期初写进 `biz_opening_balance`（本迁移加的 `source='YEAR_END'`）：
-- 它是「新的一年从多少开始」的凭证依据，报表的期初仍由历史累计给出，两者必须相等。
--
-- 反向保护：`source='YEAR_END'` 的行**不许手工覆盖**（后端 409），
-- 否则手工再写一遍又是一次双计。要调整只能先反年结。
--
-- # `ck_biz_voucher_type` 二次扩容
--
-- V0149 是 `AUTO|MANUAL`，V0189 扩成 `AUTO|MANUAL|OPENING`，本迁移扩成
-- `AUTO|MANUAL|OPENING|YEAR_END`。约束**必须先 DROP 再 ADD**（`ADD CONSTRAINT`
-- 没有 IF NOT EXISTS），只动 type 这一条，不碰红字引用与状态两条。
--
-- 「一个会计年度一张结转凭证」也在库层面兜死：部分唯一索引
-- `uk_biz_voucher_year_end_period`（period + voucher_type='YEAR_END'），
-- 与 V0189 的 `uk_biz_voucher_opening_period` 同一手法。
--
-- # 权限节点（menu_type 3 = 按钮，page 挂「ERP 台账」目录 …0946 下）
--
--   1025 business:erp:yearend:read     年度结转（菜单页，menu_type 2）
--   1026 business:erp:yearend:close    年结（损益结转 + 生成下年期初）
--   1027 business:erp:yearend:reopen   反年结（撤销整年的结转）
--
-- 三码分开的理由与 V0188 期间三码、V0189 期初三码一致：
-- 「能看年结状态」更不能等于「能把整年拆了重来」。
--   授权：read   → SUPER_ADMIN / ADMIN / FINANCE / ACCOUNTANT
--         close  → SUPER_ADMIN / ADMIN / FINANCE
--         reopen → SUPER_ADMIN / FINANCE
--
-- 幂等：列 / 索引全带 IF NOT EXISTS，节点 `ON CONFLICT (id) DO NOTHING`，
--       授权 `NOT EXISTS`。重复执行不报错、不覆盖运维改过的标题。
-- 回滚：V0190 down 只删本迁移加的**结构**与节点；期初数据与结转凭证**不删**
--       （它们已经是账的一部分），且有一道安全阀：库里还有 YEAR_END 凭证时不收白名单。
-- ======================================================

-- ---------- ① 期初来源列 ----------
-- 存量行一律 `MANUAL`（V0189 之后只有手工录入这一条路径，年结是本迁移才有的）。
ALTER TABLE biz_opening_balance
    ADD COLUMN IF NOT EXISTS source VARCHAR(16) NOT NULL DEFAULT 'MANUAL';

COMMENT ON COLUMN biz_opening_balance.source IS
    '期初来源：MANUAL 财务手工录入 / YEAR_END 由上一会计年度年结自动生成（不许手工覆盖）';

-- 白名单：除了这两个值没有第三种来源（前端按它区分「能不能改」）
ALTER TABLE biz_opening_balance DROP CONSTRAINT IF EXISTS ck_biz_opening_source;
ALTER TABLE biz_opening_balance
    ADD CONSTRAINT ck_biz_opening_source CHECK (source IN ('MANUAL', 'YEAR_END'));

-- ---------- ② 凭证类型白名单加 YEAR_END ----------
-- 注意顺序：先 DROP 再 ADD（`ADD CONSTRAINT` 没有 IF NOT EXISTS）。
ALTER TABLE biz_voucher DROP CONSTRAINT IF EXISTS ck_biz_voucher_type;
ALTER TABLE biz_voucher
    ADD CONSTRAINT ck_biz_voucher_type
    CHECK (voucher_type IN ('AUTO', 'MANUAL', 'OPENING', 'YEAR_END'));

-- ---------- ③ 「一个会计年度一张结转凭证」在库层面兜死 ----------
CREATE UNIQUE INDEX IF NOT EXISTS uk_biz_voucher_year_end_period
    ON biz_voucher (period)
    WHERE voucher_type = 'YEAR_END' AND deleted_at IS NULL;

COMMENT ON INDEX uk_biz_voucher_year_end_period IS
    '每个期间最多一张年末结转凭证（voucher_type=YEAR_END）；年结按 (年度 → 该年 12 月) 一一对应';

-- ---------- ④ 权限节点 ----------
INSERT INTO sys_menu (id, name, menu_type, auth_code, path, parent_id, component,
                      sort_order, status, icon, title, is_visible, module_code)
VALUES
    ('20000000-0000-0000-0000-000000001025'::uuid, '年度结转', 2, 'business:erp:yearend:read',
     '/business/erp/year-end', '20000000-0000-0000-0000-000000000946'::uuid,
     'business/erp/year-end/index', 26, 1, 'calendar', '年度结转', TRUE, 'business'),
    ('20000000-0000-0000-0000-000000001026'::uuid, 'business:erp:yearend:close', 3,
     'business:erp:yearend:close', NULL,
     '20000000-0000-0000-0000-000000001025'::uuid, NULL,
     10, 1, NULL, '年结', FALSE, 'business'),
    ('20000000-0000-0000-0000-000000001027'::uuid, 'business:erp:yearend:reopen', 3,
     'business:erp:yearend:reopen', NULL,
     '20000000-0000-0000-0000-000000001025'::uuid, NULL,
     20, 1, NULL, '反年结', FALSE, 'business')
ON CONFLICT (id) DO NOTHING;

-- ---------- ⑤ 自检 ----------
DO $$
DECLARE
    n_col     INT;
    n_ck      INT;
    n_uk      INT;
    n_menu    INT;
    n_row     INT;
    n_roles   INT;
    n_grants  INT;
    v_def     TEXT;
BEGIN
    -- 来源列在（且非空）
    SELECT count(*) INTO n_col
      FROM information_schema.columns
     WHERE table_schema = 'public' AND table_name = 'biz_opening_balance'
       AND column_name = 'source' AND is_nullable = 'NO';
    IF n_col <> 1 THEN
        RAISE EXCEPTION 'V0190 自检失败：biz_opening_balance.source 列没建上';
    END IF;

    -- 来源白名单约束在
    SELECT count(*) INTO n_ck
      FROM pg_constraint
     WHERE conrelid = 'public.biz_opening_balance'::regclass
       AND conname = 'ck_biz_opening_source';
    IF n_ck <> 1 THEN
        RAISE EXCEPTION 'V0190 自检失败：ck_biz_opening_source 约束缺失';
    END IF;

    -- ★ 见证「结转凭证唯一」是按 (period) 建的部分唯一索引（不是普通索引）
    SELECT count(*) INTO n_uk
      FROM pg_index i JOIN pg_class c ON c.oid = i.indexrelid
     WHERE c.relname = 'uk_biz_voucher_year_end_period' AND i.indisunique;
    IF n_uk <> 1 THEN
        RAISE EXCEPTION 'V0190 自检失败：uk_biz_voucher_year_end_period 不是唯一索引（% 行）', n_uk;
    END IF;

    -- ★ 凭证类型白名单真的含 YEAR_END（读约束定义，不是读期望）
    SELECT pg_get_constraintdef(oid) INTO v_def
      FROM pg_constraint
     WHERE conrelid = 'public.biz_voucher'::regclass AND conname = 'ck_biz_voucher_type';
    IF v_def IS NULL OR v_def NOT LIKE '%YEAR_END%' OR v_def NOT LIKE '%OPENING%' THEN
        RAISE EXCEPTION 'V0190 自检失败：ck_biz_voucher_type 白名单不完整（当前 %）', v_def;
    END IF;

    -- 反证：白名单外的类型必须被挡住（子事务里试一次再回滚）
    BEGIN
        INSERT INTO biz_voucher (voucher_no, period, voucher_date, voucher_type,
                                 total_debit, total_credit, entry_count)
        VALUES ('__V0190_SELFCHECK__', '1999-03', DATE '1999-03-01', 'YEAR_END_X',
                10, 10, 2);
        RAISE EXCEPTION 'V0190 自检失败：白名单外的 voucher_type 写进了库';
    EXCEPTION WHEN check_violation THEN
        RAISE NOTICE 'V0190 自检通过：ck_biz_voucher_type 拦住了白名单外的类型';
    END;

    -- 权限节点在
    SELECT count(*) INTO n_menu
      FROM sys_menu
     WHERE id IN ('20000000-0000-0000-0000-000000001025'::uuid,
                  '20000000-0000-0000-0000-000000001026'::uuid,
                  '20000000-0000-0000-0000-000000001027'::uuid)
       AND deleted_at IS NULL;
    IF n_menu <> 3 THEN
        RAISE EXCEPTION 'V0190 自检失败：年结三码节点没种全（%）', n_menu;
    END IF;

    SELECT count(*) INTO n_row
      FROM sys_menu
     WHERE auth_code IN ('business:erp:yearend:read',
                         'business:erp:yearend:close',
                         'business:erp:yearend:reopen')
       AND deleted_at IS NULL;
    IF n_row <> 3 THEN
        RAISE EXCEPTION 'V0190 自检失败：年结三码在 sys_menu 里出现 % 次（应为 3）', n_row;
    END IF;

    -- 授权（空库首启时 sys_role 可能为空，跳过并提示）
    SELECT count(*) INTO n_roles FROM sys_role WHERE deleted_at IS NULL;
    IF n_roles = 0 THEN
        RAISE NOTICE 'V0190：sys_role 为空（空库首启，迁移先于 bootstrap），跳过授权；bootstrap 后重放本迁移即可补齐';
    ELSE
        INSERT INTO sys_role_menu (id, role_id, menu_id)
        SELECT gen_random_uuid(), r.id, m.id
          FROM sys_role r
          CROSS JOIN (VALUES
                ('20000000-0000-0000-0000-000000001025'::uuid)) AS m(id)
         WHERE r.deleted_at IS NULL
           AND r.role_code IN ('SUPER_ADMIN', 'ADMIN', 'FINANCE', 'ACCOUNTANT')
           AND NOT EXISTS (
                SELECT 1 FROM sys_role_menu rm WHERE rm.role_id = r.id AND rm.menu_id = m.id
           );
        GET DIAGNOSTICS n_grants = ROW_COUNT;

        INSERT INTO sys_role_menu (id, role_id, menu_id)
        SELECT gen_random_uuid(), r.id, m.id
          FROM sys_role r
          CROSS JOIN (VALUES
                ('20000000-0000-0000-0000-000000001025'::uuid),
                ('20000000-0000-0000-0000-000000001026'::uuid)) AS m(id)
         WHERE r.deleted_at IS NULL
           AND r.role_code IN ('ADMIN', 'FINANCE')
           AND NOT EXISTS (
                SELECT 1 FROM sys_role_menu rm WHERE rm.role_id = r.id AND rm.menu_id = m.id
           );
        GET DIAGNOSTICS n_row = ROW_COUNT;
        n_grants := n_grants + n_row;

        INSERT INTO sys_role_menu (id, role_id, menu_id)
        SELECT gen_random_uuid(), r.id, m.id
          FROM sys_role r
          CROSS JOIN (VALUES
                ('20000000-0000-0000-0000-000000001027'::uuid)) AS m(id)
         WHERE r.deleted_at IS NULL
           AND r.role_code IN ('FINANCE')
           AND NOT EXISTS (
                SELECT 1 FROM sys_role_menu rm WHERE rm.role_id = r.id AND rm.menu_id = m.id
           );
        GET DIAGNOSTICS n_row = ROW_COUNT;
        n_grants := n_grants + n_row;

        RAISE NOTICE 'V0190：年结三码已种，授权新增 % 行', n_grants;
    END IF;

    RAISE NOTICE 'V0190 自检通过：期初来源列 + YEAR_END 白名单 + 结转凭证唯一索引 + 三个权限节点';
END $$;

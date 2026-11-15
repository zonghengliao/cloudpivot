-- ============================================================================
-- V0408__account_foreign_currency（M1-03 · 多币种第三步 · 2026-11-16）
--
-- 【本卡做什么】给科目表加一列「是否外币核算」标记 + 一个维护开关的权限码，
--   不动任何其它列、不建表、不写业务账：
--     ① `biz_account.is_foreign_currency SMALLINT NOT NULL DEFAULT 0`
--        （0 本位币核算 / 1 外币核算；存量 100% 回填 0 = 人民币账套的恒等变换）
--     ② 两条 CHECK：取值只能是 0/1；**只有末级科目**才允许标记外币核算
--     ③ 一条部分索引：期末调汇 / 外币余额表只扫「标了外币」的那几行
--     ④ 一个权限码 `business:erp:account:update`（按钮行，挂在「会计科目」页面下）
--
-- 【为什么是 SMALLINT 0/1 而不是 BOOLEAN】
--   同一个表的 `is_leaf`（V0137）就是 `SMALLINT NOT NULL DEFAULT 1` + `ck_..._leaf
--   CHECK (is_leaf IN (0,1))`，`status` 也是 smallint。**同一张表的同类布尔位同型**，
--   前端 `AccountRes` 里 `is_leaf: bool` 由 DTO 做 `== 1` 转换（本卡照抄这条转换）。
--   混用 boolean 会让「is_leaf 是 SMALLINT、is_foreign_currency 是 BOOLEAN」这种
--   半张表两套写法的坑留给下游（M1-05/M1-07 每次都要记哪个是哪个）。
--
-- 【为什么 CHECK 里要绑 is_leaf（这是本卡唯一会静默算错账的地方）】
--   外币核算的语义是「这个科目按原币 + 汇率记账，期末按牌价调汇」——
--   只有**末级科目**才挂分录、才有余额可调。给非末级科目打标记的后果：
--     · 期末调汇（M1-05）按 `is_foreign_currency = 1` 扫科目，会扫到汇总科目，
--       它自己那一行没有余额（余额由下级汇总而来）⇒ 调汇凭证金额恒 0，
--       而它的下级如果也标了，同一笔外币余额会被调两次；
--     · 外币余额表（M1-07）会出现「父子两级各一行外币余额」，报表使用者无法判断该看哪一行。
--   所以 `CHECK (is_foreign_currency = 0 OR is_leaf = 1)`：**关闭永远允许**（0 恒满足），
--   开启只对末级放行。同一条 CHECK 也顺带挡住反向操作 ——「已标外币的末级科目被改成非末级」
--   （`is_leaf` 1→0 时 CHECK 立刻不满足），这正是父子两次调汇的入口。
--   应用层（`account_admin_handler.rs`）先按主键读出 `is_leaf` 再给**指名文案**的 400；
--   DB 这一条是并发窗口里的兜底（不是靠它出人话）。
--
-- 【部分索引：为什么值得在小表上建】
--   `biz_account` 是几百行的极小表，但「哪些科目是外币核算」这个问题会被三处反复问：
--   期末调汇扫科目（M1-05）、外币余额表取科目（M1-07）、外币明细账译名（M1-08）。
--   谓词 `WHERE is_foreign_currency = 1` 与查询条件同形，索引里只有那几行（通常个位数），
--   与 V0406 的 `idx_biz_voucher_currency ... WHERE currency_code <> 'CNY'` 同口径：
--   部分索引只收真命中的行，代价是几个数据页。
--
-- 【权限码 `business:erp:account:update`】
--   科目页本身是**只读**页（`frontend/.../erp/account/index.vue` 头注释「科目由系统管理员
--   维护」），本卡只开一个极窄的写入口：PUT `/business/erp/account/{id}/foreign-currency`
--   **只改这一列**。码名取 `:update` 而不是 `:manage`：
--     · 与 `business:erp:supplier:update`（编辑 / 删除）同档 —— 这是改一个已有科目；
--     · 未来科目 CRUD 落地时同一个码继续承载「编辑科目」，不会出现两个语义重叠的码。
--   授权：SUPER_ADMIN / FINANCE_MANAGER（谁维护科目谁标记外币；`ADMIN` 不是生产角色码，
--   `sys_role` 里只有上面那 15 个，写了会静默授权 0 行 —— 见 scripts/verify/role_code_gate.py；
--   FINANCE_DIRECTOR 是只读对照角色，V0258 只给它报表类码，这里不给）。
--
-- 【幂等】
--   `ADD COLUMN IF NOT EXISTS` / 约束先查 `pg_constraint` 再 ADD / `CREATE INDEX IF NOT EXISTS`
--   / 菜单 `ON CONFLICT (id) DO NOTHING` / 授权 `NOT EXISTS` 去重 / 自检块可反复执行且
--   探针行自清场（重放零残留）。
--
-- 【回滚】
--   down 在「已有科目标了外币核算」时 RAISE EXCEPTION 拒绝执行：这一位是人工录入的
--   业务信息（哪个科目按外币记账），重放 up 也只会填回 0，删了就真丢了。
--   对比 V0407 的 `effective_to`（纯派生列，可重算）没有护栏。
-- ============================================================================

-- ---------------------------------------------------------------------------
-- ① 前置护栏：科目表必须在（缺表说明 ERP 底座迁移没跑）
-- ---------------------------------------------------------------------------
DO $$
BEGIN
    IF to_regclass('public.biz_account') IS NULL THEN
        RAISE EXCEPTION 'V0408: biz_account 不存在，请先应用 V0137（ERP 主数据与仓库）';
    END IF;
    IF to_regclass('public.sys_menu') IS NULL THEN
        RAISE EXCEPTION 'V0408: sys_menu 不存在，请先应用系统底座迁移';
    END IF;
    RAISE NOTICE 'V0408: 前置表齐备，开始加「是否外币核算」标记';
END $$;

-- ---------------------------------------------------------------------------
-- ② 加列（NOT NULL + DEFAULT 0 ⇒ 存量行自动回填 0，新写入不必显式给值）
-- ---------------------------------------------------------------------------
ALTER TABLE biz_account ADD COLUMN IF NOT EXISTS is_foreign_currency SMALLINT NOT NULL DEFAULT 0;

COMMENT ON COLUMN biz_account.is_foreign_currency IS
    '是否外币核算：0 本位币（CNY）核算 / 1 外币核算（只有末级科目可标）。标记后该科目按原币记账，期末由 M1-05 调汇；与同表 is_leaf / status 同为 SMALLINT 0/1 口径';

-- ---------------------------------------------------------------------------
-- ③ 约束（两条，全部「先查再 ADD」，重放安全）
-- ---------------------------------------------------------------------------
DO $$
DECLARE
    n integer;
BEGIN
    IF NOT EXISTS (
        SELECT 1 FROM pg_constraint
        WHERE conname = 'ck_biz_account_foreign_currency'
          AND conrelid = 'public.biz_account'::regclass
    ) THEN
        ALTER TABLE biz_account
            ADD CONSTRAINT ck_biz_account_foreign_currency
            CHECK (is_foreign_currency IN (0, 1));
    END IF;

    IF NOT EXISTS (
        SELECT 1 FROM pg_constraint
        WHERE conname = 'ck_biz_account_fc_leaf'
          AND conrelid = 'public.biz_account'::regclass
    ) THEN
        ALTER TABLE biz_account
            ADD CONSTRAINT ck_biz_account_fc_leaf
            CHECK (is_foreign_currency = 0 OR is_leaf = 1);
    END IF;

    -- 建完必须点名验一次：同名约束落在**别的表**上（历史遗留 / 别人手工加过）时上面的
    -- IF NOT EXISTS 会静默跳过，表上其实没有这条约束 —— 不在这里拦住，脏数据入口就打开了。
    SELECT count(*) INTO n
      FROM pg_constraint
     WHERE contype = 'c'
       AND conrelid = 'public.biz_account'::regclass
       AND conname IN ('ck_biz_account_foreign_currency', 'ck_biz_account_fc_leaf');
    IF n <> 2 THEN
        RAISE EXCEPTION 'V0408: 两条外币标记 CHECK 只建成 % 条（期望 2：可能同名约束落在别的表上）', n;
    END IF;
END $$;

-- ---------------------------------------------------------------------------
-- ④ 部分索引（期末调汇 / 外币余额表 / 外币明细账的取数入口，见文件头「部分索引」）
-- ---------------------------------------------------------------------------
CREATE INDEX IF NOT EXISTS idx_biz_account_foreign_currency
    ON biz_account (account_code)
    WHERE is_foreign_currency = 1;

-- ---------------------------------------------------------------------------
-- ⑤ 权限码 + 菜单按钮行 + 授权（与 `account_admin_handler.rs` 的注解逐字一致）
-- ---------------------------------------------------------------------------
INSERT INTO sys_menu (id, name, menu_type, auth_code, path, parent_id, component,
                      sort_order, status, icon, title, is_visible, module_code)
VALUES
    ('20000000-0000-0000-0000-000000001720'::uuid, 'business:erp:account:update', 3,
     'business:erp:account:update', NULL, '20000000-0000-0000-0000-000000000950'::uuid, NULL,
     10, 1, NULL, '科目外币核算标记', FALSE, 'business')
ON CONFLICT (id) DO NOTHING;

-- 授权：超管 / 管理员 / 财务经理（空库首启时 sys_role 为空 → 空操作，bootstrap 后可重放补齐）
INSERT INTO sys_role_menu (id, role_id, menu_id)
SELECT gen_random_uuid(), r.id, '20000000-0000-0000-0000-000000001720'::uuid
  FROM sys_role r
 WHERE r.deleted_at IS NULL
   AND r.role_code IN ('SUPER_ADMIN', 'FINANCE_MANAGER')
   AND NOT EXISTS (
        SELECT 1 FROM sys_role_menu rm
         WHERE rm.role_id = r.id
           AND rm.menu_id = '20000000-0000-0000-0000-000000001720'::uuid
   );

-- ---------------------------------------------------------------------------
-- ⑥ 迁移自检（结构 + 类型/默认值 + 约束在位 + 索引 + 菜单 + 行为探针；任一不符即
--    RAISE EXCEPTION 让整个迁移回滚 —— 「跑过了」和「验过了」是两件事）
-- ---------------------------------------------------------------------------
DO $$
DECLARE
    n        integer;
    txt      text;
    def      text;
    v_flag   smallint;
    v_acc_id uuid;
BEGIN
    -- ===== 结构：列在位 / NOT NULL / smallint / 默认 0 =====
    SELECT count(*) INTO n
      FROM information_schema.columns
     WHERE table_schema = 'public' AND table_name = 'biz_account'
       AND column_name = 'is_foreign_currency';
    IF n <> 1 THEN
        RAISE EXCEPTION 'V0408 自检失败：biz_account.is_foreign_currency 不存在';
    END IF;

    SELECT count(*) INTO n
      FROM information_schema.columns
     WHERE table_schema = 'public' AND table_name = 'biz_account'
       AND column_name = 'is_foreign_currency'
       AND is_nullable = 'NO';
    IF n <> 1 THEN
        RAISE EXCEPTION 'V0408 自检失败：is_foreign_currency 可空（NULL 既不是「是」也不是「否」，下游三处都会漏判）';
    END IF;

    SELECT data_type INTO txt
      FROM information_schema.columns
     WHERE table_schema = 'public' AND table_name = 'biz_account'
       AND column_name = 'is_foreign_currency';
    IF txt IS DISTINCT FROM 'smallint' THEN
        RAISE EXCEPTION 'V0408 自检失败：is_foreign_currency 类型 = %（期望 smallint，与 is_leaf / status 同型）', txt;
    END IF;

    SELECT column_default INTO def
      FROM information_schema.columns
     WHERE table_schema = 'public' AND table_name = 'biz_account'
       AND column_name = 'is_foreign_currency';
    IF def IS NULL OR btrim(def) NOT IN ('0', '0::smallint') THEN
        RAISE EXCEPTION 'V0408 自检失败：is_foreign_currency 默认值 = %（期望 0：存量与新建科目都默认本位币核算）', def;
    END IF;

    -- ===== 结构：两条 CHECK 在位（按名字 + 定义内容双判，防「同名空壳」）=====
    SELECT count(*) INTO n
      FROM pg_constraint
     WHERE contype = 'c'
       AND conrelid = 'public.biz_account'::regclass
       AND conname IN ('ck_biz_account_foreign_currency', 'ck_biz_account_fc_leaf');
    IF n <> 2 THEN
        RAISE EXCEPTION 'V0408 自检失败：外币标记 CHECK 只有 % 条在位（期望 2）', n;
    END IF;

    SELECT count(*) INTO n
      FROM pg_constraint
     WHERE contype = 'c'
       AND conrelid = 'public.biz_account'::regclass
       AND conname = 'ck_biz_account_foreign_currency'
       AND pg_get_constraintdef(oid) LIKE '%is_foreign_currency%';
    IF n <> 1 THEN
        RAISE EXCEPTION 'V0408 自检失败：ck_biz_account_foreign_currency 的定义里没有 is_foreign_currency';
    END IF;

    SELECT count(*) INTO n
      FROM pg_constraint
     WHERE contype = 'c'
       AND conrelid = 'public.biz_account'::regclass
       AND conname = 'ck_biz_account_fc_leaf'
       AND pg_get_constraintdef(oid) LIKE '%is_foreign_currency%'
       AND pg_get_constraintdef(oid) LIKE '%is_leaf%';
    IF n <> 1 THEN
        RAISE EXCEPTION 'V0408 自检失败：ck_biz_account_fc_leaf 的定义里没有同时引用 is_foreign_currency 与 is_leaf';
    END IF;

    -- ===== 结构：部分索引在位且谓词对 =====
    SELECT count(*) INTO n
      FROM pg_indexes
     WHERE schemaname = 'public'
       AND indexname = 'idx_biz_account_foreign_currency'
       AND indexdef LIKE '%WHERE%is_foreign_currency%1%';
    IF n <> 1 THEN
        RAISE EXCEPTION 'V0408 自检失败：idx_biz_account_foreign_currency 缺失或谓词不是 is_foreign_currency = 1';
    END IF;

    -- ===== 结构：存量行全部为 0（回填实证）=====
    SELECT count(*) INTO n FROM biz_account WHERE is_foreign_currency IS NULL;
    IF n > 0 THEN
        RAISE EXCEPTION 'V0408 自检失败：biz_account 有 % 行 is_foreign_currency 为 NULL', n;
    END IF;

    -- ===== 结构：权限码与菜单按钮行在位 =====
    SELECT count(*) INTO n
      FROM sys_menu
     WHERE id = '20000000-0000-0000-0000-000000001720'::uuid
       AND menu_type = 3
       AND auth_code = 'business:erp:account:update'
       AND parent_id = '20000000-0000-0000-0000-000000000950'::uuid
       AND status = 1
       AND deleted_at IS NULL;
    IF n <> 1 THEN
        RAISE EXCEPTION 'V0408 自检失败：权限码 business:erp:account:update 的菜单按钮行不对（码授不出去 = 除超管没人能标记外币）';
    END IF;

    -- ===== 行为探针 P1：默认值 0（新建科目不必显式给值）=====
    INSERT INTO biz_account (account_code, account_name, level, account_category, balance_direction)
    VALUES ('V0408PROBE', '__V0408_PROBE__', 1, 'ASSET', 'DEBIT')
    RETURNING id INTO v_acc_id;

    SELECT is_foreign_currency INTO v_flag FROM biz_account WHERE id = v_acc_id;
    IF v_flag IS DISTINCT FROM 0 THEN
        RAISE EXCEPTION 'V0408 自检失败：新建科目的默认 is_foreign_currency = %（期望 0）', v_flag;
    END IF;

    -- ===== 行为探针 P2：取值只能 0/1（2 / -1 / NULL 全被拒）=====
    BEGIN
        UPDATE biz_account SET is_foreign_currency = 2 WHERE id = v_acc_id;
        RAISE EXCEPTION 'V0408 自检失败：ck_biz_account_foreign_currency 没生效（值 2 写入成功）';
    EXCEPTION WHEN check_violation THEN
        NULL;
    END;
    BEGIN
        UPDATE biz_account SET is_foreign_currency = -1 WHERE id = v_acc_id;
        RAISE EXCEPTION 'V0408 自检失败：ck_biz_account_foreign_currency 没生效（值 -1 写入成功）';
    EXCEPTION WHEN check_violation THEN
        NULL;
    END;
    BEGIN
        UPDATE biz_account SET is_foreign_currency = NULL WHERE id = v_acc_id;
        RAISE EXCEPTION 'V0408 自检失败：is_foreign_currency 可写 NULL（NOT NULL 没生效）';
    EXCEPTION WHEN not_null_violation THEN
        NULL;
    END;

    -- ===== 行为探针 P3：末级科目可以标记 + 标记后能读回来 =====
    UPDATE biz_account SET is_foreign_currency = 1 WHERE id = v_acc_id;
    SELECT is_foreign_currency INTO v_flag FROM biz_account WHERE id = v_acc_id;
    IF v_flag IS DISTINCT FROM 1 THEN
        RAISE EXCEPTION 'V0408 自检失败：末级科目标记外币核算后读回 = %（期望 1：约束把正常业务挡死了）', v_flag;
    END IF;

    -- ===== 行为探针 P4：非末级科目不许标记（父子两次调汇的入口）=====
    BEGIN
        INSERT INTO biz_account (account_code, account_name, level, account_category, balance_direction,
                                 is_leaf, is_foreign_currency)
        VALUES ('V0408PROBE-PARENT', '__V0408_PROBE_PARENT__', 1, 'ASSET', 'DEBIT', 0, 1);
        RAISE EXCEPTION 'V0408 自检失败：ck_biz_account_fc_leaf 没生效（非末级科目标记外币核算成功）';
    EXCEPTION WHEN check_violation THEN
        NULL;
    END;

    -- ===== 行为探针 P5：已标记外币的末级科目不能被改成非末级 =====
    BEGIN
        UPDATE biz_account SET is_leaf = 0 WHERE id = v_acc_id;
        RAISE EXCEPTION 'V0408 自检失败：ck_biz_account_fc_leaf 没生效（带外币标记的科目被改成非末级）';
    EXCEPTION WHEN check_violation THEN
        NULL;
    END;

    -- ===== 行为探针 P6：关闭永远允许（含先关后开）=====
    UPDATE biz_account SET is_foreign_currency = 0 WHERE id = v_acc_id;
    SELECT is_foreign_currency INTO v_flag FROM biz_account WHERE id = v_acc_id;
    IF v_flag IS DISTINCT FROM 0 THEN
        RAISE EXCEPTION 'V0408 自检失败：关闭外币核算失败（读回 %）', v_flag;
    END IF;

    -- ===== 探针清场（重复执行零残留）=====
    DELETE FROM biz_account WHERE account_code LIKE 'V0408PROBE%';

    SELECT count(*) INTO n FROM biz_account WHERE account_code LIKE 'V0408PROBE%';
    IF n <> 0 THEN
        RAISE EXCEPTION 'V0408 自检失败：科目探针行残留 % 行', n;
    END IF;

    RAISE NOTICE 'V0408 自检通过：is_foreign_currency（smallint 0/1，默认 0）+ 2 条 CHECK（取值 / 仅末级）+ 部分索引 + 权限码 business:erp:account:update + 行为探针 6 组';
END $$;

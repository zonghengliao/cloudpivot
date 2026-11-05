-- ======================================================
-- V0250：凭证三态审核链（DRAFT → AUDITED → POSTED）· 权限码 + 状态白名单自检
--       （【P0-FIX-4A】，配套后端 `voucher_write.rs` 的 audit / un-audit / post 三个入口）
--
-- ## 病根（评审实测 2026-11-05）
--
--   select status, count(*) from biz_voucher where deleted_at is null group by 1;
--   → POSTED | 21        （草稿 0 / 已审核 0）
--
--   改造前手工凭证落库即 `POSTED`：制单人一个人就能把账做进总账，双人复核形同虚设，
--   PRD E4-09（审核 / 反审核）与 E4-10（记账）两条 P0 没有落点。
--
-- ## 本迁移做什么
--
-- ① **不动任何凭证行**：21 张 POSTED 保持 POSTED（改了状态等于让已出报表的金额消失）。
--    本迁移只做三件与数据无关的事：把 `posted_at` 放开 NOT NULL（草稿没有记账时间）、
--    种新动作的权限码、并自检库侧状态白名单是否支持三态。
-- ② 种 3 个权限码（sys_menu，按钮型，挂在「凭证」菜单下）：
--      business:erp:voucher:audit       审核   DRAFT   → AUDITED
--      business:erp:voucher:un-audit    反审核 AUDITED → DRAFT（reason 必填）
--      business:erp:voucher:post        记账   AUDITED → POSTED
--    与后端 `#[nik::permission]` 逐字对应；差一个字就等于「接口永远 403」。
-- ③ 授权口径：**谁已经能制单（business:erp:voucher:create），就把三个新码给他**。
--    这不是「放宽内控」：内控拦的是「同一个人审自己那张」（`check_self_audit` → 403 + 65019），
--    靠的是**人**而不是角色 —— 张三制的单李四能审，角色集合相同也能实现双人复核。
--    生产实测（2026-11-05）：只有 SUPER_ADMIN 持有 voucher:create，所以本迁移在生产上
--    只给 SUPER_ADMIN 授权；财务经理 / 财务总监需要这三个动作时在「角色管理」里勾选即可
--    （不在这里静默扩权，是因为那属于管理决策，不是迁移该替人做的决定）。
-- ④ 库侧自检（失败即迁移失败 → 启动失败）：
--    · `ck_biz_voucher_status` 必须允许 'DRAFT' / 'AUDITED' / 'POSTED'
--      （V0149 建表时已经是这 4 个值；任何后来把它收窄的改动都必须先被这里拦住，
--        否则接口一记账就是 23514，错误会以 500「数据不满足库约束」的形式甩给用户）
--    · 3 个权限码必须真的存在
--
-- ## 为什么不需要 UPDATE 存量数据
--
-- 三态是「手工凭证的落库状态」变了（DRAFT），不是「已有凭证的状态」变了：
-- 存量凭证都是自动 / 手工生成时即记账的真实业务，没有任何依据把它们退回草稿。
-- 卡面第 5 条要求也正是「现有 21 张 POSTED 保持 POSTED（不回退）」，本迁移以
-- 「一行都不写」实现它，并在 NOTICE 里把当时的 POSTED 张数打出来做证据。
--
-- ## 回滚
--
-- V0250__erp_voucher_three_state.down.sql：删掉 3 个权限码（先删 sys_role_menu 授权行，
-- 再删 sys_menu 行），不动 biz_voucher / biz_status_log。回滚后三个接口对非超管就是 403，
-- 但已经发生的状态流转与日志**保持原样**（历史不许被回滚抹掉）。
-- ======================================================

-- ---------- ① 库侧白名单自检（三态必须都在 CHECK 里） + posted_at 放开 NOT NULL ----------
--
-- `posted_at` 在 V0149 建表时是 `NOT NULL DEFAULT CURRENT_TIMESTAMP`（当时只有 POSTED 一种态，
-- 「凭证存在 ⟹ 已记账 ⟹ 有记账时间」成立）。三态之后这句话不再成立：草稿就是**还没记账**，
-- 硬给它盖一个记账时间会让列表 / 报表出现「草稿却有记账时间」的自相矛盾行 ——
-- 实测证据：不改这一列，落 DRAFT 直接 23502（not-null violation）。
ALTER TABLE biz_voucher ALTER COLUMN posted_at DROP NOT NULL;

DO $$
DECLARE
    v_def  text;
    v_bad  text;
    v_null boolean;
BEGIN
    SELECT pg_get_constraintdef((SELECT oid FROM pg_constraint WHERE conname = 'ck_biz_voucher_status'))
      INTO v_def;

    IF v_def IS NULL THEN
        RAISE EXCEPTION 'V0250 自检失败：biz_voucher 上没有 ck_biz_voucher_status 约束（V0149 应已建）';
    END IF;

    SELECT string_agg(want, ', ')
      INTO v_bad
      FROM unnest(ARRAY['DRAFT', 'AUDITED', 'POSTED']) AS want
     WHERE position('''' || want || '''' IN v_def) = 0;

    IF v_bad IS NOT NULL THEN
        RAISE EXCEPTION
            'V0250 自检失败：ck_biz_voucher_status 不允许 %（当前定义：%）。'
            'P0-FIX-4A 的三态需要 DRAFT/AUDITED/POSTED 全部在白名单里，'
            '请先放开约束再重跑本迁移', v_bad, v_def;
    END IF;

    -- posted_at 必须允许 NULL（草稿没有记账时间）
    SELECT is_nullable = 'YES' INTO v_null
      FROM information_schema.columns
     WHERE table_name = 'biz_voucher' AND column_name = 'posted_at';
    IF v_null IS NOT TRUE THEN
        RAISE EXCEPTION 'V0250 自检失败：biz_voucher.posted_at 仍是 NOT NULL（DRAFT 凭证写不进去）';
    END IF;

    RAISE NOTICE 'V0250 ①：ck_biz_voucher_status 已允许 DRAFT/AUDITED/POSTED，posted_at 已放开 NOT NULL';
END $$;

-- ---------- ② 种 3 个权限码（按钮型，父菜单 = 「凭证」20000000-…-000000001004） ----------
INSERT INTO sys_menu (id, name, menu_type, auth_code, path, parent_id, component,
                      sort_order, status, icon, title, is_visible, module_code)
VALUES
    ('20000000-0000-0000-0000-000000001032'::uuid, 'business:erp:voucher:audit', 3,
     'business:erp:voucher:audit', NULL,
     '20000000-0000-0000-0000-000000001004'::uuid, NULL,
     6, 1, NULL, '凭证审核（草稿 → 已审核，制单人不能审自己那张）', FALSE, 'business'),
    ('20000000-0000-0000-0000-000000001033'::uuid, 'business:erp:voucher:un-audit', 3,
     'business:erp:voucher:un-audit', NULL,
     '20000000-0000-0000-0000-000000001004'::uuid, NULL,
     7, 1, NULL, '凭证反审核（已审核 → 草稿，必须填原因）', FALSE, 'business'),
    ('20000000-0000-0000-0000-000000001034'::uuid, 'business:erp:voucher:post', 3,
     'business:erp:voucher:post', NULL,
     '20000000-0000-0000-0000-000000001004'::uuid, NULL,
     8, 1, NULL, '凭证记账（已审核 → 已记账，记账后只能红冲）', FALSE, 'business')
ON CONFLICT (id) DO NOTHING;

-- ---------- ③ 授权：跟着「制单权限」走（见文件头 ③） ----------
DO $$
DECLARE
    n_roles  integer;
    n_grants integer;
BEGIN
    SELECT count(*) INTO n_roles
      FROM sys_role r
     WHERE r.deleted_at IS NULL
       AND (
            r.role_code = 'SUPER_ADMIN'
            OR EXISTS (
                SELECT 1
                  FROM sys_role_menu rm
                  JOIN sys_menu m ON m.id = rm.menu_id
                 WHERE rm.role_id = r.id
                   AND m.auth_code = 'business:erp:voucher:create'
                   AND m.deleted_at IS NULL
            )
       );

    IF n_roles = 0 THEN
        RAISE NOTICE 'V0250 ③：没有任何角色持有 voucher:create（空库首启 / 种子未跑），跳过授权';
    ELSE
        INSERT INTO sys_role_menu (id, role_id, menu_id)
        SELECT gen_random_uuid(), r.id, m.id
          FROM sys_role r
          CROSS JOIN (VALUES
                ('20000000-0000-0000-0000-000000001032'::uuid),
                ('20000000-0000-0000-0000-000000001033'::uuid),
                ('20000000-0000-0000-0000-000000001034'::uuid)) AS m(id)
         WHERE r.deleted_at IS NULL
           AND (
                r.role_code = 'SUPER_ADMIN'
                OR EXISTS (
                    SELECT 1
                      FROM sys_role_menu rm
                      JOIN sys_menu cm ON cm.id = rm.menu_id
                     WHERE rm.role_id = r.id
                       AND cm.auth_code = 'business:erp:voucher:create'
                       AND cm.deleted_at IS NULL
                )
           )
           AND NOT EXISTS (
                SELECT 1 FROM sys_role_menu rm2
                 WHERE rm2.role_id = r.id AND rm2.menu_id = m.id
           );
        GET DIAGNOSTICS n_grants = ROW_COUNT;
        RAISE NOTICE 'V0250 ③：3 个权限码已种，授权新增 % 行（涉及 % 个角色）', n_grants, n_roles;
    END IF;
END $$;

-- ---------- ④ 结构与种子自检 + 存量证据 ----------
DO $$
DECLARE
    n_menus   integer;
    n_posted  integer;
    n_other   integer;
BEGIN
    SELECT count(*) INTO n_menus
      FROM sys_menu
     WHERE deleted_at IS NULL
       AND auth_code IN ('business:erp:voucher:audit',
                         'business:erp:voucher:un-audit',
                         'business:erp:voucher:post');
    IF n_menus <> 3 THEN
        RAISE EXCEPTION 'V0250 自检失败：预期 3 个新权限码，实际 % 个', n_menus;
    END IF;

    -- 存量口径证据（不写库，只打印）：迁移前后 POSTED 张数必须一致
    SELECT count(*) FILTER (WHERE status = 'POSTED'),
           count(*) FILTER (WHERE status <> 'POSTED')
      INTO n_posted, n_other
      FROM biz_voucher
     WHERE deleted_at IS NULL;
    RAISE NOTICE 'V0250 ④：存量凭证 POSTED % 张 / 非 POSTED % 张（本迁移不改任何凭证行）',
                 n_posted, n_other;

    RAISE NOTICE 'V0250 完成：凭证三态审核链的权限码就绪（DRAFT → AUDITED → POSTED）';
END $$;

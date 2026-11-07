-- ======================================================
-- V0317 回滚：客户对账单（SO-43）
--
-- 顺序：授权 → 按钮 → 表（先删子后删父，避免撞 FK）。
-- 只删本迁移自己插入的固定 uuid / 固定 auth_code 行，不碰别人的菜单。
-- 注：本迁移不种系统字典、不种角色，故回滚只回菜单、授权与表；
--     `biz_statement_confirm` 有数据时先 RAISE WARNING（生产回滚前请先 pg_dump -t）。
-- ======================================================

-- ---------- ① 授权 ----------
DELETE FROM sys_role_menu
 WHERE menu_id IN ('20000000-0000-0000-0000-000000003175'::uuid,
                   '20000000-0000-0000-0000-000000003176'::uuid,
                   '20000000-0000-0000-0000-000000003177'::uuid,
                   '20000000-0000-0000-0000-000000003178'::uuid,
                   '20000000-0000-0000-0000-000000003179'::uuid);

-- ---------- ② 按钮（menu_type=3） ----------
DELETE FROM sys_menu
 WHERE id IN ('20000000-0000-0000-0000-000000003175'::uuid,
              '20000000-0000-0000-0000-000000003176'::uuid,
              '20000000-0000-0000-0000-000000003177'::uuid,
              '20000000-0000-0000-0000-000000003178'::uuid,
              '20000000-0000-0000-0000-000000003179'::uuid);

-- ---------- ③ 对账确认台账 ----------
DO $$
DECLARE
    n bigint;
BEGIN
    IF to_regclass('public.biz_statement_confirm') IS NOT NULL THEN
        EXECUTE 'SELECT count(*) FROM biz_statement_confirm' INTO n;
        IF n > 0 THEN
            RAISE WARNING 'V0317 down：biz_statement_confirm 有 % 行数据将被删除（生产回滚前请先 pg_dump）', n;
        END IF;
    END IF;
END $$;

DROP TABLE IF EXISTS biz_statement_confirm;

-- ---------- ④ 自检：零残留 ----------
DO $$
DECLARE
    n integer;
BEGIN
    IF to_regclass('public.biz_statement_confirm') IS NOT NULL THEN
        RAISE EXCEPTION 'V0317 down 失败：biz_statement_confirm 仍存在';
    END IF;
    SELECT count(*) INTO n FROM sys_menu
     WHERE id IN ('20000000-0000-0000-0000-000000003175'::uuid,
                  '20000000-0000-0000-0000-000000003176'::uuid,
                  '20000000-0000-0000-0000-000000003177'::uuid,
                  '20000000-0000-0000-0000-000000003178'::uuid,
                  '20000000-0000-0000-0000-000000003179'::uuid);
    IF n <> 0 THEN
        RAISE EXCEPTION 'V0317 down 失败：对账单按钮仍剩 % 行', n;
    END IF;
    SELECT count(*) INTO n FROM sys_menu
     WHERE deleted_at IS NULL
       AND auth_code IN ('business:erp:statement:view', 'business:erp:statement:export',
                         'business:erp:statement:send', 'business:erp:statement:confirm',
                         'business:erp:statement:dispute');
    IF n <> 0 THEN
        RAISE EXCEPTION 'V0317 down 失败：5 个权限码仍剩 % 行承载', n;
    END IF;
    RAISE NOTICE 'V0317 down 完成：对账确认台账 + 按钮 + 授权零残留';
END $$;

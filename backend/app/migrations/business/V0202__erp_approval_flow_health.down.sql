-- V0202 down：撤掉 2 个权限码的登记与授权。
--
-- ⚠️ 回滚前请先确认业务侧不再依赖「默认拒绝」：
--   · 回滚本迁移**不会**把代码改回静默降级（那是二进制的事），
--     它只是让 `business:erp:skip-approval` / `business:erp:approval-flow:health`
--     从 `sys_menu` 消失 —— 此时「显式跳过审批」对**非超管**不可用（码不存在就无法授予），
--     而超管因通配权限仍可跳过。
--   · 因此 down 只用于「本迁移误上」的即时回退；正常回滚要连同二进制一起回滚。

-- ---------- ① 授权（先删引用，再删菜单） ----------
DELETE FROM sys_role_menu
 WHERE menu_id IN ('20000000-0000-0000-0000-000000001200'::uuid,
                   '20000000-0000-0000-0000-000000001201'::uuid);

-- ---------- ② 菜单行 ----------
DELETE FROM sys_menu
 WHERE id IN ('20000000-0000-0000-0000-000000001200'::uuid,
              '20000000-0000-0000-0000-000000001201'::uuid);

-- ---------- ③ 自检：零残留 ----------
DO $$
DECLARE
    n_left int;
BEGIN
    SELECT count(*) INTO n_left
      FROM sys_menu
     WHERE id IN ('20000000-0000-0000-0000-000000001200'::uuid,
                  '20000000-0000-0000-0000-000000001201'::uuid);
    IF n_left <> 0 THEN
        RAISE EXCEPTION 'V0202 down 自检失败：仍有 % 行权限码残留', n_left;
    END IF;
    SELECT count(*) INTO n_left
      FROM sys_role_menu
     WHERE menu_id IN ('20000000-0000-0000-0000-000000001200'::uuid,
                       '20000000-0000-0000-0000-000000001201'::uuid);
    IF n_left <> 0 THEN
        RAISE EXCEPTION 'V0202 down 自检失败：仍有 % 行角色授权残留', n_left;
    END IF;
    RAISE NOTICE 'V0202 down 完成：2 个权限码与授权零残留';
END $$;

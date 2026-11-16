-- ============================================================================
-- V0426__inventory_slow_moving.down（M2-10 · 呆滞料标记）
--
-- 只回滚本迁移**种下的三样东西**：
--   ① 权限码授权行（sys_role_menu，按固定 menu id 反查）；
--   ② 权限点本身（sys_menu，固定 UUID）；
--   ③ 阈值参数（sys_config.erp.inventory.slow-moving-days）。
--
-- **不动**：库龄层函数 biz_inventory_aging_layers（V0424）、流水 / 单据 / 库存（业务数据）、
-- 其它迁移种下的参数与菜单。回滚后 M2-08 明细、M2-09 报表照常可用。
--
-- ③ 的删除带 description 前缀：万一上线前运维手工先建过同名参数行，down 不该把它删掉
-- （本迁移的 up 在这种情形下本来就不会插入，所以「是我们种的」这件事由 description 认领）。
-- ============================================================================

DELETE FROM sys_role_menu
 WHERE menu_id = '20000000-0000-0000-0000-000000004260'::uuid;

DELETE FROM sys_menu
 WHERE id = '20000000-0000-0000-0000-000000004260'::uuid;

DELETE FROM sys_config
 WHERE config_key = 'erp.inventory.slow-moving-days'
   AND description LIKE '呆滞料判定阈值（天）%';

DO $$
DECLARE
    n_menu  integer;
    n_grant integer;
BEGIN
    SELECT count(*) INTO n_menu
      FROM sys_menu
     WHERE id = '20000000-0000-0000-0000-000000004260'::uuid;
    SELECT count(*) INTO n_grant
      FROM sys_role_menu
     WHERE menu_id = '20000000-0000-0000-0000-000000004260'::uuid;
    IF n_menu <> 0 OR n_grant <> 0 THEN
        RAISE EXCEPTION 'V0426.down 自检失败：呆滞料按钮/授权未删净（菜单 % 行、授权 % 行）', n_menu, n_grant;
    END IF;
    IF NOT EXISTS (SELECT 1 FROM pg_proc p JOIN pg_namespace n ON n.oid = p.pronamespace
                    WHERE n.nspname = 'public' AND p.proname = 'biz_inventory_aging_layers') THEN
        RAISE EXCEPTION 'V0426.down 自检失败：库龄层函数被误删（V0424 的东西 down 不许动）';
    END IF;
    RAISE NOTICE 'V0426.down 完成：菜单/授权已清、参数已清、库龄层函数仍在';
END $$;

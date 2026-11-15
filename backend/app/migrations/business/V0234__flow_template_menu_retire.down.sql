-- ============================================================================
-- V0234__flow_template_menu_retire.down.sql
--
-- 回滚【P2-6d】：把 `/flow/templates`（流程模板库）菜单节点的 `deleted_at` 置回 NULL。
--
-- 说明：
--   · up 的逆操作，且只翻回软删标记 —— 行内容（name / component / auth_code /
--     sort_order）与 `sys_role_menu` 绑定在软删期间一直没动，所以回滚后节点
--     一摸一样地回到侧边栏；
--   · 幂等：`AND deleted_at IS NOT NULL`，重复执行 0 行变更；
--   · 回滚**不**改 `updated_at`：回滚本身就是一次变动，保留真实时间比造一个假历史更诚实
--     （与 V0220 down 同一取舍）；
--   · 单独跑 down 会让侧边栏重新出现两个入口（一个指向 redirect 到 `?view=standard` 的
--     老路由），所以只用于「回退本卡 + 回退前端 P2-6c」的完整回滚。
-- ============================================================================

UPDATE sys_menu
   SET deleted_at = NULL
 WHERE path = '/flow/templates'
   AND deleted_at IS NOT NULL;

DO $$
DECLARE
    live_tpl int;
BEGIN
    -- 回滚后节点必须恰好是 1 个在用行（V0194 种下的是固定 UUID 单行）
    SELECT count(*) INTO live_tpl
      FROM sys_menu
     WHERE path = '/flow/templates'
       AND deleted_at IS NULL;
    IF live_tpl <> 1 THEN
        RAISE EXCEPTION 'V0234 down 自检失败：/flow/templates 在用节点数 = %（回滚后必须为 1）', live_tpl;
    END IF;
END $$;

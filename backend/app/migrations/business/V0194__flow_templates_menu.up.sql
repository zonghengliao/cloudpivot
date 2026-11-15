-- ======================================================
-- V0194：流程模板库 · 菜单节点（P-FLOW-5）
--
-- 依据：卡面 t_84fbfea1「四、验收 ① 打开模板库 → 看到标准流程分 3 组」
-- —— 没有菜单入口就「打不开」，所以本页需要一条 `sys_menu` 页面节点。
--
-- 权限口径：页面本身用 `business:flow:read`（与「流程定义」一致，V0121 已授权给所有
-- 已授权查看流程的角色）；「另存为」按钮额外受 `business:flow:update` 保护、「启用/停用」
-- 受 `business:flow:publish` 保护 —— 前端用 `v-privilege` 隐藏按钮，服务端仍是最终依据。
--
-- 幂等：固定 UUID + `ON CONFLICT (id) DO NOTHING`（不覆盖运维改过的标题 / 排序）。
-- 回滚：`V0194__flow_templates_menu.down.sql`
-- ======================================================

INSERT INTO sys_menu (id, name, menu_type, auth_code, path, parent_id, component,
                      sort_order, status, icon, title, is_visible, module_code)
VALUES
    ('20000000-0000-0000-0000-0000000007a5'::uuid, '流程模板库', 2, 'business:flow:read',
     '/flow/templates', NULL, 'business/flow/templates/index',
     69, 1, 'copy', '流程模板库（标准模板 / 启用停用 / 另存为）', TRUE, 'business')
ON CONFLICT (id) DO NOTHING;

DO $$
DECLARE
    found int;
    wrong int;
BEGIN
    SELECT count(*) INTO found
      FROM sys_menu
     WHERE id = '20000000-0000-0000-0000-0000000007a5'::uuid
       AND deleted_at IS NULL;
    IF found <> 1 THEN
        RAISE EXCEPTION 'V0194 自检失败：流程模板库菜单节点未落地';
    END IF;

    SELECT count(*) INTO wrong
      FROM sys_menu
     WHERE id = '20000000-0000-0000-0000-0000000007a5'::uuid
       AND (auth_code <> 'business:flow:read'
            OR component <> 'business/flow/templates/index'
            OR path <> '/flow/templates'
            OR menu_type <> 2
            OR is_visible = FALSE);
    IF wrong <> 0 THEN
        RAISE EXCEPTION 'V0194 自检失败：流程模板库菜单节点字段与前端路由不一致';
    END IF;

    RAISE NOTICE 'V0194 自检通过：流程模板库菜单节点（/flow/templates）';
END
$$;

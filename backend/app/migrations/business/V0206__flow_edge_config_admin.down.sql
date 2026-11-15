-- ============================================================
-- V0206 down：撤掉「状态机配置审批化」的种子与权限码
--
-- 顺序：审批人 → 边 → 节点 → 版本 → 定义 → 角色授权 → 菜单 → 角色
--      （`biz_flow_node` / `biz_flow_edge` 对 version 是 ON DELETE CASCADE，
--        这里仍按显式顺序删，便于在没有级联的环境里也能回滚干净）
--
-- # 两道前置闸（都是 EXCEPTION，不是 WARN）
--
--   ① **还有在跑的配置变更审批单**：回滚后那些实例会变成点进去 404 的孤儿。
--   ② **已经有审批历史**（任何状态的 FLOW_EDGE_CFG 实例）：`biz_flow_instance.version_id`
--      对 `biz_flow_def_version` 是 RESTRICT（真机实测会撞
--      `biz_flow_instance_version_id_fkey`），而且 —— 更重要的是 —— **审批历史是审计材料**，
--      不能因为「回滚一条迁移」被顺手删掉。要回滚请先归档 / 清理历史（另开数据处置卡）。
--      全新库（从没发起过配置变更）不受影响，down/up 往返照常可验。
--
-- # 刻意**不动**的东西
--
--   · `biz_flow_state_edge`（D1 的配置表）：回滚本迁移不该丢掉客户配好的边；
--   · `biz_status_log`（审计只增不改）。
-- ============================================================

DO $$
DECLARE
    n_running int;
    n_history int;
BEGIN
    SELECT count(*) INTO n_running
      FROM biz_flow_instance
     WHERE biz_type = 'FLOW_EDGE_CFG' AND status = 'PENDING' AND deleted_at IS NULL;
    IF n_running > 0 THEN
        RAISE EXCEPTION
            'V0206 回滚被拒绝：还有 % 张在跑的「状态机配置变更」审批单（biz_type=FLOW_EDGE_CFG）。'
            '请先在审批中心处理完（同意/驳回/撤回）再回滚，否则这些实例会变成孤儿。', n_running;
    END IF;

    SELECT count(*) INTO n_history
      FROM biz_flow_instance
     WHERE biz_type = 'FLOW_EDGE_CFG' AND deleted_at IS NULL;
    IF n_history > 0 THEN
        RAISE EXCEPTION
            'V0206 回滚被拒绝：已经有 % 张「状态机配置变更」审批单的**历史**（任何状态）。'
            '审批历史是审计材料，不随迁移回滚删除；biz_flow_instance.version_id 的外键也是 RESTRICT。'
            '要回滚本迁移，请先把这些历史归档 / 处置掉（数据处置另开卡），或在全新库上验 down/up。',
            n_history;
    END IF;
END $$;

DELETE FROM biz_flow_node_assignee
 WHERE node_id IN ('20000000-0000-4000-8000-0000000e0210'::uuid,
                   '20000000-0000-4000-8000-0000000e0211'::uuid,
                   '20000000-0000-4000-8000-0000000e0212'::uuid);

DELETE FROM biz_flow_edge
 WHERE version_id = '20000000-0000-4000-8000-0000000e0207'::uuid;

DELETE FROM biz_flow_node
 WHERE version_id = '20000000-0000-4000-8000-0000000e0207'::uuid;

-- 先清 current_version_id，再删版本（PUBLISHED 必须有版本，反过来会撞 CHECK）
UPDATE biz_flow_def
   SET status = 'DISABLED', current_version_id = NULL, current_version_no = 0, updated_at = now()
 WHERE id = '20000000-0000-4000-8000-0000000e0206'::uuid;

DELETE FROM biz_flow_def_version
 WHERE id = '20000000-0000-4000-8000-0000000e0207'::uuid;

DELETE FROM biz_flow_def
 WHERE id = '20000000-0000-4000-8000-0000000e0206'::uuid;

DELETE FROM sys_role_menu
 WHERE menu_id IN ('20000000-0000-0000-0000-000000001210'::uuid,
                   '20000000-0000-0000-0000-000000001211'::uuid);

DELETE FROM sys_menu
 WHERE id IN ('20000000-0000-0000-0000-000000001210'::uuid,
              '20000000-0000-0000-0000-000000001211'::uuid);

-- 角色：先清本角色上的全部授权（`sys_role_menu_role_id_fkey` 是真机实测撞到的：只按 menu_id 删
-- 会漏掉「本角色被授予的其他菜单」），再删角色；有人绑着就留着（`sys_user_role` 外键）
DELETE FROM sys_role_menu
 WHERE role_id = '20000000-0000-4000-8000-0000000f0015'::uuid;

DELETE FROM sys_role
 WHERE id = '20000000-0000-4000-8000-0000000f0015'::uuid
   AND NOT EXISTS (
        SELECT 1 FROM sys_user_role ur WHERE ur.role_id = '20000000-0000-4000-8000-0000000f0015'::uuid
   );

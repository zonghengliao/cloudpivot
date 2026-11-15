-- ============================================================================
-- V0400__hr_flow_approver_role_grants.down.sql
--
-- 只删本迁移加的那一条授权 (HR_MANAGER → business:flow:approve …0803)。
--
-- ⚠ 授权行（sys_role_menu）没有「来源」列：若升级前有人手工给 HR_MANAGER 勾过
--   审批权限点，回滚会一并删掉。回滚只用于开发/演练环境；生产回滚前先导出：
--     SELECT rm.* FROM sys_role_menu rm
--       JOIN sys_role r ON r.id = rm.role_id
--      WHERE r.role_code = 'HR_MANAGER'
--        AND rm.menu_id = '20000000-0000-0000-0000-000000000803';
--
-- 范围刻意收到 role_code = 'HR_MANAGER'：V0239 的 down 是按**节点**整片清理，
-- 本文件不重复那个口径，免得回滚一张 HR 卡把别的角色的审批权一起删掉。
-- ============================================================================

DELETE FROM sys_role_menu rm
 USING sys_role r
 WHERE rm.role_id = r.id
   AND r.role_code = 'HR_MANAGER'
   AND rm.menu_id = '20000000-0000-0000-0000-000000000803'::uuid;

-- 自检：本迁移删掉的那一对必须不在了（其余角色的审批授权不动，不做总数断言）
DO $$
DECLARE
    left_rows int;
BEGIN
    SELECT count(*) INTO left_rows
      FROM sys_role_menu rm
      JOIN sys_role r ON r.id = rm.role_id
     WHERE r.role_code = 'HR_MANAGER'
       AND rm.menu_id = '20000000-0000-0000-0000-000000000803'::uuid;
    IF left_rows <> 0 THEN
        RAISE EXCEPTION 'V0400 down 回滚不完整：(HR_MANAGER, business:flow:approve) 仍剩 % 行', left_rows;
    END IF;
    RAISE NOTICE 'V0400 down 回滚完成；(HR_MANAGER, business:flow:approve) 已清除';
END $$;

-- ======================================================
-- V0247 回滚：数据权限最小授权 → 还原为迁移前基线
--
-- 迁移前基线（【P0-FIX-2】评审实测）：sys_role 全库 data_scope = 1（默认值），
-- sys_role_dept 0 行。本 down 逐条还原到那个状态。
--
-- 注意（与其它种子类迁移同口径）：
--   · 只碰 V0247 改过的 5 个角色，其它角色/用户/部门一律不动；
--   · FINANCE_MANAGER / FINANCE_DIRECTOR 的 sys_role_dept 行会被**整批删除**
--     （含后来在管理界面新配的），因为它们还原的是「基线」而不是「上一次的值」。
-- ======================================================

UPDATE sys_role
   SET data_scope = 1,
       updated_at = CURRENT_TIMESTAMP
 WHERE role_code IN ('DEPT_LEADER', 'PURCHASE_MANAGER', 'WAREHOUSE_MANAGER',
                     'FINANCE_MANAGER', 'FINANCE_DIRECTOR')
   AND deleted_at IS NULL;

DELETE FROM sys_role_dept
 WHERE role_id IN (SELECT id FROM sys_role
                    WHERE role_code IN ('FINANCE_MANAGER', 'FINANCE_DIRECTOR'));

DO $$
BEGIN
    IF EXISTS (SELECT 1 FROM sys_role
                WHERE role_code IN ('DEPT_LEADER', 'PURCHASE_MANAGER', 'WAREHOUSE_MANAGER',
                                    'FINANCE_MANAGER', 'FINANCE_DIRECTOR')
                  AND deleted_at IS NULL AND data_scope <> 1) THEN
        RAISE EXCEPTION 'V0247 down 自检失败：仍有角色 data_scope <> 1';
    END IF;
    RAISE NOTICE 'V0247 down 完成：5 个角色回 1，财务两个角色的 sys_role_dept 已清空';
END $$;

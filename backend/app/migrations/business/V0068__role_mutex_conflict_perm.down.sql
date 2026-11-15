-- ======================================================
-- V0068 回滚：收回「冲突查询」按钮上的补授
--
-- 语义：把 `system:role-mutex:conflict` 从**非超管**且持 `system:user:role` 的角色上收回。
--
-- 为什么保留超管：超管本来就该能看冲突清单（V0065 已单独授过），
-- 回滚 V0068 不该顺手剥夺超管的能力。
--
-- 为什么无法精确「只删本迁移插入的行」：`sys_role_menu` 没有 `source` 列，
-- 迁移插入的行与运维手工授予的行在表里不可区分。所以本回滚的口径是
-- 「回到 V0068 之前的状态」，即：这些角色不该有这个按钮。
-- 如果运维在 V0068 之后曾手工授予该按钮给某个非超管角色，回滚会一并收回 ——
-- 这正是「回滚到迁移前」的定义，需要保留就得在回滚后重新授一次。
-- ======================================================

DELETE FROM sys_role_menu granted
 WHERE granted.menu_id = '20000000-0000-0000-0000-000000000624'::uuid
   AND granted.role_id IN (
           SELECT DISTINCT rm.role_id
             FROM sys_role_menu rm
            INNER JOIN sys_menu anchor
               ON anchor.id = rm.menu_id
              AND anchor.auth_code = 'system:user:role'
            INNER JOIN sys_role r
               ON r.id = rm.role_id
              AND r.deleted_at IS NULL
              AND r.role_code <> 'SUPER_ADMIN'
       );

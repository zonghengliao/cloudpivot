-- ============================================================================
-- 回滚 V0366：撤掉 ①两条种子流程的 is_system、②全新库种的根部门 HQ
--
-- 口径：
--   · is_system 按**同一对种子 UUID** 回退成 FALSE（V0193 的列默认值）。
--     选择键与 up 完全对称，回滚的语义就是「回到本迁移之前」。
--   · HQ 部门只删**本迁移自己种的那一个固定 UUID**，绝不按 dept_code 删 ——
--     运维按环境建的 HQ 是别人的数据。
--   · up 不动 `sys_user`（admin 落位在启动期做），所以 down 也不碰账号；
--     但删部门前仍先把指向它的引用摘干净（`sys_user.dept_id` / 子部门 `parent_id`），
--     否则下次启动在 N2 / 部门树里看到悬空引用。
-- ============================================================================

UPDATE biz_flow_def
   SET is_system = FALSE,
       updated_at = now()
 WHERE id IN (
       '20000000-0000-4000-8000-0000000d0331'::uuid,
       '20000000-0000-4000-8000-0000000e0331'::uuid
       )
   AND flow_code IN ('pr_approve', 'prt_approve')
   AND deleted_at IS NULL
   AND is_system IS DISTINCT FROM FALSE;

-- 摘引用：只摘「指向本迁移种的那一行 HQ」的，运维后来给别人调整的部门不动
UPDATE sys_user
   SET dept_id = NULL,
       updated_at = now()
 WHERE dept_id = '20000000-0000-4000-8000-0000000f0366'::uuid;

-- 子部门挂回根（正常不会有，防悬空）
UPDATE sys_dept
   SET parent_id = NULL,
       updated_at = now()
 WHERE parent_id = '20000000-0000-4000-8000-0000000f0366'::uuid;

-- 只删本迁移种的那一行，且要求已无任何用户挂靠
DELETE FROM sys_dept
 WHERE id = '20000000-0000-4000-8000-0000000f0366'::uuid
   AND dept_code = 'HQ'
   AND NOT EXISTS (SELECT 1 FROM sys_user u WHERE u.dept_id = '20000000-0000-4000-8000-0000000f0366'::uuid);

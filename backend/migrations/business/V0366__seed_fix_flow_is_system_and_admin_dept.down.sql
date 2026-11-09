-- ============================================================================
-- 回滚 V0366：撤掉 ①两条种子流程的 is_system、②③全新库 HQ 部门与 admin 落位
--
-- 口径：
--   · is_system 按**同一对种子 UUID** 回退成 FALSE（V0193 的列默认值）。
--     已经被后续版本 / 客户改过的行不动？—— 不：这里与 up 完全对称（同一 id 集合），
--     回滚的语义是「回到本迁移之前」，所以无条件回退这两行。
--   · HQ 部门只删**本迁移自己种的那一个固定 UUID**，绝不按 dept_code 删 ——
--     运维按环境建的 HQ 是别人的数据。
--   · 删 HQ 之前先把指向它的引用清空（`sys_user.dept_id` / `sys_dept.parent_id` /
--     `leader_id` 不涉及），否则外键或悬空引用会让下一次启动的巡检报错。
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

-- admin 落位回退（只回退「指向本迁移种的 HQ」的那一行，别把运维后来调整的部门清掉）
UPDATE sys_user
   SET dept_id = NULL,
       updated_at = now()
 WHERE username = 'admin'
   AND dept_id = '20000000-0000-4000-8000-0000000f0366'::uuid;

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

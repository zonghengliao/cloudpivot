-- T7b 验收数据清理（软删，可回滚）：任务 + 临时账号 + 临时角色
BEGIN;

-- ① 软删本轮造的验收 / 分页任务（标题前缀是唯一判据）
UPDATE biz_task
   SET deleted_at = CURRENT_TIMESTAMP, updated_at = CURRENT_TIMESTAMP
 WHERE deleted_at IS NULL
   AND (title LIKE 'T7b验收-%' OR title LIKE 'T7b分页-%');

-- ② 临时账号：软删 + 停用 + 解绑角色
DELETE FROM sys_user_role WHERE user_id = '7f000000-0000-0000-0000-000000000b02'::uuid;
UPDATE sys_user
   SET deleted_at = CURRENT_TIMESTAMP, status = 0, updated_at = CURRENT_TIMESTAMP
 WHERE id = '7f000000-0000-0000-0000-000000000b02'::uuid;

-- ③ 临时角色：软删 + 停用 + 撤授权
DELETE FROM sys_role_menu WHERE role_id = '7f000000-0000-0000-0000-000000000b01'::uuid;
UPDATE sys_role
   SET deleted_at = CURRENT_TIMESTAMP, status = 0, updated_at = CURRENT_TIMESTAMP
 WHERE id = '7f000000-0000-0000-0000-000000000b01'::uuid;

COMMIT;

-- 复核
SELECT 'live_tasks' AS k, count(*)::text AS v FROM biz_task WHERE deleted_at IS NULL
UNION ALL
SELECT 't7b_live', count(*)::text FROM biz_task
 WHERE deleted_at IS NULL AND (title LIKE 'T7b验收-%' OR title LIKE 'T7b分页-%')
UNION ALL
SELECT 'qa.boarder_live', count(*)::text FROM sys_user WHERE username = 'qa.boarder' AND deleted_at IS NULL
UNION ALL
SELECT 't7b_role_live', count(*)::text FROM sys_role WHERE role_code = 'T7B_BOARDER' AND deleted_at IS NULL;

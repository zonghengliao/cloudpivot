-- ============================================================================
-- P0-FIX-3B · 只读盘点（不改库）
--   ① 4 个 QA 夹具角色的 data_scope / status / 活跃持有者数
--   ② 它们的全部持有者（含已禁用/已软删）—— 说明「为什么现在能安全收紧」
--   ③ 生产全部角色与范围（确认不误伤业务角色）
--   ④ 所有在用账号的「有效数据范围」逐账号核算
-- ============================================================================
\pset pager off
\echo '=== ① 目标角色：范围 / 状态 / 活跃持有者数（deleted_at is null and status=1）==='
SELECT r.role_code,
       r.data_scope,
       r.status,
       (SELECT count(*) FROM sys_user_role ur
          JOIN sys_user u ON u.id = ur.user_id
         WHERE ur.role_id = r.id
           AND u.deleted_at IS NULL AND u.status = 1) AS active_holders,
       (SELECT count(*) FROM sys_user_role ur WHERE ur.role_id = r.id)                 AS holder_rows_all
  FROM sys_role r
 WHERE r.deleted_at IS NULL
   AND r.role_code IN ('QA_LIMITED','QA_VIEWER','MUTEX_A','MUTEX_B')
 ORDER BY r.role_code;

\echo ''
\echo '=== ② 目标角色的全部持有者明细（含已禁用/已软删账号）==='
SELECT r.role_code, u.username, u.status AS user_status,
       (u.deleted_at IS NOT NULL) AS user_deleted, ur.expire_time
  FROM sys_role r
  JOIN sys_user_role ur ON ur.role_id = r.id
  JOIN sys_user u       ON u.id = ur.user_id
 WHERE r.deleted_at IS NULL
   AND r.role_code IN ('QA_LIMITED','QA_VIEWER','MUTEX_A','MUTEX_B')
 ORDER BY r.role_code, u.username;

\echo ''
\echo '=== ③ 生产全部角色（确认业务角色范围，不许误伤）==='
SELECT r.role_code, r.data_scope, r.status,
       (SELECT count(*) FROM sys_user_role ur
          JOIN sys_user u ON u.id = ur.user_id
         WHERE ur.role_id = r.id
           AND u.deleted_at IS NULL AND u.status = 1) AS active_holders
  FROM sys_role r
 WHERE r.deleted_at IS NULL
 ORDER BY r.data_scope DESC, r.role_code;

\echo ''
\echo '=== ④ 在用账号逐账号：持有的启用角色 + 有效范围（收紧前后对比用）==='
SELECT u.username,
       u.status,
       coalesce(string_agg(r.role_code || ':' || r.data_scope::text, ',' ORDER BY r.role_code), '(无启用角色)') AS roles,
       (EXISTS (SELECT 1 FROM sys_user_role ur JOIN sys_role r2 ON r2.id = ur.role_id
                 WHERE ur.user_id = u.id AND r2.deleted_at IS NULL AND r2.status = 1
                   AND r2.role_code = 'SUPER_ADMIN'
                   AND (ur.expire_time IS NULL OR ur.expire_time > now()))) AS is_super
  FROM sys_user u
  LEFT JOIN sys_user_role ur ON ur.user_id = u.id AND (ur.expire_time IS NULL OR ur.expire_time > now())
  LEFT JOIN sys_role r       ON r.id = ur.role_id AND r.deleted_at IS NULL AND r.status = 1
 WHERE u.deleted_at IS NULL AND u.status = 1
 GROUP BY u.id, u.username, u.status
 ORDER BY u.username;

\echo ''
\echo '=== ⑤ sys_role_dept 现状（4 个目标角色应 0 行；全库行数仅作基线）==='
SELECT (SELECT count(*) FROM sys_role_dept) AS role_dept_total,
       (SELECT count(*) FROM sys_role_dept rd JOIN sys_role r ON r.id = rd.role_id
         WHERE r.role_code IN ('QA_LIMITED','QA_VIEWER','MUTEX_A','MUTEX_B')) AS target_role_dept_rows;

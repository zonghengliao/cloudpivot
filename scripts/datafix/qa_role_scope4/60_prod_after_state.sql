\pset pager off
\echo '=== ① 4 个夹具角色（应用后：data_scope / status / 是否存活 / 活跃持有者）==='
SELECT r.role_code, r.data_scope, r.status, (r.deleted_at IS NULL) AS alive,
       (SELECT count(*) FROM sys_user_role ur JOIN sys_user u ON u.id = ur.user_id
         WHERE ur.role_id = r.id AND u.deleted_at IS NULL AND u.status = 1) AS active_holders
  FROM sys_role r
 WHERE r.role_code IN ('QA_LIMITED','QA_VIEWER','MUTEX_A','MUTEX_B')
 ORDER BY r.role_code;

\echo ''
\echo '=== ② 生产全部角色（12 个：业务角色范围未变 / 夹具角色=4）==='
SELECT role_code, data_scope, status,
       (SELECT count(*) FROM sys_user_role ur JOIN sys_user u ON u.id = ur.user_id
         WHERE ur.role_id = r.id AND u.deleted_at IS NULL AND u.status = 1) AS active_holders
  FROM sys_role r WHERE r.deleted_at IS NULL
 ORDER BY data_scope DESC, role_code;

\echo ''
\echo '=== ③ 迁移记账尾部（246 之后）==='
SELECT version, name, applied_via, checksum, applied_seq, success
  FROM schema_migrations
 WHERE version ~ '^[0-9]+$' AND version::int >= 246
 ORDER BY version::int;

\echo ''
\echo '=== ④ 本卡新增的对象只有一个迁移文件（无遗留 view/表）==='
SELECT count(*) AS leftover_qa4_objects
  FROM information_schema.tables
 WHERE table_name ILIKE '%qa4%' OR table_name ILIKE '%qa_role%';

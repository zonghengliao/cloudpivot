\pset pager off
\echo '=== schema_migrations 结构 ==='
SELECT column_name, data_type FROM information_schema.columns
 WHERE table_name = 'schema_migrations' ORDER BY ordinal_position;
\echo ''
\echo '=== 最近 12 条已应用迁移 ==='
SELECT version, kind, applied_via, applied_at, checksum IS NOT NULL AS has_checksum
  FROM schema_migrations ORDER BY version DESC LIMIT 12;
\echo ''
\echo '=== 计数 ==='
SELECT count(*) AS total FROM schema_migrations;

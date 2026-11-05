\pset pager off
\echo '=== 最近 15 条（按 applied_at）==='
SELECT version, name, kind, applied_via, applied_at
  FROM schema_migrations ORDER BY applied_at DESC, applied_seq DESC LIMIT 15;
\echo ''
\echo '=== 所有含 data_scope / role 的迁移记录 ==='
SELECT version, name, kind, applied_via, applied_at
  FROM schema_migrations WHERE name ILIKE '%data_scope%' OR name ILIKE '%role%' ORDER BY applied_at DESC;
\echo ''
\echo '=== version 的形态分布（前 20）==='
SELECT version, name FROM schema_migrations ORDER BY applied_at DESC LIMIT 20;

\pset pager off
\echo '=== version >= 235 的全部记录（看 247/248 是否已在账）==='
SELECT version, name, kind, applied_via, applied_at, success
  FROM schema_migrations WHERE version ~ '^[0-9]+$' AND version::int >= 235
 ORDER BY version::int;
\echo ''
\echo '=== 有没有 247 / 248 ==='
SELECT count(*) FILTER (WHERE version = '247') AS v247,
       count(*) FILTER (WHERE version = '248') AS v248,
       count(*) FILTER (WHERE version = '246') AS v246
  FROM schema_migrations;

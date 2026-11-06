-- ======================================================
-- SO-12 回滚演练：事务内 down → up → ROLLBACK（不留副作用）
-- 用法：PGPASSWORD=alun psql -h 127.0.0.1 -U alun -d <已应用 V0287 的库> -v ON_ERROR_STOP=1 -f rollback-drill.sql
-- 断言：down 后表消失 + AR 取号行消失 + payable 仍在；up 重放后表/索引/约束/取号行全部回来；
--       ROLLBACK 后库回到演练前状态（表仍在）。
-- ======================================================
\set ON_ERROR_STOP on
\echo '=== [1] 演练前 ==='
SELECT to_regclass('public.biz_receivable')                   AS table_before,
       (SELECT count(*) FROM biz_seq WHERE seq_type = 'AR')   AS ar_seq_before,
       (SELECT count(*) FROM pg_indexes WHERE tablename = 'biz_receivable') AS idx_before;

BEGIN;

\echo '=== [2] 执行 down ==='
\i /opt/cloudpivot/backend/migrations/business/V0287__sales_receivable.down.sql
SELECT to_regclass('public.biz_receivable')                   AS table_after_down,
       (SELECT count(*) FROM biz_seq WHERE seq_type = 'AR')   AS ar_seq_after_down,
       (SELECT count(*) FROM pg_indexes WHERE tablename = 'biz_receivable') AS idx_after_down,
       to_regclass('public.biz_payable')                      AS payable_still_there;

\echo '=== [3] 重放 up ==='
\i /opt/cloudpivot/backend/migrations/business/V0287__sales_receivable.up.sql
SELECT to_regclass('public.biz_receivable')                   AS table_after_up,
       (SELECT count(*) FROM biz_seq WHERE seq_type = 'AR')   AS ar_seq_after_up,
       (SELECT count(*) FROM pg_indexes WHERE tablename = 'biz_receivable') AS idx_after_up,
       (SELECT count(*) FROM pg_constraint
         WHERE conrelid = 'public.biz_receivable'::regclass AND contype = 'c') AS checks_after_up;

ROLLBACK;

\echo '=== [4] ROLLBACK 之后（库必须回到演练前：表在、取号行在、探针零残留）==='
SELECT to_regclass('public.biz_receivable')                   AS table_after_rollback,
       (SELECT count(*) FROM biz_seq WHERE seq_type = 'AR')   AS ar_seq_after_rollback,
       (SELECT count(*) FROM biz_receivable)                  AS rows_after_rollback;

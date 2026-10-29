-- T1 EXPLAIN 取证：灌 5 万行真实分布数据，验证两条索引真的被选中
--
-- 说明：数据量刻意做大 —— 小表上 PG 一律选 Seq Scan，那样「索引生效」根本证不出来。
-- 5 万行里 60% 是 DONE（模拟长期运行后的真实分布），正好检验部分索引的价值。

-- ---------- 1) 清掉上一轮的批量行 ----------
DELETE FROM biz_task WHERE task_no LIKE 'BULK-%';

-- ---------- 2) 灌 50000 行 ----------
-- owner 与 status **必须用互质的模数**（3 与 10）分开分配：否则「t1_emp 名下全是 DONE」，
-- 「我的任务」查询会走不到 owner 索引（第一版就踩了这个坑，plan 里出现的是 due_open 索引）。
-- owner：g % 3 → 0 是 t1_emp（约 1.67 万条），1 是 t1_other，2 是 t1_mgr。
-- 状态：g % 10 < 6 → DONE（60%），其余 TODO / DOING / CANCELLED / REVIEW 混合（未终态约 30%）。
-- 截止日：CURRENT_DATE - ((g % 90) - 30) → 2/3 已过期，用于逾期扫描。
INSERT INTO biz_task (id, task_no, title, status, priority, progress, need_review,
                      owner_id, creator_id, dept_id, due_date, source, created_at, updated_at)
SELECT gen_random_uuid(),
       'BULK-' || g,
       '批量任务 ' || g,
       CASE
           WHEN g % 10 < 6 THEN 'DONE'
           WHEN g % 10 = 6 THEN 'CANCELLED'
           WHEN g % 10 = 7 THEN 'DOING'
           WHEN g % 10 = 8 THEN 'REVIEW'
           ELSE 'TODO'
       END,
       (ARRAY['P0', 'P1', 'P2', 'P3'])[1 + (g % 4)],
       0,
       FALSE,
       CASE (g % 3)
           WHEN 0 THEN '7d000000-0000-0000-0000-000000000001'::uuid
           WHEN 1 THEN '7d000000-0000-0000-0000-000000000004'::uuid
           ELSE '7d000000-0000-0000-0000-000000000002'::uuid
       END,
       '7d000000-0000-0000-0000-000000000001'::uuid,
       CASE (g % 3)
           WHEN 0 THEN '7a000000-0000-0000-0000-000000000002'::uuid
           WHEN 1 THEN '7a000000-0000-0000-0000-000000000003'::uuid
           ELSE '7a000000-0000-0000-0000-000000000001'::uuid
       END,
       CURRENT_DATE - ((g % 90) - 30),
       'SELF',
       now() - (g || ' minutes')::interval,
       now()
  FROM generate_series(1, 50000) AS g;

ANALYZE biz_task;

\echo ''
\echo '==== 数据分布 ===='
SELECT status, count(*) FROM biz_task WHERE task_no LIKE 'BULK-%' GROUP BY 1 ORDER BY 1;
SELECT 'total' AS k, count(*) FROM biz_task
UNION ALL SELECT 'open(未终态)', count(*) FROM biz_task WHERE status NOT IN ('DONE', 'CANCELLED')
UNION ALL SELECT 'overdue(未终态且过期)', count(*)
           FROM biz_task WHERE status NOT IN ('DONE', 'CANCELLED') AND due_date < CURRENT_DATE;
SELECT 't1_emp 名下' AS k, count(*) FROM biz_task WHERE owner_id = '7d000000-0000-0000-0000-000000000001'::uuid
UNION ALL SELECT 't1_emp 名下未终态', count(*) FROM biz_task
           WHERE owner_id = '7d000000-0000-0000-0000-000000000001'::uuid
             AND status NOT IN ('DONE', 'CANCELLED');

\echo ''
\echo '==== EXPLAIN 1：「我的任务」查询（model::my_tasks 的 SQL 逐字） ===='
\echo '期望：Bitmap Index Scan 走 idx_biz_task_owner_status_due，Index Cond 含 owner_id + status'
EXPLAIN (ANALYZE, BUFFERS OFF, TIMING OFF)
SELECT t.id, t.task_no, t.title, t.status, t.priority, t.progress, t.need_review,
       t.owner_id, u.username AS owner_name, t.dept_id, t.source,
       t.start_date, t.due_date, t.created_at, t.updated_at,
       (SELECT count(*) FROM biz_task c WHERE c.parent_id = t.id AND c.deleted_at IS NULL) AS child_count
  FROM biz_task t LEFT JOIN sys_user u ON u.id = t.owner_id
 WHERE t.owner_id = '7d000000-0000-0000-0000-000000000001'::uuid
   AND t.deleted_at IS NULL
   AND t.status IN ('TODO', 'DOING', 'REVIEW')
 ORDER BY t.due_date ASC NULLS LAST, t.created_at DESC, t.id DESC;

\echo ''
\echo '==== EXPLAIN 1-b：同一查询改回 NOT IN(...) 的对照（证明「为什么用 IN」） ===='
EXPLAIN (ANALYZE, BUFFERS OFF, TIMING OFF)
SELECT t.id FROM biz_task t LEFT JOIN sys_user u ON u.id = t.owner_id
 WHERE t.owner_id = '7d000000-0000-0000-0000-000000000001'::uuid
   AND t.deleted_at IS NULL
   AND t.status NOT IN ('DONE', 'CANCELLED')
 ORDER BY t.due_date ASC NULLS LAST, t.created_at DESC, t.id DESC;

\echo ''
\echo '==== EXPLAIN 2：逾期扫描（model::scan_overdue 的 SQL 逐字） ===='
\echo '期望：Bitmap Index Scan / Index Scan on idx_biz_task_due_open（部分索引）'
EXPLAIN (ANALYZE, BUFFERS OFF, TIMING OFF)
SELECT t.id, t.task_no, t.title, t.status, t.priority, t.progress, t.need_review,
       t.owner_id, u.username AS owner_name, t.dept_id, t.source,
       t.start_date, t.due_date, t.created_at, t.updated_at,
       (SELECT count(*) FROM biz_task c WHERE c.parent_id = t.id AND c.deleted_at IS NULL) AS child_count
  FROM biz_task t LEFT JOIN sys_user u ON u.id = t.owner_id
 WHERE t.status NOT IN ('DONE', 'CANCELLED')
   AND t.due_date < CURRENT_DATE
   AND t.deleted_at IS NULL
 ORDER BY t.due_date ASC, t.id ASC
 LIMIT 50;

\echo ''
\echo '==== 索引清单（pg_indexes + 是否部分索引） ===='
SELECT i.relname AS index_name, pg_get_indexdef(i.oid) AS definition
  FROM pg_class t
  JOIN pg_index x ON t.oid = x.indrelid
  JOIN pg_class i ON i.oid = x.indexrelid
 WHERE t.relname = 'biz_task'
 ORDER BY i.relname;

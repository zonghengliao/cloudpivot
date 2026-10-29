-- ======================================================================
-- T1 返工 EXPLAIN 取证（一脚本出全部证据）
--
-- 用法（144 上）：
--   su postgres -c "psql -d cloudpivot_t1e -f /tmp/t1r_explain.sql" 2>&1 \
--     | sed 's/\(.\{260\}\).*/\1 …（后续为 id 数组字面量，已截断显示）/' > /root/t1r_explain.out
--
-- 口径：
--   * 库 cloudpivot_t1e（独立验证库，不污染 cloudpivot_t1v 的接口走查夹具）
--   * 5 万条 biz_task + 425 条协作关系（与 T1 证据文档 §8 同规模）
--   * 身份 t1_mgr（data_scope=3 DEPT_AND_CHILD，部门集合 = 研发部 + 研发一组）
--   * 两条 SQL 的 SELECT/FROM/WHERE/ORDER BY **逐字取自 model.rs**
--     （CARD_COLUMNS / CARD_FROM / 新旧 VISIBLE_OR_TAIL），参数走 PREPARE 绑定
-- ======================================================================

DELETE FROM biz_task_member WHERE user_id = '7d000000-0000-0000-0000-000000000002'::uuid;
DELETE FROM biz_task WHERE task_no LIKE 'BULK-%';

-- ---------- 1) 灌 5 万行 ----------
INSERT INTO biz_task (id, task_no, title, status, priority, progress, need_review,
                      owner_id, creator_id, assigner_id, dept_id, due_date, source,
                      created_at, updated_at)
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
       CASE WHEN g % 125 = 0
            THEN '7d000000-0000-0000-0000-000000000002'::uuid
            ELSE '7d000000-0000-0000-0000-000000000004'::uuid
       END,
       '7d000000-0000-0000-0000-000000000004'::uuid,
       '7d000000-0000-0000-0000-000000000004'::uuid,
       CASE (g % 3)
           WHEN 0 THEN '7a000000-0000-0000-0000-000000000001'::uuid
           WHEN 1 THEN '7a000000-0000-0000-0000-000000000002'::uuid
           ELSE '7a000000-0000-0000-0000-000000000003'::uuid
       END,
       CURRENT_DATE - ((g % 90) - 30),
       'SELF',
       now() - (g || ' minutes')::interval,
       now()
  FROM generate_series(1, 50000) AS g;

-- ---------- 2) 425 条协作关系（范围外的市场部任务） ----------
INSERT INTO biz_task_member (task_id, user_id, role)
SELECT id, '7d000000-0000-0000-0000-000000000002'::uuid, 'COLLABORATOR'
  FROM biz_task
 WHERE task_no LIKE 'BULK-%'
   AND owner_id <> '7d000000-0000-0000-0000-000000000002'::uuid
 LIMIT 425;

ANALYZE biz_task;
ANALYZE biz_task_member;

\echo '==== §1 数据分布 ===='
SELECT 'biz_task 总数' AS 指标, count(*) AS 值 FROM biz_task
UNION ALL SELECT 'BULK 行', count(*) FROM biz_task WHERE task_no LIKE 'BULK-%'
UNION ALL SELECT 't1_mgr 部门范围内（研发部+研发一组）', count(*)
            FROM biz_task WHERE deleted_at IS NULL
             AND dept_id = ANY('{7a000000-0000-0000-0000-000000000001,7a000000-0000-0000-0000-000000000002}'::text::uuid[])
UNION ALL SELECT 't1_mgr 负责的', count(*) FROM biz_task
            WHERE deleted_at IS NULL AND owner_id = '7d000000-0000-0000-0000-000000000002'::uuid
UNION ALL SELECT 't1_mgr 协作的', count(*) FROM biz_task_member
            WHERE user_id = '7d000000-0000-0000-0000-000000000002'::uuid
UNION ALL SELECT 't1_mgr 参与 id 数（去重并集）',
            (SELECT count(*) FROM (
                SELECT id FROM biz_task
                 WHERE deleted_at IS NULL
                   AND (owner_id = '7d000000-0000-0000-0000-000000000002'::uuid
                     OR creator_id = '7d000000-0000-0000-0000-000000000002'::uuid
                     OR assigner_id = '7d000000-0000-0000-0000-000000000002'::uuid)
                UNION
                SELECT m.task_id FROM biz_task_member m
                 WHERE m.user_id = '7d000000-0000-0000-0000-000000000002'::uuid) z);

\echo ''
\echo '==== §2 「我参与」id 集合查询（model::my_member_task_ids 的 SQL 逐字） ===='
EXPLAIN (ANALYZE, BUFFERS OFF, TIMING OFF)
SELECT t.id FROM biz_task t
 WHERE t.deleted_at IS NULL
   AND (t.owner_id = '7d000000-0000-0000-0000-000000000002'::uuid
     OR t.creator_id = '7d000000-0000-0000-0000-000000000002'::uuid
     OR t.assigner_id = '7d000000-0000-0000-0000-000000000002'::uuid)
UNION
SELECT m.task_id FROM biz_task_member m WHERE m.user_id = '7d000000-0000-0000-0000-000000000002'::uuid
LIMIT 2001;

SELECT array_agg(id)::text AS pids FROM (
    SELECT t.id FROM biz_task t
     WHERE t.deleted_at IS NULL
       AND (t.owner_id = '7d000000-0000-0000-0000-000000000002'::uuid
         OR t.creator_id = '7d000000-0000-0000-0000-000000000002'::uuid
         OR t.assigner_id = '7d000000-0000-0000-0000-000000000002'::uuid)
    UNION
    SELECT m.task_id FROM biz_task_member m WHERE m.user_id = '7d000000-0000-0000-0000-000000000002'::uuid
) z
\gset

\echo 'id 数组体积（字节）= '
SELECT length(:'pids') AS array_bytes;

-- ---------- §3 返工前：多 OR + EXISTS ----------
\echo ''
\echo '==== §3 返工前 EXPLAIN（旧 VISIBLE_OR_TAIL：4 列 OR + EXISTS） ===='
PREPARE vis_old(text[], uuid, uuid, uuid, uuid) AS
SELECT t.id, t.task_no, t.title, t.status, t.priority, t.progress, t.need_review,
       t.owner_id, COALESCE(NULLIF(u.real_name, ''), u.username, '') AS owner_name,
       t.dept_id, t.source, t.start_date, t.due_date, t.created_at, t.updated_at,
       (SELECT count(*) FROM biz_task c WHERE c.parent_id = t.id AND c.deleted_at IS NULL) AS child_count
  FROM biz_task t LEFT JOIN sys_user u ON u.id = t.owner_id
 WHERE t.deleted_at IS NULL
   AND ( (1 = 1 AND t.dept_id = ANY($1::text::uuid[]))
         OR t.owner_id = $2
         OR t.creator_id = $3
         OR t.assigner_id = $4
         OR EXISTS (SELECT 1 FROM biz_task_member m
                     WHERE m.task_id = t.id AND m.user_id = $5) )
 ORDER BY t.due_date ASC NULLS LAST, t.created_at DESC, t.id DESC
 LIMIT 20 OFFSET 0;

EXPLAIN (ANALYZE, BUFFERS OFF, TIMING OFF)
EXECUTE vis_old('{7a000000-0000-0000-0000-000000000001,7a000000-0000-0000-0000-000000000002}',
                '7d000000-0000-0000-0000-000000000002',
                '7d000000-0000-0000-0000-000000000002',
                '7d000000-0000-0000-0000-000000000002',
                '7d000000-0000-0000-0000-000000000002');

\echo ''
\echo '---- 返工前的 count（paginate 的 count_sql） ----'
PREPARE vis_old_cnt(text[], uuid, uuid, uuid, uuid) AS
SELECT count(*) FROM biz_task t
 WHERE t.deleted_at IS NULL
   AND ( (1 = 1 AND t.dept_id = ANY($1::text::uuid[]))
         OR t.owner_id = $2
         OR t.creator_id = $3
         OR t.assigner_id = $4
         OR EXISTS (SELECT 1 FROM biz_task_member m
                     WHERE m.task_id = t.id AND m.user_id = $5) );
EXPLAIN (ANALYZE, BUFFERS OFF, TIMING OFF)
EXECUTE vis_old_cnt('{7a000000-0000-0000-0000-000000000001,7a000000-0000-0000-0000-000000000002}',
                    '7d000000-0000-0000-0000-000000000002',
                    '7d000000-0000-0000-0000-000000000002',
                    '7d000000-0000-0000-0000-000000000002',
                    '7d000000-0000-0000-0000-000000000002');

-- ---------- §4 返工后：参与 id 数组 ----------
\echo ''
\echo '==== §4 返工后 EXPLAIN（新 VISIBLE_OR_TAIL：scope ∪ t.id = ANY(?)） ===='
PREPARE vis_new(text[], text[]) AS
SELECT t.id, t.task_no, t.title, t.status, t.priority, t.progress, t.need_review,
       t.owner_id, COALESCE(NULLIF(u.real_name, ''), u.username, '') AS owner_name,
       t.dept_id, t.source, t.start_date, t.due_date, t.created_at, t.updated_at,
       (SELECT count(*) FROM biz_task c WHERE c.parent_id = t.id AND c.deleted_at IS NULL) AS child_count
  FROM biz_task t LEFT JOIN sys_user u ON u.id = t.owner_id
 WHERE t.deleted_at IS NULL
   AND ( (1 = 1 AND t.dept_id = ANY($1::text::uuid[]))
         OR t.id = ANY($2::text::uuid[]) )
 ORDER BY t.due_date ASC NULLS LAST, t.created_at DESC, t.id DESC
 LIMIT 20 OFFSET 0;

EXPLAIN (ANALYZE, BUFFERS OFF, TIMING OFF)
EXECUTE vis_new('{7a000000-0000-0000-0000-000000000001,7a000000-0000-0000-0000-000000000002}', :'pids');

\echo ''
\echo '---- 返工后的 count ----'
PREPARE vis_new_cnt(text[], text[]) AS
SELECT count(*) FROM biz_task t
 WHERE t.deleted_at IS NULL
   AND ( (1 = 1 AND t.dept_id = ANY($1::text::uuid[]))
         OR t.id = ANY($2::text::uuid[]) );
EXPLAIN (ANALYZE, BUFFERS OFF, TIMING OFF)
EXECUTE vis_new_cnt('{7a000000-0000-0000-0000-000000000001,7a000000-0000-0000-0000-000000000002}', :'pids');

-- ---------- §5 语义等价：两条 SQL 的 total 必须相等 ----------
\echo ''
\echo '==== §5 语义等价：返工前/后的 count 结果（必须相等） ===='
SELECT
  (SELECT count(*) FROM biz_task t
    WHERE t.deleted_at IS NULL
      AND ((1 = 1 AND t.dept_id = ANY('{7a000000-0000-0000-0000-000000000001,7a000000-0000-0000-0000-000000000002}'::text::uuid[]))
           OR t.owner_id = '7d000000-0000-0000-0000-000000000002'::uuid
           OR t.creator_id = '7d000000-0000-0000-0000-000000000002'::uuid
           OR t.assigner_id = '7d000000-0000-0000-0000-000000000002'::uuid
           OR EXISTS (SELECT 1 FROM biz_task_member m
                       WHERE m.task_id = t.id AND m.user_id = '7d000000-0000-0000-0000-000000000002'::uuid))) AS 返工前_visible_total,
  (SELECT count(*) FROM biz_task t
    WHERE t.deleted_at IS NULL
      AND ((1 = 1 AND t.dept_id = ANY('{7a000000-0000-0000-0000-000000000001,7a000000-0000-0000-0000-000000000002}'::text::uuid[]))
           OR t.id = ANY(:'pids'::text::uuid[]))) AS 返工后_visible_total;

DEALLOCATE ALL;

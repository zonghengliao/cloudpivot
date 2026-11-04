-- ============================================================
-- P2-8 · 项目模块「接口口径复算」（只读，零写入）
--
-- 为什么需要它：本机 dev 库 (cloudpivot_manual) 的 admin 口令不是仓库默认值，
-- 登录接口 401（不猜凭据），所以「页面上 0 成员 / 工时汇总是多少」不能靠点页面取证。
-- 这里把 handler::page + model::stats_for_projects 的口径**用 SQL 原样复算**一遍：
--   · member_count  = 成员表行数（接口 ProjectRes.member_count 同源）
--   · task_total / task_done = 我可见任务（超管视角 = 全部未删任务）
--   · estimate_hours / actual_hours = sum(biz_task.estimate_hours / actual_hours)
--     与 model.rs:442-443 的 SELECT 逐字一致
--   · worklog_rows / worklog_hours = 挂在项目任务身上的工时记录（项目页当前**不展示**这一层）
--
-- 用法（两轮对比可复现）：
--   bash docs/evidence/20261104-p2-8/recompute_two_rounds.sh
-- ============================================================
\pset footer off
\timing off

\echo '=== 1) cp_p23_prod（评审实测看到「3 项目 0 成员」的那套库）'
\connect cp_p23_prod
SELECT p.project_no, p.project_name, p.status,
       (SELECT count(*) FROM biz_project_member m WHERE m.project_id = p.id) AS member_count,
       count(t.id)                                                    AS task_total,
       count(*) FILTER (WHERE t.status = 'DONE')                       AS task_done,
       COALESCE(sum(t.estimate_hours), 0)::float8                      AS estimate_hours,
       COALESCE(sum(t.actual_hours), 0)::float8                        AS actual_hours,
       (SELECT count(*) FROM biz_task_worklog w
          JOIN biz_task t2 ON t2.id = w.task_id
         WHERE t2.project_id = p.id AND t2.deleted_at IS NULL)         AS worklog_rows,
       (SELECT COALESCE(sum(w.hours), 0)::float8 FROM biz_task_worklog w
          JOIN biz_task t2 ON t2.id = w.task_id
         WHERE t2.project_id = p.id AND t2.deleted_at IS NULL)         AS worklog_hours
  FROM biz_project p
  LEFT JOIN biz_task t ON t.project_id = p.id AND t.deleted_at IS NULL
 WHERE p.deleted_at IS NULL
 GROUP BY p.id, p.project_no, p.project_name, p.status, p.created_at
 ORDER BY p.created_at;

\echo '=== 2) cloudpivot_manual（本机后端 :8023 正在跑的库）'
\connect cloudpivot_manual
SELECT p.project_no, p.project_name, p.status,
       (SELECT count(*) FROM biz_project_member m WHERE m.project_id = p.id) AS member_count,
       count(t.id)                                                    AS task_total,
       count(*) FILTER (WHERE t.status = 'DONE')                       AS task_done,
       COALESCE(sum(t.estimate_hours), 0)::float8                      AS estimate_hours,
       COALESCE(sum(t.actual_hours), 0)::float8                        AS actual_hours,
       (SELECT count(*) FROM biz_task_worklog w
          JOIN biz_task t2 ON t2.id = w.task_id
         WHERE t2.project_id = p.id AND t2.deleted_at IS NULL)         AS worklog_rows,
       (SELECT COALESCE(sum(w.hours), 0)::float8 FROM biz_task_worklog w
          JOIN biz_task t2 ON t2.id = w.task_id
         WHERE t2.project_id = p.id AND t2.deleted_at IS NULL)         AS worklog_hours
  FROM biz_project p
  LEFT JOIN biz_task t ON t.project_id = p.id AND t.deleted_at IS NULL
 WHERE p.deleted_at IS NULL
 GROUP BY p.id, p.project_no, p.project_name, p.status, p.created_at
 ORDER BY p.created_at;

\echo '=== 3) 三件套能力存量（表/列层面，任何库都成立）'
\connect cp_p23_prod
SELECT 'table:biz_project_member' AS item, (to_regclass('public.biz_project_member') IS NOT NULL)::text AS present
UNION ALL SELECT 'table:biz_task_worklog',  (to_regclass('public.biz_task_worklog')  IS NOT NULL)::text
UNION ALL SELECT 'table:biz_budget',        (to_regclass('public.biz_budget')        IS NOT NULL)::text
UNION ALL SELECT 'table:%milestone%',       (SELECT (count(*) > 0)::text FROM information_schema.tables
                                              WHERE table_schema='public' AND table_name LIKE '%milestone%')
UNION ALL SELECT 'col:biz_project.budget%', (SELECT (count(*) > 0)::text FROM information_schema.columns
                                              WHERE table_name='biz_project' AND column_name LIKE '%budget%')
UNION ALL SELECT 'col:biz_task.milestone%', (SELECT (count(*) > 0)::text FROM information_schema.columns
                                              WHERE table_name='biz_task' AND column_name LIKE '%milestone%')
UNION ALL SELECT 'col:biz_task.hours%',     (SELECT string_agg(column_name, ',' ORDER BY column_name)
                                               FROM information_schema.columns
                                              WHERE table_name='biz_task' AND column_name LIKE '%hours%');

#!/usr/bin/env bash
# P0-FIX-1：隔离实例（cloudpivot_p0fix1 / :18111）证据快照
set -uo pipefail
DB=cloudpivot_p0fix1
export PGPASSWORD=alun
Q() { psql -h 127.0.0.1 -U alun -d "$DB" -X -q -tAF' | ' -c "$1"; }

echo "### 1. sys_job（4 个提醒 + 其它）"
Q "select job_name, job_group, cron_expr, invoke_target, status::text,
          coalesce(to_char(last_run_at at time zone 'Asia/Shanghai','YYYY-MM-DD HH24:MI:SS'),'-'),
          coalesce(to_char(next_run_at at time zone 'Asia/Shanghai','YYYY-MM-DD HH24:MI:SS'),'-'),
          coalesce(left(remark,40),'')
     from sys_job where deleted_at is null order by job_name"

echo
echo "### 2. sys_job_log 全量（按时间正序；含 12:05:00 那次『到点自动触发』）"
Q "select job_name, status::text,
          to_char(started_at at time zone 'Asia/Shanghai','YYYY-MM-DD HH24:MI:SS'),
          duration_ms::text, coalesce(message,'')
     from sys_job_log order by started_at"

echo
echo "### 3. 每个任务的成功执行条数"
Q "select job_name, count(*)::text from sys_job_log where status = 1 group by job_name order by job_name"

echo
echo "### 4. 通知 sys_notice / 台账 biz_reminder_log"
Q "select (select count(*) from sys_notice)::text || ' 条通知 / ' ||
          (select count(*) from biz_reminder_log)::text || ' 行台账'"

echo
echo "### 5. 迁移记录 V0245"
Q "select version::text, name from schema_migrations where name like '%reminder_jobs_enabled%'"

echo
echo "### 6. 审计日志里的启停 / 触发（走接口的痕迹）"
Q "select operation || ' x' || count(*)::text from audit_logs
    where operation in ('JOB_STATUS','JOB_TRIGGER') group by operation order by operation"

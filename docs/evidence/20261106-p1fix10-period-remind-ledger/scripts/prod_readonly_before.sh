#!/usr/bin/env bash
# 生产只读核对（172.18.10.144 / cloudpivot）：不改任何数据
# 用途：证明「评审查到的 biz_reminder_log = 0 行」是真实事实，但它的原因不是「定时任务被停用」
set -u
HOST=root@172.18.10.144

runsql() {
  # $1 = 标题；SQL 从 stdin 读，经 ssh → su postgres → psql 执行（避免多层引号被吃掉）
  echo
  echo "## $1"
  { cat; echo '\echo'; } | ssh -o BatchMode=yes "$HOST" "su - postgres -c 'psql -d cloudpivot -P pager=off'"
}

echo "# 生产只读核对（172.18.10.144 / cloudpivot）—— $(date '+%F %T %Z')"

runsql "① 生产 biz_reminder_log 行数（评审查到的是 0）" <<'SQL'
select count(*) as reminder_log_rows from biz_reminder_log;
SQL

runsql "② 生产 sys_notice 里 period_remind 的通知（说明催办一直在同步发信）" <<'SQL'
select notice_no, left(title, 44) as title, receiver_count, created_at
  from sys_notice where biz_type = 'period_remind' order by created_at;
SQL

runsql "③ 生产 sys_job 全量（没有任何「结账催办」任务，也就无所谓停用）" <<'SQL'
select job_name, status, cron_expr from sys_job where deleted_at is null order by job_name;
SQL

runsql "④ 生产 结账相关权限授予" <<'SQL'
select m.auth_code, r.role_code
  from sys_menu m
  join sys_role_menu rm on rm.menu_id = m.id
  join sys_role r on r.id = rm.role_id
 where m.auth_code in ('business:erp:period:remind', 'business:erp:period:close',
                       'business:erp:voucher:create')
   and m.deleted_at is null
 order by 1, 2;
SQL

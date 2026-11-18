#!/usr/bin/env bash
# 反证②（库侧污染）：手工把演示账号在 FINANCE_MANAGER 上的绑定删掉（= 老写法「整表删」的效果），
# 新护栏必须当场变红并点名；跑完把绑定补回去。
set -uo pipefail
cd /opt/cloudpivot/backend/app || exit 1
export ERP_DB_URL='postgres://cloudpivot:fbb1263691ff75c31ec99eaf258138b4@172.18.10.145:5432/cloudpivot_test_t18v3'
export SYS_DB_URL="$ERP_DB_URL"; export TASK_DB_URL="$ERP_DB_URL"; export P0FIX_DB_URL="$ERP_DB_URL"
PG="psql -h 172.18.10.145 -U cloudpivot -d cloudpivot_test_t18v3 -Atc"
export PGPASSWORD=fbb1263691ff75c31ec99eaf258138b4

echo "===== ① 污染前 ====="
$PG "select r.role_code || ' | ' || coalesce((select string_agg(u.username,',' order by u.username)
  from sys_user_role ur join sys_user u on u.id=ur.user_id where ur.role_id=r.id),'(空)')
  from sys_role r where r.role_code in ('FINANCE_MANAGER','FINANCE_DIRECTOR') order by 1;"

echo "===== ② 模拟老写法：把 FINANCE_MANAGER 的绑定整表删掉 ====="
$PG "delete from sys_user_role where role_id = (select id from sys_role where role_code='FINANCE_MANAGER');" >/dev/null
$PG "select 'FINANCE_MANAGER 剩余成员=' || coalesce((select string_agg(u.username,',') from sys_user_role ur
  join sys_user u on u.id=ur.user_id where ur.role_id=(select id from sys_role where role_code='FINANCE_MANAGER')),'(空)');"

echo "===== ③ 跑护栏：期望 RED 并点名 ====="
cargo test -p cloudpivot --lib "business::erp::cost::cost_period_approval_db_tests::db::zz_teardown_purges_fixtures" \
  -- --test-threads=1 --exact 2>&1 | grep -vE "^\s*(Compiling|Finished|Running|Blocking)" | tail -20
echo "rc=${PIPESTATUS[0]}"

echo "===== ④ 补回绑定（把库恢复成种子基线） ====="
$PG "insert into sys_user_role (user_id, role_id)
     select u.id, r.id from sys_user u, sys_role r
      where u.username in ('demo_finance','demo_finance_mgr') and r.role_code='FINANCE_MANAGER'
     on conflict do nothing;" >/dev/null
$PG "select r.role_code || ' | ' || coalesce((select string_agg(u.username,',' order by u.username)
  from sys_user_role ur join sys_user u on u.id=ur.user_id where ur.role_id=r.id),'(空)')
  from sys_role r where r.role_code in ('FINANCE_MANAGER','FINANCE_DIRECTOR') order by 1;"

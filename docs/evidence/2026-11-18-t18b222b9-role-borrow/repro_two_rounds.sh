#!/usr/bin/env bash
# 卡 t_18b222b9 终版验收：全新种子库 v3 + 同一库连跑两轮全量（fmt/clippy 已在前面过）
set -uo pipefail
cd /opt/cloudpivot || exit 1
ERP_DB=cloudpivot_test_t18v3 bash scripts/verify/make_full_suite_db.sh > /root/t18b_v3_mkdb.out 2>&1
echo "mkdb rc=$?"; tail -3 /root/t18b_v3_mkdb.out

cd /opt/cloudpivot/backend/app || exit 1
export ERP_DB_URL='postgres://cloudpivot:fbb1263691ff75c31ec99eaf258138b4@172.18.10.145:5432/cloudpivot_test_t18v3'
export SYS_DB_URL="$ERP_DB_URL"
export TASK_DB_URL="$ERP_DB_URL"
export P0FIX_DB_URL="$ERP_DB_URL"

roles() {
  PGPASSWORD=fbb1263691ff75c31ec99eaf258138b4 psql -h 172.18.10.145 -U cloudpivot -d cloudpivot_test_t18v3 -Atc "
select r.role_code || ' | ' || coalesce((select string_agg(u.username, ',' order by u.username)
   from sys_user_role ur join sys_user u on u.id = ur.user_id and u.deleted_at is null
  where ur.role_id = r.id), '(空)')
from sys_role r
where r.role_code in ('FINANCE_MANAGER','FINANCE_DIRECTOR','PURCHASE_MANAGER','GENERAL_MANAGER','WAREHOUSE_MANAGER','DEPT_LEADER','SALES_SUPERVISOR')
order by 1;"
}
residue() {
  PGPASSWORD=fbb1263691ff75c31ec99eaf258138b4 psql -h 172.18.10.145 -U cloudpivot -d cloudpivot_test_t18v3 -Atc \
    "select 'biz_cost_period=' || (select count(*) from biz_cost_period)
         || ' biz_cost_period_item=' || (select count(*) from biz_cost_period_item)
         || ' COST_SETTLE_instances=' || (select count(*) from biz_flow_instance where biz_type='COST_SETTLE');"
}

echo "=== 起始库形态 ==="; roles; residue
for round in 1 2; do
  echo "=== ROUND $round 开始 $(date +%H:%M:%S) ==="
  cargo test -p cloudpivot --lib -- --test-threads=1 > /root/t18b_v3_round${round}.out 2>&1
  echo "=== ROUND $round 结束 rc=$? $(date +%H:%M:%S) ==="
  grep -E "^test result:" /root/t18b_v3_round${round}.out
  sed -n '/^failures:$/,/^test result/p' /root/t18b_v3_round${round}.out | grep -E "^    (business|system)" | head -30
  echo "--- round $round 后库形态 ---"; roles; residue
done
echo ALLDONE $(date +%H:%M:%S)

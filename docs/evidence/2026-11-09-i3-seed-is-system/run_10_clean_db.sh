#!/usr/bin/env bash
# 145 · 验收①：全新空库跑全量迁移（含 V0366）→ 不变量（前后对比 + 幂等 + 启动期落位）
set -uo pipefail
export PATH=/root/.cargo/bin:$PATH
cd /opt/cloudpivot || exit 1

DB=cloudpivot_i3_clean
PORT=18166
OUT=/tmp/i3/invariants_clean.txt
UP=/tmp/i3/V0366__seed_fix_flow_is_system_and_admin_dept.up.sql

set -a; . /etc/cloudpivot/cloudpivot-dev.env; set +a
DB_USER=${NIK_DATABASE_USER:-cloudpivot}
DB_PASS=${NIK_DATABASE_PASSWORD:-}

psql_db() { su - postgres -c "psql -d $DB -Atc \"$1\""; }
upfile()  { su - postgres -c "psql -d $DB -v ON_ERROR_STOP=1 -f $UP" 2>&1 | grep -E "UPDATE|INSERT|DO" ; }
run_inv() {
  NIK_DATABASE_NAME=$DB python3 scripts/verify/erp_invariants.py > "$OUT" 2>&1
  echo "[$1] $(grep -E '结果：' "$OUT")"
  grep -E "^(FAIL|🚨 \[|⚠️ \[)|^  · " "$OUT" | grep -vE "group_|执行异常" | head -8
  grep -E "H6 |H5 |N2 " "$OUT" | grep -vE "^\s*·" | head -3
}

echo "===== 1) 全新空库 + 全量迁移（含 V0366）====="
DB_USER=$DB_USER DB_PASS=$DB_PASS OWNER=$DB_USER \
ERP_DB=$DB RECREATE=1 PORT=$PORT BIN=/opt/cloudpivot/backend/target/debug/cloudpivot \
  bash scripts/verify/erp_clean_db.sh 2>&1 | grep -E "==> (dropped|created|启动|库指纹|完成)|schema_migrations|超管人数|迁移没有完成" | head -12

echo
echo "===== 2) 迁移与数据指纹 ====="
psql_db "select 'migrations max/num = ' || max(version::int) || ' / ' || count(*) from schema_migrations"
psql_db "select 'V0366 落库条数 = ' || count(*) from schema_migrations where version::int = 366"
psql_db "select flow_code || ' | ' || flow_name || ' | is_system=' || is_system::text from biz_flow_def where deleted_at is null and flow_code in ('pr_approve','prt_approve') order by 1"
psql_db "select '标准流程数 / 名字含标准但非系统(H5) = ' || count(*) filter (where is_system) || ' / ' || count(*) filter (where not is_system and flow_name like '%标准%') from biz_flow_def where deleted_at is null"
psql_db "select 'dept: ' || dept_code || ' | ' || dept_name || ' | parent=' || coalesce(parent_id::text,'NULL') from sys_dept where deleted_at is null"
psql_db "select 'admin dept = ' || coalesce(d.dept_code,'NULL') from sys_user u left join sys_dept d on d.id=u.dept_id where username='admin'"
psql_db "select 'N2 违规账号 = ' || coalesce(string_agg(username, ','), '(无)') from sys_user where status=1 and is_locked=false and user_type='HUMAN' and dept_id is null and deleted_at is null"

echo
echo "===== 3) 迁移后不变量 ====="
run_inv "迁移后"

echo
echo "===== 4) 幂等：把 V0366 再跑一遍（应 UPDATE 0 / INSERT 0 0）====="
upfile

echo
echo "===== 5) 因果对照：回退成缺陷态 → 应重新变红 ====="
psql_db "update biz_flow_def set is_system=false where flow_code in ('pr_approve','prt_approve') and deleted_at is null"
psql_db "update sys_user set dept_id=NULL where username='admin'"
run_inv "回退成缺陷态"

echo
echo "===== 6) 启动期落位：清空 admin 部门后重启一次实例 ====="
DB_USER=$DB_USER DB_PASS=$DB_PASS OWNER=$DB_USER \
ERP_DB=$DB PORT=$PORT BIN=/opt/cloudpivot/backend/target/debug/cloudpivot \
  bash scripts/verify/erp_clean_db.sh 2>&1 | grep -E "==> (已存在|启动|库指纹|完成)" | head -6
grep -E "超管部门落位|迁移完成" /root/erp-clean-db.log | head -4
psql_db "select 'admin dept（重启后）= ' || coalesce(d.dept_code,'NULL') from sys_user u left join sys_dept d on d.id=u.dept_id where username='admin'"

echo
echo "===== 7) 再跑 V0366 修复 is_system → 不变量（最终）====="
upfile
run_inv "最终"

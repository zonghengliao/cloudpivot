#!/usr/bin/env bash
# NEAR-FIX-4 演练（补）：验 deploy.sh 第 [7/9] 步的**逐字命令**在「结论早于最后一次迁移」时
# 真的会重跑并落一行新结论。用演练库 cp_drill_inv4（不碰生产库）。
set -uo pipefail
DB="${DRILL_DB:-cp_drill_inv4}"
[ "${DB#cp_drill_}" != "$DB" ] || { echo "拒绝执行：DRILL_DB 必须形如 cp_drill_*（防止误连生产库）"; exit 2; }

set -a; . /etc/cloudpivot/cloudpivot.env; set +a
export PGPASSWORD="$NIK_DATABASE_PASSWORD"
PSQL="psql -X -q -A -t -h ${NIK_DATABASE_HOST} -p ${NIK_DATABASE_PORT:-5432} -U ${NIK_DATABASE_USER} -d ${DB}"

echo "=== 1) 把演练库的结论弄「旧」：只剩 17h 前那条 FAIL，迁移在 2h 前 ==="
$PSQL -v ON_ERROR_STOP=1 >/dev/null <<SQL
DELETE FROM sys_invariant_result;
DELETE FROM schema_migrations;
INSERT INTO schema_migrations (kind,name,version,checksum,applied_via,applied_at,applied_seq,execution_ms,success,source)
VALUES ('versioned','V0366__seed_fix_flow_is_system','366','c3','executed',
        (extract(epoch from now() - interval '2 hours'))::bigint, 1, 0, true,
        'migrations/business/V0366__seed_fix_flow_is_system.up.sql');
INSERT INTO sys_invariant_result (id,run_at,trigger_type,status,total,passed,failed,blockers,warns,duration_ms,database_name,script_path,error)
VALUES (gen_random_uuid(), now() - interval '17 hours','SCHEDULED','FAIL',79,77,2,1,1,3200,
        'cloudpivot','/opt/cloudpivot/scripts/verify/erp_invariants.py',NULL);
SQL
$PSQL -c "select '结论 ' || status || ' ' || round(extract(epoch from (now()-run_at))/3600) || 'h 前；迁移 applied_at=' || (select max(applied_at) from schema_migrations) from sys_invariant_result"

echo
echo "=== 2) 跑 deploy.sh [7/9] 步的逐字命令（--repo/--env-file/--base-url 与脚本里一致）==="
python3 /opt/cloudpivot/scripts/ops/invariant_rerun.py --repo /opt/cloudpivot \
  --env-file /tmp/drill-inv.env --base-url http://127.0.0.1:8023
echo "rc=$?"

echo
echo "=== 3) 落库后：结论新鲜了，告警报的是真实结论（不再是「过期」）==="
NIK_ALERT_ENV_FILE=/tmp/drill-inv.env \
NIK_ALERT_LOG=/tmp/drill-inv-alert.log \
NIK_ALERT_STATE_DIR=/tmp/drill-inv-state \
NIK_ALERT_KANBAN=off \
NIK_ALERT_BASE_URL=http://127.0.0.1:9 \
bash /opt/cloudpivot/scripts/ops/alert_check.sh --only invariant --dry-run 2>&1 | grep -E "数据源|invariant: "

echo
echo "=== 4) 演练库里两行对比（新行 run_at 必须晚于 applied_at）==="
$PSQL -c "select to_char(run_at,'MM-DD HH24:MI:SS') || ' | ' || trigger_type || ' | ' || status || ' | blockers=' || blockers from sys_invariant_result order by run_at"
$PSQL -c "select 'max(applied_at)=' || max(applied_at) from schema_migrations"

#!/usr/bin/env bash
# NEAR-FIX-4 演练（补）：验 alert_check 的 **/metrics 分支** 也认「结论过期」。
#
# 为什么还要单独验：144 的 /metrics 目前**没有** cloudpivot_invariant_* 这几条 gauge（实测 200 但无该指标），
# 所以生产上走的是 psql 分支。metric 分支是「后端升级后」的主路，必须现在就把它的
# 「另查一次 psql 求 migration_newer」验通，否则等指标上线才发现那半边是瞎的。
set -uo pipefail
DB="${DRILL_DB:-cp_drill_inv4}"
case "$DB" in cp_drill_*) ;; *) echo "拒绝执行：DRILL_DB 必须形如 cp_drill_*（当前 $DB）"; exit 2 ;; esac
ALERT=/opt/cloudpivot/scripts/ops/alert_check.sh
DRILL_ENV=/tmp/drill-inv.env
PORT="${FAKE_METRICS_PORT:-18099}"

set -a; . /etc/cloudpivot/cloudpivot.env; set +a
export PGPASSWORD="$NIK_DATABASE_PASSWORD"
PSQL="psql -X -q -A -t -h ${NIK_DATABASE_HOST} -p ${NIK_DATABASE_PORT:-5432} -U ${NIK_DATABASE_USER} -d ${DB}"

say() { printf '\n### %s\n' "$*"; }

say "1) 演练库铺回 A 场景（结论 17h 前 FAIL + 之后一条无关迁移）"
$PSQL -v ON_ERROR_STOP=1 >/dev/null <<SQL
TRUNCATE sys_invariant_result; TRUNCATE schema_migrations;
INSERT INTO schema_migrations (kind,name,version,checksum,applied_via,applied_at,applied_seq,execution_ms,success,source)
VALUES ('versioned','V0331__purchase_requisition','331','c1','executed',
        (extract(epoch from now() - interval '30 hours'))::bigint, 1, 0, true,
        'migrations/business/V0331__purchase_requisition.up.sql'),
       ('versioned','V0366__seed_fix_flow_is_system','366','c3','executed',
        (extract(epoch from now() - interval '2 hours'))::bigint, 2, 0, true,
        'migrations/business/V0366__seed_fix_flow_is_system.up.sql');
INSERT INTO sys_invariant_result (id,run_at,trigger_type,status,total,passed,failed,blockers,warns,duration_ms,database_name,script_path,error)
VALUES (gen_random_uuid(), now() - interval '17 hours','SCHEDULED','FAIL',79,77,2,1,1,3200,
        'cloudpivot','/opt/cloudpivot/scripts/verify/erp_invariants.py',NULL);
SQL
$PSQL -c "select '结论 ' || status || ' blockers=' || blockers || ' ' || round(extract(epoch from (now()-run_at))/3600) || 'h 前' from sys_invariant_result"

say "2) 起一个假 /metrics（只提供 4 条 cloudpivot_invariant_* gauge）"
FAKE_DIR=/tmp/fake-metrics
mkdir -p "$FAKE_DIR"
cat >"$FAKE_DIR/metrics" <<'M'
# HELP cloudpivot_invariant_ok 最近一次巡检是否通过
cloudpivot_invariant_ok 0
cloudpivot_invariant_blockers 1
cloudpivot_invariant_failed 2
cloudpivot_invariant_age_seconds 61200
M
python3 -m http.server "$PORT" --directory "$FAKE_DIR" >/tmp/fake-metrics.log 2>&1 &
SRV=$!
trap 'kill "$SRV" 2>/dev/null' EXIT
sleep 1
curl -s -m 3 "http://127.0.0.1:$PORT/metrics" | head -2 | sed 's/^/   /'

say "3) alert_check --only invariant（数据源应变成 /metrics，判定应为 WARN「待重跑」）"
NIK_ALERT_BASE_URL="http://127.0.0.1:$PORT" \
NIK_ALERT_ENV_FILE="$DRILL_ENV" \
NIK_ALERT_LOG=/tmp/drill-inv-alert.log \
NIK_ALERT_STATE_DIR=/tmp/drill-inv-state \
NIK_ALERT_KANBAN=off \
bash "$ALERT" --only invariant --dry-run 2>&1 | sed 's/^/   /'

say "4) psql 不可用时（env 文件不存在）→ 不能崩，且必须显式说明「无法判断是否过期」，按旧口径判"
NIK_ALERT_BASE_URL="http://127.0.0.1:$PORT" \
NIK_ALERT_ENV_FILE=/tmp/does-not-exist.env \
NIK_ALERT_LOG=/tmp/drill-inv-alert.log \
NIK_ALERT_STATE_DIR=/tmp/drill-inv-state \
NIK_ALERT_KANBAN=off \
bash "$ALERT" --only invariant --dry-run 2>&1 | sed 's/^/   /'

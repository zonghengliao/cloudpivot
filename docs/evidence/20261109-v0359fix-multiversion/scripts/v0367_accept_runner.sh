#!/bin/bash
# ============================================================================
# V0367 验收 ⑤：144 备份副本上跑**真实迁移器**（alun 启动期 auto_migrate）
#   ① 恢复 144 生产备份 → cloudpivot_preview2（对象属主 cloudpivot，模拟生产库）
#   ② 基线：295 条迁移 + V0359 手工跳过记录 + po_approve v1..v4
#   ③ 启动实例：auto_migrate 应用 pending（V0365 / V0366 / V0367），验证无 unique violation
#   ④ 再启动一次：应「应用 0 个迁移」（迁移器层面的幂等）
# ============================================================================
set -uo pipefail

DB=cloudpivot_preview2
DUMP=/tmp/prod-221715.dump
STAGE=/tmp/v0367stage2
BIN=/opt/cloudpivot/backend/target/debug/cloudpivot
PORT=18097
LOG1=/tmp/v0367_run1.log
LOG2=/tmp/v0367_run2.log

ps() { su - postgres -c "psql -d $DB -tAc \"$1\""; }

echo "############ ① 恢复 144 备份（--role=cloudpivot，对象属主与生产一致） ############"
su - postgres -c "dropdb --if-exists $DB; createdb -O cloudpivot $DB" || exit 1
su - postgres -c "pg_restore --role=cloudpivot -d $DB $DUMP" 2>&1 | tail -3
echo "restore_rc=$?"
ps "select 'objects_owned_by_cloudpivot=' || count(*) from pg_class c join pg_namespace n on n.oid=c.relnamespace where n.nspname='public' and pg_get_userbyid(c.relowner)='cloudpivot'"

echo "############ ② 基线（部署前） ############"
ps "select 'migrations=' || count(*) from schema_migrations"
ps "select 'v0359=' || coalesce((select source from schema_migrations where version='359'),'<none>')"
ps "select flow_code || ' : ' || string_agg(version_no || ':' || version_status, ',' order by version_no) from biz_flow_def d join biz_flow_def_version v on v.def_id=d.id where d.flow_code in ('po_approve','contract_approve','payable_pay','pr_approve','prt_approve') group by flow_code order by flow_code"
ps "select 'bad_nodes_before=' || count(*) from biz_flow_def d join biz_flow_def_version v on v.def_id=d.id and v.is_current and v.version_status='PUBLISHED' join biz_flow_node n on n.version_id=v.id where d.deleted_at is null and n.node_type='APPROVAL' and n.assignee_type='ROLE' and n.approval_mode='ALL'"

set -a
# shellcheck disable=SC1091
. /etc/cloudpivot/cloudpivot-dev.env
set +a
export NIK_DATABASE_NAME=$DB
export NIK_SERVER_LISTEN=127.0.0.1:$PORT
export NIK_REDIS_URL="$(printf '%s' "$NIK_REDIS_URL" | sed -E 's#/[0-9]+$##')/6"
export RUST_LOG=info
cd "$STAGE" || exit 1

run_once() {   # $1=日志文件  $2=轮次说明
    local log=$1 label=$2 pid health
    timeout 240 "$BIN" > "$log" 2>&1 &
    pid=$!
    health=000
    for _ in $(seq 1 120); do
        health=$(curl -s -o /dev/null -w '%{http_code}' "http://127.0.0.1:$PORT/health" || true)
        [ "$health" = "200" ] && break
        kill -0 "$pid" 2>/dev/null || break
        sleep 1
    done
    echo "[$label] health=$health"
    kill "$pid" 2>/dev/null; wait "$pid" 2>/dev/null
}

echo "############ ③ 第 1 次启动（应用 pending 迁移） ############"
run_once "$LOG1" "run1"
echo "--- run1 迁移相关日志 ---"
grep -E "数据库迁移|迁移已应用|nik_db::migrate|ERROR|FATAL" "$LOG1" | sed 's/^.*Z //' | head -30

echo "############ ④ 第 2 次启动（迁移器幂等：应应用 0 个） ############"
run_once "$LOG2" "run2"
echo "--- run2 迁移相关日志 ---"
grep -E "数据库迁移|迁移已应用|ERROR|FATAL" "$LOG2" | sed 's/^.*Z //' | head -20

echo "############ ⑤ 迁移后 schema_migrations ############"
ps "select version || '|' || name || '|' || coalesce(source,'') from schema_migrations where version::text in ('359','365','366','367') order by version"
ps "select 'migrations=' || count(*) from schema_migrations"

#!/usr/bin/env bash
# 采集「修之前」的对照证据（库 cloudpivot_so23before 已由 before 迁移集迁移完毕）
set -uo pipefail
WS=/root/.hermes/kanban/boards/dev/workspaces/t_1dadde81
EV=/opt/cloudpivot/docs/evidence/20261106-so23-followup-so-approve
DB=cloudpivot_so23before
export PGPASSWORD=alun
{
  echo "== 对照基线（同一份冻结迁移集，但**不含** V0301）：库 $DB =="
  psql -h 127.0.0.1 -U alun -d "$DB" -tA -F'|' \
    -c "select '迁移数=' || count(*) from schema_migrations" \
    -c "select 'so_approve：flow_name=' || flow_name || '，is_system=' || is_system::text
          from biz_flow_def where flow_code='so_approve'" \
    -c "select 'v1 hash=' || btrim(graph_hash) from biz_flow_def_version
         where id='20000000-0000-4000-8000-0000000d0282'" \
    -c "select '模板默认口径（AND d.is_system）命中=' || count(*)
          from biz_flow_def d where d.deleted_at is null and d.is_system and d.flow_code='so_approve'"
  echo
  echo "== erp_invariants --group 审批（迁移根目录也指到不含 V0301 的那份）=="
} > "$EV/raw/before-baseline.txt" 2>&1
cd /opt/cloudpivot || exit 1
ERP_MIGRATIONS_ROOT="$WS/migrations_before" \
NIK_DATABASE_HOST=127.0.0.1 NIK_DATABASE_USER=alun NIK_DATABASE_PASSWORD=alun NIK_DATABASE_NAME="$DB" \
  timeout 200 python3 scripts/verify/erp_invariants.py --group 审批 >> "$EV/raw/before-baseline.txt" 2>&1
tail -30 "$EV/raw/before-baseline.txt"

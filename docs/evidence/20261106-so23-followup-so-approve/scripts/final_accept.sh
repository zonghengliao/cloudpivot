#!/usr/bin/env bash
# =============================================================================
# 最终验收（一次跑完，全部落 docs/evidence/20261106-so23-followup-so-approve/raw/）
#
# 前置：bash fresh_db_frozen.sh 已经跑过（cloudpivot_so23fresh = 干净基线，226 条迁移）。
# 所有派生库都从它 TEMPLATE 克隆，不动基线库本身。
# =============================================================================
set -uo pipefail
WS=/root/.hermes/kanban/boards/dev/workspaces/t_1dadde81
EV=/opt/cloudpivot/docs/evidence/20261106-so23-followup-so-approve
RUN=$WS/run_env
BASE=cloudpivot_so23fresh
mkdir -p "$EV/raw"
export PGPASSWORD=alun
su_psql() { su postgres -s /bin/sh -c "psql -v ON_ERROR_STOP=1 -At $*"; }
clone() {  # clone <newdb>
  su_psql "-c \"select pg_terminate_backend(pid) from pg_stat_activity where datname='$1'\"" >/dev/null
  printf 'DROP DATABASE IF EXISTS %s;\nCREATE DATABASE %s TEMPLATE %s OWNER alun;\n' "$1" "$1" "$BASE" \
    | su_psql "-f -" >/dev/null
}

# ── ① 基线库状态 ────────────────────────────────────────────────────────────
psql -h 127.0.0.1 -U alun -d "$BASE" -tA -F'|' \
  -c "select '迁移数=' || count(*) || '，applied_via 非 executed/imported=' ||
             (select count(*) from schema_migrations where applied_via not in ('executed','imported'))
        from schema_migrations" \
  -c "select 'so_approve: flow_name=' || flow_name || '，is_system=' || is_system::text ||
             '，status=' || status || '，current_version_no=' || current_version_no
        from biz_flow_def where flow_code='so_approve'" \
  -c "select 'v' || version_no || ' ' || version_status || ' hash=' || btrim(graph_hash) ||
             ' nodes=' || node_count || ' edges=' || edge_count
        from biz_flow_def_version where def_id='20000000-0000-4000-8000-0000000d0281' order by version_no" \
  -c "select '快照边序=' || string_agg(x.item->>'key', ',' order by x.ord)
        from biz_flow_def_version v, jsonb_array_elements(v.graph_snapshot->'edges') with ordinality as x(item, ord)
       where v.id='20000000-0000-4000-8000-0000000d0282'" \
  -c "select '模板默认口径命中=' || count(*) from biz_flow_def d where d.deleted_at is null and d.is_system and d.flow_code='so_approve'" \
  > "$EV/raw/db-state.txt" 2>&1
cat "$EV/raw/db-state.txt"

# ── ② pflow1a 全套（含 publish_regression_keeps_graph_hash） ────────────────
clone cloudpivot_pflow1a
cd /opt/cloudpivot/backend || exit 1
PFLOW_DB_URL="postgres://alun:alun@127.0.0.1:5432/cloudpivot_pflow1a" \
  timeout 1800 cargo test --test pflow1a -- --test-threads=1 --nocapture \
  > "$EV/raw/pflow1a-full.txt" 2>&1
echo "pflow1a exit=$?"; grep -E "test result|so_approve" "$EV/raw/pflow1a-full.txt" | tail -3

# ── ③ erp_invariants（干净库，两轮：H5/H6 + 全量） ─────────────────────────
cd /opt/cloudpivot || exit 1
NIK_DATABASE_HOST=127.0.0.1 NIK_DATABASE_USER=alun NIK_DATABASE_PASSWORD=alun NIK_DATABASE_NAME="$BASE" \
  timeout 200 python3 scripts/verify/erp_invariants.py --group 审批 \
  > "$EV/raw/erp-invariants-group-approval.txt" 2>&1
echo "invariants(审批) exit=$?"
NIK_DATABASE_HOST=127.0.0.1 NIK_DATABASE_USER=alun NIK_DATABASE_PASSWORD=alun NIK_DATABASE_NAME="$BASE" \
  timeout 200 python3 scripts/verify/erp_invariants.py \
  > "$EV/raw/erp-invariants-full.txt" 2>&1
echo "invariants(full) exit=$?"
grep -E "^结果：" "$EV/raw/erp-invariants-full.txt" "$EV/raw/erp-invariants-group-approval.txt"

# ── ④ p16 快照自检（`flow::maintain` 行/快照/hash 三方对账，历史上因 so_approve 常红） ──
clone cloudpivot_p16probe
cd /opt/cloudpivot/backend || exit 1
ERP_DB_URL="postgres://alun:alun@127.0.0.1:5432/cloudpivot_p16probe" \
  timeout 1500 cargo test --lib p16_graph_snapshot_selfcheck_is_clean -- --nocapture \
  > "$EV/raw/p16-graph-selfcheck.txt" 2>&1
echo "p16 exit=$?"; grep -E "test result" "$EV/raw/p16-graph-selfcheck.txt" | tail -1

# ── ⑤ 模板列表接口（默认口径 = 标准流程） ──────────────────────────────────
clone cloudpivot_tplprobe
psql -h 127.0.0.1 -U alun -d cloudpivot_tplprobe -q -c \
  "UPDATE sys_user SET must_change_password = FALSE WHERE username='admin'"
kill "$(cat /tmp/t_1dadde81_api.pid 2>/dev/null)" 2>/dev/null; sleep 2
cd "$RUN" || exit 1
NIK_PROFILE=test NIK_DATABASE_NAME=cloudpivot_tplprobe NIK_DATABASE_USER=alun NIK_DATABASE_PASSWORD=alun \
NIK_SERVER_LISTEN=127.0.0.1:18224 NIK_ADMIN_PASSWORD='Admin@123456' \
NIK_MIDDLEWARE_AUTH_JWT_SECRET=$(openssl rand -hex 32) \
CLOUDPIVOT_STORAGE_MASTER_KEY=$(openssl rand -hex 32) \
setsid nohup /opt/cloudpivot/backend/target/debug/cloudpivot > /tmp/t_1dadde81_api.log 2>&1 < /dev/null &
echo $! > /tmp/t_1dadde81_api.pid
for _ in $(seq 1 60); do
  code=$(curl -s -o /dev/null -w '%{http_code}' -m 2 http://127.0.0.1:18224/health || true)
  [ "$code" = "200" ] && break
  sleep 2
done
echo "api health=$code"
cd "$WS" || exit 1
python3 probe_templates.py > "$EV/raw/templates-api-probe.txt" 2>&1
cat "$EV/raw/templates-api-probe.txt" | head -6
kill "$(cat /tmp/t_1dadde81_api.pid)" 2>/dev/null; rm -f /tmp/t_1dadde81_api.pid

# ── ⑥ 迁移双向 + 守卫自测 ─────────────────────────────────────────────────
cd "$WS" || exit 1
bash mig_roundtrip.sh > "$EV/raw/migration-roundtrip.txt" 2>&1
bash mig_guard.sh > "$EV/raw/migration-guard.txt" 2>&1
python3 h6_detail.py > "$EV/raw/h6-detail.txt" 2>&1
echo "evidence written:"; ls -la "$EV/raw"

#!/usr/bin/env bash
# 在 145 上做「克隆库 → 跑 V0355 up → 验证 → down → 验证」的隔离测试（不碰 cloudpivot_dev）
set -uo pipefail
ENV=/etc/cloudpivot/cloudpivot-dev.env
set -a; . "$ENV"; set +a
export PGPASSWORD="$NIK_DATABASE_PASSWORD"
PSQL="psql -h 127.0.0.1 -U $NIK_DATABASE_USER -At -F| -P pager=off"
CLONE=cloudpivot_fix1_probe

echo "== 0) 重建克隆库 $CLONE"
$PSQL -d postgres -c "drop database if exists $CLONE" >/dev/null
$PSQL -d postgres -c "create database $CLONE" >/dev/null
pg_dump -h 127.0.0.1 -U "$NIK_DATABASE_USER" -d "$NIK_DATABASE_NAME" --no-owner --no-acl \
  --exclude-table=schema_migrations_att05_bak \
  | $PSQL -d "$CLONE" > /tmp/fix1_clone_load.log 2>&1 || { echo CLONE_LOAD_FAIL; tail -5 /tmp/fix1_clone_load.log; exit 1; }
echo "   克隆完成：$( $PSQL -d "$CLONE" -c "select count(*) from biz_flow_def" )"

echo
echo "== 1) 应用前状态"
$PSQL -d "$CLONE" -c "select 'v1 hash='||btrim(graph_hash)||' ver='||version_no from biz_flow_def_version where id='20000000-0000-4000-8000-0000000c0229'"
$PSQL -d "$CLONE" -c "select node_key||' / '||assignee_type||' / '||coalesce(approval_mode,'NULL') from biz_flow_node where version_id='20000000-0000-4000-8000-0000000c0229' and node_key='n_dept'"

echo
echo "== 2) 跑 up"
psql -h 127.0.0.1 -U "$NIK_DATABASE_USER" -d "$CLONE" -P pager=off -v ON_ERROR_STOP=1 \
  -f /tmp/V0355.up.sql 2>&1 | tail -20
echo "   up rc=$?"

echo
echo "== 3) 应用后状态（当前版本 / n_dept / 版本列表 / 定义指针）"
$PSQL -d "$CLONE" -c "select version_no||'|'||version_status||'|'||is_current||'|'||btrim(graph_hash) from biz_flow_def_version where def_id='20000000-0000-4000-8000-0000000c0228' order by version_no"
$PSQL -d "$CLONE" -c "select n.node_key||'|'||n.assignee_type||'|'||coalesce(n.approval_mode,'NULL')||'|'||n.superior_levels||'|'||(select count(*) from biz_flow_node_assignee a where a.node_id=n.id) from biz_flow_node n where n.version_id='20000000-0000-4000-8000-0000000c022b' order by n.node_key"
$PSQL -d "$CLONE" -c "select 'def.current=v'||current_version_no||' id='||current_version_id from biz_flow_def where flow_code='budget_approve'"
$PSQL -d "$CLONE" -c "select 'v2 nodes='||(select count(*) from biz_flow_node where version_id='20000000-0000-4000-8000-0000000c022b')||' edges='||(select count(*) from biz_flow_edge where version_id='20000000-0000-4000-8000-0000000c022b')||' bind='||(select count(*) from biz_flow_node_assignee a join biz_flow_node n on n.id=a.node_id where n.version_id='20000000-0000-4000-8000-0000000c022b')"

echo
echo "== 4) 幂等：再跑一次 up（应 0 变更、NOTICE 跳过）"
psql -h 127.0.0.1 -U "$NIK_DATABASE_USER" -d "$CLONE" -P pager=off -v ON_ERROR_STOP=1 \
  -f /tmp/V0355.up.sql 2>&1 | tail -5
$PSQL -d "$CLONE" -c "select 'versions='||count(*) from biz_flow_def_version where def_id='20000000-0000-4000-8000-0000000c0228'"

echo
echo "== 5) down 回滚"
psql -h 127.0.0.1 -U "$NIK_DATABASE_USER" -d "$CLONE" -P pager=off -v ON_ERROR_STOP=1 \
  -f /tmp/V0355.down.sql 2>&1 | tail -10
$PSQL -d "$CLONE" -c "select version_no||'|'||version_status||'|'||is_current||'|'||btrim(graph_hash) from biz_flow_def_version where def_id='20000000-0000-4000-8000-0000000c0228' order by version_no"
$PSQL -d "$CLONE" -c "select 'def.current=v'||current_version_no from biz_flow_def where flow_code='budget_approve'"
$PSQL -d "$CLONE" -c "select 'v2 residue nodes='||(select count(*) from biz_flow_node where version_id='20000000-0000-4000-8000-0000000c022b')||' edges='||(select count(*) from biz_flow_edge where version_id='20000000-0000-4000-8000-0000000c022b')"

echo
echo "== 6) 回滚后再 up（证明可重复升降）"
psql -h 127.0.0.1 -U "$NIK_DATABASE_USER" -d "$CLONE" -P pager=off -v ON_ERROR_STOP=1 \
  -f /tmp/V0355.up.sql 2>&1 | tail -3
$PSQL -d "$CLONE" -c "select 'after re-up current=v'||current_version_no from biz_flow_def where flow_code='budget_approve'"

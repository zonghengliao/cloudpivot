#!/usr/bin/env bash
# V0355 的「跳过」分支探针：在克隆库上故意造出四种不该动的形状，确认
#   ① 不抛异常（不会挡住升级）② 不产生 v2 ③ NOTICE 点名原因。
# 用法：bash fix1_migration_skip_probe.sh
set -uo pipefail
ENV=/etc/cloudpivot/cloudpivot-dev.env
set -a; . "$ENV"; set +a
export PGPASSWORD="$NIK_DATABASE_PASSWORD"
PSQL="psql -h 127.0.0.1 -U $NIK_DATABASE_USER -At -F| -P pager=off"
CLONE=cloudpivot_fix1_skip
DEF=20000000-0000-4000-8000-0000000c0228
V1=20000000-0000-4000-8000-0000000c0229
P=0; F=0
ok()  { P=$((P+1)); echo "  PASS $*"; }
bad() { F=$((F+1)); echo "  FAIL $*"; }

fresh_clone() {
  $PSQL -d postgres -c "drop database if exists $CLONE" >/dev/null
  $PSQL -d postgres -c "create database $CLONE" >/dev/null
  pg_dump -h 127.0.0.1 -U "$NIK_DATABASE_USER" -d "$NIK_DATABASE_NAME" --no-owner --no-acl \
    --exclude-table=schema_migrations_att05_bak | $PSQL -d "$CLONE" >/dev/null 2>&1
  # 源库（cloudpivot_dev）已经跑过 V0355，这里先退回到「v1 是当前版本」的迁移前状态：
  # 删 v2 的行 / 边 / 绑定 / 版本，把跑在 v2 上的实例指回 v1（克隆库不关心历史完整性）。
  $PSQL -d "$CLONE" -c "
    delete from biz_flow_node_assignee where node_id in (select id from biz_flow_node where version_id='20000000-0000-4000-8000-0000000c022b');
    delete from biz_flow_edge where version_id='20000000-0000-4000-8000-0000000c022b';
    delete from biz_flow_node where version_id='20000000-0000-4000-8000-0000000c022b';
    update biz_flow_instance set version_id='$V1' where version_id='20000000-0000-4000-8000-0000000c022b';
    delete from biz_flow_def_version where id='20000000-0000-4000-8000-0000000c022b';
    update biz_flow_def_version set is_current=true, version_status='PUBLISHED' where id='$V1';
    update biz_flow_def set current_version_id='$V1', current_version_no=1 where id='$DEF';" >/dev/null
  local n
  n=$($PSQL -d "$CLONE" -c "select count(*) from biz_flow_def_version where def_id='$DEF'")
  if [ "$n" != "1" ]; then echo "FATAL: 克隆复位失败（版本数 $n）"; exit 1; fi
}

run_up() {  # run_up <case>
  local out rc
  out=$(psql -h 127.0.0.1 -U "$NIK_DATABASE_USER" -d "$CLONE" -v ON_ERROR_STOP=1 \
        -f /tmp/V0355.up.sql 2>&1); rc=$?
  echo "$out" | grep -E "NOTICE|ERROR" | sed 's/^/     /'
  if [ $rc -ne 0 ]; then bad "$1：up 抛异常（应 NOTICE 跳过）"; return 1; fi
  local vs
  vs=$($PSQL -d "$CLONE" -c "select count(*) from biz_flow_def_version where def_id='$DEF'")
  if [ "$vs" = "1" ]; then ok "$1：未产生 v2（版本数仍为 1）"; else bad "$1：产生了 v2（版本数 $vs）"; fi
  local cur
  cur=$($PSQL -d "$CLONE" -c "select current_version_no from biz_flow_def where id='$DEF'")
  if [ "$cur" = "1" ]; then ok "$1：定义仍指向 v1"; else bad "$1：定义指向 v$cur"; fi
}

echo "== A) 图里没有 n_dept 节点"
fresh_clone
$PSQL -d "$CLONE" -c "update biz_flow_def_version set graph_snapshot = jsonb_set(graph_snapshot, '{nodes}',
     (select coalesce(jsonb_agg(n), '[]'::jsonb) from jsonb_array_elements(graph_snapshot->'nodes') n
       where (n->>'key') <> 'n_dept')) where id='$V1'" >/dev/null
run_up "A 无 n_dept"

echo "== B) n_dept 已是别的形状（模拟客户自己改过）"
fresh_clone
$PSQL -d "$CLONE" -c "update biz_flow_def_version set graph_snapshot = jsonb_set(graph_snapshot, '{nodes}',
     (select jsonb_agg(case when (n->>'key')='n_dept'
                            then jsonb_set(n, '{assignee_type}', '\"INITIATOR_SUPERIOR\"'::jsonb)
                            else n end)
        from jsonb_array_elements(graph_snapshot->'nodes') n)) where id='$V1'" >/dev/null
run_up "B n_dept 已改"

echo "== C) 快照被直接改过（hash 对不上 → 无法安全现算 v2 hash）"
fresh_clone
$PSQL -d "$CLONE" -c "update biz_flow_def_version set graph_hash = repeat('d', 64) where id='$V1'" >/dev/null
run_up "C hash 不一致"

echo "== D) 本库没有 budget_approve 流程"
fresh_clone
$PSQL -d "$CLONE" -c "update biz_flow_def set deleted_at = now() where id='$DEF'" >/dev/null
$PSQL -d "$CLONE" -c "update biz_flow_def_version set graph_snapshot = '{}'::jsonb where id='$V1'" >/dev/null
run_up "D 无流程"

echo
echo "== 汇总：PASS=$P FAIL=$F"
$PSQL -d postgres -c "drop database if exists $CLONE" >/dev/null
[ "$F" = "0" ]

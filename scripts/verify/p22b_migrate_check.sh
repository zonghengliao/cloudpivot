#!/usr/bin/env bash
# P2-2b 迁移验收：在**干净的克隆库**上真跑 V0216 的 up / 幂等 / down / 再 up。
#
#   bash migrate_check.sh <db_name>        # 默认必须是一个从 cloudpivot_p16md 克隆出来的新库
#
# 输出全部落 stdout（由调用方重定向进 docs/evidence/）。
set -uo pipefail
DB=${1:-cloudpivot_p22b_c}
HERE=$(cd "$(dirname "$0")" && pwd)
MIG_DIR=${MIG_DIR:-$HERE/../../backend/migrations/business}
export PGPASSWORD=alun
PSQL="psql -h 127.0.0.1 -U alun -d $DB --no-psqlrc -v ON_ERROR_STOP=1 -P pager=off"

state() {
  echo "--- $1"
  $PSQL -tAF'|' -c "
    select 'def', flow_code, status, current_version_no::text from biz_flow_def
     where id='20000000-0000-4000-8000-0000000a0182'
    union all
    select 'ver', version_no::text, version_status, is_current::text from biz_flow_def_version
     where def_id='20000000-0000-4000-8000-0000000a0182'
    union all
    select 'hash', version_no::text, graph_hash, node_count::text||'/'||edge_count::text
      from biz_flow_def_version where def_id='20000000-0000-4000-8000-0000000a0182'
    order by 1, 2"
}

echo "== 库 $DB =="
state "迁移前（应只有 v1）"

echo
echo "== ① 首次 up =="
$PSQL -q -a < "$MIG_DIR/V0216__erp_p2_2b_po_credit_branch.up.sql"
state "首次 up 之后"

echo
echo "== ② 再跑一次 up（幂等：应 NOTICE 跳过、零变更）=="
$PSQL -q -a < "$MIG_DIR/V0216__erp_p2_2b_po_credit_branch.up.sql"
state "二次 up 之后"

echo
echo "== ③ down（应把 v1 复位、v2 删掉）=="
$PSQL -q -a < "$MIG_DIR/V0216__erp_p2_2b_po_credit_branch.down.sql"
state "down 之后"

echo
echo "== ④ 再 up（回滚后可重新发布）=="
$PSQL -q -a < "$MIG_DIR/V0216__erp_p2_2b_po_credit_branch.up.sql" | tail -3
state "再 up 之后"

echo
echo "== ⑤ 边界：客户自己发过 v3 时 up 必须跳过（不覆盖）=="
$PSQL -q -c "
  insert into biz_flow_def_version (id, def_id, version_no, version_status, is_current, change_note)
  values ('20000000-0000-4000-8000-00000000c3c3', '20000000-0000-4000-8000-0000000a0182', 3,
          'DRAFT', false, '测试：模拟客户自己另存的 v3 草稿');" >/dev/null
$PSQL -q -a < "$MIG_DIR/V0216__erp_p2_2b_po_credit_branch.up.sql" | grep -E "NOTICE|ERROR" || true
$PSQL -q -c "delete from biz_flow_def_version where id='20000000-0000-4000-8000-00000000c3c3'" >/dev/null
echo "（v3 草稿已清理）"

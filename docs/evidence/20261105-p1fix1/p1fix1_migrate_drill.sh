#!/bin/bash
# V0248 迁移演练：up（已应用）→ down → up，三次核对「节点配置 + hash + 审批人绑定」。
set -uo pipefail
DB=${1:-cloudpivot_p1fix1_a}
M=/opt/cloudpivot/backend/migrations/business
export PGPASSWORD=alun
q() { psql -h 127.0.0.1 -U alun -d "$DB" -Atc "$1"; }

state() {
  printf '  节点: %s\n' "$(q "select n.assignee_type||'/'||n.empty_assignee||'/'||n.self_approval
      from biz_flow_node n join biz_flow_def d on d.current_version_id = n.version_id
     where d.flow_code='stock_in_cancel' and n.node_type='APPROVAL'")"
  printf '  绑定: %s\n' "$(q "select coalesce(r.role_code,'(无)')
      from biz_flow_node_assignee a
      join biz_flow_node n on n.id = a.node_id
      join biz_flow_def_version v on v.id = n.version_id
      join biz_flow_def d on d.id = v.def_id
      left join sys_role r on r.id = a.ref_id
     where d.flow_code='stock_in_cancel' and v.is_current and n.node_type='APPROVAL'")"
  printf '  hash: %s\n' "$(q "select graph_hash from biz_flow_def_version v join biz_flow_def d on d.id=v.def_id
      where d.flow_code='stock_in_cancel' and v.is_current")"
}

echo "== ① 现状（V0248 已应用）=="
state
echo "-- SELECT count(*) 断言：应为 ROLE/ERROR/FORBID + WAREHOUSE_MANAGER + 026766… --"
[ "$(q "select count(*) from biz_flow_node n join biz_flow_def d on d.current_version_id=n.version_id
        where d.flow_code='stock_in_cancel' and n.node_key='n_review'
          and n.assignee_type='ROLE' and n.empty_assignee='ERROR' and n.self_approval='FORBID'")" = "1" ] && echo "  [PASS] 节点口径" || echo "  [FAIL] 节点口径"
[ "$(q "select count(*) from biz_flow_def_version v join biz_flow_def d on d.id=v.def_id
        where d.flow_code='stock_in_cancel' and v.is_current and btrim(v.graph_hash)='0267661a1b7d37ebc94a3e8859eecde63173325f57d3949970dcbdd4f8b9b5a0'")" = "1" ] && echo "  [PASS] hash = 026766…" || echo "  [FAIL] hash"

echo
echo "== ② down（回滚到 V0174 种子口径）=="
psql -h 127.0.0.1 -U alun -d "$DB" -f "$M/V0248__stock_in_cancel_reviewer_role.down.sql" 2>&1 | grep -E "NOTICE|ERROR"
state
[ "$(q "select count(*) from biz_flow_node n join biz_flow_def d on d.current_version_id=n.version_id
        where d.flow_code='stock_in_cancel' and n.node_key='n_review'
          and n.assignee_type='ADMIN' and n.empty_assignee='FALLBACK_ADMIN' and n.self_approval='FORBID'")" = "1" ] && echo "  [PASS] 已回滚为 ADMIN/FALLBACK_ADMIN/FORBID" || echo "  [FAIL] 回滚不彻底"
[ "$(q "select count(*) from biz_flow_node_assignee a join biz_flow_node n on n.id=a.node_id
        join biz_flow_def d on d.current_version_id=n.version_id
        where d.flow_code='stock_in_cancel'")" = "0" ] && echo "  [PASS] 审批人绑定已清空" || echo "  [FAIL] 绑定残留"

echo
echo "== ③ 再 up（幂等重放）=="
psql -h 127.0.0.1 -U alun -d "$DB" -f "$M/V0248__stock_in_cancel_reviewer_role.up.sql" 2>&1 | grep -E "NOTICE|ERROR"
state
[ "$(q "select count(*) from biz_flow_node n join biz_flow_def d on d.current_version_id=n.version_id
        where d.flow_code='stock_in_cancel' and n.node_key='n_review'
          and n.assignee_type='ROLE' and n.empty_assignee='ERROR' and n.self_approval='FORBID'")" = "1" ] && echo "  [PASS] 重新应用后口径正确" || echo "  [FAIL] 重新应用口径不对"
[ "$(q "select count(*) from biz_flow_def_version v join biz_flow_def d on d.id=v.def_id
        where d.flow_code='stock_in_cancel' and v.is_current and btrim(v.graph_hash)='0267661a1b7d37ebc94a3e8859eecde63173325f57d3949970dcbdd4f8b9b5a0'")" = "1" ] && echo "  [PASS] hash 回到 026766…" || echo "  [FAIL] hash 不对"

echo
echo "== ④ 再 up 一次（幂等：应 NOTICE 已迁移并 0 变更）=="
psql -h 127.0.0.1 -U alun -d "$DB" -f "$M/V0248__stock_in_cancel_reviewer_role.up.sql" 2>&1 | grep -E "NOTICE|ERROR"

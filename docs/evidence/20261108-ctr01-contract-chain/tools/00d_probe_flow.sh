#!/usr/bin/env bash
# 只读体检（3）：contract_approve 流程图（节点 / 审批人 / 条件边）
set -uo pipefail
set -a; . /etc/cloudpivot/cloudpivot-dev.env; set +a
export PGPASSWORD="$NIK_DATABASE_PASSWORD"
DB=${CTR01_DB:-cloudpivot_ctr01}
q() { psql -h 127.0.0.1 -U "$NIK_DATABASE_USER" -At -d "$DB" -c "$1"; }
echo "== def"
q "select 'def '||d.id||' '||d.flow_code||' '||d.biz_type||' status='||d.status||' cur_ver='||coalesce(d.current_version_id::text,'-') from biz_flow_def d where d.flow_code='contract_approve'"
echo "== version"
q "select 'ver '||v.id||' no='||v.version_no||' status='||v.version_status||' current='||v.is_current||' hash='||coalesce(v.graph_hash,'-') from biz_flow_def_version v join biz_flow_def d on d.id=v.def_id where d.flow_code='contract_approve'"
echo "== nodes"
q "select 'node '||n.node_key||' type='||n.node_type||' name='||n.node_name||' empty='||n.empty_assignee||' self='||n.self_approval from biz_flow_node n join biz_flow_def_version v on v.id=n.version_id join biz_flow_def d on d.id=v.def_id where d.flow_code='contract_approve' and v.is_current order by n.node_key"
echo "== assignees"
q "select 'assignee '||a.node_key||' '||a.assignee_type||' '||coalesce(a.ref_code,'-') from biz_flow_node_assignee a join biz_flow_def_version v on v.id=a.version_id join biz_flow_def d on d.id=v.def_id where d.flow_code='contract_approve' and v.is_current order by a.node_key"
echo "== edges"
q "select 'edge '||e.from_key||'->'||e.to_key||' cond='||coalesce(e.condition_expr,'-') from biz_flow_edge e join biz_flow_def_version v on v.id=e.version_id join biz_flow_def d on d.id=v.def_id where d.flow_code='contract_approve' and v.is_current order by e.from_key,e.to_key"

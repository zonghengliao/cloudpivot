#!/usr/bin/env bash
# 只读体检（4）：contract_approve 的节点审批人（node → ROLE/USER）与条件边
set -uo pipefail
set -a; . /etc/cloudpivot/cloudpivot-dev.env; set +a
export PGPASSWORD="$NIK_DATABASE_PASSWORD"
DB=${CTR01_DB:-cloudpivot_ctr01}
q() { psql -h 127.0.0.1 -U "$NIK_DATABASE_USER" -At -d "$DB" -c "$1"; }
echo "== 节点审批人（角色名）"
q "select 'assignee '||n.node_key||'('||n.node_name||') type='||a.assignee_type||' role='||coalesce(r.role_code,'-')||' user='||coalesce(u.username,'-')
     from biz_flow_node_assignee a
     join biz_flow_node n on n.id=a.node_id
     join biz_flow_def_version v on v.id=n.version_id
     join biz_flow_def d on d.id=v.def_id
     left join sys_role r on r.id=a.ref_id
     left join sys_user u on u.id=a.ref_id
    where d.flow_code='contract_approve' and v.is_current
    order by n.node_key"
echo "== 条件边（金额分级阈值）"
q "select 'edge '||fn.node_key||'->'||tn.node_key||' default='||e.is_default||' cond='||coalesce(e.condition_expr::text,'-')
     from biz_flow_edge e
     join biz_flow_def_version v on v.id=e.version_id
     join biz_flow_def d on d.id=v.def_id
     join biz_flow_node fn on fn.id=e.from_node_id
     join biz_flow_node tn on tn.id=e.to_node_id
    where d.flow_code='contract_approve' and v.is_current
    order by fn.node_key, tn.node_key"

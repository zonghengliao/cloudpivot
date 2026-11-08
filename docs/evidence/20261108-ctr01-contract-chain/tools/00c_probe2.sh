#!/usr/bin/env bash
# 只读体检（2）：流程定义 / 会计期间 / 菜单与权限码
set -uo pipefail
set -a; . /etc/cloudpivot/cloudpivot-dev.env; set +a
export PGPASSWORD="$NIK_DATABASE_PASSWORD"
DB=${CTR01_DB:-cloudpivot_ctr01}
q() { psql -h 127.0.0.1 -U "$NIK_DATABASE_USER" -At -d "$DB" -c "$1"; }
echo "== flow contract_approve"
q "select 'def '||d.id||' '||d.flow_code||' '||d.biz_type||' status='||d.status||' path='||coalesce(d.definition_key,'-') from biz_flow_def d where d.flow_code='contract_approve'"
q "select 'ver '||v.id||' version='||v.version||' current='||v.is_current||' published='||coalesce(v.is_published::text,'-') from biz_flow_def_version v join biz_flow_def d on d.id=v.flow_id where d.flow_code='contract_approve'"
q "select 'node '||n.node_key||' type='||n.node_type||' name='||n.node_name from biz_flow_node n join biz_flow_def d on d.id=n.flow_id where d.flow_code='contract_approve' order by n.node_key"
q "select 'assignee '||a.node_key||' '||a.assignee_type||' '||coalesce(a.ref_code,'-') from biz_flow_node_assignee a join biz_flow_def d on d.id=a.flow_id where d.flow_code='contract_approve' order by a.node_key"
q "select 'edge '||e.from_key||'->'||e.to_key||' cond='||coalesce(e.condition_expr,'-') from biz_flow_edge e join biz_flow_def d on d.id=e.flow_id where d.flow_code='contract_approve' order by e.from_key,e.to_key"
echo "== accounting period"
q "select period_code||' '||status from biz_accounting_period order by period_code desc limit 6"
echo "== contract menu/auth codes"
q "select path||' | '||name||' | '||title||' | '||coalesce(auth_code,'-') from sys_menu where path like '%contract%' order by path"
echo "== roles with contract perms"
q "select r.role_code||' '||m.auth_code from sys_role_menu rm join sys_role r on r.id=rm.role_id join sys_menu m on m.id=rm.menu_id where m.auth_code like 'business:erp:contract:%' order by r.role_code, m.auth_code"

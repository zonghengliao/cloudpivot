#!/usr/bin/env bash
set -uo pipefail
set -a; . /etc/cloudpivot/cloudpivot-dev.env; set +a
export PGPASSWORD="$NIK_DATABASE_PASSWORD"
DB=${INV01_DB:-cloudpivot_inv01}
q() { psql -h 127.0.0.1 -U "$NIK_DATABASE_USER" -At -d "$DB" -c "$1"; }
echo "== stock_in_post nodes/assignees"
q "select 'node '||n.node_key||' '||n.node_type||' '||n.node_name||' role='||coalesce(a.assignee_type,'-')||':'||coalesce(r.role_code, a.ref_id::text, '-') from biz_flow_node n left join biz_flow_node_assignee a on a.node_id=n.id left join sys_role r on r.id=a.ref_id join biz_flow_def_version v on v.id=n.version_id join biz_flow_def d on d.id=v.def_id where d.flow_code='stock_in_post' and v.is_current order by n.node_key"
echo "== tasks"; q "select t.status||' '||t.node_key||' '||coalesce(t.assignee_id::text,'-') from biz_flow_task t order by t.created_at"
echo "== stock_in"; q "select in_no||' '||status from biz_stock_in"
echo "== approver roles"; q "select r.role_code from sys_user_role ur join sys_role r on r.id=ur.role_id join sys_user u on u.id=ur.user_id where u.username='inv01_approver' order by 1"

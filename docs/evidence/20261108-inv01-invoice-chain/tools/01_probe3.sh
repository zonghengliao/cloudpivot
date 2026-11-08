#!/usr/bin/env bash
set -uo pipefail
set -a; . /etc/cloudpivot/cloudpivot-dev.env; set +a
export PGPASSWORD="$NIK_DATABASE_PASSWORD"
DB=${INV01_DB:-cloudpivot_inv01}
q() { psql -h 127.0.0.1 -U "$NIK_DATABASE_USER" -At -d "$DB" -c "$1"; }
echo "== assignee cols"; q "select column_name from information_schema.columns where table_name='biz_flow_node_assignee' order by ordinal_position" | tr '\n' ' '
echo; echo "== edge cols"; q "select column_name from information_schema.columns where table_name='biz_flow_edge' order by ordinal_position" | tr '\n' ' '
echo; echo "== assignees so_approve"; q "select 'assignee '||a.node_key||' '||a.assignee_type||' '||coalesce(a.ref_code,'-') from biz_flow_node_assignee a join biz_flow_def_version v on v.id=a.version_id join biz_flow_def d on d.id=v.def_id where d.flow_code='so_approve' and v.is_current order by a.node_key"
echo "== edges so_approve"; q "select 'edge '||e.from_key||'->'||e.to_key||' cond='||coalesce(e.condition_expr,'-') from biz_flow_edge e join biz_flow_def_version v on v.id=e.version_id join biz_flow_def d on d.id=v.def_id where d.flow_code='so_approve' and v.is_current order by e.from_key,e.to_key"
echo "== admin roles"; q "select r.role_code from sys_user_role ur join sys_role r on r.id=ur.role_id join sys_user u on u.id=ur.user_id where u.username='admin'"
echo "== user cols"; q "select column_name from information_schema.columns where table_name='sys_user' order by ordinal_position" | tr '\n' ' '
echo; echo "== stockout/nearby: biz_stock_out cols"; q "select column_name from information_schema.columns where table_name='biz_stock_out' order by ordinal_position" | tr '\n' ' '
echo; echo "== report items"; q "select report_code||' | '||item_code||' | '||item_name||' | pattern='||coalesce(account_pattern,'-')||' | reclass='||coalesce(reclass_negative_to,'-') from biz_report_item where report_code in ('BALANCE_SHEET','INCOME_STATEMENT') and (coalesce(account_pattern,'') like '%2221%' or coalesce(account_pattern,'') like '%5403%' or item_name like '%税%') order by report_code, item_code"

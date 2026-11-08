#!/usr/bin/env bash
set -uo pipefail
set -a; . /etc/cloudpivot/cloudpivot-dev.env; set +a
export PGPASSWORD="$NIK_DATABASE_PASSWORD"
DB=${INV01_DB:-cloudpivot_inv01}
q() { psql -h 127.0.0.1 -U "$NIK_DATABASE_USER" -At -d "$DB" -c "$1"; }
echo "== biz_account cols"; q "select column_name from information_schema.columns where table_name='biz_account' order by ordinal_position" | tr '\n' ' '
echo; echo "== accounts"; q "select account_code||' | '||account_name||' | status='||status from biz_account where account_code in ('1122','2221','2221.01','2221.02','5001','5403','1002','2202') order by account_code"
echo "== biz_report_item cols"; q "select column_name from information_schema.columns where table_name='biz_report_item' order by ordinal_position" | tr '\n' ' '
echo; echo "== report items 2221/5403"; q "select item_code||' | '||item_name from biz_report_item where item_code in ('TAX','TAX_SURCHARGE','CASH_TAX') or coalesce(account_code,'')<>'' order by item_code" | head -40
echo "== sales order cols"; q "select column_name from information_schema.columns where table_name='biz_sales_order' order by ordinal_position" | tr '\n' ' '
echo; echo "== sales order item cols"; q "select column_name from information_schema.columns where table_name='biz_sales_order_item' order by ordinal_position" | tr '\n' ' '
echo; echo "== shipment cols"; q "select column_name from information_schema.columns where table_name='biz_shipment' order by ordinal_position" | tr '\n' ' '
echo; echo "== biz_seq"; q "select seq_type||' | '||coalesce(prefix,'-')||' | next='||next_no from biz_seq order by seq_type"
echo "== so_approve flow"; q "select 'node '||n.node_key||' type='||n.node_type||' name='||n.node_name from biz_flow_node n join biz_flow_def_version v on v.id=n.version_id join biz_flow_def d on d.id=v.def_id where d.flow_code='so_approve' and v.is_current order by n.node_key"
q "select 'assignee '||a.node_key||' '||a.assignee_type||' '||coalesce(a.ref_code,'-') from biz_flow_node_assignee a join biz_flow_def_version v on v.id=a.version_id join biz_flow_def d on d.id=v.def_id where d.flow_code='so_approve' and v.is_current order by a.node_key"
q "select 'edge '||e.from_key||'->'||e.to_key||' cond='||coalesce(e.condition_expr,'-') from biz_flow_edge e join biz_flow_def_version v on v.id=e.version_id join biz_flow_def d on d.id=v.def_id where d.flow_code='so_approve' and v.is_current order by e.from_key,e.to_key"
echo "== roles"; q "select role_code||' | '||role_name from sys_role order by role_code"
echo "== so_approve perms on roles"; q "select r.role_code||' | '||m.auth_code from sys_role_menu rm join sys_role r on r.id=rm.role_id join sys_menu m on m.id=rm.menu_id where m.auth_code like 'business:erp:sales-order:%' order by 1,2"

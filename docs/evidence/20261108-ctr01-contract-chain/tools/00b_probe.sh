#!/usr/bin/env bash
# 只读体检：CTR-01 隔离库的种子资源与流程/任务/附件域配置
set -uo pipefail
set -a; . /etc/cloudpivot/cloudpivot-dev.env; set +a
export PGPASSWORD="$NIK_DATABASE_PASSWORD"
DB=${CTR01_DB:-cloudpivot_ctr01}
q() { psql -h 127.0.0.1 -U "$NIK_DATABASE_USER" -At -d "$DB" -c "$1"; }
echo "== roles"; q "select id||' '||role_code||' '||role_name from sys_role order by role_code"
echo "== suppliers"; q "select id||' '||supplier_code||' '||supplier_name||' status='||status from biz_supplier"
echo "== customers"; q "select id||' '||code||' '||name||' status='||status from biz_customer"
echo "== materials"; q "select id||' '||material_code||' '||material_name||' unit='||unit_id||' tax='||tax_rate||' status='||status from biz_material"
echo "== warehouses"; q "select id||' '||warehouse_code||' '||warehouse_name||' neg='||allow_negative from biz_warehouse"
echo "== flow contract_approve"
q "select d.id||' '||d.flow_code||' '||d.biz_type||' v'||d.version||' cur='||d.is_current||' status='||d.status from biz_flow_definition d where d.flow_code='contract_approve' order by d.version desc"
q "select 'node '||n.node_key||' type='||n.node_type||' name='||n.node_name from biz_flow_node n join biz_flow_definition d on d.id=n.definition_id where d.flow_code='contract_approve' and d.is_current order by n.node_key"
q "select 'assignee '||a.node_key||' '||a.assignee_type||' '||coalesce(a.ref_code,'-')||' '||coalesce(a.ref_id::text,'-') from biz_flow_node_assignee a join biz_flow_definition d on d.id=a.definition_id where d.flow_code='contract_approve' and d.is_current order by a.node_key"
q "select 'edge '||e.from_key||'->'||e.to_key||' cond='||coalesce(e.condition_expr,'-') from biz_flow_edge e join biz_flow_definition d on d.id=e.definition_id where d.flow_code='contract_approve' and d.is_current order by e.from_key,e.to_key"
echo "== sys_job"; q "select job_name||' | status='||status||' | cron='||cron_expr||' | target='||invoke_target from sys_job where deleted_at is null order by job_name"
echo "== doc attachment kinds/roles"
q "select pg_get_constraintdef(oid) from pg_constraint where conname like '%doc_attachment%kind%' or conname like '%doc_attachment%role%'"
echo "== contract columns"; q "select column_name||':'||data_type from information_schema.columns where table_name='biz_contract' order by ordinal_position"
echo "== contract status check"; q "select pg_get_constraintdef(oid) from pg_constraint where conname='ck_biz_contract_status'"
echo "== reminder log"; q "select pg_get_constraintdef(oid) from pg_constraint where conname like 'ck_biz_reminder%'"
echo "== accounting period"; q "select period||' '||status from biz_accounting_period order by period desc limit 5"
echo "== admin"; q "select username||' must_change='||must_change_password||' status='||status from sys_user"

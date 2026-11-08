#!/usr/bin/env bash
set -uo pipefail
set -a; . /etc/cloudpivot/cloudpivot-dev.env; set +a
export PGPASSWORD="$NIK_DATABASE_PASSWORD"
DB=${INV01_DB:-cloudpivot_inv01}
q() { psql -h 127.0.0.1 -U "$NIK_DATABASE_USER" -At -d "$DB" -c "$1"; }
echo "== attachment rule cols"; q "select column_name from information_schema.columns where table_name='biz_attachment_rule' order by ordinal_position" | tr '\n' ' '
echo; echo "== INVOICE rules"; q "select * from biz_attachment_rule where doc_type='INVOICE'"
echo "== rule count by doc_type"; q "select doc_type||'='||count(*) from biz_attachment_rule group by doc_type order by doc_type"
echo "== flow assignees so_approve"; q "select 'assignee '||n.node_key||' '||a.assignee_type||' '||coalesce(a.ref_id::text,'-') from biz_flow_node_assignee a join biz_flow_node n on n.id=a.node_id join biz_flow_def_version v on v.id=n.version_id join biz_flow_def d on d.id=v.def_id where d.flow_code='so_approve' and v.is_current order by n.node_key, a.sort_order"
echo "== flow edges so_approve"; q "select 'edge '||e.edge_key||' '||f.node_key||'->'||t.node_key||' cond='||coalesce(e.condition_expr,'-') from biz_flow_edge e join biz_flow_node f on f.id=e.from_node_id join biz_flow_node t on t.id=e.to_node_id join biz_flow_def_version v on v.id=e.version_id join biz_flow_def d on d.id=v.def_id where d.flow_code='so_approve' and v.is_current order by e.edge_key"
echo "== seq cols"; q "select column_name from information_schema.columns where table_name='biz_seq' order by ordinal_position" | tr '\n' ' '
echo; echo "== seq"; q "select seq_type||' | '||coalesce(prefix,'-') from biz_seq order by seq_type"

set -e
export PGPASSWORD=e2eiso_2026
PSQL="psql -h 127.0.0.1 -U e2erole -X -q -v ON_ERROR_STOP=1"
DOWN=/opt/cloudpivot/backend/migrations/business/V0251__flow_deadmin_assignee_roles.down.sql
UP=/opt/cloudpivot/backend/migrations/business/V0251__flow_deadmin_assignee_roles.up.sql
echo "=== down on path_a ==="
$PSQL -d cloudpivot_deadmin_a -f $DOWN 2>&1 | tail -5
$PSQL -d cloudpivot_deadmin_a -c "select d.flow_code, d.current_version_no, v.version_no, v.version_status, v.is_current, count(n.id) nodes from biz_flow_def d join biz_flow_def_version v on v.def_id=d.id left join biz_flow_node n on n.version_id=v.id where d.flow_code in ('payable_pay','stock_in_cancel','stock_transfer') group by 1,2,3,4,5 order by 1,3"
echo "=== 再 up（回到 v2，验证幂等往返）==="
$PSQL -d cloudpivot_deadmin_a -f $UP 2>&1 | tail -4
$PSQL -d cloudpivot_deadmin_a -c "select d.flow_code, d.current_version_no, count(n.id) filter (where v.version_no=2) v2_nodes, count(b.node_id) binds from biz_flow_def d join biz_flow_def_version v on v.def_id=d.id left join biz_flow_node n on n.version_id=v.id left join biz_flow_node_assignee b on b.node_id=n.id where d.flow_code in ('payable_pay','stock_in_cancel','stock_transfer') group by 1,2 order by 1"

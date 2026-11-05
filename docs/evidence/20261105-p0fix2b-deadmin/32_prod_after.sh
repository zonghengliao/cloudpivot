set -a; . /etc/cloudpivot/cloudpivot.env; set +a
export PGPASSWORD="$NIK_DATABASE_PASSWORD"
PSQL="psql -h $NIK_DATABASE_HOST -p $NIK_DATABASE_PORT -U $NIK_DATABASE_USER -d $NIK_DATABASE_NAME -X -q -A -F |"
OUT=/tmp/prod_after_deadmin.txt
{
echo "## 生成时间（prod）: $(date -Is)"
echo
echo "### 1) B1 已发布流程里 assignee_type=ADMIN 节点数"
$PSQL -c "select count(*) from biz_flow_node n join biz_flow_def_version v on v.id=n.version_id join biz_flow_def d on d.id=v.def_id where d.deleted_at is null and d.status='PUBLISHED' and v.is_current and n.assignee_type='ADMIN'"
echo
echo "### 2) 5 个节点的最终配置 + 绑定"
$PSQL -c "select d.flow_code, n.node_key, n.node_name, n.assignee_type, coalesce(n.approval_mode,'-'), n.empty_assignee, n.self_approval, coalesce(string_agg(r.role_code, ',' order by a.sort_order),'-')
 from biz_flow_node n join biz_flow_def_version v on v.id=n.version_id join biz_flow_def d on d.id=v.def_id
 left join biz_flow_node_assignee a on a.node_id=n.id left join sys_role r on r.id=a.ref_id
 where d.flow_code in ('payable_pay','stock_in_cancel','stock_transfer') and v.is_current and n.node_type in ('APPROVAL','COUNTERSIGN')
 group by 1,2,3,4,5,6,7 order by 1,2"
echo
echo "### 3) 版本与 hash"
$PSQL -c "select d.flow_code, d.current_version_no, v.version_no, v.version_status, v.is_current, btrim(v.graph_hash) from biz_flow_def d join biz_flow_def_version v on v.def_id=d.id where d.flow_code in ('payable_pay','stock_in_cancel','stock_transfer') order by 1,3"
echo
echo "### 4) B3 两张 PENDING 实例待办（after）"
$PSQL -c "select i.instance_no, i.flow_code, i.status, t.id, t.node_key, t.status, t.assignee_id, t.assignee_name, t.assignee_source, t.created_at::text from biz_flow_instance i join biz_flow_task t on t.instance_id=i.id where i.instance_no in ('FLW-20261102-94417B93','FLW-20261105-5D6B0905') order by 1, t.created_at"
echo
echo "### 5) 计数"
$PSQL -c "select (select count(*) from biz_flow_task) tasks, (select count(*) from biz_flow_instance) instances, (select count(*) from biz_flow_instance where status='PENDING') pending, (select count(*) from sys_user_role) user_roles"
} > $OUT 2>&1
cat $OUT
echo
echo "### 6) health"
curl -s -o /dev/null -w "health=%{http_code}\n" http://127.0.0.1:8023/health
echo
echo "### 7) erp_invariants 全量"
python3 /opt/cloudpivot/scripts/verify/erp_invariants.py 2>&1 | tail -6

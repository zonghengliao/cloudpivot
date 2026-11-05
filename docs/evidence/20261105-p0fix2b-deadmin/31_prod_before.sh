set -a; . /etc/cloudpivot/cloudpivot.env; set +a
export PGPASSWORD="$NIK_DATABASE_PASSWORD"
PSQL="psql -h $NIK_DATABASE_HOST -p $NIK_DATABASE_PORT -U $NIK_DATABASE_USER -d $NIK_DATABASE_NAME -X -q -A -F |"
OUT=/tmp/prod_before_deadmin.txt
{
echo "## 生成时间（prod）: $(date -Is)"
echo
echo "### 1) 已发布流程的 ADMIN 节点（B1 before）"
$PSQL -c "select d.flow_code, n.node_key, n.node_name, n.assignee_type, coalesce(n.approval_mode,'-'), n.empty_assignee, n.self_approval, v.version_no
 from biz_flow_node n join biz_flow_def_version v on v.id=n.version_id join biz_flow_def d on d.id=v.def_id
 where d.deleted_at is null and d.status='PUBLISHED' and v.is_current and n.assignee_type='ADMIN' order by 1,2"
echo
echo "### 2) 两张 PENDING 实例的待办（B3 before）"
$PSQL -c "select i.instance_no, i.flow_code, i.status inst_status, t.id task_id, t.node_key, t.status task_status, t.assignee_id, t.assignee_name, t.assignee_source, t.is_self_approval, t.created_at::text
 from biz_flow_instance i join biz_flow_task t on t.instance_id=i.id
 where i.instance_no in ('FLW-20261102-94417B93','FLW-20261105-5D6B0905')
 order by i.instance_no, t.created_at"
echo
echo "### 3) 这些实例钉的版本"
$PSQL -c "select instance_no, flow_code, version_id, version_no, status from biz_flow_instance where instance_no in ('FLW-20261102-94417B93','FLW-20261105-5D6B0905') order by 1"
echo
echo "### 4) 全库 biz_flow_task / biz_flow_instance 计数（B3 判据用）"
$PSQL -c "select (select count(*) from biz_flow_task) tasks, (select count(*) from biz_flow_instance) instances, (select count(*) from biz_flow_instance where status='PENDING') pending"
} > $OUT 2>&1
cat $OUT

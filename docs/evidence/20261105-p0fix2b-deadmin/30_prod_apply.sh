set -e
set -a; . /etc/cloudpivot/cloudpivot.env; set +a
export PGPASSWORD="$NIK_DATABASE_PASSWORD"
PSQL="psql -h $NIK_DATABASE_HOST -p $NIK_DATABASE_PORT -U $NIK_DATABASE_USER -d $NIK_DATABASE_NAME -X -q -v ON_ERROR_STOP=1"
TAG=$(date +%Y%m%dT%H%M%S)
BK=/root/flowdeadmin-backup-$TAG
mkdir -p $BK
echo "=== 1) 备份（只备这次会动的 4 张表 + schema_migrations）→ $BK ==="
pg_dump -h "$NIK_DATABASE_HOST" -p "$NIK_DATABASE_PORT" -U "$NIK_DATABASE_USER" -d "$NIK_DATABASE_NAME" \
  --no-owner --no-privileges \
  -t biz_flow_def -t biz_flow_def_version -t biz_flow_node -t biz_flow_node_assignee -t biz_flow_edge \
  > $BK/flow_tables.sql
ls -l $BK/flow_tables.sql
echo "=== 2) 应用前状态快照 ==="
$PSQL -c "select d.flow_code, d.current_version_no, v.id v_current, left(btrim(v.graph_hash),16) h from biz_flow_def d join biz_flow_def_version v on v.id=d.current_version_id where d.flow_code in ('payable_pay','stock_in_cancel','stock_transfer') order by 1"
$PSQL -c "select (select count(*) from biz_flow_task) tasks, (select count(*) from biz_flow_instance) inst, (select count(*) from biz_flow_node where assignee_type='ADMIN') admin_nodes"
echo "=== 3) 应用 V0251（单事务）==="
$PSQL -1 -f /tmp/V0251__flow_deadmin_assignee_roles.up.sql
echo "=== 4) 应用后状态 ==="
$PSQL -c "select d.flow_code, d.current_version_no, v.version_no, v.version_status, v.is_current, left(btrim(v.graph_hash),16) h from biz_flow_def d join biz_flow_def_version v on v.def_id=d.id where d.flow_code in ('payable_pay','stock_in_cancel','stock_transfer') order by 1,3"
$PSQL -c "select d.flow_code, n.node_key, n.node_name, n.assignee_type, coalesce(n.approval_mode,'-'), n.empty_assignee, n.self_approval, coalesce(string_agg(r.role_code, ',' order by a.sort_order),'-') roles from biz_flow_node n join biz_flow_def_version v on v.id=n.version_id join biz_flow_def d on d.id=v.def_id left join biz_flow_node_assignee a on a.node_id=n.id left join sys_role r on r.id=a.ref_id where d.flow_code in ('payable_pay','stock_in_cancel','stock_transfer') and v.is_current and n.node_type in ('APPROVAL','COUNTERSIGN') group by 1,2,3,4,5,6,7 order by 1,2"
echo "=== 5) B1：已发布流程里 assignee_type=ADMIN 的节点数 ==="
$PSQL -c "select count(*) admin_nodes_on_published_current from biz_flow_node n join biz_flow_def_version v on v.id=n.version_id join biz_flow_def d on d.id=v.def_id where d.deleted_at is null and d.status='PUBLISHED' and v.is_current and n.assignee_type='ADMIN'"
echo "=== 6) B3：两张 PENDING 实例的待办（应用后）==="
$PSQL -c "select i.instance_no, i.flow_code, i.status, t.id, t.node_key, t.status, t.assignee_id, t.assignee_name, t.assignee_source, t.created_at::text from biz_flow_instance i join biz_flow_task t on t.instance_id=i.id where i.instance_no in ('FLW-20261102-94417B93','FLW-20261105-5D6B0905') order by 1, t.created_at"
echo "=== 7) 计数复核（应与 apply 前一致）==="
$PSQL -c "select (select count(*) from biz_flow_task) tasks, (select count(*) from biz_flow_instance) inst"

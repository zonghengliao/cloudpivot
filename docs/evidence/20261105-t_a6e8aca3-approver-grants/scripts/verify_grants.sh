#!/bin/bash
set -uo pipefail
export PGPASSWORD="${ALUN_DATABASE_PASSWORD:-alun}"   # 本机 dev 库口令，允许用环境变量覆盖
DB=${1:-cloudpivot_pg239_fresh}
q() { psql -h 127.0.0.1 -U alun -d "$DB" -At -F'|' -c "$1"; }
echo "== roles: $(q "select count(*) from sys_role where deleted_at is null")"
q "select role_code from sys_role where deleted_at is null order by 1" | tr '\n' ' '; echo
echo "== schema_migrations V0239:"; q "select kind, name, version from schema_migrations where name like '%0239%'"
echo "== approve grants:"
q "select r.role_code from sys_role_menu rm join sys_role r on r.id=rm.role_id where rm.menu_id='20000000-0000-0000-0000-000000000803' order by 1"
echo "== approve count: $(q "select count(*) from sys_role_menu where menu_id='20000000-0000-0000-0000-000000000803'")"
echo "== instance:read count: $(q "select count(*) from sys_role_menu where menu_id='20000000-0000-0000-0000-000000000801'")"
echo "== 860/861/862 counts: $(q "select count(*) from sys_role_menu where menu_id in ('20000000-0000-0000-0000-000000000860','20000000-0000-0000-0000-000000000861','20000000-0000-0000-0000-000000000862')")"
echo "== admin(0804) grants:"; q "select r.role_code from sys_role_menu rm join sys_role r on r.id=rm.role_id where rm.menu_id='20000000-0000-0000-0000-000000000804'"
echo "== roles still lacking approve among flow assignees (expect 0):"
q "select count(*) from (select distinct a.ref_id role_id from biz_flow_node_assignee a join biz_flow_node n on n.id=a.node_id join biz_flow_def_version v on v.id=n.version_id join biz_flow_def d on d.id=v.def_id where a.assignee_type='ROLE' and n.node_type in ('APPROVAL','COUNTERSIGN') and v.is_current and v.version_status='PUBLISHED' and d.deleted_at is null and d.status='PUBLISHED') x join sys_role r on r.id=x.role_id and r.deleted_at is null where not exists (select 1 from sys_role_menu rm where rm.role_id=r.id and rm.menu_id='20000000-0000-0000-0000-000000000803')"
echo "== dept count: $(q "select count(*) from sys_dept")  user count: $(q "select count(*) from sys_user where deleted_at is null")"

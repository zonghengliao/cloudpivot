#!/bin/bash
set -uo pipefail
export PGPASSWORD="${ALUN_DATABASE_PASSWORD:-alun}"   # 本机 dev 库口令，允许用环境变量覆盖
DB=cloudpivot_manual2
q() { psql -h 127.0.0.1 -U alun -d "$DB" -At -F'|' -c "$1"; }
echo "== 最近应用的 3 条迁移："
q "select version, name, kind from schema_migrations order by applied_seq desc limit 4"
echo "== approve 授权（应为 7 个审批岗位）："
q "select r.role_code from sys_role_menu rm join sys_role r on r.id=rm.role_id where rm.menu_id='20000000-0000-0000-0000-000000000803' order by 1" | tr '\n' ' '; echo
echo "== instance:read 授权行数：$(q "select count(*) from sys_role_menu where menu_id='20000000-0000-0000-0000-000000000801'")"
echo "== 860/861/862 授权行数：$(q "select count(*) from sys_role_menu where menu_id in ('20000000-0000-0000-0000-000000000860','20000000-0000-0000-0000-000000000861','20000000-0000-0000-0000-000000000862')")"
echo "== admin(0804)：$(q "select r.role_code from sys_role_menu rm join sys_role r on r.id=rm.role_id where rm.menu_id='20000000-0000-0000-0000-000000000804'" | tr '\n' ' ')"
echo "== 仍缺 approve 的审批角色（应为空）："
q "select distinct r.role_code from biz_flow_node_assignee a join biz_flow_node n on n.id=a.node_id join biz_flow_def_version v on v.id=n.version_id join biz_flow_def d on d.id=v.def_id join sys_role r on r.id=a.ref_id and r.deleted_at is null where a.assignee_type='ROLE' and n.node_type in ('APPROVAL','COUNTERSIGN') and v.is_current and v.version_status='PUBLISHED' and d.deleted_at is null and d.status='PUBLISHED' and not exists (select 1 from sys_role_menu rm where rm.role_id=r.id and rm.menu_id='20000000-0000-0000-0000-000000000803')"
echo "== sys_dept：$(q "select count(*) from sys_dept") 行 -> $(q "select string_agg(dept_code||':'||dept_name, ',') from sys_dept")"

\pset pager off
\echo == budget_approve versions
select d.flow_code, v.version_no, v.version_status, v.is_current, v.graph_hash, v.id
  from biz_flow_def d join biz_flow_def_version v on v.def_id=d.id
 where d.flow_code='budget_approve' order by v.version_no;

\echo == current version nodes
select n.node_key, n.node_type, n.assignee_type, n.approval_mode, n.empty_assignee, n.self_approval,
       coalesce(string_agg(coalesce(r.role_code,'?')||'/'||a.assignee_type, ',' order by a.sort_order),'-') as assignees
  from biz_flow_node n
  left join biz_flow_node_assignee a on a.node_id=n.id
  left join sys_role r on r.id=a.ref_id
 where n.version_id=(select v.id from biz_flow_def d join biz_flow_def_version v on v.def_id=d.id
                     where d.flow_code='budget_approve' and v.is_current)
 group by 1,2,3,4,5,6 order by 1;

\echo == max migration version applied
select max(version) from schema_migrations;

\echo == bad password users
select username, role_codes from sys_user u left join lateral (
  select string_agg(r.role_code,',') as role_codes from sys_user_role ur join sys_role r on r.id=ur.role_id where ur.user_id=u.id) x on true
 where u.deleted_at is null and u.password not like '$argon2id$%';

\echo == roles holding attachment:upload
select r.role_code from sys_role r join sys_role_menu rm on rm.role_id=r.id
  join sys_menu m on m.id=rm.menu_id
 where m.auth_code='business:attachment:upload' and r.deleted_at is null order by 1;

\echo == EMPLOYEE-ish roles
select code_or_role, cnt from (
 select 'role:'||role_code as code_or_role, 1 as cnt from sys_role where deleted_at is null and role_code in ('EMPLOYEE','DRILL_CLERK')) z;

\echo == attachment rule for EXPENSE_REPORT
select doc_type, required_at, min_count, file_roles from biz_attachment_rule where doc_type='EXPENSE_REPORT';

\echo == dept leaders sample
select count(*) from sys_dept where deleted_at is null and leader_id is not null;

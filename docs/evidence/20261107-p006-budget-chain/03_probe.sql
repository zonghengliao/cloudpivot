\pset pager off
\echo === 审批角色成员（部门主管 / 财务经理 / 总经理 / 财务总监）===
select r.role_code, r.role_name, u.username, u.real_name, u.status
  from sys_role r
  left join sys_user_role ur on ur.role_id = r.id
  left join sys_user u on u.id = ur.user_id
 where r.role_code in ('DEPT_LEADER','FINANCE_MANAGER','GENERAL_MANAGER','FINANCE_DIRECTOR','SUPER_ADMIN')
 order by r.role_code, u.username;
\echo === 这些用户的密码标志位 ===
select username, must_change_password, status from sys_user where username in ('admin','demo_lead','lead_drill','fin_drill','emp_drill','gm_drill');

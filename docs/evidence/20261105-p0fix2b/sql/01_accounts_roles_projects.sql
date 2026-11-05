\pset pager off
\echo == A. 账号全景（含最近登录成功时间） ==
select u.username, u.is_locked, coalesce(d.dept_code,'-') dept_code, coalesce(d.dept_name,'(无部门)') dept_name,
       u.created_at::date, (select max(l.created_at)::date from auth_logs l where l.user_id=u.id and l.auth_status='SUCCESS') last_login_ok,
       (select count(*) from auth_logs l where l.user_id=u.id and l.auth_status='SUCCESS') login_cnt
from sys_user u left join sys_dept d on d.id=u.dept_id
where u.deleted_at is null order by u.username;

\echo == B. 账号-角色-角色范围 ==
select u.username, r.role_code, r.role_name, r.data_scope, r.status role_status, ur.created_at::date granted_on, ur.expire_time
from sys_user u
join sys_user_role ur on ur.user_id=u.id
join sys_role r on r.id=ur.role_id and r.deleted_at is null
where u.deleted_at is null order by u.username, r.role_code;

\echo == E. biz_project 行详情 ==
select p.project_no, p.project_name, coalesce(d.dept_code,'-') dept_code, coalesce(ou.username,'-') owner,
       coalesce(cu.username,'-') creator, p.created_at::date, (p.deleted_at is not null) deleted
from biz_project p
left join sys_dept d on d.id=p.dept_id
left join sys_user ou on ou.id=p.owner_id
left join sys_user cu on cu.id=p.create_by
order by p.created_at;

\echo == E2. biz_project_member 行 ==
select coalesce(u.username,'-') member, m.role, p.project_no from biz_project_member m
left join sys_user u on u.id=m.user_id left join biz_project p on p.id=m.project_id order by 1;

\echo == D4. 存活任务（deleted_at is null）按负责人 + 参与人 ==
select coalesce(u.username,'?') owner, count(*) tasks,
       count(*) filter (where t.dept_id is null) dept_null
from biz_task t left join sys_user u on u.id=t.owner_id
where t.deleted_at is null group by 1 order by 2 desc;

\echo == D5. 存活任务的状态分布 ==
select status, count(*) from biz_task where deleted_at is null group by 1 order by 2 desc;

\echo == D6. biz_task_member 关系（存活任务） ==
select coalesce(u.username,'?') member, m.role, count(*) from biz_task_member m
join biz_task t on t.id=m.task_id and t.deleted_at is null
left join sys_user u on u.id=m.user_id group by 1,2 order by 3 desc;

\echo == G. 角色总表 ==
select role_code, role_name, data_scope, status from sys_role where deleted_at is null order by data_scope, role_code;

\echo == G2. 每个角色的账号数 ==
select r.role_code, r.data_scope, count(ur.id) users from sys_role r
left join sys_user_role ur on ur.role_id=r.id
left join sys_user u on u.id=ur.user_id and u.deleted_at is null
where r.deleted_at is null group by 1,2 order by 3 desc, 1;

\echo == H. 30 天登录成功活跃度 ==
select coalesce(u.username,'(已删用户)') username, coalesce(d.dept_name,'(无部门)') dept, count(*) cnt, max(l.created_at)::date last_day
from auth_logs l left join sys_user u on u.id=l.user_id left join sys_dept d on d.id=u.dept_id
where l.auth_status='SUCCESS' and l.created_at > now() - interval '30 days'
group by 1,2 order by 3 desc;

\echo == J. 其余用户总数（含软删）/ 是否有其它用户 ==
select (select count(*) from sys_user) all_users, (select count(*) from sys_user where deleted_at is null) live_users,
       (select count(*) from sys_user where deleted_at is not null) deleted_users;

\echo == K. sys_dept.leader_id 是否已设 ==
select dept_code, dept_name, leader_id from sys_dept where deleted_at is null order by dept_code;

\echo == L. biz_expense_report / biz_budget / biz_contract 的 dept 填充（决策影响面） ==
select 'biz_contract' t, count(*) cnt, count(dept_id) notnull from biz_contract;

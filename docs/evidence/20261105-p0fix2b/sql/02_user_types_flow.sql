\pset pager off
\echo == A. 账号 user_type / real_name / 建档人 / 备注 ==
select u.username, u.real_name, u.user_type, u.status, u.is_locked,
       coalesce(cu.username,'-') created_by, u.remark,
       (select count(*) from sys_ai_agent a where a.user_id=u.id) ai_agent_rows
from sys_user u left join sys_user cu on cu.id=u.create_by
where u.deleted_at is null order by u.username;

\echo == B. 流程节点 assignee_type 分布（已发布版本） ==
select table_name from information_schema.tables where table_schema='public' and table_name like 'biz_flow%' order by 1;

\echo == C. 是否有 biz_flow_node 表 ==
select column_name,data_type from information_schema.columns where table_name='biz_flow_node' order by ordinal_position;

\echo == D. dept_path 现状 ==
select dept_code, dept_name, dept_path, parent_id is null as is_root from sys_dept where deleted_at is null order by dept_code;

\echo == E. 存量审批实例状态分布 ==
select status, count(*) from biz_flow_instance group by 1 order by 2 desc;

\echo == F. 待办任务（未完成）归属人 ==
select coalesce(u.username,'(按角色/ADMIN解析)') assignee, t.status, count(*)
from biz_flow_task t left join sys_user u on u.id=t.assignee_id
group by 1,2 order by 3 desc;

-- 2 张 PENDING 实例的待办归属（谁在卡单）
\pset pager off
\echo == 2 张 PENDING 实例 ==
select i.instance_no, i.biz_type, i.status, i.created_at::date,
       (select string_agg(coalesce(u.username,'?')||':'||t.status, ' ') from biz_flow_task t left join sys_user u on u.id=t.assignee_id where t.instance_id=i.id) tasks
from biz_flow_instance i where i.status='PENDING';

\echo == 全库审批实例状态分布 ==
select status, count(*) from biz_flow_instance group by 1 order by 2 desc;

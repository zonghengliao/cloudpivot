-- 已发布流程版本的节点 assignee_type 分布 + ADMIN 类节点逐条（收敛超管的风险面）
\pset pager off
\echo == B2. 已发布流程节点的 assignee_type 分布 ==
select n.assignee_type, n.node_type, n.approval_mode, count(*) cnt,
       count(*) filter (where n.empty_assignee is null or n.empty_assignee='') empty_null
from biz_flow_node n
join biz_flow_def_version v on v.id=n.version_id and v.is_current=true
join biz_flow_def f on f.id=v.def_id and f.deleted_at is null
group by 1,2,3 order by 4 desc;

\echo == B3. ADMIN 类 / 指定人 / 空审批人兜底的节点明细 ==
select f.flow_code, n.node_key, n.node_name, n.assignee_type, coalesce(n.approval_mode,'(空->ALL)') mode,
       coalesce(n.empty_assignee,'(空)') empty_assignee, n.allow_robot
from biz_flow_node n
join biz_flow_def_version v on v.id=n.version_id and v.is_current=true
join biz_flow_def f on f.id=v.def_id and f.deleted_at is null
where n.assignee_type in ('ADMIN','ROLE','USER') or coalesce(n.empty_assignee,'') <> ''
order by f.flow_code, n.sort_order;

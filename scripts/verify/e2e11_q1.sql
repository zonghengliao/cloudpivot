select d.flow_code, n.node_key, n.node_type, coalesce(n.assignee_type, '-') as atype,
       (select string_agg(a.ref_id::text, ',') from biz_flow_node_assignee a where a.node_id = n.id) as assignees
  from biz_flow_def d
  join biz_flow_def_version v on v.id = d.current_version_id
  join biz_flow_node n on n.version_id = v.id
 where d.biz_type = 'TASK_ACCEPT' and d.deleted_at is null
 order by d.flow_code, n.sort_order;

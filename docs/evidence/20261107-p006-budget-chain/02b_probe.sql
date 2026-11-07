\pset pager off
\echo === 费用/预算审批流的节点（看有没有「超预算」分支节点）===
select d.flow_code, v.version_no, v.is_current, n.node_key, n.node_name, n.node_type, n.assignee_type, v.node_count, v.edge_count
  from biz_flow_def d
  join biz_flow_def_version v on v.def_id = d.id
  join biz_flow_node n on n.version_id = v.id
 where d.flow_code ilike '%expense%' or d.flow_code ilike '%budget%'
 order by d.flow_code, v.version_no, n.node_key;
\echo === 条件边（是否引用 over_budget）===
select d.flow_code, fn.node_key as from_key, tn.node_key as to_key, e.condition_expr::text, e.is_default
  from biz_flow_def d
  join biz_flow_def_version v on v.def_id = d.id
  join biz_flow_edge e on e.version_id = v.id
  join biz_flow_node fn on fn.id = e.from_node_id
  join biz_flow_node tn on tn.id = e.to_node_id
 where (d.flow_code ilike '%expense%' or d.flow_code ilike '%budget%')
 order by d.flow_code, fn.node_key, e.priority;

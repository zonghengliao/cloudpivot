\pset pager off
\pset tuples_only on
\pset format unaligned
select json_build_object(
  'nodes', coalesce((select json_agg(json_build_object(
      'key', n.node_key, 'name', n.node_name, 'node_type', n.node_type,
      'assignee_type', n.assignee_type, 'approval_mode', n.approval_mode,
      'approve_ratio', n.approve_ratio, 'reject_veto', n.reject_veto,
      'join_mode', n.join_mode, 'fork_mode', n.fork_mode, 'form_var_key', n.form_var_key,
      'superior_levels', n.superior_levels, 'empty_assignee', n.empty_assignee,
      'self_approval', n.self_approval, 'allow_robot', n.allow_robot,
      'fallback_user_id', n.fallback_user_id, 'fallback_role_id', n.fallback_role_id,
      'timeout_hours', n.timeout_hours, 'timeout_action', n.timeout_action,
      'remind_before_hours', n.remind_before_hours, 'max_remind_times', n.max_remind_times,
      'cc_on_approved', n.cc_on_approved, 'x', n.position_x, 'y', n.position_y,
      'sort_order', n.sort_order, 'config', n.config,
      'assignees', coalesce((select json_agg(json_build_object(
            'assignee_type', a.assignee_type, 'ref_id', a.ref_id, 'sort_order', a.sort_order)
            order by a.assignee_type, a.ref_id)
          from biz_flow_node_assignee a where a.node_id = n.id), '[]'::json))
    order by n.node_key) from biz_flow_node n where n.version_id = '20000000-0000-4000-8000-0000000c022b'), '[]'::json),
  'edges', coalesce((select json_agg(json_build_object(
      'key', e.edge_key, 'from', f.node_key, 'to', t.node_key,
      'condition', e.condition_expr, 'label', e.condition_label,
      'priority', e.priority, 'is_default', e.is_default)
    order by f.node_key, t.node_key, e.edge_key)
    from biz_flow_edge e join biz_flow_node f on f.id=e.from_node_id join biz_flow_node t on t.id=e.to_node_id
   where e.version_id = '20000000-0000-4000-8000-0000000c022b'), '[]'::json),
  'snapshot', (select graph_snapshot from biz_flow_def_version where id='20000000-0000-4000-8000-0000000c022b'),
  'hash', (select btrim(graph_hash) from biz_flow_def_version where id='20000000-0000-4000-8000-0000000c022b')
);

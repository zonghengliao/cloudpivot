\pset pager off
\echo === biz_budget 列 ===
select column_name, data_type from information_schema.columns where table_name='biz_budget' order by ordinal_position;
\echo === 预算控制全局策略 ===
select config_key, config_value, description from sys_config where config_key like 'erp.budget%';
\echo === 费用审批流定义里的节点（看有没有「超预算」分支节点）===
select d.flow_code, d.flow_name, n.node_key, n.node_name, n.node_type
  from biz_flow_def d
  join biz_flow_def_version v on v.def_id = d.id and v.status = 'ACTIVE'
  join biz_flow_node n on n.version_id = v.id
 where d.flow_code ilike '%EXPENSE%' or d.flow_code ilike '%BUDGET%'
 order by d.flow_code, n.node_key;

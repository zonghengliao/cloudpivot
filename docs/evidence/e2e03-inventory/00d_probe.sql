SELECT table_name, string_agg(column_name, ', ' ORDER BY ordinal_position) AS cols
  FROM information_schema.columns
 WHERE table_name IN ('biz_flow_def','biz_flow_def_version','biz_flow_node','biz_flow_node_assignee')
 GROUP BY table_name;

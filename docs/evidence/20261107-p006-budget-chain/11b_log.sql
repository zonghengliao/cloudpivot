\pset pager off
select entity_type, count(*), max(created_at) from biz_status_log group by entity_type order by 2 desc;
\echo --- 最近 20 条 ---
select entity_type, entity_no, from_status, to_status, operator_name, created_at from biz_status_log order by created_at desc limit 20;

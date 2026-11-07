\pset pager off
\echo === sys_config 预算控制相关键 ===
select * from sys_config limit 0;
\echo --- 用 information_schema 看列名 ---
select column_name from information_schema.columns where table_name='sys_config' order by ordinal_position;
\echo === 现有预算 ===
select budget_no, fiscal_year, dept_name, account_code, control_mode, total_amount, status, effective_from, version_no
  from biz_budget order by created_at;
\echo === 预算占用台账 ===
select source_no, period_code, amount, status, over_budget, budget_id is not null as has_budget
  from biz_budget_usage order by created_at;
\echo === 5602 开头的末级科目 ===
select id, account_code, account_name, is_leaf, status from biz_account
 where account_code like '5602%' order by account_code;
\echo === 演练相关部门 ===
select id, dept_code, dept_name, leader_id from sys_dept where dept_code like 'DRILL%' or dept_name like '%演练%';
\echo === 演练相关用户 ===
select id, username, real_name, dept_id from sys_user where username like '%drill%' or username in ('admin','demo_lead','gm_drill');
\echo === 费用类型 ===
select id, type_code, type_name, account_id, budget_controlled, amount_limit from biz_expense_type order by type_code;

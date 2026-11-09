-- E2E05-FIX-3 数据库侧复核（145 · cloudpivot_dev · RUN_TAG=evid1）
\pset pager off
\echo '=== ① 状态日志：动作码（建卡是状态流转 action=NULL；转移/使用人变更是业务动作） ==='
select l.created_at::date as d, l.from_status, l.to_status, coalesce(l.action,'NULL') as action, l.reason
  from biz_status_log l
 where l.entity_type='FIXED_ASSET'
   and l.entity_id=(select id from biz_fixed_asset where asset_name like 'E2E05FIX3-evid1-A%' order by created_at desc limit 1)
 order by l.created_at, l.id;

\echo '=== ① 变动时间线（API GET /asset/{id}.changes 的数据源）：from=to，change_label 由 asset_dto 按 change_type 给出 ==='
select c.change_type, c.from_status, c.to_status, c.from_dept_name, c.to_dept_name, c.from_user_name, c.to_user_name, c.change_date
  from biz_fixed_asset_change c
 where c.asset_id=(select id from biz_fixed_asset where asset_name like 'E2E05FIX3-evid1-A%' order by created_at desc limit 1)
 order by c.created_at;

\echo '=== ② PUT 被拒后原值/分类一分没动 ==='
select asset_no, original_value, category_id, asset_name
  from biz_fixed_asset
 where asset_name like 'E2E05FIX3-evid1-A%' and deleted_at is null;

\echo '=== ③ 作废处置写出的 ENABLE 留痕行：change_date = 原处置日 2030-01-15，第二条显式 2030-02-03 ==='
select c.change_type, c.change_date, c.from_status, c.to_status, c.reason
  from biz_fixed_asset_change c
 where c.asset_id=(select id from biz_fixed_asset where asset_name like 'E2E05FIX3-evid1-B%' order by created_at desc limit 1)
   and c.change_type in ('DISPOSAL','ENABLE')
 order by c.created_at;

\echo '=== ③ 对照：卡片上的 disposal_date（作废后已清空），与「今天」 ==='
select asset_no, status, current_date as today,
       coalesce(disposal_date::text,'NULL') as disposal_date
  from biz_fixed_asset where asset_name like 'E2E05FIX3-evid1-B%' and deleted_at is null;

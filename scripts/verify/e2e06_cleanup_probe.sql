-- 清理本卡探针遗留：admin 在 DRILL_BDG 建的两张 FY2029 探针预算（SUBMITTED，占着别人的待办）
\set ON_ERROR_STOP on
begin;
create temp table _p on commit drop as
  select id from biz_budget
   where fiscal_year = 2029 and remark like 'E2E-06 探针%' and deleted_at is null;
create temp table _i on commit drop as
  select id from biz_flow_instance where biz_type='BUDGET_APPROVE' and biz_id in (select id from _p);
delete from biz_flow_cc where instance_id in (select id from _i);
delete from biz_flow_task where instance_id in (select id from _i);
delete from biz_flow_instance_node where instance_id in (select id from _i);
delete from biz_flow_instance where id in (select id from _i);
delete from biz_budget_period where budget_id in (select id from _p);
delete from biz_budget where id in (select id from _p);
commit;
select 'CLEAN_OK' , (select count(*) from biz_budget where fiscal_year=2029 and remark like 'E2E-06 探针%');

-- E2E-06 预算/报销演练数据清理（只删本卡自己造的数据：部门 E2E06-BDG 的预算 + e2e06_clerk 的报销单）
-- 禁止 DROP/TRUNCATE；不碰其他人的演练数据。可反复执行。
\set ON_ERROR_STOP on
begin;

create temp table _dept on commit drop as
  select id from sys_dept where dept_code = 'E2E06-BDG' and deleted_at is null;
create temp table _bg on commit drop as
  select id from biz_budget where dept_id in (select id from _dept);
create temp table _exp on commit drop as
  select id from biz_expense_report
   where dept_id in (select id from _dept)
      or reporter_id = (select id from sys_user where username = 'e2e06_clerk')
      or title like 'E2E06%';
create temp table _vou on commit drop as
  select id from biz_voucher
   where (source_type in ('EXPENSE_REPORT', 'EXPENSE_PAYMENT') and source_id in (select id from _exp));
create temp table _inst on commit drop as
  select id from biz_flow_instance
   where (biz_type = 'BUDGET_APPROVE' and biz_id in (select id from _bg))
      or (biz_type = 'EXPENSE' and biz_id in (select id from _exp));

delete from biz_flow_cc where instance_id in (select id from _inst);
delete from biz_task where flow_instance_id in (select id from _inst);
delete from biz_flow_task where instance_id in (select id from _inst);
delete from biz_flow_instance_node where instance_id in (select id from _inst);
delete from biz_flow_instance where id in (select id from _inst);

delete from biz_voucher_entry where voucher_id in (select id from _vou);
delete from biz_voucher where id in (select id from _vou);

delete from biz_budget_usage where source_id in (select id from _exp) or budget_id in (select id from _bg);
delete from biz_budget_period where budget_id in (select id from _bg);
delete from biz_budget where id in (select id from _bg);

delete from biz_expense_payment where report_id in (select id from _exp);
delete from biz_expense_item where report_id in (select id from _exp);
delete from biz_expense_report where id in (select id from _exp);

delete from biz_status_log
 where (entity_id in (select id from _exp) or entity_id in (select id from _bg))
   and entity_type in ('EXPENSE_REPORT', 'EXPENSE', 'BUDGET');
delete from biz_doc_attachment where doc_type = 'EXPENSE_REPORT' and doc_id in (select id from _exp);
delete from biz_budget_subject_map where remark like 'E2E-06%' and deleted_at is null;
delete from biz_expense_type where type_code in ('E2E06_TRAVEL', 'E2E06_BAD') and deleted_at is null;

commit;
select 'RESET_OK' as result,
       (select count(*) from biz_budget where dept_id in (select id from sys_dept where dept_code='E2E06-BDG')) as budgets_left;

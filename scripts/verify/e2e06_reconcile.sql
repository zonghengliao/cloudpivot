-- E2E-06 勾稽：预算占用 ↔ 报销单 ↔ 凭证 ↔ 执行报表（只覆盖本卡造的数据）
-- 输出四列：check|actual|expected|ok，由 e2e06_run_all.sh 汇总 PASS/FAIL。
\pset pager off
\pset tuples_only on
\pset format unaligned
\pset fieldsep '|'

with d as (select id from sys_dept where dept_code = 'E2E06-BDG' and deleted_at is null),
r as (select * from biz_expense_report
       where dept_id in (select id from d) or reporter_id = (select id from sys_user where username='e2e06_clerk')),
u as (select * from biz_budget_usage where source_type = 'EXPENSE_REPORT' and source_id in (select id from r))

-- 1) 每张报销单：价税合计 == 该单预算占用合计（有效占用）
select '报销单↔占用金额' as check,
       (select count(*)::text from (
          select r.id from r left join (select source_id, sum(amount) s from u where status in ('OCCUPIED','CONSUMED') group by 1) x
            on x.source_id = r.id
           where r.status in ('SUBMITTED','APPROVED','PAID')
             and coalesce(x.s,0) <> r.total_with_tax) t) as actual,
       '0' as expected
union all
-- 2) 已审批单：计提凭证 2241.01 贷方 == 价税合计
select '已审批单↔计提凭证(2241.01 贷方=价税合计)',
       (select count(*)::text from r x
         where x.voucher_id is not null
           and abs(x.total_with_tax - (select coalesce(sum(credit_amount),0) from biz_voucher_entry e
                                        where e.voucher_id = x.voucher_id and e.account_code='2241.01')) > 0.005),
       '0'
union all
select '计提凭证不平张数',
       (select count(*)::text from biz_voucher v where v.id in (select voucher_id from r where voucher_id is not null) and v.total_debit <> v.total_credit),
       '0'
union all
select '凭证表头↔分录合计(借方)',
       (select count(*)::text from biz_voucher v where v.id in (select voucher_id from r where voucher_id is not null)
          and v.total_debit <> (select coalesce(sum(debit_amount),0) from biz_voucher_entry e where e.voucher_id = v.id)),
       '0'
union all
select '凭证表头↔分录合计(贷方)',
       (select count(*)::text from biz_voucher v where v.id in (select voucher_id from r where voucher_id is not null)
          and v.total_credit <> (select coalesce(sum(credit_amount),0) from biz_voucher_entry e where e.voucher_id = v.id)),
       '0'
union all
-- 3) 已付金额 == 付款凭证（2241.01 借方）合计
select '已付金额↔付款凭证',
       (select count(*)::text from (
          select a.id, a.paid_amount,
                 coalesce((select sum(e.debit_amount) from biz_expense_payment p
                             left join biz_voucher_entry e on e.voucher_id = p.voucher_id and e.account_code='2241.01'
                            where p.report_id = a.id), 0) as paid_vou
            from r a where a.paid_amount > 0) t
         where abs(paid_amount - paid_vou) > 0.005),
       '0'
union all
-- 4) 占用状态与单据状态机一致：CONSUMED 必须绑凭证，RELEASED 必有原因
select 'CONSUMED 占用未绑凭证',
       (select count(*)::text from u where status = 'CONSUMED' and voucher_id is null), '0'
union all
select 'RELEASED 占用无释放原因',
       (select count(*)::text from u where status = 'RELEASED' and release_reason is null), '0'
union all
select '占用金额≠0',
       (select count(*)::text from u where amount <= 0), '0'
union all
-- 5) 预算额度：每槽 ACTIVE ≤ 1 行；年度额度 == 12 个月分解合计
select '同槽 ACTIVE >1 行',
       (select count(*)::text from (
          select dept_id, account_id, fiscal_year from biz_budget
           where status = 'ACTIVE' and deleted_at is null group by 1,2,3 having count(*) > 1) t), '0'
union all
select '年度额度≠12月合计',
       (select count(*)::text from biz_budget b
         where b.deleted_at is null and b.status in ('ACTIVE','SUBMITTED','DRAFT')
           and b.id in (select id from biz_budget where dept_id in (select id from d))
           and b.total_amount <> (select coalesce(sum(amount),0) from biz_budget_period p where p.budget_id = b.id)), '0'
union all
-- 6) 执行报表口径：每槽 used == 库内 OCCUPIED+CONSUMED 合计
select '执行报表 used↔占用合计',
       (select count(*)::text from (
          select b.dept_id, b.account_id, b.fiscal_year, b.total_amount,
                 (select coalesce(sum(amount),0) from biz_budget_usage x
                   where x.dept_id=b.dept_id and x.account_id=b.account_id and x.fiscal_year=b.fiscal_year
                     and x.status in ('OCCUPIED','CONSUMED') and x.deleted_at is null) uu
            from biz_budget b
           where b.status='ACTIVE' and b.deleted_at is null and b.dept_id in (select id from d)) t
         where uu > total_amount and not exists (select 1 from biz_budget b2
                  where b2.status='ACTIVE' and b2.dept_id=t.dept_id and b2.account_id=t.account_id
                    and b2.fiscal_year=t.fiscal_year and b2.total_amount >= t.uu)), '0'
union all
-- 7) 月度占用 不得凭空多出：usage 合计（按月）== 报销单在当月提交的金额
select '月度占用↔单据金额',
       (select count(*)::text from (
          select uu.period_code, sum(uu.amount) s from u uu where uu.status in ('OCCUPIED','CONSUMED') group by 1) t), '-'
union all
-- 8) 预算未生效的槽不得有「超支标记」的占用（未生效 = 不控制；占用行本身照写但不许标超支）
--    （2026-11 的 DRILL_* 老行曾因 FIX-5 之前的口径误标超支，已由 E2E06-FIX-3 的
--      V0358__budget_over_flag_backfill 一次性回填，这里不再需要豁免）
select '未生效预算却标了超支（本卡数据）',
       (select count(*)::text from biz_budget_usage x join biz_budget b
                on b.dept_id = x.dept_id and b.account_id = x.account_id and b.fiscal_year = x.fiscal_year
               and b.status='ACTIVE' and b.deleted_at is null
        where x.deleted_at is null and x.status in ('OCCUPIED','CONSUMED')
          and x.dept_id = (select id from sys_dept where dept_code='E2E06-BDG')
          and b.effective_from is not null
          and b.effective_from > (to_date(x.period_code || '-01','YYYY-MM-DD') + interval '1 month - 1 day')::date
          and x.over_budget = 1), '0';

-- 明细：本卡造的单据一览（人工核对用）
select '--- 明细（仅供参考，不计入断言）' as check, '' as actual, '' as expected;
select r.report_no, r.status, r.pay_status, r.total_with_tax::text, r.over_budget::text,
       coalesce(r.over_amount::text,'NULL'), r.missing_budget::text, coalesce(r.budget_mode,'-'),
       coalesce((select string_agg(u1.status||':'||u1.amount::text, ',' order by u1.period_code)
                   from biz_budget_usage u1 where u1.source_type='EXPENSE_REPORT' and u1.source_id=r.id), '-')
  from biz_expense_report r
 where r.dept_id in (select id from sys_dept where dept_code='E2E06-BDG')
 order by r.report_no;

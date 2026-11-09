-- E2E-05 资产域勾稽（只统计本卡自建数据：资产名 E2E05%）
\pset pager off
\echo == 0. 本卡资产台账 ==
select asset_no, status, original_value, residual_value, depreciable_base,
       accumulated_depreciation, net_value, depreciation_months_done, last_depreciation_period
  from biz_fixed_asset where asset_name like 'E2E05%' order by asset_no;

\echo == 1. 月折旧公式：(原值-残值)/月数 ==
select asset_no, useful_life_months,
       round((original_value - residual_value) / useful_life_months, 4) as by_formula,
       monthly_depreciation,
       (monthly_depreciation = round((original_value - residual_value) / useful_life_months, 4)) as ok
  from biz_fixed_asset where asset_name like 'E2E05%' order by asset_no;

\echo == 2. 不能超提 + 缓存 = 明细合计 ==
select a.asset_no, a.accumulated_depreciation, coalesce(sum(d.amount) filter (where d.status='POSTED'),0) as detail,
       a.depreciable_base,
       (a.accumulated_depreciation <= a.depreciable_base) as not_over
  from biz_fixed_asset a left join biz_depreciation d on d.asset_id=a.id
 where a.asset_name like 'E2E05%'
 group by a.asset_no, a.accumulated_depreciation, a.depreciable_base order by a.asset_no;

\echo == 3. 逐月折旧序列（含尾差/末月标记）==
select asset_no, period_code, amount, accumulated_after, net_value_after, is_last_month, status
  from biz_depreciation
 where asset_id in (select id from biz_fixed_asset where asset_name like 'E2E05%')
 order by asset_no, period_code;

\echo == 4. 折旧批次 vs 凭证（金额/平衡/明细合计）==
select b.period_code, b.status, b.asset_count, b.total_amount, v.voucher_no,
       v.total_debit, v.total_credit, (v.total_debit = v.total_credit) as balanced,
       (select coalesce(sum(amount),0) from biz_depreciation d where d.batch_id=b.id and d.status='POSTED') as detail_sum
  from biz_depreciation_batch b left join biz_voucher v on v.id=b.voucher_id
 where b.period_code between '2027-12' and '2028-06' order by b.period_code;

\echo == 5. 折旧凭证科目（借 5602.01 按部门 / 贷 1602）==
select b.period_code, e.aux_dept_id, e.account_code, sum(e.debit_amount) as deb, sum(e.credit_amount) as cred
  from biz_depreciation_batch b
  join biz_voucher_entry e on e.voucher_id=b.voucher_id
 where b.period_code between '2027-12' and '2028-06'
 group by 1,2,3 order by 1,3;

\echo == 6. 本卡处置凭证（逐行）==
select v.voucher_no, v.source_no, v.is_red, e.line_no, e.account_code, e.debit_amount, e.credit_amount,
       e.summary, (v.total_debit=v.total_credit) as balanced
  from biz_voucher v join biz_voucher_entry e on e.voucher_id=v.id
 where v.source_type='FIXED_ASSET_DISPOSAL'
   and v.source_no in (select asset_no from biz_fixed_asset where asset_name like 'E2E05%')
 order by v.source_no, v.voucher_no, e.line_no;

\echo == 7. 1601 净额 = 在册（未处置/未删除）卡片原值 ==
with mine as (select id, asset_no, original_value, status, deleted_at from biz_fixed_asset where asset_name like 'E2E05%')
select
  coalesce((select sum(e.debit_amount - e.credit_amount) from biz_voucher v join biz_voucher_entry e on e.voucher_id=v.id
             where v.source_type in ('FIXED_ASSET_ACQ','FIXED_ASSET_DISPOSAL') and v.source_no in (select asset_no from mine)
               and e.account_code='1601'), 0) as gl_1601,
  coalesce((select sum(original_value) from mine where deleted_at is null and status<>'DISPOSED'), 0) as card_1601;

\echo == 8. 1602 净额 = 在册卡片累计折旧 ==
with mine as (select id, asset_no, accumulated_depreciation, status, deleted_at from biz_fixed_asset where asset_name like 'E2E05%')
select
  coalesce((select sum(e.debit_amount - e.credit_amount) from biz_voucher v join biz_voucher_entry e on e.voucher_id=v.id
             where v.source_type in ('DEPRECIATION','FIXED_ASSET_DISPOSAL')
               and (v.source_no in (select asset_no from mine)
                    or v.source_id in (select id from biz_depreciation_batch where period_code between '2027-12' and '2028-06')
                    or v.id in (select reverse_voucher_id from biz_depreciation_batch where period_code between '2027-12' and '2028-06' and reverse_voucher_id is not null))
               and e.account_code='1602'), 0) as gl_1602,
  coalesce((select sum(accumulated_depreciation) from mine where deleted_at is null and status<>'DISPOSED'), 0) as card_1602;

\echo == 9. 1606 清理科目：本卡处置后净额 ==
select coalesce(sum(e.debit_amount - e.credit_amount),0) as net_1606
  from biz_voucher v join biz_voucher_entry e on e.voucher_id=v.id
 where v.source_type='FIXED_ASSET_DISPOSAL' and e.account_code='1606'
   and v.source_no in (select asset_no from biz_fixed_asset where asset_name like 'E2E05%');

\echo == 10. 处置损益（5301 / 5711）==
select e.account_code,
       sum(e.credit_amount - e.debit_amount) as net_credit
  from biz_voucher v join biz_voucher_entry e on e.voucher_id=v.id
 where v.source_type='FIXED_ASSET_DISPOSAL' and e.account_code in ('5301','5711')
   and v.source_no in (select asset_no from biz_fixed_asset where asset_name like 'E2E05%')
 group by 1 order by 1;

\echo == 11. 全库凭证平衡 / 凭证 total = 分录合计 ==
select count(*) as unbalanced from biz_voucher where deleted_at is null and total_debit <> total_credit;
select count(*) as total_mismatch from (
  select v.id from biz_voucher v join biz_voucher_entry e on e.voucher_id=v.id
   where v.deleted_at is null group by v.id, v.total_debit, v.total_credit
  having v.total_debit <> sum(e.debit_amount) or v.total_credit <> sum(e.credit_amount)) t;

\echo == 12. 状态机留痕：本卡状态日志 + 变动留痕 ==
select entity_no, from_status, to_status, reason from biz_status_log
 where entity_no in (select asset_no from biz_fixed_asset where asset_name like 'E2E05%')
 order by created_at;
select a.asset_no, c.change_type, c.from_status, c.to_status, c.change_date
  from biz_fixed_asset_change c join biz_fixed_asset a on a.id=c.asset_id
 where a.asset_name like 'E2E05%' order by a.asset_no, c.created_at;

\echo == 13. FIXED_ASSET 联动通知（FIX-3 三规则）==
select biz_id, idempotency_key, title from sys_notice
 where biz_type='FIXED_ASSET' and biz_id::text in (select id::text from biz_fixed_asset where asset_name like 'E2E05%')
 order by created_at;

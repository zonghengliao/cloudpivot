-- 三本账 + E4 口径复核（不经业务代码，直接读库）
\echo '== ① 库存（成本账）=='
select m.material_code, w.warehouse_code, i.qty::text as qty, i.amount::text as amount, i.avg_cost::text as avg_cost
from biz_inventory i join biz_material m on m.id=i.material_id join biz_warehouse w on w.id=i.warehouse_id
where m.material_code='ST9-M-001';

\echo '== ② 库位账合计 =='
select coalesce(sum(l.qty),0)::text as loc_qty from biz_inventory_location l
join biz_material m on m.id=l.material_id where m.material_code='ST9-M-001';

\echo '== ③ 流水净额（全部 biz_type）=='
select coalesce(sum(case when txn_type='IN' then qty else -qty end),0)::text as txn_net,
       string_agg(distinct biz_type, ',') as biz_types
from biz_inventory_txn t join biz_material m on m.id=t.material_id where m.material_code='ST9-M-001';

\echo '== ④ 总账 1405 / 1901 余额（只认 POSTED 且非红冲在途）=='
select a.account_code, a.account_name,
       coalesce(sum(e.debit_amount),0)::text as debit, coalesce(sum(e.credit_amount),0)::text as credit,
       coalesce(sum(e.debit_amount-e.credit_amount),0)::text as balance
from biz_account a left join biz_voucher_entry e on e.account_id=a.id
left join biz_voucher v on v.id=e.voucher_id and v.deleted_at is null and v.status='POSTED'
where a.account_code in ('1405','1901','3103','1002','3001')
group by 1,2 order by 1;

\echo '== ⑤ 本库全部 inventory_txn 的 biz_type 分布 =='
select biz_type, txn_type, count(*) as rows, sum(qty)::text as qty from biz_inventory_txn group by 1,2 order by 1;

\echo '== ⑥ 盘点单 / 凭证一览 =='
select take_no, status, take_type, total_book_qty::text, total_actual_qty::text,
       total_gain_amount::text, total_loss_amount::text, voucher_id, reverse_voucher_id, approval_instance_id
from biz_stock_take order by take_no;
select voucher_no, voucher_type, status, period, total_debit::text, total_credit::text, is_red, source_type, reverse_of
from biz_voucher where deleted_at is null order by voucher_no;

\echo '==== ① 本轮验收产生的凭证分录（辅助核算四列） ===='
\echo '  source=voucher.source_type  line=行号  mat/wh/dept/sup = 四个辅助列（空 = NULL）'
select v.source_type as source, v.source_no, e.line_no, e.account_code,
       e.debit_amount, e.credit_amount,
       coalesce(e.aux_material_id::text, '-') as mat,
       coalesce(e.aux_warehouse_id::text, '-') as wh,
       coalesce(e.aux_dept_id::text, '-') as dept,
       coalesce(e.aux_supplier_id::text, '-') as sup
  from biz_voucher_entry e
  join biz_voucher v on v.id = e.voucher_id and v.deleted_at is null
 where v.source_id in (select id from biz_stock_in where warehouse_id = 'e21a0101-0000-4000-8000-000000000003'::uuid)
    or v.source_id in (select id from biz_stock_out where warehouse_id = 'e21a0101-0000-4000-8000-000000000003'::uuid)
    or v.source_type = 'PAYMENT'
 order by v.source_type, v.source_no, e.line_no;

\echo ''
\echo '==== ② 本库辅助核算覆盖率（凭证分录口径） ===='
select count(*) as entries, count(aux_material_id) as mat, count(aux_warehouse_id) as wh,
       count(aux_dept_id) as dept, count(aux_supplier_id) as sup
  from biz_voucher_entry;

\echo ''
\echo '==== ③ 那张「历史凭证」跑完全流程后仍然四列全空（存量不回填） ===='
select e.line_no, e.account_code, e.debit_amount, e.credit_amount,
       (e.aux_material_id is null) as mat_null,
       (e.aux_warehouse_id is null) as wh_null,
       (e.aux_dept_id is null) as dept_null,
       (e.aux_supplier_id is null) as sup_null
  from biz_voucher_entry e
 where e.voucher_id = 'e21a0101-0000-4000-8000-0000000000f1'::uuid
 order by e.line_no;

\echo ''
\echo '==== ④ 迁移 V0214：辅助账权限码 ===='
select id, auth_code, menu_type, title, parent_id
  from sys_menu where auth_code = 'business:erp:ledger:aux-balance';

\echo ''
\echo '==== ⑤ 付款凭证的部门来源（申请人 → sys_user.dept_id） ===='
select p.payment_no, p.create_by, u.username, u.dept_id, d.dept_name, v.voucher_no
  from biz_payment p
  left join sys_user u on u.id = p.create_by
  left join sys_dept d on d.id = u.dept_id
  left join biz_voucher v on v.source_id = p.id
 where p.created_at > now() - interval '1 hour'
 order by p.created_at desc limit 3;

\pset pager off
\pset format unaligned
\pset fieldsep '|'
select voucher_no, source_type, coalesce(source_no,'-'), status,
       total_debit::text, total_credit::text, entry_count, created_at
  from biz_voucher
 where voucher_no = 'P2C2-V-0001';
select count(*) as entries from biz_voucher_entry e
  join biz_voucher v on v.id = e.voucher_id
 where v.voucher_no = 'P2C2-V-0001';

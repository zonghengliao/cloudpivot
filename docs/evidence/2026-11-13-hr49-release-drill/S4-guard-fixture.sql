-- 反证夹具：一条凭证把 500 元直接记到父科目 2211（老库里它是末级，记账闸允许）
INSERT INTO biz_voucher (id, voucher_word, voucher_no, period, voucher_date, voucher_type, source_type,
                         summary, total_debit, total_credit, entry_count, status)
VALUES ('22222222-2222-4222-8222-222222222222','记','HR49-0002', to_char(current_date,'YYYY-MM'), current_date,
        'MANUAL','MANUAL','HR-49 护栏反证：父科目上有分录', 500, 500, 2, 'POSTED');
INSERT INTO biz_voucher_entry (voucher_id, line_no, summary, account_id, account_code, account_name, debit_amount, credit_amount)
SELECT '22222222-2222-4222-8222-222222222222'::uuid, 1, 'HR-49 护栏反证',
       a.id, a.account_code, a.account_name, 500, 0 FROM biz_account a WHERE a.account_code='2211' AND a.deleted_at IS NULL;
INSERT INTO biz_voucher_entry (voucher_id, line_no, summary, account_id, account_code, account_name, debit_amount, credit_amount)
SELECT '22222222-2222-4222-8222-222222222222'::uuid, 2, 'HR-49 护栏反证',
       a.id, a.account_code, a.account_name, 0, 500 FROM biz_account a WHERE a.account_code='1002' AND a.deleted_at IS NULL;

-- 历史凭证 1：借 1002 银行存款 1000 / 贷 5602.01 管理费用-工资 1000
INSERT INTO biz_voucher (id, voucher_word, voucher_no, period, voucher_date, voucher_type, source_type,
                         summary, total_debit, total_credit, entry_count, status)
VALUES ('11111111-1111-4111-8111-111111111111','记','HR49-0001', to_char(current_date,'YYYY-MM'), current_date,
        'MANUAL','MANUAL','HR-49 老库历史凭证', 1000, 1000, 2, 'POSTED');
INSERT INTO biz_voucher_entry (voucher_id, line_no, summary, account_id, account_code, account_name, debit_amount, credit_amount)
SELECT '11111111-1111-4111-8111-111111111111'::uuid, 1, 'HR-49 老库历史凭证',
       a.id, a.account_code, a.account_name, 1000, 0 FROM biz_account a WHERE a.account_code='1002' AND a.deleted_at IS NULL;
INSERT INTO biz_voucher_entry (voucher_id, line_no, summary, account_id, account_code, account_name, debit_amount, credit_amount)
SELECT '11111111-1111-4111-8111-111111111111'::uuid, 2, 'HR-49 老库历史凭证',
       a.id, a.account_code, a.account_name, 0, 1000 FROM biz_account a WHERE a.account_code='5602.01' AND a.deleted_at IS NULL;

-- 标记配置行（非凭证类数据也要一条不少）
INSERT INTO sys_config (config_key, config_value, config_type, description, config_level)
VALUES ('hr49.legacy.marker','before-upgrade','STRING','HR-49 演练：升级后必须原样还在',1);

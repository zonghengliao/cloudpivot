SELECT table_name FROM information_schema.tables WHERE table_schema='public' AND table_name LIKE 'biz_voucher%';
SELECT column_name FROM information_schema.columns WHERE table_name='biz_voucher_entry' ORDER BY ordinal_position;

SELECT column_name FROM information_schema.columns WHERE table_name='biz_accounting_period' ORDER BY ordinal_position;
SELECT * FROM biz_accounting_period WHERE period_code >= '2026-06' ORDER BY period_code;

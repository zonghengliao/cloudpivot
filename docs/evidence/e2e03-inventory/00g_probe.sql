SELECT table_name FROM information_schema.tables WHERE table_schema='public' AND table_name LIKE '%period%';
\echo '--- 2026 期间 ---'
SELECT * FROM biz_account_period WHERE period_code >= '2026-06' ORDER BY period_code;

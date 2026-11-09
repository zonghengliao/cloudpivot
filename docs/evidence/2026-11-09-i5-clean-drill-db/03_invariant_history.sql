\pset pager off
\echo '===== dev 最近 6 次巡检 ====='
SELECT run_at, trigger_type, status, total, passed, failed, blockers, warns, duration_ms
FROM sys_invariant_result ORDER BY run_at DESC LIMIT 6;

\echo '===== 最近一次 FAIL 批次的 failures（原始 JSON，含 B4/B1/S6 明细）====='
SELECT jsonb_pretty(failures) FROM sys_invariant_result
WHERE status='FAIL' ORDER BY run_at DESC LIMIT 1;

\echo '===== 现存出库单 ====='
SELECT out_no, status, source_type, out_date, created_at FROM biz_stock_out ORDER BY created_at;

\echo '===== 应收里 source_type 分布 ====='
SELECT source_type, count(*), min(created_at), max(created_at) FROM biz_receivable GROUP BY 1;

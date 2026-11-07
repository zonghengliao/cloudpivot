-- P0-04 终局核对（只读）
\echo '== ① 每台资产：累计折旧缓存 vs 明细合计（巡检 K2） =='
SELECT a.asset_no,
       a.accumulated_depreciation AS cache_accum,
       COALESCE(d.detail_sum, 0)  AS detail_sum,
       (a.accumulated_depreciation = COALESCE(d.detail_sum, 0)) AS k2_ok
  FROM biz_fixed_asset a
  LEFT JOIN (SELECT asset_id, sum(amount) AS detail_sum FROM biz_depreciation WHERE status = 'POSTED' GROUP BY asset_id) d
    ON d.asset_id = a.id
 ORDER BY a.asset_no;

\echo '== ② 每台资产：月度折旧序列（月份 / 本期 / 累计 / 净值） =='
SELECT asset_no, period_code, amount AS this_month, accumulated_after, net_value_after, is_last_month
  FROM biz_depreciation ORDER BY asset_no, period_code;

\echo '== ③ 折旧凭证逐期借贷平衡 + 与批次合计一致 =='
SELECT v.voucher_no, v.source_no AS period, b.total_amount AS batch_total,
       v.total_debit, v.total_credit,
       (v.total_debit = v.total_credit) AS balanced,
       (b.total_amount = v.total_debit) AS matches_batch
  FROM biz_voucher v
  JOIN biz_depreciation_batch b ON b.voucher_id = v.id
 WHERE v.source_type = 'DEPRECIATION'
 ORDER BY v.source_no;

\echo '== ④ 折旧凭证借方按科目汇总（5602.01）+ 贷方 1602 =='
SELECT a.account_code, a.account_name, sum(e.debit_amount) AS deb, sum(e.credit_amount) AS cred
  FROM biz_voucher_entry e
  JOIN biz_account a ON a.id = e.account_id
  JOIN biz_voucher v ON v.id = e.voucher_id
 WHERE v.source_type = 'DEPRECIATION'
 GROUP BY a.account_code, a.account_name ORDER BY a.account_code;

\echo '== ⑤ 处置当月（2026-06）折旧扫描口径：已处置卡被状态过滤排除（=漏提最后一个月） =='
SELECT (SELECT count(*) FROM biz_fixed_asset a
         WHERE a.deleted_at IS NULL AND a.status IN ('IN_USE','IDLE')
           AND a.start_depreciation_date <= DATE '2026-06-30'
           AND a.accumulated_depreciation < a.depreciable_base
           AND (a.disposal_date IS NULL OR a.disposal_date >= DATE '2026-06-01')
           AND COALESCE(a.last_depreciation_period,'') < '2026-06') AS scan_hits_as_coded,
       (SELECT count(*) FROM biz_fixed_asset a
         WHERE a.deleted_at IS NULL AND a.status IN ('IN_USE','IDLE','DISPOSED')
           AND a.start_depreciation_date <= DATE '2026-06-30'
           AND a.accumulated_depreciation < a.depreciable_base
           AND (a.disposal_date IS NULL OR a.disposal_date >= DATE '2026-06-01')
           AND COALESCE(a.last_depreciation_period,'') < '2026-06') AS scan_hits_if_disposed_kept;

\echo '== ⑥ 全账凭证借贷平衡（含本次演练全部凭证） =='
SELECT count(*) AS vouchers, sum((total_debit = total_credit)::int) AS balanced, sum((total_debit <> total_credit)::int) AS unbalanced
  FROM biz_voucher WHERE deleted_at IS NULL;

\echo '== ⑦ 资产卡不变量（净值恒等 / 不超提 / 已处置三件套） =='
SELECT count(*) AS assets,
       sum((net_value = original_value - accumulated_depreciation)::int) AS net_identity_ok,
       sum((accumulated_depreciation <= depreciable_base)::int) AS not_over_depreciated,
       sum(((status = 'DISPOSED') = (disposal_date IS NOT NULL AND disposal_type IS NOT NULL))::int) AS disposed_consistent
  FROM biz_fixed_asset;

\echo '== ⑧ 累计折旧 1602 / 管理费用 5602.01 / 营业外 5301 5711 期末余额 =='
SELECT a.account_code, a.account_name,
       sum(e.debit_amount) AS deb, sum(e.credit_amount) AS cred,
       sum(e.debit_amount - e.credit_amount) AS balance
  FROM biz_voucher_entry e JOIN biz_account a ON a.id = e.account_id
  JOIN biz_voucher v ON v.id = e.voucher_id
 WHERE v.deleted_at IS NULL AND a.account_code IN ('1601','1602','1606','5301','5711','5602.01','2221.02','1002','2202')
 GROUP BY a.account_code, a.account_name ORDER BY a.account_code;

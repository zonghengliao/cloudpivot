-- E2E-04 财务域演练：勾稽/对账取数（145 cloudpivot_dev · 演练年度 2022）
-- 用法：psql -h 127.0.0.1 -U cloudpivot -d cloudpivot_dev -f e2e04_reconcile.sql
-- 每条都应返回 0 行 / 0 值；非 0 即为口径裂缝。

\echo '① 全库已记账凭证借贷不平（应 0 张）'
SELECT v.voucher_no, v.period, v.total_debit, v.total_credit
  FROM biz_voucher v
 WHERE v.deleted_at IS NULL AND v.status = 'POSTED'
   AND COALESCE((SELECT SUM(e.debit_amount)  FROM biz_voucher_entry e WHERE e.voucher_id = v.id), 0)
    <> COALESCE((SELECT SUM(e.credit_amount) FROM biz_voucher_entry e WHERE e.voucher_id = v.id), 0);

\echo '② 2022 年度试算平衡（借方合计 = 贷方合计；应相等）'
SELECT SUM(e.debit_amount) AS debit, SUM(e.credit_amount) AS credit, SUM(e.debit_amount - e.credit_amount) AS diff
  FROM biz_voucher_entry e JOIN biz_voucher v ON v.id = e.voucher_id
 WHERE v.deleted_at IS NULL AND v.status = 'POSTED' AND v.period BETWEEN '2022-01' AND '2022-12';

\echo '③ 年结后损益类科目 2022 年度净额（应全 0）'
SELECT a.account_code, a.account_name, COALESCE(SUM(e.debit_amount - e.credit_amount), 0) AS net
  FROM biz_account a
  LEFT JOIN biz_voucher_entry e ON e.account_code = a.account_code
  LEFT JOIN biz_voucher v ON v.id = e.voucher_id
 WHERE a.deleted_at IS NULL AND a.account_category = 'PROFIT_LOSS' AND a.is_leaf = 1
   AND (e.id IS NULL OR (v.status = 'POSTED' AND v.deleted_at IS NULL
                         AND v.period BETWEEN '2022-01' AND '2022-12'))
 GROUP BY a.account_code, a.account_name
HAVING ABS(COALESCE(SUM(e.debit_amount - e.credit_amount), 0)) > 0.004;

\echo '④ 次年（2023-01）期初 = 上年（2022）期末（应 0 行差异）'
WITH closing AS (
  SELECT e.account_code, SUM(e.debit_amount - e.credit_amount) AS net
    FROM biz_voucher_entry e JOIN biz_voucher v ON v.id = e.voucher_id
   WHERE v.deleted_at IS NULL AND v.status = 'POSTED' AND v.period BETWEEN '2022-01' AND '2022-12'
   GROUP BY e.account_code),
     op AS (SELECT account_code, debit_opening - credit_opening AS net
              FROM biz_opening_balance WHERE period_code = '2023-01')
SELECT COALESCE(c.account_code, o.account_code) AS account_code,
       COALESCE(c.net, 0) AS closing_2022, COALESCE(o.net, 0) AS opening_2023
  FROM closing c FULL JOIN op o USING (account_code)
 WHERE COALESCE(c.net, 0) <> COALESCE(o.net, 0);

\echo '⑤ 年结结转凭证（YEAR_END）自检：借贷相等 + 3103 净额 0 + 3104 = 净利润'
SELECT v.voucher_no, v.period, v.total_debit, v.total_credit,
       (SELECT COALESCE(SUM(debit_amount - credit_amount), 0) FROM biz_voucher_entry e
         WHERE e.voucher_id = v.id AND e.account_code = '3103') AS net_3103,
       (SELECT COALESCE(SUM(credit_amount - debit_amount), 0) FROM biz_voucher_entry e
         WHERE e.voucher_id = v.id AND e.account_code = '3104') AS net_3104,
       (SELECT COUNT(*) FROM biz_voucher_entry e WHERE e.voucher_id = v.id) AS entries
  FROM biz_voucher v
 WHERE v.deleted_at IS NULL AND v.voucher_type = 'YEAR_END' AND v.period LIKE '2022-%';

\echo '⑥ 期初余额借贷平衡（每年首期；应 0 行不平）'
SELECT period_code, SUM(debit_opening) AS debit, SUM(credit_opening) AS credit
  FROM biz_opening_balance
 GROUP BY period_code
HAVING SUM(debit_opening) <> SUM(credit_opening);

\echo '⑦ 凭证期间必有期间行（应 0 行）'
SELECT v.voucher_no, v.period FROM biz_voucher v
 WHERE v.deleted_at IS NULL
   AND NOT EXISTS (SELECT 1 FROM biz_accounting_period p WHERE p.period_code = v.period);

\echo '⑧ 2022 年度残留非终态凭证（应 0 行）'
SELECT voucher_no, status FROM biz_voucher
 WHERE deleted_at IS NULL AND period LIKE '2022-%' AND status <> 'POSTED';

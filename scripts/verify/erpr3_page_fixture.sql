-- ERP-R3 页面验收夹具（隔离副本库专用；**禁止**对生产库执行）
--
-- 7 张 POSTED 凭证，2026-10 为编制期间：
--   0001 2026-09-10 借 1002 / 贷 3001 10,000  股东投入       → 筹资流入（只进本年累计）
--   0002 2026-10-05 借 1002 / 贷 5001  5,000  销售收款       → 经营流入
--   0003 2026-10-06 借 5401 / 贷 1002  2,000  采购付款       → 经营流出
--   0004 2026-10-07 借 2211 / 贷 1002    800  发工资         → 经营流出
--   0005 2026-10-08 借 2221 / 贷 1002    300  交税           → 经营流出
--   0006 2026-10-09 借 1601 / 贷 1002  1,500  买设备         → 投资流出
--   0007 2026-10-10 借 1002 / 贷 2501  4,000  借款           → 筹资流入
--
-- 期望：经营 净额 1,900 / 投资 −1,500 / 筹资 4,000 / 净增 4,400
--       期初 10,000 → 期末 14,400 = 资产负债表「货币资金」14,400
DELETE FROM biz_voucher;

INSERT INTO biz_accounting_period (period_year, period_month, period_code, start_date, end_date, status)
VALUES (2026, 7, '2026-07', '2026-07-01', '2026-07-31', 'OPEN'),
       (2026, 9, '2026-09', '2026-09-01', '2026-09-30', 'CLOSED'),
       (2026, 10, '2026-10', '2026-10-01', '2026-10-31', 'OPEN')
ON CONFLICT (period_code) DO NOTHING;

-- 只留这三个期间：页面默认选中**最新**期间，生产副本里还带着 2026-11 之类，
-- 不清掉的话页面会默认选 11 月（本期全 0），断言就没法钉死 2026-10。
DELETE FROM biz_accounting_period
 WHERE period_code NOT IN ('2026-07', '2026-09', '2026-10');

INSERT INTO biz_voucher (id, voucher_word, voucher_no, period, voucher_date, voucher_type,
                         source_type, source_no, summary, total_debit, total_credit,
                         entry_count, status, is_red)
VALUES
 ('e4f40001-0000-4000-8000-000000000001', 'ERPR3P', '0001', '2026-09', '2026-09-10', 'AUTO',
  'MANUAL', 'ERPR3P-1', '股东投入（筹资流入）', 10000, 10000, 2, 'POSTED', 0),
 ('e4f40001-0000-4000-8000-000000000002', 'ERPR3P', '0002', '2026-10', '2026-10-05', 'AUTO',
  'MANUAL', 'ERPR3P-2', '销售收款（经营流入）', 5000, 5000, 2, 'POSTED', 0),
 ('e4f40001-0000-4000-8000-000000000003', 'ERPR3P', '0003', '2026-10', '2026-10-06', 'AUTO',
  'MANUAL', 'ERPR3P-3', '采购付款（经营流出）', 2000, 2000, 2, 'POSTED', 0),
 ('e4f40001-0000-4000-8000-000000000004', 'ERPR3P', '0004', '2026-10', '2026-10-07', 'AUTO',
  'MANUAL', 'ERPR3P-4', '发工资（经营流出）', 800, 800, 2, 'POSTED', 0),
 ('e4f40001-0000-4000-8000-000000000005', 'ERPR3P', '0005', '2026-10', '2026-10-08', 'AUTO',
  'MANUAL', 'ERPR3P-5', '交税（经营流出）', 300, 300, 2, 'POSTED', 0),
 ('e4f40001-0000-4000-8000-000000000006', 'ERPR3P', '0006', '2026-10', '2026-10-09', 'AUTO',
  'MANUAL', 'ERPR3P-6', '买设备（投资流出）', 1500, 1500, 2, 'POSTED', 0),
 ('e4f40001-0000-4000-8000-000000000007', 'ERPR3P', '0007', '2026-10', '2026-10-10', 'AUTO',
  'MANUAL', 'ERPR3P-7', '借款（筹资流入）', 4000, 4000, 2, 'POSTED', 0);

INSERT INTO biz_voucher_entry (voucher_id, line_no, summary, account_id, account_code,
                               account_name, debit_amount, credit_amount)
SELECT x.voucher_id, x.line_no, x.summary, a.id, a.account_code, a.account_name, x.debit, x.credit
  FROM (VALUES
    ('e4f40001-0000-4000-8000-000000000001'::uuid, 1, '投入',     '1002', 10000, 0),
    ('e4f40001-0000-4000-8000-000000000001'::uuid, 2, '实收资本', '3001', 0, 10000),
    ('e4f40001-0000-4000-8000-000000000002'::uuid, 1, '收款',     '1002', 5000, 0),
    ('e4f40001-0000-4000-8000-000000000002'::uuid, 2, '收入',     '5001', 0, 5000),
    ('e4f40001-0000-4000-8000-000000000003'::uuid, 1, '成本',     '5401', 2000, 0),
    ('e4f40001-0000-4000-8000-000000000003'::uuid, 2, '付款',     '1002', 0, 2000),
    ('e4f40001-0000-4000-8000-000000000004'::uuid, 1, '工资',     '2211', 800, 0),
    ('e4f40001-0000-4000-8000-000000000004'::uuid, 2, '付款',     '1002', 0, 800),
    ('e4f40001-0000-4000-8000-000000000005'::uuid, 1, '交税',     '2221', 300, 0),
    ('e4f40001-0000-4000-8000-000000000005'::uuid, 2, '付款',     '1002', 0, 300),
    ('e4f40001-0000-4000-8000-000000000006'::uuid, 1, '设备',     '1601', 1500, 0),
    ('e4f40001-0000-4000-8000-000000000006'::uuid, 2, '付款',     '1002', 0, 1500),
    ('e4f40001-0000-4000-8000-000000000007'::uuid, 1, '借款',     '1002', 4000, 0),
    ('e4f40001-0000-4000-8000-000000000007'::uuid, 2, '长期借款', '2501', 0, 4000)
  ) AS x(voucher_id, line_no, summary, account_code, debit, credit)
  JOIN biz_account a ON a.account_code = x.account_code AND a.deleted_at IS NULL
  JOIN biz_voucher v ON v.id = x.voucher_id;

-- 自检：7 张凭证 × 2 条分录 = 14 行，且每张凭证借贷相等
DO $$
DECLARE
    n_entry integer;
    n_bad   integer;
BEGIN
    SELECT count(*) INTO n_entry
      FROM biz_voucher_entry e JOIN biz_voucher v ON v.id = e.voucher_id
     WHERE v.voucher_word = 'ERPR3P';
    IF n_entry <> 14 THEN
        RAISE EXCEPTION 'ERP-R3 页面夹具失败：分录应 14 行，实际 %', n_entry;
    END IF;
    SELECT count(*) INTO n_bad
      FROM (SELECT v.id, SUM(e.debit_amount) AS d, SUM(e.credit_amount) AS c
              FROM biz_voucher v JOIN biz_voucher_entry e ON e.voucher_id = v.id
             WHERE v.voucher_word = 'ERPR3P'
             GROUP BY v.id HAVING SUM(e.debit_amount) <> SUM(e.credit_amount)) t;
    IF n_bad > 0 THEN
        RAISE EXCEPTION 'ERP-R3 页面夹具失败：% 张凭证借贷不等', n_bad;
    END IF;
END $$;

-- 「破」模式追加：借 1012 其他货币资金 / 贷 1002 200
-- 1012 不在现金类口径（1001/1002）也没有分类行 → 归「其他」+ warnings + 勾稽差 −200
INSERT INTO biz_voucher (id, voucher_word, voucher_no, period, voucher_date, voucher_type,
                         source_type, source_no, summary, total_debit, total_credit,
                         entry_count, status, is_red)
VALUES ('e4f40001-0000-4000-8000-000000000009', 'ERPR3P', '0009', '2026-10', '2026-10-20', 'AUTO',
        'MANUAL', 'ERPR3P-9', '转出到其他货币资金', 200, 200, 2, 'POSTED', 0);

INSERT INTO biz_voucher_entry (voucher_id, line_no, summary, account_id, account_code,
                               account_name, debit_amount, credit_amount)
SELECT x.voucher_id, x.line_no, x.summary, a.id, a.account_code, a.account_name, x.debit, x.credit
  FROM (VALUES
    ('e4f40001-0000-4000-8000-000000000009'::uuid, 1, '其他货币资金', '1012', 200, 0),
    ('e4f40001-0000-4000-8000-000000000009'::uuid, 2, '银行付款',     '1002', 0, 200)
  ) AS x(voucher_id, line_no, summary, account_code, debit, credit)
  JOIN biz_account a ON a.account_code = x.account_code AND a.deleted_at IS NULL
  JOIN biz_voucher v ON v.id = x.voucher_id;

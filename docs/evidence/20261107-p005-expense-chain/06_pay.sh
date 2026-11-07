#!/usr/bin/env bash
# ⑥ 付款 3500（财务经理 fin_drill 走 /expense/{id}/pay）→ PAID + 付款凭证
set -uo pipefail
source /opt/cloudpivot/docs/evidence/20261107-p005-expense-chain/00_env.sh
FIN=$(cat "$(tokf fin_drill)")
RPT=$(cat "$EV/raw/report.id")

echo "===== ⑥-0 付款前单据状态 ====="
cat > /tmp/p005_pay_before.sql <<SQL
\pset pager off
select report_no, status, pay_status, paid_amount, unpaid_amount from biz_expense_report where id = '$RPT'::uuid;
SQL
psqlf /tmp/p005_pay_before.sql

echo "===== ⑥-1 登记付款 3500（银行转账 / 1002 银行存款）====="
api POST "/business/erp/expense/$RPT/pay" "$FIN" '{"amount":"3500","pay_date":"2026-11-07","pay_method":"BANK_TRANSFER","pay_account_code":"1002","remark":"P0-05 演练付款"}' > "$EV/raw/p0_pay.json"
echo "http=$HTTP $(jq -c '{code,msg}' "$EV/raw/p0_pay.json")"
jq -c '{report_no:.data.report.report_no,status:.data.report.status,pay_status:.data.report.pay_status,paid:.data.report.paid_amount,unpaid:.data.report.unpaid_amount,payments:(.data.payments|length)}' "$EV/raw/p0_pay.json"

echo "===== ⑥-2 DB 复核：付款记录 / 单据 / 付款凭证 ====="
cat > /tmp/p005_pay_verify.sql <<SQL
\pset pager off
select report_no, status, pay_status, paid_amount, unpaid_amount, last_pay_date, period_code
  from biz_expense_report where id = '$RPT'::uuid;
\echo --- biz_expense_payment ---
select payment_no, amount, pay_date, pay_method, pay_account_id, period_code, status, voucher_id
  from biz_expense_payment where report_id = '$RPT'::uuid order by created_at;
\echo --- 该单名下全部凭证 ---
select v.voucher_no, v.source_type, v.source_no, v.total_debit, v.total_credit,
       (v.total_debit = v.total_credit) as balanced
  from biz_voucher v
 where v.source_no = (select report_no from biz_expense_report where id = '$RPT'::uuid)
    or v.source_no in (select payment_no from biz_expense_payment where report_id = '$RPT'::uuid)
 order by v.voucher_no;
\echo --- 付款凭证分录 ---
select v.voucher_no, e.line_no, e.account_code, e.account_name, e.debit_amount, e.credit_amount, e.summary
  from biz_voucher v join biz_voucher_entry e on e.voucher_id = v.id
 where v.source_type = 'EXPENSE_PAYMENT'
   and v.source_no in (select payment_no from biz_expense_payment where report_id = '$RPT'::uuid)
 order by v.voucher_no, e.line_no;
SQL
psqlf /tmp/p005_pay_verify.sql

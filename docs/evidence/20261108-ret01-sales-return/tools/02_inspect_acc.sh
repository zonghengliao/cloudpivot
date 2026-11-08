#!/usr/bin/env bash
set -uo pipefail
set -a; . /etc/cloudpivot/cloudpivot-dev.env; set +a
export PGPASSWORD="$NIK_DATABASE_PASSWORD"
DB=cloudpivot_ret01
q() { psql -h 127.0.0.1 -U "$NIK_DATABASE_USER" -At -d "$DB" -c "$1"; }
echo "== 应收"
q "select ar_no||' | '||status||' | amount='||amount||' | received='||received_amount||' | unsettled='||unsettled_amount from biz_receivable"
echo "== 发票"
q "select invoice_no||' | '||invoice_type||' | '||status||' | amt='||amount||' | tax='||tax_amount||' | total='||total_with_tax||' | ar='||coalesce(receivable_id::text,'-') from biz_invoice order by created_at"
echo "== 凭证头"
q "select voucher_no||' | '||voucher_type||' | src='||source_type||' | '||coalesce(source_no,'-')||' | red='||is_red||' | '||total_debit||'/'||total_credit||' | '||coalesce(summary,'-') from biz_voucher order by created_at"
echo "== 分录（按凭证）"
q "select v.voucher_no||' | '||e.line_no||' | '||e.account_code||' '||e.account_name||' | D='||e.debit_amount||' | C='||e.credit_amount||' | '||coalesce(e.summary,'-') from biz_voucher_entry e join biz_voucher v on v.id=e.voucher_id order by v.created_at, e.line_no"
echo "== 按科目汇总（全部凭证）"
q "select account_code||' | D='||sum(debit_amount)||' | C='||sum(credit_amount)||' | net='||sum(debit_amount-credit_amount) from biz_voucher_entry group by account_code order by account_code"
echo "== 退款/红冲相关表"
q "select table_name from information_schema.tables where table_schema='public' and table_name like '%refund%'"

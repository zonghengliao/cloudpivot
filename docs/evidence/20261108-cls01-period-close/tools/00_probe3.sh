#!/usr/bin/env bash
set -uo pipefail
DB=${CLS01_DB:-cloudpivot_cls01}
set -a; . /etc/cloudpivot/cloudpivot-dev.env; set +a
export PGPASSWORD="$NIK_DATABASE_PASSWORD"
P() { psql -h 127.0.0.1 -U "$NIK_DATABASE_USER" -At -d "$DB" -c "$1"; }
echo "== 固定资产购置凭证"
P "select v.voucher_no||' | '||v.source_type||' | '||v.voucher_date||' | '||v.summary||' | 借='||v.total_debit from biz_voucher v where v.source_type like '%ASSET%' or v.summary like '%购置%'"
P "select e.line_no||' '||e.account_code||' '||e.account_name||' 借='||e.debit_amount||' 贷='||e.credit_amount||' | '||e.summary from biz_voucher_entry e join biz_voucher v on v.id=e.voucher_id where v.source_type like '%ASSET%' or v.summary like '%购置%' order by e.line_no"
echo "== 11 月凭证"
P "select voucher_no||' '||voucher_type||' '||source_type||' '||summary from biz_voucher where period='2026-11' order by voucher_no"
echo "== 报销单状态"
P "select report_no||' '||status||' '||coalesce(approval_instance_id::text,'-') from biz_expense_report"

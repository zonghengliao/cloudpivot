#!/usr/bin/env bash
# ③ 提交报销单（多明细：机票 2000 + 住宿 1500 = 3500，税率 0）
set -uo pipefail
source /opt/cloudpivot/docs/evidence/20261107-p005-expense-chain/00_env.sh
EMP=$(cat "$(tokf emp_drill)")
DEPT=$(cat "$EV/raw/dept.id")
TYPE=$(cat "$EV/raw/type.id")

echo "===== ③-1 建报销单（报销人 emp_drill，2 条明细）====="
BODY=$(cat <<JSON
{"title":"P0-05 演练差旅报销（机票+住宿）","expense_date":"2026-11-01","dept_id":"$DEPT","remark":"P0-05 费用报销链演练",
 "items":[
  {"expense_date":"2026-11-01","expense_type_id":"$TYPE","with_tax_amount":"2000","tax_rate":"0","summary":"机票","invoice_no":"INV-DRILL-0001","invoice_date":"2026-11-01","invoice_type":"VAT_NORMAL"},
  {"expense_date":"2026-11-01","expense_type_id":"$TYPE","with_tax_amount":"1500","tax_rate":"0","summary":"住宿","invoice_no":"INV-DRILL-0002","invoice_date":"2026-11-01","invoice_type":"VAT_NORMAL"}
 ]}
JSON
)
api POST "/business/erp/expense" "$EMP" "$BODY" > "$EV/raw/r1_create.json"
echo "http=$HTTP $(jq -c '{code,msg}' "$EV/raw/r1_create.json")"
RPT=$(jq -r '.data.report.id // empty' "$EV/raw/r1_create.json")
echo "report_id=$RPT"
echo "$RPT" > "$EV/raw/report.id"
[ -n "$RPT" ] || { echo "FATAL: 报销单没建起来"; jq -c . "$EV/raw/r1_create.json"; exit 1; }

echo "===== ③-2 出参金额 ====="
jq -c '{report_no:.data.report.report_no,status:.data.report.status,total_amount:.data.report.total_amount,total_tax:.data.report.total_tax,total_with_tax:.data.report.total_with_tax,item_count:.data.report.item_count,dept_name:.data.report.dept_name,reporter_name:.data.report.reporter_name,items:(.data.items|length)}' "$EV/raw/r1_create.json"

echo "===== ③-3 DB 复核：主表 / 明细（条数 + 金额勾稽）====="
cat > /tmp/p005_report_verify.sql <<SQL
\pset pager off
select report_no, status, reporter_name, dept_name, expense_date, period_code,
       total_amount, total_tax, total_with_tax, item_count
  from biz_expense_report where id = '$RPT'::uuid;
select line_no, type_code, account_code, amount, tax_amount, with_tax_amount, summary, invoice_no
  from biz_expense_item where report_id = '$RPT'::uuid order by line_no;
select (select total_with_tax from biz_expense_report where id = '$RPT'::uuid) as header_amount,
       (select sum(with_tax_amount) from biz_expense_item where report_id = '$RPT'::uuid) as item_sum,
       (select count(*) from biz_expense_item where report_id = '$RPT'::uuid) as item_rows,
       (select total_with_tax from biz_expense_report where id = '$RPT'::uuid)
         = (select sum(with_tax_amount) from biz_expense_item where report_id = '$RPT'::uuid) as header_eq_items;
SQL
psqlf /tmp/p005_report_verify.sql

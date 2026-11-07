#!/usr/bin/env bash
# ② 建报销类型 DRILL_TRAVEL（关联 5602.02 管理费用-差旅费）
set -uo pipefail
source /opt/cloudpivot/docs/evidence/20261107-p005-expense-chain/00_env.sh
ADMIN=$(cat "$(tokf admin)")

echo "===== ②-1 建费用类型 ====="
BODY="{\"type_code\":\"DRILL_TRAVEL\",\"type_name\":\"差旅费(演练)\",\"account_id\":\"$ACC_TRAVEL\",\"tax_deductible\":true,\"budget_controlled\":true,\"need_invoice_no\":true,\"amount_limit\":\"0\",\"sort_order\":91,\"remark\":\"P0-05 演练\"}"
api POST "/business/erp/expense-type" "$ADMIN" "$BODY" > "$EV/raw/t1_type.json"
echo "http=$HTTP $(jq -c '{code,msg}' "$EV/raw/t1_type.json")"
TYPE=$(jq -r '.data.id // empty' "$EV/raw/t1_type.json")
if [ -z "$TYPE" ]; then
  api GET "/business/erp/expense-type/list" "$ADMIN" > "$EV/raw/t1_type_list.json"
  TYPE=$(jq -r '[.data[]?|select(.type_code=="DRILL_TRAVEL")][0].id // empty' "$EV/raw/t1_type_list.json")
fi
echo "type_id=$TYPE"
echo "$TYPE" > "$EV/raw/type.id"
[ -n "$TYPE" ] || { echo "FATAL: 费用类型没建起来"; exit 1; }

echo "===== ②-2 出参（科目是否落到 5602.02）====="
jq -c '{id:.data.id,type_code:.data.type_code,type_name:.data.type_name,account_id:.data.account_id,account_code:.data.account_code,account_name:.data.account_name,tax_deductible:.data.tax_deductible,budget_controlled:.data.budget_controlled}' "$EV/raw/t1_type.json"

echo "===== ②-3 DB 复核 biz_expense_type ====="
cat > /tmp/p005_type_verify.sql <<SQL
\pset pager off
select t.type_code, t.type_name, t.account_id, a.account_code, a.account_name, a.is_leaf,
       t.tax_deductible, t.budget_controlled, t.need_invoice_no, t.status
  from biz_expense_type t join biz_account a on a.id = t.account_id
 where t.type_code = 'DRILL_TRAVEL' and t.deleted_at is null;
SQL
psqlf /tmp/p005_type_verify.sql

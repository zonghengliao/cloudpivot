#!/usr/bin/env bash
# ⑥ 反证：为什么业务上会看到「超支不拦截」
#   (a) 预算编了但没走完审批（DRAFT）→ 槽里没有 ACTIVE 预算 → 不拦截（额度按 0 处理、全局 WARN 放行）
#   (b) 槽里根本没编预算 → 同上（P0-05 已记的 P3：未编预算也被标 over_budget=1 / over_amount=全额）
set -uo pipefail
cd /opt/cloudpivot/docs/evidence/20261107-p006-budget-chain
source ./00_env.sh

ADMIN=$(cat "$(tokf admin)")
EMP=$(cat "$(tokf emp_drill)")
DEPT=$(cat "$EV/raw/dept.id")

ACC_MEAL='c1ac64b2-bec5-4106-b1e6-94bf27befb6a'    # 5602.04 业务招待费
ACC_COMM='eaa2e27c-c6cf-48d6-b74f-9eb217bfe3b8'    # 5602.05 通讯费
TYPE_MEAL=$(psqlat "select id from biz_expense_type where type_code = 'MEAL' and deleted_at is null")
TYPE_COMM=$(psqlat "select id from biz_expense_type where type_code = 'COMM' and deleted_at is null")
TGEN=$(psqlat "select id from biz_account where account_code = '5602.04' and is_leaf = 1")

mkexp() { # tag type amount day title
  local tag="$1" type="$2" amt="$3" day="$4" title="$5" id
  local body="{\"title\":\"$title\",\"expense_date\":\"$day\",\"dept_id\":\"$DEPT\",\"remark\":\"P0-06 反证\",\"items\":[{\"expense_date\":\"$day\",\"expense_type_id\":\"$type\",\"with_tax_amount\":\"$amt\",\"tax_rate\":\"0\",\"summary\":\"反证用\",\"invoice_no\":\"INV-$tag\",\"invoice_date\":\"$day\",\"invoice_type\":\"VAT_NORMAL\"}]}"
  api POST "/business/erp/expense" "$EMP" "$body" > "$EV/raw/${tag}_create.json"
  id=$(jq -r '.data.report.id // empty' "$EV/raw/${tag}_create.json")
  echo "  [$tag] create http=$HTTP report=$id"
  printf '%s' "$id" > "$EV/raw/${tag}.id"
  api POST "/business/erp/expense/$id/submit" "$EMP" > "$EV/raw/${tag}_submit.json"
  echo "  [$tag] submit http=$HTTP $(jq -c '{code,msg,status:.data.report.status,over_budget:.data.report.over_budget,over_amount:.data.report.over_amount,usages:(.data.budget_usages|length)}' "$EV/raw/${tag}_submit.json")"
}

echo "===== ⑥-8 (a) 5602.04 编 500 预算但只停在 DRAFT（不送审）→ 报 500 应放行 ====="
PERIODS='[{"month":1,"amount":"0"},{"month":2,"amount":"0"},{"month":3,"amount":"0"},{"month":4,"amount":"0"},{"month":5,"amount":"0"},{"month":6,"amount":"0"},{"month":7,"amount":"0"},{"month":8,"amount":"0"},{"month":9,"amount":"0"},{"month":10,"amount":"0"},{"month":11,"amount":"500"},{"month":12,"amount":"0"}]'
BODY="{\"fiscal_year\":2026,\"dept_id\":\"$DEPT\",\"account_id\":\"$ACC_MEAL\",\"total_amount\":\"500\",\"control_mode\":\"BLOCK\",\"periods\":$PERIODS,\"remark\":\"P0-06 反证：DRAFT 预算不参与控制\"}"
api POST "/business/erp/budget" "$ADMIN" "$BODY" > "$EV/raw/y1_draft_budget.json"
echo "  budget create http=$HTTP $(jq -c '{status:.data.budget.status,control_mode:.data.budget.control_mode,total:.data.budget.total_amount}' "$EV/raw/y1_draft_budget.json")"
echo "  DB: $(psqlat "select status || ' / ' || control_mode from biz_budget where account_id = '$ACC_MEAL'::uuid and fiscal_year = 2026 and status <> 'SUPERSEDED' order by version_no desc limit 1")"
mkexp y2_draft_slot "$TYPE_MEAL" 500 2026-11-12 "P0-06 反证：DRAFT 预算下报 500"
psqlq "select report_no, status, over_budget, over_amount from biz_expense_report where id = '$(cat "$EV/raw/y2_draft_slot.id")'::uuid"
psqlq "select source_no, amount, status, over_budget, budget_id from biz_budget_usage where source_id = '$(cat "$EV/raw/y2_draft_slot.id")'::uuid"

echo "===== ⑥-9 (b) 5602.05 完全没编预算（全局 WARN）→ 报 800 应放行并标 over_budget=1/over_amount=800 ====="
mkexp y3_no_budget "$TYPE_COMM" 800 2026-11-13 "P0-06 反证：未编预算报 800"
psqlq "select report_no, status, over_budget, over_amount, budget_mode from biz_expense_report where id = '$(cat "$EV/raw/y3_no_budget.id")'::uuid"
psqlq "select source_no, amount, status, over_budget, over_amount, budget_id from biz_budget_usage where source_id = '$(cat "$EV/raw/y3_no_budget.id")'::uuid"

echo "===== ⑥-10 结论所需数据：当前全局策略与两个反证槽的预算状态 ====="
psqlq "select config_key, config_value from sys_config where config_key = 'erp.budget.control_mode'"
psqlq "select b.account_code, b.status, b.control_mode, b.total_amount from biz_budget b where b.dept_id = '$DEPT'::uuid order by b.account_code, b.version_no"

#!/usr/bin/env bash
# ⑦-附2 / ⑧-附：处置触发点的联动（配 DISPOSED 规则）+ 资产 B 出售（售价高于净值 → 5301 收益分支）
set -uo pipefail
source /opt/cloudpivot/docs/evidence/20261107-p004-fixed-asset-chain/00_env.sh
ADMIN=$(cat "$EV/raw/admin.token")
GM=$(cat "$EV/raw/gm.token")
B=$(cat "$EV/raw/assetb.id")

echo "===== ⑧-9 配规则：FIXED_ASSET/DISPOSED → SEND_NOTICE ====="
api PUT "/business/flow/linkage" "$ADMIN" '{"name":"演练：资产处置 → 通知制单人","trigger_entity":"FIXED_ASSET","trigger_status":"DISPOSED","action_code":"SEND_NOTICE","action_params":{"recipient":"CREATOR"},"enabled":true,"seq":10,"confirm":true}' > "$EV/raw/l3_rule_disposed.json"
echo "http=$HTTP $(jq -c '{code,msg}' "$EV/raw/l3_rule_disposed.json")"

echo "===== ⑧-10 资产 B 处置（净值 50，售价 60 → 净收益 10 走 5301）====="
api POST "/business/erp/asset/$B/dispose" "$ADMIN" '{"disposal_type":"SALE","disposal_date":"2026-06-20","reason":"演练出售（溢价）","sale_price":"60","output_tax":"0"}' > "$EV/raw/p4_dispose_b.json"
echo "http=$HTTP $(jq -c '{code,msg,status:.data.asset.status}' "$EV/raw/p4_dispose_b.json")"

api GET "/business/flow/task/todo?page=1&page_size=20" "$GM" > "$EV/raw/p5_todo_b.json"
TASK=$(jq -r '.data.list[0].id // empty' "$EV/raw/p5_todo_b.json")
echo "gm todo total=$(jq -r '.data.total' "$EV/raw/p5_todo_b.json") task=$TASK"
api POST "/business/flow/task/$TASK/approve" "$GM" '{"comment":"P0-04 演练：同意溢价出售"}' > "$EV/raw/p6_approve_b.json"
echo "approve http=$HTTP $(jq -c '{code,msg}' "$EV/raw/p6_approve_b.json")"
sleep 2

echo "===== ⑧-11 资产 B 终态 + 处置凭证（应含 5301 收益）====="
psqlq "select asset_no, status, disposal_type, disposal_date, original_value, accumulated_depreciation, net_value from biz_fixed_asset order by asset_no"
psqlq "select v.voucher_no, v.total_debit, v.total_credit, e.line_no, e.account_code, e.account_name, e.debit_amount, e.credit_amount from biz_voucher v join biz_voucher_entry e on e.voucher_id = v.id where v.source_type = 'FIXED_ASSET_DISPOSAL' and v.source_no = 'FA-20261107-000002' order by v.voucher_no, e.line_no"

echo "===== ⑧-12 DISPOSED 事件的通知 ====="
psqlq "select notice_no, biz_type, title, content, idempotency_key from sys_notice where biz_type = 'FIXED_ASSET' order by created_at"

echo "===== ⑧-13 全账处置凭证平衡 + 1606 / 1901 归零检查 ====="
psqlq "select v.voucher_no, v.source_no, v.total_debit, v.total_credit, (v.total_debit = v.total_credit) as balanced from biz_voucher v where v.source_type = 'FIXED_ASSET_DISPOSAL' order by v.voucher_no"
psqlq "select a.account_code, a.account_name, sum(e.debit_amount - e.credit_amount) as net_balance from biz_voucher_entry e join biz_account a on a.id = e.account_id join biz_voucher v on v.id = e.voucher_id where v.source_type = 'FIXED_ASSET_DISPOSAL' group by a.account_code, a.account_name order by a.account_code"
psqlq "select count(*) as unbalanced_vouchers from biz_voucher where total_debit <> total_credit and deleted_at is null"

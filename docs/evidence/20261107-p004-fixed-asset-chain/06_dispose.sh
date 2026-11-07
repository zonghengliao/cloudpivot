#!/usr/bin/env bash
# ⑧ 资产处置（出售）：提交审批 → gm_drill 审批 → 处置凭证 + 卡片 DISPOSED + 损益
set -uo pipefail
source /opt/cloudpivot/docs/evidence/20261107-p004-fixed-asset-chain/00_env.sh
ADMIN=$(cat "$EV/raw/admin.token")
GM=$(cat "$EV/raw/gm.token")
A=$(cat "$EV/raw/asset.id")

echo "===== ⑧-1 处置前卡片状态 ====="
psqlq "select asset_no, status, original_value, accumulated_depreciation, net_value from biz_fixed_asset where id = '$A'::uuid"

echo "===== ⑧-2 提交处置（SALE / 售价 100000 / 销项税 13000 / 处置日 2026-06-15）====="
api POST "/business/erp/asset/$A/dispose" "$ADMIN" '{"disposal_type":"SALE","disposal_date":"2026-06-15","reason":"演练出售","sale_price":"100000","output_tax":"13000"}' > "$EV/raw/p1_dispose.json"
echo "http=$HTTP $(jq -c '{code,msg,status:.data.asset.status,pending_disposal:.data.pending_disposal}' "$EV/raw/p1_dispose.json")"

echo "===== ⑧-3 审批实例 + 待办 ====="
psqlq "select instance_no, biz_type, biz_no, status, current_node_name, initiator_name from biz_flow_instance where biz_type = 'ASSET_DISPOSAL' order by created_at desc limit 3"
api GET "/business/flow/task/todo?page=1&page_size=20" "$GM" > "$EV/raw/p2_todo.json"
echo "gm_drill todo http=$HTTP"
jq -c '{total:.data.total, rows:[.data.list[]|{id,title,biz_no,biz_type,node_name}]}' "$EV/raw/p2_todo.json"
TASK=$(jq -r '.data.list[0].id // empty' "$EV/raw/p2_todo.json")
echo "task=$TASK"
echo "$TASK" > "$EV/raw/task.id"

[ -n "$TASK" ] || { echo "FATAL: gm_drill 没有待办（审批人没派到）"; exit 1; }

echo "===== ⑧-4 gm_drill 审批通过 ====="
api POST "/business/flow/task/$TASK/approve" "$GM" '{"comment":"P0-04 演练：同意出售处置"}' > "$EV/raw/p3_approve.json"
echo "http=$HTTP $(jq -c '{code,msg}' "$EV/raw/p3_approve.json")"
sleep 2

echo "===== ⑧-5 实例 / 卡片 / 留痕 ====="
psqlq "select instance_no, biz_no, status, finished_at from biz_flow_instance where biz_type = 'ASSET_DISPOSAL' order by created_at desc limit 3"
psqlq "select asset_no, status, disposal_type, disposal_date, disposal_voucher_id, accumulated_depreciation, net_value from biz_fixed_asset where id = '$A'::uuid"
psqlq "select change_type, change_date, from_status, to_status, reason from biz_fixed_asset_change order by created_at"
psqlq "select notice_no, biz_type, title, content, idempotency_key from sys_notice where biz_type = 'FIXED_ASSET' order by created_at"

echo "===== ⑧-6 处置凭证（全套）====="
psqlq "select v.voucher_no, v.source_type, v.source_no, v.voucher_date, v.period, v.total_debit, v.total_credit, e.line_no, e.account_code, e.account_name, e.debit_amount, e.credit_amount, e.summary from biz_voucher v join biz_voucher_entry e on e.voucher_id = v.id where v.source_type = 'FIXED_ASSET_DISPOSAL' order by v.voucher_no, e.line_no"

echo "===== ⑧-7 处置凭证借贷平衡（每张）====="
psqlq "select v.voucher_no, v.total_debit, v.total_credit, (v.total_debit = v.total_credit) as balanced, sum(e.debit_amount) as entry_debit, sum(e.credit_amount) as entry_credit, (v.total_debit = sum(e.debit_amount) and v.total_credit = sum(e.credit_amount)) as entries_match from biz_voucher v join biz_voucher_entry e on e.voucher_id = v.id where v.source_type = 'FIXED_ASSET_DISPOSAL' group by v.voucher_no, v.total_debit, v.total_credit order by v.voucher_no"

echo "===== ⑧-8 清理科目 1606 是否归零 + 损益科目 ====="
psqlq "select a.account_code, a.account_name, sum(e.debit_amount) as deb, sum(e.credit_amount) as cred, sum(e.debit_amount - e.credit_amount) as balance from biz_voucher_entry e join biz_account a on a.id = e.account_id join biz_voucher v on v.id = e.voucher_id where v.source_type = 'FIXED_ASSET_DISPOSAL' group by a.account_code, a.account_name order by a.account_code"

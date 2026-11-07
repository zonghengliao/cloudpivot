#!/usr/bin/env bash
set -euo pipefail
source /opt/cloudpivot/docs/evidence/20261107-p003-inventory-chain/00_env.sh
ADMIN=$(cat "$EV/raw/admin.token")
LEAD=$(cat "$EV/raw/demo_lead.token")
ST=$(jq -r '.data.id' "$EV/raw/06_st_gain.json")
echo "$ST" > "$EV/raw/st_gain.id"
IT1=$(jq -r '.data.items[0].id' "$EV/raw/06_st_gain.json")
IT2=$(jq -r '.data.items[1].id' "$EV/raw/06_st_gain.json")

echo "===== ⑤-2 填实盘数：行1（未分配桶 60）→ 65（盘盈 +5）====="
api PUT "/business/erp/stock-take/$ST" "$ADMIN" "{
  \"lines\":[{\"item_id\":\"$IT1\",\"actual_qty\":65},{\"item_id\":\"$IT2\",\"actual_qty\":50}]
}" > "$EV/raw/06_st_gain_fill.json"
echo "http=$HTTP"
jq -c '{code,msg,status:.data.status,total_book_qty:.data.total_book_qty,total_actual_qty:.data.total_actual_qty,gain:.data.total_gain_qty,gain_amt:.data.total_gain_amount,diff_amt:.data.total_diff_amount}' "$EV/raw/06_st_gain_fill.json"

echo "===== ⑤-3 提交审批 DRAFT→PENDING_POST ====="
api PATCH "/business/erp/stock-take/$ST/status" "$ADMIN" '{"status":"PENDING_POST"}' > "$EV/raw/06_st_gain_submit.json"
echo "http=$HTTP $(jq -c '{code,msg}' "$EV/raw/06_st_gain_submit.json")"

api GET '/business/flow/task/todo?page=1&page_size=20' "$LEAD" > "$EV/raw/06_todo.json"
jq -c '.data.list[]? | {id,title,status}' "$EV/raw/06_todo.json"
TASK=$(jq -r '.data.list[] | select(.title|test("ST-20261107-000001")) | .id' "$EV/raw/06_todo.json")
echo "task=$TASK"

echo "===== ⑤-4 审批通过（demo_lead）====="
api POST "/business/flow/task/$TASK/approve" "$LEAD" '{"comment":"P0-03 盘点盘盈：同意"}' > "$EV/raw/06_approve.json"
echo "http=$HTTP $(jq -c '{code,msg}' "$EV/raw/06_approve.json")"
sleep 1
api GET "/business/erp/stock-take/$ST" "$ADMIN" | jq -c '{status:.data.status,approval:.data.approval_instance_id}'

echo "===== ⑤-5 过账（PENDING_POST→POSTED）====="
api PATCH "/business/erp/stock-take/$ST/status" "$ADMIN" '{"status":"POSTED"}' > "$EV/raw/06_st_gain_post.json"
echo "http=$HTTP $(jq -c '{code,msg}' "$EV/raw/06_st_gain_post.json")"
sleep 1
api GET "/business/erp/stock-take/$ST" "$ADMIN" > "$EV/raw/06_st_gain_final.json"
jq -c '{status:.data.status,book:.data.total_book_qty,actual:.data.total_actual_qty,gain:.data.total_gain_qty,loss:.data.total_loss_qty,gain_amt:.data.total_gain_amount,diff_amt:.data.total_diff_amount,voucher:.data.voucher_no}' "$EV/raw/06_st_gain_final.json"

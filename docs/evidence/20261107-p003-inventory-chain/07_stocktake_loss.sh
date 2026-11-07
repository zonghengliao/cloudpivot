#!/usr/bin/env bash
set -euo pipefail
source /opt/cloudpivot/docs/evidence/20261107-p003-inventory-chain/00_env.sh
ADMIN=$(cat "$EV/raw/admin.token")
LEAD=$(cat "$EV/raw/demo_lead.token")
A=$(cat "$EV/raw/wh_a.id")

echo "===== ⑤-6 建第二张全盘单（仓 A，做盘亏）====="
api POST '/business/erp/stock-take' "$ADMIN" "{
  \"warehouse_id\":\"$A\",\"take_type\":\"FULL\",\"remark\":\"P0-03 库存演练：盘亏 3 件\"
}" > "$EV/raw/07_st_loss.json"
echo "http=$HTTP"
ST=$(jq -r '.data.id' "$EV/raw/07_st_loss.json"); echo "$ST" > "$EV/raw/st_loss.id"
jq -c '{no:.data.take_no,status:.data.status,book:.data.total_book_qty}' "$EV/raw/07_st_loss.json"
jq -c '.data.items[] | {id, book_qty, location_id}' "$EV/raw/07_st_loss.json"

IT1=$(jq -r '.data.items[0].id' "$EV/raw/07_st_loss.json")
IT2=$(jq -r '.data.items[1].id' "$EV/raw/07_st_loss.json")
B1=$(jq -r '.data.items[0].book_qty' "$EV/raw/07_st_loss.json")
B2=$(jq -r '.data.items[1].book_qty' "$EV/raw/07_st_loss.json")
LOSS=$(echo "$B1 - 3" | bc)

echo "===== ⑤-7 填实盘数：行1 $B1 → $LOSS（盘亏 3）====="
api PUT "/business/erp/stock-take/$ST" "$ADMIN" "{
  \"lines\":[{\"item_id\":\"$IT1\",\"actual_qty\":$LOSS},{\"item_id\":\"$IT2\",\"actual_qty\":$B2}]
}" > "$EV/raw/07_st_loss_fill.json"
echo "http=$HTTP"
jq -c '{code,status:.data.status,book:.data.total_book_qty,actual:.data.total_actual_qty}' "$EV/raw/07_st_loss_fill.json"

echo "===== ⑤-8 提交审批 → 审批 → 过账 ====="
api PATCH "/business/erp/stock-take/$ST/status" "$ADMIN" '{"status":"PENDING_POST"}' > "$EV/raw/07_submit.json"
echo "submit http=$HTTP $(jq -c '{code,msg}' "$EV/raw/07_submit.json")"
api GET '/business/flow/task/todo?page=1&page_size=20' "$LEAD" > "$EV/raw/07_todo.json"
TASK=$(jq -r '.data.list[] | select(.title|test("ST-20261107-000002")) | .id' "$EV/raw/07_todo.json")
echo "task=$TASK"
api POST "/business/flow/task/$TASK/approve" "$LEAD" '{"comment":"P0-03 盘点盘亏：同意"}' > "$EV/raw/07_approve.json"
echo "approve http=$HTTP $(jq -c '{code,msg}' "$EV/raw/07_approve.json")"
sleep 1
api PATCH "/business/erp/stock-take/$ST/status" "$ADMIN" '{"status":"POSTED"}' > "$EV/raw/07_post.json"
echo "post http=$HTTP $(jq -c '{code,msg}' "$EV/raw/07_post.json")"
sleep 1
api GET "/business/erp/stock-take/$ST" "$ADMIN" > "$EV/raw/07_st_loss_final.json"
jq -c '{status:.data.status,book:.data.total_book_qty,actual:.data.total_actual_qty,gain:.data.total_gain_qty,loss:.data.total_loss_qty,loss_amt:.data.total_loss_amount,diff_amt:.data.total_diff_amount,voucher:.data.voucher_no}' "$EV/raw/07_st_loss_final.json"

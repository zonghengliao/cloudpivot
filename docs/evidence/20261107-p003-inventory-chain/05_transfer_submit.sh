#!/usr/bin/env bash
set -euo pipefail
source /opt/cloudpivot/docs/evidence/20261107-p003-inventory-chain/00_env.sh
ADMIN=$(cat "$EV/raw/admin.token")
LEAD=$(cat "$EV/raw/demo_lead.token")
TID=$(cat "$EV/raw/transfer.id")

echo "===== ④-1 提交审批 DRAFT→PENDING ====="
api PATCH "/business/erp/transfer/$TID/status" "$ADMIN" '{"status":"PENDING"}' > "$EV/raw/05_t_pending.json"
echo "http=$HTTP $(jq -c '{code,msg}' "$EV/raw/05_t_pending.json")"

echo "===== ④-2 审批人 todo（demo_lead）====="
api GET '/business/flow/task/todo?page=1&page_size=20' "$LEAD" > "$EV/raw/05_todo.json"
echo "http=$HTTP"
jq -c '.data.list[]? | {id,biz_no,title,biz_type,status,node_name}' "$EV/raw/05_todo.json"

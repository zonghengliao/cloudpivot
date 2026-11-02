#!/usr/bin/env bash
# A4 真机全链路（隔离实例 127.0.0.1:18091 + 独立库 cloudpivot_a4_e2e）
# 用法：bash /root/a4-e2e/chain.sh
set -uo pipefail
export PGPASSWORD="${PGPASSWORD:?运行前先 export PGPASSWORD=<本机测试库口令>（仓库里不写明文凭据）}"
BASE=http://127.0.0.1:18091/api
OUT=/root/a4-e2e
DB="psql -h 127.0.0.1 -U alun -d cloudpivot_a4_e2e -Atc"
SUP=a4000000-0000-4000-8000-000000000002
WH=a4000000-0000-4000-8000-000000000003
MAT=a4000000-0000-4000-8000-000000000011
TODAY=$(date +%F)

step() { echo; echo "===== $* ====="; }

step "0 登录"
curl -s -X POST "$BASE/system/auth/login" -H 'Content-Type: application/json' \
  -d '{"username":"admin","password":"Admin@123456"}' > $OUT/00-login.json
jq -c '{code,msg,has_token:(.data.access_token|type=="string")}' $OUT/00-login.json
TOKEN=$(jq -r .data.access_token $OUT/00-login.json)
H="Authorization: Bearer $TOKEN"

step "1 建采购合同（乙方 = A4 一号供应商）"
curl -s -X POST "$BASE/business/erp/contract" -H "$H" -H 'Content-Type: application/json' -d "{
  \"name\":\"A4 全链路采购合同\",
  \"contract_type\":\"PURCHASE\",
  \"party_b_id\":\"$SUP\",
  \"amount\":1000,
  \"sign_date\":\"$TODAY\",
  \"start_date\":\"$TODAY\",
  \"end_date\":\"2027-12-31\"
}" > $OUT/01-contract-create.json
jq -c '{code,msg,no:.data.contract_no,status:.data.status,party_b:.data.party_b_name}' $OUT/01-contract-create.json
CID=$(jq -r .data.id $OUT/01-contract-create.json)

step "2 合同签署 + 生效（DRAFT → SIGNED → ACTIVE）"
curl -s -X PATCH "$BASE/business/erp/contract/$CID/status" -H "$H" -H 'Content-Type: application/json' \
  -d '{"target_status":"SIGNED"}' > $OUT/02a-contract-signed.json
jq -c '{code,msg,status:.data.status}' $OUT/02a-contract-signed.json
curl -s -X PATCH "$BASE/business/erp/contract/$CID/status" -H "$H" -H 'Content-Type: application/json' \
  -d '{"target_status":"ACTIVE"}' > $OUT/02-contract-active.json
jq -c '{code,msg,status:.data.status}' $OUT/02-contract-active.json

step "3 建 CONTRACT 型采购单（**不传 supplier_id**，应从合同乙方派生）"
curl -s -X POST "$BASE/business/erp/purchase-order" -H "$H" -H 'Content-Type: application/json' -d "{
  \"purchase_type\":\"CONTRACT\",
  \"contract_id\":\"$CID\",
  \"warehouse_id\":\"$WH\",
  \"order_date\":\"$TODAY\",
  \"items\":[{\"material_id\":\"$MAT\",\"qty\":10,\"price_excl\":2,\"tax_rate\":13}]
}" > $OUT/03-po-create.json
jq -c '{code,msg,no:.data.order.order_no,supplier_id:.data.order.supplier_id,
        supplier_name:.data.order.supplier_name,purchase_type:.data.order.purchase_type,
        contract_no:.data.order.contract_no}' $OUT/03-po-create.json
POID=$(jq -r .data.order.id $OUT/03-po-create.json)

step "4 采购单提交（DRAFT → SUBMITTED，可收货）"
curl -s -X PATCH "$BASE/business/erp/purchase-order/$POID/status" -H "$H" -H 'Content-Type: application/json' \
  -d '{"status":"SUBMITTED"}' > $OUT/04-po-submitted.json
jq -c '{code,msg,status:.data.status}' $OUT/04-po-submitted.json
step "5 采购单待收（生成收货单的数据源）"
curl -s "$BASE/business/erp/purchase-order/$POID/pending-items" -H "$H" > $OUT/05-pending.json
jq -c '{code,order_no:.data.order.order_no,supplier_id:.data.order.supplier_id,
        item_id:.data.items[0].item_id,qty:.data.items[0].qty,remaining:.data.items[0].pending_qty}' $OUT/05-pending.json
ITEM=$(jq -r '.data.items[0].item_id' $OUT/05-pending.json)

step "6 生成收货单（**不传 supplier_id**：服务端从采购单派生）"
curl -s -X POST "$BASE/business/erp/stock-in" -H "$H" -H 'Content-Type: application/json' -d "{
  \"source_order_id\":\"$POID\",
  \"warehouse_id\":\"$WH\",
  \"items\":[{\"purchase_order_item_id\":\"$ITEM\",\"material_id\":\"$MAT\",\"qty\":6,\"price_excl\":2,\"tax_rate\":13}]
}" > $OUT/06-stockin-create.json
jq -c '{code,msg,no:.data.header.in_no,supplier_id:.data.header.supplier_id,
        supplier_name:.data.header.supplier_name,is_sourceless:.data.header.is_sourceless,
        source_order_id:.data.header.source_order_id,source_order_no:.data.header.source_order_no,
        contract_no:.data.contract_no,source_item_id:.data.items[0].source_item_id}' $OUT/06-stockin-create.json
SIID=$(jq -r .data.header.id $OUT/06-stockin-create.json)

step "7 基线三账（过账前）"
$DB "select 'inventory', coalesce(qty::text,'0'), coalesce(amount::text,'0') from biz_inventory where material_id='$MAT'::uuid" | sed 's/^/基线 /'
echo "基线 凭证 = $($DB "select count(*) from biz_voucher where source_id='$SIID'::uuid") / 应付 = $($DB "select count(*) from biz_payable where source_id='$SIID'::uuid") / 流水 = $($DB "select count(*) from biz_inventory_txn where doc_id='$SIID'::uuid")"
echo "基线 采购已收 = $($DB "select received_qty::text from biz_purchase_order_item where id='$ITEM'::uuid") / 采购状态 = $($DB "select status from biz_purchase_order where id='$POID'::uuid")"

step "8 过账（DRAFT → POSTED：库存 / 流水 / 凭证 / 应付 / 采购回写 一个事务）"
curl -s -X PATCH "$BASE/business/erp/stock-in/$SIID/status" -H "$H" -H 'Content-Type: application/json' \
  -d '{"status":"POSTED"}' > $OUT/07-post.json
jq -c '{code,msg}' $OUT/07-post.json

step "9 过账后三账 + 采购回写"
$DB "select 'inventory', qty::text, amount::text, avg_cost::text from biz_inventory where material_id='$MAT'::uuid" | sed 's/^/过账 /'
echo "过账 凭证 = $($DB "select count(*) from biz_voucher where source_id='$SIID'::uuid") / 应付 = $($DB "select count(*) from biz_payable where source_id='$SIID'::uuid") / 流水 = $($DB "select count(*) from biz_inventory_txn where doc_id='$SIID'::uuid")"
echo "过账 采购已收 = $($DB "select received_qty::text from biz_purchase_order_item where id='$ITEM'::uuid") / 采购状态 = $($DB "select status from biz_purchase_order where id='$POID'::uuid")"
curl -s "$BASE/business/erp/stock-in/$SIID" -H "$H" > $OUT/08-stockin-detail-posted.json
jq -c '{code,status:.data.header.status,voucher_id:.data.header.voucher_id,payable_id:.data.header.payable_id,total_with_tax:.data.header.total_with_tax}' $OUT/08-stockin-detail-posted.json
curl -s "$BASE/business/erp/purchase-order/$POID" -H "$H" > $OUT/09-po-detail-posted.json
jq -c '{code,status:.data.status,received_qty:.data.items[0].received_qty,qty:.data.items[0].qty}' $OUT/09-po-detail-posted.json

step "10 作废收货单（POSTED → CANCELLED，六本账 + 采购数量回冲）"
curl -s -X PATCH "$BASE/business/erp/stock-in/$SIID/status" -H "$H" -H 'Content-Type: application/json' \
  -d '{"status":"CANCELLED","reason":"A4 全链路：作废回基线"}' > $OUT/10-cancel.json
jq -c '{code,msg}' $OUT/10-cancel.json

step "11 作废后三账 + 采购回写（必须回基线）"
$DB "select 'inventory', coalesce(qty::text,'0'), coalesce(amount::text,'0'), coalesce(avg_cost::text,'0') from biz_inventory where material_id='$MAT'::uuid" | sed 's/^/作废 /'
echo "作废 凭证 = $($DB "select count(*) from biz_voucher where source_id='$SIID'::uuid")（含红冲）/ 红冲 = $($DB "select count(*) from biz_voucher where source_id='$SIID'::uuid and is_red=1") / 应付 = $($DB "select count(*) from biz_payable where source_id='$SIID'::uuid")"
echo "作废 采购已收 = $($DB "select received_qty::text from biz_purchase_order_item where id='$ITEM'::uuid") / 采购状态 = $($DB "select status from biz_purchase_order where id='$POID'::uuid")"
curl -s "$BASE/business/erp/stock-in/$SIID" -H "$H" > $OUT/11-stockin-detail-cancelled.json
jq -c '{code,status:.data.header.status,cancel_reason:.data.header.cancel_reason}' $OUT/11-stockin-detail-cancelled.json

step "12 列表快照（source_order_no / supplier_name / is_sourceless）"
curl -s "$BASE/business/erp/stock-in/page?page=1&page_size=10" -H "$H" > $OUT/12-list.json
jq -c '{code,total:.data.total,rows:[.data.list[]|{in_no,supplier_name,source_order_no,is_sourceless,status}]}' $OUT/12-list.json

echo
echo "A4 全链路脚本执行完毕：$OUT"

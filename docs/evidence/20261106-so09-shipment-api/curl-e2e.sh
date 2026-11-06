#!/usr/bin/env bash
# ============================================================================
# SO-09（t_b115c1a4）发货单 API 真实 token curl 实测
#
# 跑法（实例已在 127.0.0.1:18099，库 cloudpivot_so09）：
#   bash docs/evidence/20261106-so09-shipment-api/curl-e2e.sh
#
# 覆盖：登录 → 建客户订单（夹具）→ **从订单带出未发完的行** → 建单（取号）
#       → 详情 / 分页 → 订单状态闸 → 超发 400 → 非法迁移 409
#       → **库存不足过账 409** → 补库存 → **过账 200** → 终态再流转 409
#       → 草稿作废 200 / 待过账作废缺原因 400 → 带原因 200
#
# 数据库夹具（psql，均在脚本里写明为什么）：
#   · 订单状态置 APPROVED：审批链（SO-06）需要「审批人 ≠ 发起人」，admin 单账号
#     走不通三档审批 —— 本卡验的是**发货单**，不是审批，所以直接改状态。
#   · 库位账补货：本卡只**校验**库存充足（扣减是 SO-10），所以直接写
#     biz_inventory_location（`fifo_plan` 读的就是这张表）。
# ============================================================================
set -uo pipefail
BASE=${BASE:-http://127.0.0.1:18099}
USER_NAME=${USER_NAME:-admin}
PASSWORD=${PASSWORD:-Admin@123456}
PASSWORD_ALT=${PASSWORD_ALT:-Admin@20261106}
export PGPASSWORD=${PGPASSWORD:-alun}
PSQL="psql -h 127.0.0.1 -U alun -d ${DB:-cloudpivot_so09} -tAq"
WH=${WH:-d1370001-0000-4000-8000-000000000001}     # WH01 总仓（种子）
MAT_A=${MAT_A:-d1370003-0000-4000-8000-000000000002}  # M-W1A-002 原料B
MAT_B=${MAT_B:-d1370003-0000-4000-8000-000000000005}  # M-W1A-005 成品E
CUST=${CUST:-d0260000-0000-4000-8000-000000000001}    # CUS-0001 北京云枢（额度 50 万）
FAIL=0

say() { printf '\n===== %s =====\n' "$*"; }

# $1=label $2=expected_http $3=method $4=path [$5=json body]
call() {
    local label=$1 expect=$2 method=$3 path=$4 body=${5:-}
    local out code payload
    if [ -n "$body" ]; then
        out=$(curl -s -w '\n%{http_code}' -X "$method" "$BASE$path" \
              -H "Authorization: Bearer $TOKEN" -H 'Content-Type: application/json' -d "$body")
    else
        out=$(curl -s -w '\n%{http_code}' -X "$method" "$BASE$path" \
              -H "Authorization: Bearer $TOKEN")
    fi
    code=$(printf '%s' "$out" | tail -1)
    payload=$(printf '%s' "$out" | sed '$d')
    printf -- '--- %s\nHTTP %s (期望 %s)\n%s\n' "$label" "$code" "$expect" "$payload"
    if [ "$code" != "$expect" ]; then
        printf '!!! %s 状态码不符：实际 %s 期望 %s\n' "$label" "$code" "$expect"
        FAIL=1
    fi
    LAST=$payload
}

say '① 登录（admin / Admin@123456）→ 首登强制改密'
LOGIN=''
for try in "$PASSWORD" "$PASSWORD_ALT"; do
    LOGIN=$(curl -s -X POST "$BASE/api/system/auth/login" -H 'Content-Type: application/json' \
            -d "{\"username\":\"$USER_NAME\",\"password\":\"$try\"}")
    if [ "$(printf '%s' "$LOGIN" | jq -r '.code')" = "0" ]; then PASSWORD="$try"; break; fi
done
TOKEN=$(printf '%s' "$LOGIN" | jq -r '.data.access_token')
printf 'code=%s token_len=%s must_change_password=%s\n' \
  "$(printf '%s' "$LOGIN" | jq -r '.code')" "${#TOKEN}" \
  "$(printf '%s' "$LOGIN" | jq -r '.data.must_change_password')"
[ -n "$TOKEN" ] && [ "$TOKEN" != "null" ] || { echo '!!! 登录失败'; exit 1; }

if [ "$(printf '%s' "$LOGIN" | jq -r '.data.must_change_password')" = "true" ]; then
    PASSWORD_NEW='Admin@20261106'
    CHG=$(curl -s -w '\n%{http_code}' -X PUT "$BASE/api/system/auth/password" \
          -H "Authorization: Bearer $TOKEN" -H 'Content-Type: application/json' \
          -d "{\"old_password\":\"$PASSWORD\",\"new_password\":\"$PASSWORD_NEW\"}")
    printf '改密 HTTP %s %s\n' "$(printf '%s' "$CHG" | tail -1)" "$(printf '%s' "$CHG" | sed '$d')"
    LOGIN=$(curl -s -X POST "$BASE/api/system/auth/login" -H 'Content-Type: application/json' \
            -d "{\"username\":\"$USER_NAME\",\"password\":\"$PASSWORD_NEW\"}")
    TOKEN=$(printf '%s' "$LOGIN" | jq -r '.data.access_token')
    PASSWORD="$PASSWORD_NEW"
    printf '重新登录 code=%s token_len=%s\n' "$(printf '%s' "$LOGIN" | jq -r '.code')" "${#TOKEN}"
    [ -n "$TOKEN" ] && [ "$TOKEN" != "null" ] || { echo '!!! 改密后登录失败'; exit 1; }
fi

say '② 夹具：一张销售订单（2 行：10 件 / 5 件）'
SO_BODY=$(cat <<JSON
{"customer_id":"$CUST","order_date":"2026-11-06","remark":"SO-09 发货单 e2e 夹具",
 "items":[{"material_id":"$MAT_A","qty":10,"price":100,"tax_rate":13},
          {"material_id":"$MAT_B","qty":5,"price":200,"tax_rate":13}]}
JSON
)
call 'POST /business/erp/sales-order（夹具订单）' 200 POST /api/business/erp/sales-order "$SO_BODY"
SO_ID=$(printf '%s' "$LAST" | jq -r '.data.id')
SO_NO=$(printf '%s' "$LAST" | jq -r '.data.so_no')
ITEM_A=$(printf '%s' "$LAST" | jq -r '.data.items[0].id')
ITEM_B=$(printf '%s' "$LAST" | jq -r '.data.items[1].id')
printf 'so_no=%s so_id=%s\n  item1(line1)=%s item2(line2)=%s\n' "$SO_NO" "$SO_ID" "$ITEM_A" "$ITEM_B"
[ "$SO_ID" != "null" ] || { echo '!!! 建订单失败，后续无法继续'; exit 1; }

say '③ 夹具：订单置 APPROVED（审批链要「审批人 ≠ 发起人」，本卡验发货单不验审批）'
$PSQL -c "UPDATE biz_sales_order SET status='APPROVED', updated_at=now() WHERE id='$SO_ID'" \
  && echo "已置 APPROVED：$($PSQL -c "select status from biz_sales_order where id='$SO_ID'")"

say '④ 订单状态闸：草稿订单不允许开发货单'
DRAFT_SO=$(curl -s -X POST "$BASE/api/business/erp/sales-order" \
  -H "Authorization: Bearer $TOKEN" -H 'Content-Type: application/json' \
  -d "{\"customer_id\":\"$CUST\",\"order_date\":\"2026-11-06\",\"items\":[{\"material_id\":\"$MAT_A\",\"qty\":2,\"price\":100,\"tax_rate\":13}]}")
DRAFT_SO_ID=$(printf '%s' "$DRAFT_SO" | jq -r '.data.id')
call 'POST /shipment（草稿订单）→ 409' 409 POST /api/business/erp/shipment \
  "{\"so_id\":\"$DRAFT_SO_ID\",\"warehouse_id\":\"$WH\"}"

say '⑤ 从订单带出未发完的行（GET /shippable/{so_id}）'
call 'GET /business/erp/shipment/shippable/{so_id}' 200 GET "/api/business/erp/shipment/shippable/$SO_ID"
printf '%s' "$LAST" | jq -c '.data[] | {line_no, qty_ordered:.ordered, delivered, reserved, remaining}'

say '⑥ 建单：items 留空 ⇒ 全部未发完的行自动带出（数量 = 未发量）'
call 'POST /business/erp/shipment（items 留空 = 带出）' 200 POST /api/business/erp/shipment \
  "{\"so_id\":\"$SO_ID\",\"warehouse_id\":\"$WH\",\"ship_date\":\"2026-11-06\",\"remark\":\"SO-09 e2e 首单\"}"
SH_ID=$(printf '%s' "$LAST" | jq -r '.data.id')
SH_NO=$(printf '%s' "$LAST" | jq -r '.data.ship_no')
printf '%s' "$LAST" | jq -c '{ship_no:.data.ship_no, status:.data.status, total_qty:.data.total_qty,
  warehouse:.data.warehouse_code, so_no:.data.so_no,
  items:[.data.items[] | {line_no, qty, material_id}]}'
printf 'ship_no=%s id=%s\n' "$SH_NO" "$SH_ID"
[ "$SH_ID" != "null" ] || { echo '!!! 建单失败'; exit 1; }

say '⑦ 详情 + 分页'
call 'GET /business/erp/shipment/{id}' 200 GET "/api/business/erp/shipment/$SH_ID"
call 'GET /business/erp/shipment/page（status=DRAFT）' 200 GET \
  "/api/business/erp/shipment/page?status=DRAFT&so_id=$SO_ID&page=1&page_size=10"
printf '%s' "$LAST" | jq -c '{total:.data.total, rows:[.data.list[] | {ship_no, status, total_qty}]}'
call 'GET /business/erp/shipment/page（status=SHIPPED 域外值）→ 400' 400 GET \
  '/api/business/erp/shipment/page?status=SHIPPED'

say '⑧ 超发守卫：另一张干净订单（10 件）上分别试「超发 / 重复行 / qty=0」'
SO2=$(curl -s -X POST "$BASE/api/business/erp/sales-order" \
  -H "Authorization: Bearer $TOKEN" -H 'Content-Type: application/json' \
  -d "{\"customer_id\":\"$CUST\",\"order_date\":\"2026-11-06\",\"remark\":\"SO-09 超发夹具\",\"items\":[{\"material_id\":\"$MAT_A\",\"qty\":10,\"price\":100,\"tax_rate\":13}]}")
SO2_ID=$(printf '%s' "$SO2" | jq -r '.data.id')
ITEM2_A=$(printf '%s' "$SO2" | jq -r '.data.items[0].id')
$PSQL -c "UPDATE biz_sales_order SET status='APPROVED', updated_at=now() WHERE id='$SO2_ID'" > /dev/null
printf '超发夹具订单：%s（%s，行 %s，10 件）\n' "$(printf '%s' "$SO2" | jq -r '.data.so_no')" "$SO2_ID" "$ITEM2_A"

call 'POST /shipment（qty 11 > 未发 10）→ 400' 400 POST /api/business/erp/shipment \
  "{\"so_id\":\"$SO2_ID\",\"warehouse_id\":\"$WH\",\"items\":[{\"so_item_id\":\"$ITEM2_A\",\"qty\":11}]}"
call 'POST /shipment（同一订单行重复两行 1+1）→ 400' 400 POST /api/business/erp/shipment \
  "{\"so_id\":\"$SO2_ID\",\"warehouse_id\":\"$WH\",\"items\":[{\"so_item_id\":\"$ITEM2_A\",\"qty\":1},{\"so_item_id\":\"$ITEM2_A\",\"qty\":1}]}"
call 'POST /shipment（qty = 0）→ 400' 400 POST /api/business/erp/shipment \
  "{\"so_id\":\"$SO2_ID\",\"warehouse_id\":\"$WH\",\"items\":[{\"so_item_id\":\"$ITEM2_A\",\"qty\":0}]}"
call 'POST /shipment（不存在的订单行）→ 400' 400 POST /api/business/erp/shipment \
  "{\"so_id\":\"$SO2_ID\",\"warehouse_id\":\"$WH\",\"items\":[{\"so_item_id\":\"$(uuidgen 2>/dev/null || echo 00000000-0000-0000-0000-000000000000)\",\"qty\":1}]}"
call 'POST /shipment（仓库不存在）→ 400' 400 POST /api/business/erp/shipment \
  "{\"so_id\":\"$SO2_ID\",\"warehouse_id\":\"$(uuidgen 2>/dev/null || echo 00000000-0000-0000-0000-000000000000)\"}"
call 'POST /shipment（so_id 不是 UUID）→ 400' 400 POST /api/business/erp/shipment \
  "{\"so_id\":\"not-a-uuid\",\"warehouse_id\":\"$WH\"}"

say '⑨ 状态机：DRAFT → POSTED 不是合法边（必须先提交待过账）→ 409'
call 'PATCH /status（DRAFT → POSTED 跳步）→ 409' 409 PATCH \
  "/api/business/erp/shipment/$SH_ID/status" '{"target_status":"POSTED"}'
call 'PATCH /status（target=SHIPPED 域外值）→ 400' 400 PATCH \
  "/api/business/erp/shipment/$SH_ID/status" '{"target_status":"SHIPPED"}'

say '⑩ 提交待过账：DRAFT → PENDING_POST'
call 'PATCH /status（→ PENDING_POST）' 200 PATCH \
  "/api/business/erp/shipment/$SH_ID/status" '{"target_status":"PENDING_POST"}'
printf '%s' "$LAST" | jq -c '{status:.data.status, label:.data.status_label}'

say '⑪ 过账前的库存闸：仓库里一件都没有 → 409（不许把账扣成负数）'
# 幂等：先把这两个物料的库位账清空（上一轮跑过会留下 10/5，不清空就测不出「库存不足」）
$PSQL -c "DELETE FROM biz_inventory_location
           WHERE warehouse_id='$WH' AND material_id IN ('$MAT_A','$MAT_B')" > /dev/null
call 'PATCH /status（→ POSTED，库存 0）→ 409' 409 PATCH \
  "/api/business/erp/shipment/$SH_ID/status" '{"target_status":"POSTED"}'
printf '%s' "$LAST" | jq -r '.msg' | head -3

say '⑫ 夹具：补库位账（fifo_plan 读的就是这张表；扣减是 SO-10 的事）'
$PSQL -c "INSERT INTO biz_inventory_location (material_id, warehouse_id, location_id, qty)
          VALUES ('$MAT_A','$WH',NULL,10),('$MAT_B','$WH',NULL,5)
          ON CONFLICT (material_id, warehouse_id, location_id) DO UPDATE SET qty = EXCLUDED.qty"
$PSQL -c "SELECT material_id, qty FROM biz_inventory_location WHERE warehouse_id='$WH'"

say '⑬ 过账：PENDING_POST → POSTED（库存充足）'
call 'PATCH /status（→ POSTED）' 200 PATCH \
  "/api/business/erp/shipment/$SH_ID/status" '{"target_status":"POSTED"}'
printf '%s' "$LAST" | jq -c '{ship_no:.data.ship_no, status:.data.status, label:.data.status_label, total_qty:.data.total_qty}'

say '⑭ 终态不可再流转：POSTED → CANCELLED → 409（已过账作废要走销售出库红冲）'
call 'PATCH /status（POSTED → CANCELLED）→ 409' 409 PATCH \
  "/api/business/erp/shipment/$SH_ID/status" '{"target_status":"CANCELLED","reason":"试图作废"}'

say '⑮ 作废：草稿直接作废 200 / 待过账作废缺 reason 400 / 带 reason 200'
call 'POST /shipment（第二张：显式 1 件，草稿）' 200 POST /api/business/erp/shipment \
  "{\"so_id\":\"$SO_ID\",\"warehouse_id\":\"$WH\",\"items\":[{\"so_item_id\":\"$ITEM_B\",\"qty\":1}]}"
SH2=$(printf '%s' "$LAST" | jq -r '.data.id')
call 'PATCH /status（草稿 → CANCELLED）' 200 PATCH \
  "/api/business/erp/shipment/$SH2/status" '{"target_status":"CANCELLED"}'
printf '%s' "$LAST" | jq -c '{ship_no:.data.ship_no, status:.data.status}'

call 'POST /shipment（第三张：显式 1 件）' 200 POST /api/business/erp/shipment \
  "{\"so_id\":\"$SO_ID\",\"warehouse_id\":\"$WH\",\"items\":[{\"so_item_id\":\"$ITEM_B\",\"qty\":1}]}"
SH3=$(printf '%s' "$LAST" | jq -r '.data.id')
call 'PATCH /status（第三张 → PENDING_POST）' 200 PATCH \
  "/api/business/erp/shipment/$SH3/status" '{"target_status":"PENDING_POST"}'
call 'PATCH /status（待过账 → CANCELLED，缺 reason）→ 400' 400 PATCH \
  "/api/business/erp/shipment/$SH3/status" '{"target_status":"CANCELLED"}'
call 'PATCH /status（待过账 → CANCELLED，带 reason）' 200 PATCH \
  "/api/business/erp/shipment/$SH3/status" '{"target_status":"CANCELLED","reason":"客户临时取消，未出库"}'
printf '%s' "$LAST" | jq -c '{ship_no:.data.ship_no, status:.data.status}'

say '⑯ 库层落点核对（状态日志 / 占用释放 / 出库单未回填）'
$PSQL -c "SELECT entity_type, entity_no, from_status, to_status, reason
            FROM biz_status_log WHERE entity_type='SHIPMENT' AND entity_id IN
            ('$SH_ID','$SH2','$SH3') ORDER BY created_at"
echo '--- biz_shipment：'
$PSQL -c "SELECT ship_no, status, total_qty, stock_out_id IS NULL AS stock_out_null
            FROM biz_shipment WHERE id IN ('$SH_ID','$SH2','$SH3') ORDER BY ship_no"

printf '\n===== 结果：%s =====\n' "$([ "$FAIL" -eq 0 ] && echo '全部断言通过' || echo '有断言失败（见上方 !!!）')"
exit "$FAIL"

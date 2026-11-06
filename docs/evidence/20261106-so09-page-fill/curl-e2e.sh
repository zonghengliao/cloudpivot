#!/usr/bin/env bash
# ============================================================================
# 缺陷卡 t_45900ea7 —— 发货单列表未下发 so_no / stock_out_no 的**真 token curl 闭环**
#
# 跑法（隔离实例 127.0.0.1:18099 / 库 cloudpivot_so09fix，从 cloudpivot_so11 克隆）：
#   BASE=http://127.0.0.1:18099 DB=cloudpivot_so09fix \
#     bash docs/evidence/20261106-so09-page-fill/curl-e2e.sh
#
# 覆盖（全部 HTTP 断言 + 库内断言）：
#   ① 登录
#   ② 夹具复位
#   ③ 建订单 A（10 件）+ 订单 B（5 件）并置 APPROVED
#   ④ 建发货单 A（DRAFT，未过账）：列表 so_no 非空且 == 详情 so_no；stock_out_no 仍 null
#   ⑤ 一页里同时有「草稿」与「已过账」两行：草稿 stock_out_no=null、已过账=OUT 单号
#      （同一页两条补齐 SQL 各自命中，不是整页同值）
#   ⑥ 过账后：列表 stock_out_no == 详情 stock_out_no，so_no 仍逐字相等
#   ⑦ 订单软删：列表与详情**同时**变 null（列表 / 详情同判据，不会一边有一边没有）
#   ⑧ 出库单软删：列表与详情同时变 null
#   ⑨ 恢复：两列都回到有值（证明前面不是"永久变空"）
#
# 断言口径：本卡的病灶是「详情有、列表没有」，所以每一条都拿**列表 ↔ 详情逐字比对**，
# 不做「都有值就算过」的松断言（那是本次缺陷能混过去的原因）。
# ============================================================================
set -uo pipefail
BASE=${BASE:-http://127.0.0.1:18099}
USER_NAME=${USER_NAME:-admin}
PASSWORD=${PASSWORD:-Admin@123456}
export PGPASSWORD=${PGPASSWORD:-alun}
PSQL="psql -h 127.0.0.1 -U alun -d ${DB:-cloudpivot_so09fix} -tAq"
WH=${WH:-d1370001-0000-4000-8000-000000000001}      # WH01 总仓（种子）
MAT_A=${MAT_A:-d1370003-0000-4000-8000-000000000002} # M-W1A-002
CUST=${CUST:-d0260000-0000-4000-8000-000000000001}   # CUS-0001
TAG='SO09FILL'
FAIL=0

say() { printf '\n===== %s =====\n' "$*"; }
ok()  { printf '  ✓ %s\n' "$*"; }
bad() { printf '  ✗ %s\n' "$*"; FAIL=1; }
eq()  { if [ "$2" = "$3" ]; then ok "$1 = $2"; else bad "$1 = ${2:-<empty>}（期望 ${3:-<empty>}）"; fi; }

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
    if [ "$code" != "$expect" ]; then bad "$label 状态码 $code ≠ $expect"; fi
    LAST=$payload
}

say '① 登录（admin / Admin@123456；首登强制改密兜住）'
for try in "$PASSWORD" 'Admin@20261106'; do
    LOGIN=$(curl -s -X POST "$BASE/api/system/auth/login" -H 'Content-Type: application/json' \
            -d "{\"username\":\"$USER_NAME\",\"password\":\"$try\"}")
    if [ "$(printf '%s' "$LOGIN" | jq -r '.code')" = "0" ]; then PASSWORD="$try"; break; fi
done
TOKEN=$(printf '%s' "$LOGIN" | jq -r '.data.access_token')
[ -n "$TOKEN" ] && [ "$TOKEN" != "null" ] || { echo '!!! 登录失败'; exit 1; }
if [ "$(printf '%s' "$LOGIN" | jq -r '.data.must_change_password')" = "true" ]; then
    PASSWORD_NEW='Admin@20261106'
    CHG=$(curl -s -w '\n%{http_code}' -X PUT "$BASE/api/system/auth/password" \
          -H "Authorization: Bearer $TOKEN" -H 'Content-Type: application/json' \
          -d "{\"old_password\":\"$PASSWORD\",\"new_password\":\"$PASSWORD_NEW\"}")
    printf '改密 HTTP %s\n' "$(printf '%s' "$CHG" | tail -1)"
    LOGIN=$(curl -s -X POST "$BASE/api/system/auth/login" -H 'Content-Type: application/json' \
            -d "{\"username\":\"$USER_NAME\",\"password\":\"$PASSWORD_NEW\"}")
    TOKEN=$(printf '%s' "$LOGIN" | jq -r '.data.access_token'); PASSWORD="$PASSWORD_NEW"
fi
printf 'token_len=%s\n' "${#TOKEN}"

say '② 夹具复位（本卡独占备注前缀 SO09FILL，绝不按 SH- / SO- / OUT- 号段删）'
$PSQL <<SQL
DELETE FROM biz_shipment_item WHERE shipment_id IN (
    SELECT s.id FROM biz_shipment s JOIN biz_sales_order o ON o.id = s.so_id WHERE o.remark LIKE '$TAG%');
DELETE FROM biz_status_log WHERE (entity_type = 'SHIPMENT' AND entity_id IN (
    SELECT s.id FROM biz_shipment s JOIN biz_sales_order o ON o.id = s.so_id WHERE o.remark LIKE '$TAG%'));
DELETE FROM biz_stock_out WHERE source_id IN (
    SELECT s.id FROM biz_shipment s JOIN biz_sales_order o ON o.id = s.so_id WHERE o.remark LIKE '$TAG%');
DELETE FROM biz_shipment WHERE so_id IN (SELECT id FROM biz_sales_order WHERE remark LIKE '$TAG%');
DELETE FROM biz_sales_order_item WHERE so_id IN (SELECT id FROM biz_sales_order WHERE remark LIKE '$TAG%');
DELETE FROM biz_sales_order WHERE remark LIKE '$TAG%';
SQL

# 期初库存：过账要扣三本账，给 MAT_A 备 20（两张单 10 + 5 用不完也不用它）
$PSQL <<SQL > /dev/null
DELETE FROM biz_inventory_txn WHERE material_id = '$MAT_A';
DELETE FROM biz_inventory WHERE material_id = '$MAT_A';
DELETE FROM biz_inventory_location WHERE material_id = '$MAT_A';
INSERT INTO biz_inventory (material_id, warehouse_id, qty, amount, avg_cost)
VALUES ('$MAT_A','$WH', 20::numeric, 200, 10);
INSERT INTO biz_inventory_location (material_id, warehouse_id, location_id, qty)
VALUES ('$MAT_A','$WH', NULL, 20::numeric);
SQL
cost_qty() { $PSQL -c "SELECT COALESCE(SUM(qty),0)::numeric(18,4) FROM biz_inventory WHERE material_id='$1'"; }
eq "期初 成本账 MAT_A" "$(cost_qty "$MAT_A")" '20.0000'

say '③ 建两张订单（A=10 件 / B=5 件）并置 APPROVED'
new_order() { # $1=remark 后缀 $2=qty → 设 SO_ID / SO_NO / ITEM_ID
    local body="{\"customer_id\":\"$CUST\",\"order_date\":\"2026-11-06\",\"remark\":\"$TAG $1\",
\"items\":[{\"material_id\":\"$MAT_A\",\"qty\":$2,\"price\":100,\"tax_rate\":13}]}"
    call "POST /sales-order（$1）" 200 POST /api/business/erp/sales-order "$body"
    SO_ID=$(printf '%s' "$LAST" | jq -r '.data.id')
    SO_NO=$(printf '%s' "$LAST" | jq -r '.data.so_no')
    ITEM_ID=$(printf '%s' "$LAST" | jq -r '.data.items[0].id')
    $PSQL -c "UPDATE biz_sales_order SET status='APPROVED', updated_at=now() WHERE id='$SO_ID'" > /dev/null
    printf 'order %s / %s / item %s\n' "$SO_NO" "$SO_ID" "$ITEM_ID"
}
new_order '订单A' 10; SO_A=$SO_ID; SO_NO_A=$SO_NO; ITEM_A=$ITEM_ID
new_order '订单B' 5;  SO_B=$SO_ID; SO_NO_B=$SO_NO; ITEM_B=$ITEM_ID

say '④ 建发货单 A（DRAFT）→ 列表 so_no 必须非空且 == 详情（本卡的缺陷原状是 null）'
call 'POST /shipment（订单A）' 200 POST /api/business/erp/shipment \
  "{\"so_id\":\"$SO_A\",\"warehouse_id\":\"$WH\",\"ship_date\":\"2026-11-06\",\"remark\":\"$TAG 发货A\",
\"items\":[{\"so_item_id\":\"$ITEM_A\",\"qty\":\"10\"}]}"
SH_A=$(printf '%s' "$LAST" | jq -r '.data.id'); SH_NO_A=$(printf '%s' "$LAST" | jq -r '.data.ship_no')
eq "建单状态" "$(printf '%s' "$LAST" | jq -r '.data.status')" 'DRAFT'

call "GET /shipment/page?ship_no=$SH_NO_A" 200 GET \
  "/api/business/erp/shipment/page?ship_no=$SH_NO_A&page=1&page_size=20"
LIST_SO_NO=$(printf '%s' "$LAST" | jq -r '.data.list[0].so_no')
eq "列表命中条数" "$(printf '%s' "$LAST" | jq -r '.data.total')" '1'
eq "列表 so_no 非空（缺陷原状：null）" "$(printf '%s' "$LIST_SO_NO" | sed 's/^null$//')" "$SO_NO_A"
eq "列表 so_no == 订单号" "$LIST_SO_NO" "$SO_NO_A"
eq "列表 stock_out_no（未过账）" "$(printf '%s' "$LAST" | jq -r '.data.list[0].stock_out_no')" 'null'
eq "列表 items 恒为空" "$(printf '%s' "$LAST" | jq -r '.data.list[0].items | length')" '0'
eq "列表 customer_code 已补齐" "$(printf '%s' "$LAST" | jq -r '.data.list[0].customer_code')" 'CUS-0001'

call 'GET /shipment/{id}（A，草稿）' 200 GET "/api/business/erp/shipment/$SH_A"
DET_SO_NO=$(printf '%s' "$LAST" | jq -r '.data.so_no')
eq "详情 so_no" "$DET_SO_NO" "$SO_NO_A"
eq "列表 so_no 与详情逐字相等" "$LIST_SO_NO" "$DET_SO_NO"
eq "详情 stock_out_no（未过账）" "$(printf '%s' "$LAST" | jq -r '.data.stock_out_no')" 'null'

say '⑤ 再过账 A 之前先建发货单 B（DRAFT）——一页里同时有「已过账」与「草稿」两行'
call 'POST /shipment（订单B）' 200 POST /api/business/erp/shipment \
  "{\"so_id\":\"$SO_B\",\"warehouse_id\":\"$WH\",\"ship_date\":\"2026-11-06\",\"remark\":\"$TAG 发货B\",
\"items\":[{\"so_item_id\":\"$ITEM_B\",\"qty\":\"5\"}]}"
SH_B=$(printf '%s' "$LAST" | jq -r '.data.id'); SH_NO_B=$(printf '%s' "$LAST" | jq -r '.data.ship_no')

say '⑥ 提交 + 过账 A（生成销售出库单）'
call 'PATCH /status（A → PENDING_POST）' 200 PATCH "/api/business/erp/shipment/$SH_A/status" \
  '{"target_status":"PENDING_POST"}'
call 'PATCH /status（A → POSTED）' 200 PATCH "/api/business/erp/shipment/$SH_A/status" \
  '{"target_status":"POSTED"}'
OUT_NO=$(printf '%s' "$LAST" | jq -r '.data.stock_out_no')
printf '关联销售出库单号 = %s\n' "$OUT_NO"
eq "过账后状态" "$(printf '%s' "$LAST" | jq -r '.data.status')" 'POSTED'
eq "过账后成本账 MAT_A（20−10）" "$(cost_qty "$MAT_A")" '10.0000'

call 'GET /shipment/{id}（A，已过账）' 200 GET "/api/business/erp/shipment/$SH_A"
DET_OUT_NO=$(printf '%s' "$LAST" | jq -r '.data.stock_out_no')
DET_SO_NO=$(printf '%s' "$LAST" | jq -r '.data.so_no')
eq "详情 stock_out_no" "$DET_OUT_NO" "$OUT_NO"
eq "详情 so_no（过账后）" "$DET_SO_NO" "$SO_NO_A"

say '⑦ 关键断言：一页两行（A 已过账 / B 草稿）——so_no 各自对、stock_out_no 各不同'
call 'GET /shipment/page?so_id=A' 200 GET \
  "/api/business/erp/shipment/page?so_id=$SO_A&page=1&page_size=50"
LIST_SO_NO_A=$(printf '%s' "$LAST" | jq -r --arg id "$SH_A" '.data.list[] | select(.id == $id) | .so_no')
LIST_OUT_A=$(printf '%s' "$LAST" | jq -r --arg id "$SH_A" '.data.list[] | select(.id == $id) | .stock_out_no')
eq "列表（A 行）so_no 与详情逐字相等" "$LIST_SO_NO_A" "$DET_SO_NO"
eq "列表（A 行）stock_out_no 与详情逐字相等" "$LIST_OUT_A" "$DET_OUT_NO"

call 'GET /shipment/page?so_id=B' 200 GET \
  "/api/business/erp/shipment/page?so_id=$SO_B&page=1&page_size=50"
LIST_SO_NO_B=$(printf '%s' "$LAST" | jq -r --arg id "$SH_B" '.data.list[] | select(.id == $id) | .so_no')
LIST_OUT_B=$(printf '%s' "$LAST" | jq -r --arg id "$SH_B" '.data.list[] | select(.id == $id) | .stock_out_no')
eq "列表（B 行）so_no = 订单B 号" "$LIST_SO_NO_B" "$SO_NO_B"
eq "列表（B 行）stock_out_no（未过账）" "$LIST_OUT_B" 'null'
[ "$LIST_SO_NO_A" != "$LIST_SO_NO_B" ] && ok "同页两行 so_no 各自命中（不是整页同值）" \
  || bad "同页两行 so_no 相同（$LIST_SO_NO_A）——批量补齐按行取值失败"

say '⑧ 订单 A 软删 → 列表与详情**同时**变 null（列表/详情同判据）'
$PSQL -c "UPDATE biz_sales_order SET deleted_at = now() WHERE id='$SO_A'" > /dev/null
call 'GET /shipment/page?so_id=A（订单已软删）' 200 GET \
  "/api/business/erp/shipment/page?so_id=$SO_A&page=1&page_size=50"
eq "列表 so_no（订单软删）" "$(printf '%s' "$LAST" | jq -r --arg id "$SH_A" '.data.list[] | select(.id == $id) | .so_no')" 'null'
call 'GET /shipment/{id}（A，订单已软删）' 200 GET "/api/business/erp/shipment/$SH_A"
eq "详情 so_no（订单软删）" "$(printf '%s' "$LAST" | jq -r '.data.so_no')" 'null'
eq "详情 stock_out_no 不受订单软删影响" "$(printf '%s' "$LAST" | jq -r '.data.stock_out_no')" "$OUT_NO"
$PSQL -c "UPDATE biz_sales_order SET deleted_at = NULL WHERE id='$SO_A'" > /dev/null

say '⑨ 出库单软删 → 列表与详情**同时**变 null'
$PSQL -c "UPDATE biz_stock_out SET deleted_at = now() WHERE out_no='$OUT_NO'" > /dev/null
call 'GET /shipment/page?so_id=A（出库单已软删）' 200 GET \
  "/api/business/erp/shipment/page?so_id=$SO_A&page=1&page_size=50"
eq "列表 stock_out_no（出库单软删）" "$(printf '%s' "$LAST" | jq -r --arg id "$SH_A" '.data.list[] | select(.id == $id) | .stock_out_no')" 'null'
call 'GET /shipment/{id}（A，出库单已软删）' 200 GET "/api/business/erp/shipment/$SH_A"
eq "详情 stock_out_no（出库单软删）" "$(printf '%s' "$LAST" | jq -r '.data.stock_out_no')" 'null'

say '⑩ 恢复出库单 → 两列都回到有值（证明前面不是永久变空）'
$PSQL -c "UPDATE biz_stock_out SET deleted_at = NULL WHERE out_no='$OUT_NO'" > /dev/null
call 'GET /shipment/page?so_id=A（恢复后）' 200 GET \
  "/api/business/erp/shipment/page?so_id=$SO_A&page=1&page_size=50"
eq "列表 so_no（恢复）" "$(printf '%s' "$LAST" | jq -r --arg id "$SH_A" '.data.list[] | select(.id == $id) | .so_no')" "$SO_NO_A"
eq "列表 stock_out_no（恢复）" "$(printf '%s' "$LAST" | jq -r --arg id "$SH_A" '.data.list[] | select(.id == $id) | .stock_out_no')" "$OUT_NO"

say '⑪ 回归：客户 / 仓库编码仍在（这次改动没碰掉既有批量补齐）'
call 'GET /shipment/page（编码列）' 200 GET \
  "/api/business/erp/shipment/page?so_id=$SO_A&page=1&page_size=50"
eq "customer_code" "$(printf '%s' "$LAST" | jq -r '.data.list[0].customer_code')" 'CUS-0001'
eq "warehouse_code" "$(printf '%s' "$LAST" | jq -r '.data.list[0].warehouse_code')" 'WH01'

printf '\n===== 结果：FAIL=%s =====\n' "$FAIL"
exit "$FAIL"

#!/usr/bin/env bash
# ============================================================================
# SO-11（t_b143cd85）发货单前端页面：**页面消费的 5 个接口**真实 token curl 闭环实测
#
# 跑法（实例在 127.0.0.1:18099，库 cloudpivot_so11 —— 本卡从 cloudpivot_so10 克隆）：
#   BASE=http://127.0.0.1:18099 DB=cloudpivot_so11 \
#     bash docs/evidence/20261106-so11-shipment-ui/curl-e2e.sh
#
# 覆盖（全是 HTTP 断言 + 库内断言；请求体形状与前端 `buildCreatePayload` 逐字一致）：
#   ① 登录（admin，首登强制改密兜住）
#   ② 夹具复位 + 期初库存（成本账 / 库位账各 10）
#   ③ 订单下拉的数据源：`GET /sales-order/page?status=APPROVED` 能查到刚建的订单
#   ④ `GET /shipment/shippable/{so_id}`：只带未发完的行，`remaining = ordered - delivered - reserved`
#   ⑤ `POST /shipment`（前端 payload 形状：显式 items / qty 为字符串 / 带库位与行备注）→ 200 DRAFT
#   ⑥ `GET /shipment/page`：单号模糊 + 状态精确命中；**列表行 items=[] 且 stock_out_no=null**
#   ⑦ `GET /shipment/{id}`：so_no / customer_name / warehouse_name 齐全，items 1 行
#   ⑧ `PATCH /status` → PENDING_POST（不动库存）
#   ⑨ `PATCH /status` → POSTED：**stock_out_no = OUT-yyyyMMdd-000001**（前端「关联出库单号」直接显示它）
#   ⑩ 库内：三本账 10→0、出库单 source=SALES、成本结转凭证、delivered_qty=10、订单 OUTBOUND
#   ⑪ 重复过账 → 409，库存不再变
#   ⑫ 作废两条边：DRAFT→CANCELLED 不要原因；PENDING_POST→CANCELLED 缺 reason 400 / 带 reason 200
#   ⑬ 超发：qty=11 > 未发量 10 → 400（与前端表单闸同一口径）
#   ⑭ 库存不足：零库存物料过账 → 409（报文点名物料与数量）
#   ⑮ 订单未审批（DRAFT）建单 → 409
#
# 数据口径说明：期初库存与「订单置 APPROVED」直接写 SQL —— 本卡验的是**页面消费的接口契约**
# 与过账后的可见结果；入库侧凭证/审批链属于别的卡（与 SO-10 证据同一口径）。
# ============================================================================
set -uo pipefail
BASE=${BASE:-http://127.0.0.1:18099}
USER_NAME=${USER_NAME:-admin}
PASSWORD=${PASSWORD:-Admin@123456}
export PGPASSWORD=${PGPASSWORD:-alun}
PSQL="psql -h 127.0.0.1 -U alun -d ${DB:-cloudpivot_so11} -tAq"
WH=${WH:-d1370001-0000-4000-8000-000000000001}      # WH01 总仓（种子）
MAT_A=${MAT_A:-d1370003-0000-4000-8000-000000000002} # M-W1A-002（有货）
MAT_B=${MAT_B:-d1370003-0000-4000-8000-000000000005} # M-W1A-005（零库存）
CUST=${CUST:-d0260000-0000-4000-8000-000000000001}   # CUS-0001
LOC=${LOC:-d1370002-0000-4000-8000-000000000001}     # WH01 的默认库位（biz_location.id）
TAG='SO11E2E'
FAIL=0

say() { printf '\n===== %s =====\n' "$*"; }
ok()  { printf '  ✓ %s\n' "$*"; }
bad() { printf '  ✗ %s\n' "$*"; FAIL=1; }
eq()  { if [ "$2" = "$3" ]; then ok "$1 = $2"; else bad "$1 = $2（期望 $3）"; fi; }
# 数值比较（后端 numeric 有无标度不一致：`SUM` 空表可能回 `0`，有值回 `0.0000`）
neq() { if [ "$(awk -v a="$2" -v b="$3" 'BEGIN{print (a+0==b+0)?"1":"0"}')" = '1' ]; then ok "$1 = $2"; else bad "$1 = $2（期望 $3）"; fi; }

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

say '② 夹具复位 + 期初库存（成本账 / 库位账各 10、另一物料 0）'
$PSQL <<SQL
DELETE FROM biz_voucher WHERE source_type = 'STOCK_OUT' AND source_id IN (
    SELECT id FROM biz_stock_out WHERE source_id IN (
        SELECT s.id FROM biz_shipment s JOIN biz_sales_order o ON o.id = s.so_id
         WHERE o.remark LIKE '$TAG%'));
DELETE FROM biz_status_log WHERE
    (entity_type = 'STOCK_OUT' AND entity_id IN (
        SELECT id FROM biz_stock_out WHERE source_id IN (
            SELECT s.id FROM biz_shipment s JOIN biz_sales_order o ON o.id = s.so_id
             WHERE o.remark LIKE '$TAG%')))
 OR (entity_type = 'SHIPMENT' AND entity_id IN (
        SELECT s.id FROM biz_shipment s JOIN biz_sales_order o ON o.id = s.so_id
         WHERE o.remark LIKE '$TAG%'))
 OR (entity_type = 'SALE_ORDER' AND entity_id IN (
        SELECT id FROM biz_sales_order WHERE remark LIKE '$TAG%'));
DELETE FROM biz_stock_out WHERE source_id IN (
    SELECT s.id FROM biz_shipment s JOIN biz_sales_order o ON o.id = s.so_id
     WHERE o.remark LIKE '$TAG%');
DELETE FROM biz_shipment WHERE so_id IN (SELECT id FROM biz_sales_order WHERE remark LIKE '$TAG%');
DELETE FROM biz_sales_order WHERE remark LIKE '$TAG%';
DELETE FROM biz_inventory_txn WHERE material_id IN ('$MAT_A','$MAT_B');
DELETE FROM biz_inventory WHERE material_id IN ('$MAT_A','$MAT_B');
DELETE FROM biz_inventory_location WHERE material_id IN ('$MAT_A','$MAT_B');
SQL
seed_stock() { # $1=material $2=qty
    $PSQL -c "INSERT INTO biz_inventory (material_id, warehouse_id, qty, amount, avg_cost)
              VALUES ('$1','$WH', $2::numeric, ($2::numeric * 10), 10)
              ON CONFLICT (material_id, warehouse_id, COALESCE(location_id,'00000000-0000-0000-0000-000000000000'::uuid))
              DO UPDATE SET qty = EXCLUDED.qty, amount = EXCLUDED.amount, avg_cost = EXCLUDED.avg_cost;
              INSERT INTO biz_inventory_location (material_id, warehouse_id, location_id, qty)
              VALUES ('$1','$WH', NULL, $2::numeric)
              ON CONFLICT (material_id, warehouse_id, location_id) DO UPDATE SET qty = EXCLUDED.qty;" > /dev/null
}
seed_stock "$MAT_A" 10
cost_qty() { $PSQL -c "SELECT COALESCE(SUM(qty),0)::numeric(18,4) FROM biz_inventory WHERE material_id='$1'"; }
loc_qty()  { $PSQL -c "SELECT COALESCE(SUM(qty),0)::numeric(18,4) FROM biz_inventory_location WHERE material_id='$1'"; }
out_txn()  { $PSQL -c "SELECT COALESCE(SUM(qty),0)::numeric(18,4) FROM biz_inventory_txn
                        WHERE material_id='$1' AND doc_type='STOCK_OUT'"; }
eq "期初 成本账 MAT_A" "$(cost_qty "$MAT_A")" '10.0000'
eq "期初 库位账 MAT_A" "$(loc_qty "$MAT_A")" '10.0000'
eq "期初 MAT_B（零库存）" "$(cost_qty "$MAT_B")" '0.0000'

# 建订单；$1=remark 后缀 $2=material $3=qty $4=目标状态（APPROVED / DRAFT）
new_order() {
    local body="{\"customer_id\":\"$CUST\",\"order_date\":\"2026-11-06\",\"remark\":\"$TAG $1\",
\"items\":[{\"material_id\":\"$2\",\"qty\":$3,\"price\":100,\"tax_rate\":13}]}"
    call "POST /sales-order（$1）" 200 POST /api/business/erp/sales-order "$body"
    SO_ID=$(printf '%s' "$LAST" | jq -r '.data.id')
    SO_NO=$(printf '%s' "$LAST" | jq -r '.data.so_no')
    ITEM_ID=$(printf '%s' "$LAST" | jq -r '.data.items[0].id')
    if [ "${4:-APPROVED}" = 'APPROVED' ]; then
        $PSQL -c "UPDATE biz_sales_order SET status='APPROVED', updated_at=now() WHERE id='$SO_ID'" > /dev/null
    fi
    printf 'order %s / %s / item %s 状态=%s\n' "$SO_NO" "$SO_ID" "$ITEM_ID" "${4:-APPROVED}"
}

say '③ 订单下拉数据源：GET /sales-order/page?status=APPROVED'
new_order '正常' "$MAT_A" 10
call 'GET /sales-order/page?status=APPROVED' 200 GET '/api/business/erp/sales-order/page?status=APPROVED&page=1&page_size=200'
eq "订单下拉命中刚建的订单" \
   "$(printf '%s' "$LAST" | jq -r --arg id "$SO_ID" '[.data.list[] | select(.id == $id)] | length')" '1'

say '④ GET /shipment/shippable/{so_id}：只带未发完的行'
call 'GET /shipment/shippable/{so_id}' 200 GET "/api/business/erp/shipment/shippable/$SO_ID"
eq "可发行数" "$(printf '%s' "$LAST" | jq -r '.data | length')" '1'
eq "行号" "$(printf '%s' "$LAST" | jq -r '.data[0].line_no')" '1'
eq "订单量" "$(printf '%s' "$LAST" | jq -r '.data[0].ordered')" '10.0000'
eq "已发" "$(printf '%s' "$LAST" | jq -r '.data[0].delivered')" '0.0000'
eq "占用" "$(printf '%s' "$LAST" | jq -r '.data[0].reserved' | sed 's/^$/0/')" '0'
eq "未发量" "$(printf '%s' "$LAST" | jq -r '.data[0].remaining')" '10.0000'

say '⑤ POST /shipment（前端 payload 形状：显式 items / qty 字符串 / 行备注；库位留空 = 整仓 FIFO）'
call 'POST /shipment' 200 POST /api/business/erp/shipment \
  "{\"so_id\":\"$SO_ID\",\"warehouse_id\":\"$WH\",\"ship_date\":\"2026-11-06\",\"remark\":\"$TAG 正常发货\",
\"items\":[{\"so_item_id\":\"$ITEM_ID\",\"qty\":\"10\",\"remark\":\"整托发\"}]}"
SH_ID=$(printf '%s' "$LAST" | jq -r '.data.id')
SH_NO=$(printf '%s' "$LAST" | jq -r '.data.ship_no')
eq "建单状态" "$(printf '%s' "$LAST" | jq -r '.data.status')" 'DRAFT'
eq "状态中文（后端给）" "$(printf '%s' "$LAST" | jq -r '.data.status_label')" '草稿'
eq "合计数量" "$(printf '%s' "$LAST" | jq -r '.data.total_qty')" '10.0000'
eq "单号格式 SH-yyyyMMdd-nnnnnn" "$(printf '%s' "$SH_NO" | grep -cqE '^SH-[0-9]{8}-[0-9]{6}$' && echo ok)" 'ok'
eq "回包 stock_out_no 为空（未过账）" "$(printf '%s' "$LAST" | jq -r '.data.stock_out_no')" 'null'
eq "明细库位留空 = 整仓 FIFO" "$(printf '%s' "$LAST" | jq -r '.data.items[0].location_id')" 'null'
eq "明细带行备注" "$(printf '%s' "$LAST" | jq -r '.data.items[0].remark')" '整托发'

say '⑥ GET /shipment/page：单号模糊 + 状态精确；列表行不返回明细与出库单号'
call "GET /shipment/page?ship_no=$SH_NO&status=DRAFT" 200 \
  GET "/api/business/erp/shipment/page?ship_no=$SH_NO&status=DRAFT&page=1&page_size=20"
eq "命中条数" "$(printf '%s' "$LAST" | jq -r '.data.total')" '1'
eq "列表行单号" "$(printf '%s' "$LAST" | jq -r '.data.list[0].ship_no')" "$SH_NO"
# ⚠️ 已知后端缺口（本卡实测发现，已开缺陷卡给 backend-developer）：
#    `shipment_handler::page_shipment` 只批量补了 customer_* / warehouse_*，
#    **没补 `so_no`**（`ShipmentRes::of` 里 `so_no: None`，列表分支未回填），
#    而 `ShipmentRes` 的文档注释写的是「so_no / customer_code / warehouse_code 会批量补齐」。
#    前端列表的「销售订单」列因此只能显示兜底文案（详情里有 so_no）。
eq "列表行 so_no（后端缺口：详情有、列表没有）" "$(printf '%s' "$LAST" | jq -r '.data.list[0].so_no')" 'null'
eq "列表行客户名已补齐" \
   "$(printf '%s' "$LAST" | jq -r '.data.list[0].customer_name | length > 0')" 'true'
eq "列表行仓库名已补齐" \
   "$(printf '%s' "$LAST" | jq -r '.data.list[0].warehouse_name | length > 0')" 'true'
eq "列表行 items 恒为空" "$(printf '%s' "$LAST" | jq -r '.data.list[0].items | length')" '0'
eq "列表行 stock_out_no 恒为 null" "$(printf '%s' "$LAST" | jq -r '.data.list[0].stock_out_no')" 'null'

say '⑦ GET /shipment/{id}：详情装配（so_no / 客户 / 仓库 / 明细）'
call 'GET /shipment/{id}' 200 GET "/api/business/erp/shipment/$SH_ID"
eq "详情单号" "$(printf '%s' "$LAST" | jq -r '.data.ship_no')" "$SH_NO"
eq "详情 so_no" "$(printf '%s' "$LAST" | jq -r '.data.so_no')" "$SO_NO"
eq "详情明细行数" "$(printf '%s' "$LAST" | jq -r '.data.items | length')" '1'
eq "详情明细物料" "$(printf '%s' "$LAST" | jq -r '.data.items[0].material_id')" "$MAT_A"
eq "详情 stock_out_no（未过账）" "$(printf '%s' "$LAST" | jq -r '.data.stock_out_no')" 'null'

say '⑧ PATCH /shipment/{id}/status → PENDING_POST（不动库存）'
call 'PATCH /status（→ PENDING_POST）' 200 PATCH "/api/business/erp/shipment/$SH_ID/status" \
  '{"target_status":"PENDING_POST"}'
eq "状态" "$(printf '%s' "$LAST" | jq -r '.data.status')" 'PENDING_POST'
eq "状态中文" "$(printf '%s' "$LAST" | jq -r '.data.status_label')" '待过账'
eq "提交后库存未动（成本账）" "$(cost_qty "$MAT_A")" '10.0000'
eq "提交后没有出库单" "$($PSQL -c "SELECT count(*) FROM biz_stock_out WHERE source_id='$SH_ID'")" '0'

say '⑨ PATCH /shipment/{id}/status → POSTED（过账：生成销售出库单 + 扣三账 + 回写）'
call 'PATCH /status（→ POSTED）' 200 PATCH "/api/business/erp/shipment/$SH_ID/status" \
  '{"target_status":"POSTED"}'
eq "过账后状态" "$(printf '%s' "$LAST" | jq -r '.data.status')" 'POSTED'
eq "过账后状态中文" "$(printf '%s' "$LAST" | jq -r '.data.status_label')" '已过账'
eq "回包 stock_out_id 非空" "$(printf '%s' "$LAST" | jq -r '.data.stock_out_id != null')" 'true'
SOUT_NO=$(printf '%s' "$LAST" | jq -r '.data.stock_out_no')
printf '关联销售出库单号 = %s\n' "$SOUT_NO"
eq "关联出库单号格式 OUT-yyyyMMdd-nnnnnn" \
   "$(printf '%s' "$SOUT_NO" | grep -cqE '^OUT-[0-9]{8}-[0-9]{6}$' && echo ok)" 'ok'

say '⑩ 库内断言：三本账 / 出库单来源 / 成本结转凭证 / delivered_qty / 订单状态'
eq "出库单 status" "$($PSQL -c "SELECT status FROM biz_stock_out WHERE source_id='$SH_ID'")" 'POSTED'
eq "出库单 source_type" "$($PSQL -c "SELECT source_type FROM biz_stock_out WHERE source_id='$SH_ID'")" 'SALES'
eq "成本结转凭证已生成" "$($PSQL -c "SELECT (voucher_id IS NOT NULL)::text FROM biz_stock_out WHERE source_id='$SH_ID'")" 'true'
eq "成本账 MAT_A（10−10）" "$(cost_qty "$MAT_A")" '0.0000'
eq "库位账 MAT_A（10−10）" "$(loc_qty "$MAT_A")" '0.0000'
eq "出库流水 MAT_A" "$(out_txn "$MAT_A")" '10.0000'
eq "订单行 delivered_qty" "$($PSQL -c "SELECT delivered_qty FROM biz_sales_order_item WHERE id='$ITEM_ID'")" '10.0000'
eq "订单状态（全发完）" "$($PSQL -c "SELECT status FROM biz_sales_order WHERE id='$SO_ID'")" 'OUTBOUND'

say '⑪ 详情回读 stock_out_no（前端「关联出库单号」的数据源）'
call 'GET /shipment/{id}（过账后）' 200 GET "/api/business/erp/shipment/$SH_ID"
eq "详情 stock_out_no == 过账回包" "$(printf '%s' "$LAST" | jq -r '.data.stock_out_no')" "$SOUT_NO"
eq "详情 stock_out_id 已回填" "$(printf '%s' "$LAST" | jq -r '.data.stock_out_id != null')" 'true'

say '⑫ 重复过账 → 409（幂等：库存与出库单都不变）'
call 'PATCH /status（POSTED 再来一次）→ 409' 409 PATCH "/api/business/erp/shipment/$SH_ID/status" \
  '{"target_status":"POSTED"}'
eq "成本账仍 0" "$(cost_qty "$MAT_A")" '0.0000'
eq "销售出库单仍 1 张" "$($PSQL -c "SELECT count(*) FROM biz_stock_out WHERE source_id='$SH_ID'")" '1'
eq "delivered_qty 没加两遍" "$($PSQL -c "SELECT delivered_qty FROM biz_sales_order_item WHERE id='$ITEM_ID'")" '10.0000'

say '⑬ 作废两条边：DRAFT 不要原因；PENDING_POST 缺 reason → 400'
seed_stock "$MAT_A" 10
new_order '作废-草稿' "$MAT_A" 2
call 'POST /shipment（草稿）' 200 POST /api/business/erp/shipment \
  "{\"so_id\":\"$SO_ID\",\"warehouse_id\":\"$WH\",\"ship_date\":\"2026-11-06\",\"items\":[{\"so_item_id\":\"$ITEM_ID\"}]}"
SH_D=$(printf '%s' "$LAST" | jq -r '.data.id')
eq "items 里 qty 留空 = 服务端带全部未发量" "$(printf '%s' "$LAST" | jq -r '.data.total_qty')" '2.0000'
call 'PATCH /status（DRAFT → CANCELLED，无原因）' 200 PATCH "/api/business/erp/shipment/$SH_D/status" \
  '{"target_status":"CANCELLED"}'
eq "作废后状态" "$(printf '%s' "$LAST" | jq -r '.data.status')" 'CANCELLED'
eq "作废后中文（后端：已取消）" "$(printf '%s' "$LAST" | jq -r '.data.status_label')" '已取消'
eq "草稿作废不动库存" "$(cost_qty "$MAT_A")" '10.0000'

new_order '作废-待过账' "$MAT_A" 2
call 'POST /shipment（提交前）' 200 POST /api/business/erp/shipment \
  "{\"so_id\":\"$SO_ID\",\"warehouse_id\":\"$WH\",\"ship_date\":\"2026-11-06\",\"items\":[{\"so_item_id\":\"$ITEM_ID\"}]}"
SH_P=$(printf '%s' "$LAST" | jq -r '.data.id')
call 'PATCH /status（→ PENDING_POST）' 200 PATCH "/api/business/erp/shipment/$SH_P/status" \
  '{"target_status":"PENDING_POST"}'
call 'PATCH /status（PENDING_POST → CANCELLED 无原因）→ 400' 400 \
  PATCH "/api/business/erp/shipment/$SH_P/status" '{"target_status":"CANCELLED"}'
call 'PATCH /status（PENDING_POST → CANCELLED 带原因）' 200 \
  PATCH "/api/business/erp/shipment/$SH_P/status" \
  '{"target_status":"CANCELLED","reason":"客户改单，本单不发"}'
eq "作废后状态" "$(printf '%s' "$LAST" | jq -r '.data.status')" 'CANCELLED'
eq "未过账作废仍有 0 张出库单" "$($PSQL -c "SELECT count(*) FROM biz_stock_out WHERE source_id='$SH_P'")" '0'

say '⑭ 超发：qty=11 > 未发量 10 → 400（与前端表单闸同一口径）'
new_order '超发' "$MAT_A" 10
call 'POST /shipment（qty 11）→ 400' 400 POST /api/business/erp/shipment \
  "{\"so_id\":\"$SO_ID\",\"warehouse_id\":\"$WH\",\"ship_date\":\"2026-11-06\",
\"items\":[{\"so_item_id\":\"$ITEM_ID\",\"qty\":\"11\"}]}"

say '⑮ 库存不足：零库存物料过账 → 409，且一分账都不动'
new_order '缺货' "$MAT_B" 5
call 'POST /shipment（MAT_B 5 件）' 200 POST /api/business/erp/shipment \
  "{\"so_id\":\"$SO_ID\",\"warehouse_id\":\"$WH\",\"ship_date\":\"2026-11-06\",
\"items\":[{\"so_item_id\":\"$ITEM_ID\",\"qty\":\"5\"}]}"
SH_S=$(printf '%s' "$LAST" | jq -r '.data.id')
call 'PATCH /status（→ PENDING_POST）' 200 PATCH "/api/business/erp/shipment/$SH_S/status" \
  '{"target_status":"PENDING_POST"}'
call 'PATCH /status（→ POSTED，库存 0）→ 409' 409 PATCH "/api/business/erp/shipment/$SH_S/status" \
  '{"target_status":"POSTED"}'
printf '%s\n' "$LAST" | jq -r '.msg'
eq "成本账 MAT_B 仍 0" "$(cost_qty "$MAT_B")" '0.0000'
eq "没有留下半张销售出库单" "$($PSQL -c "SELECT count(*) FROM biz_stock_out WHERE source_id='$SH_S'")" '0'
eq "发货单仍在 PENDING_POST" "$($PSQL -c "SELECT status FROM biz_shipment WHERE id='$SH_S'")" 'PENDING_POST'

say '⑯ 订单未审批（DRAFT）建单 → 409（前端订单下拉只列 APPROVED / PARTIAL_OUT 的原因）'
new_order '未审批' "$MAT_A" 3 'DRAFT'
call 'POST /shipment（草稿订单）→ 409' 409 POST /api/business/erp/shipment \
  "{\"so_id\":\"$SO_ID\",\"warehouse_id\":\"$WH\",\"ship_date\":\"2026-11-06\",
\"items\":[{\"so_item_id\":\"$ITEM_ID\"}]}"

say '⑰ 指定库位（前端「发货库位」下拉）：目标库位有账 → 过账按该库位扣减'
$PSQL -c "DELETE FROM biz_inventory WHERE material_id='$MAT_A';
          DELETE FROM biz_inventory_location WHERE material_id='$MAT_A';" > /dev/null
seed_stock_loc() { # $1=material $2=qty $3=location_id（字典 id）
    # 指定库位要**两本账同时就位**：`biz_inventory_location`（库位账，location_id = 字典 id）
    # 与 `biz_inventory`（成本账，location_id = **库位账行 id**）。两套 id 混用会 409
    # （`stockout_model::fifo_location_row` 的注释写明：账行 id 只在具体库位口径下与成本账一一对应）。
    $PSQL -c "WITH ins AS (
                INSERT INTO biz_inventory_location (material_id, warehouse_id, location_id, qty)
                VALUES ('$1','$WH','$3'::uuid, $2::numeric)
                ON CONFLICT (material_id, warehouse_id, location_id) DO UPDATE SET qty = EXCLUDED.qty
                RETURNING id
              )
              INSERT INTO biz_inventory (material_id, warehouse_id, location_id, qty, amount, avg_cost)
              SELECT '$1','$WH', ins.id, $2::numeric, ($2::numeric * 10), 10 FROM ins
              ON CONFLICT (material_id, warehouse_id, COALESCE(location_id,'00000000-0000-0000-0000-000000000000'::uuid))
              DO UPDATE SET qty = EXCLUDED.qty, amount = EXCLUDED.amount, avg_cost = EXCLUDED.avg_cost;" > /dev/null
}
loc_qty_at() { $PSQL -c "SELECT COALESCE(SUM(qty),0)::numeric(18,4) FROM biz_inventory_location WHERE material_id='$1' AND location_id='$2'::uuid"; }
seed_stock_loc "$MAT_A" 5 "$LOC"
eq "期初 成本账 MAT_A" "$(cost_qty "$MAT_A")" '5.0000'
eq "期初 库位账（该库位）" "$(loc_qty_at "$MAT_A" "$LOC")" '5.0000'
new_order '库位发货' "$MAT_A" 5
call 'POST /shipment（带 location_id）' 200 POST /api/business/erp/shipment \
  "{\"so_id\":\"$SO_ID\",\"warehouse_id\":\"$WH\",\"ship_date\":\"2026-11-06\",
\"items\":[{\"so_item_id\":\"$ITEM_ID\",\"qty\":\"5\",\"location_id\":\"$LOC\"}]}"
eq "明细库位 = biz_location.id" "$(printf '%s' "$LAST" | jq -r '.data.items[0].location_id')" "$LOC"
SH_L=$(printf '%s' "$LAST" | jq -r '.data.id')
call 'PATCH /status（→ PENDING_POST）' 200 PATCH "/api/business/erp/shipment/$SH_L/status" '{"target_status":"PENDING_POST"}'
call 'PATCH /status（→ POSTED，指定库位）' 200 PATCH "/api/business/erp/shipment/$SH_L/status" '{"target_status":"POSTED"}'
eq "成本账（5−5）" "$(cost_qty "$MAT_A")" '0.0000'
eq "该库位账（5−5）" "$(loc_qty_at "$MAT_A" "$LOC")" '0.0000'

say '⑱ 指定库位但该库位无账 → 409（绝不静默改走 FIFO）'
$PSQL -c "DELETE FROM biz_inventory_location WHERE material_id='$MAT_A';" > /dev/null
seed_stock "$MAT_A" 5   # 只有「整仓」账行（location_id = NULL）
new_order '库位无账' "$MAT_A" 5
call 'POST /shipment（带一个没有账的库位）' 200 POST /api/business/erp/shipment \
  "{\"so_id\":\"$SO_ID\",\"warehouse_id\":\"$WH\",\"ship_date\":\"2026-11-06\",
\"items\":[{\"so_item_id\":\"$ITEM_ID\",\"qty\":\"5\",\"location_id\":\"$LOC\"}]}"
SH_N=$(printf '%s' "$LAST" | jq -r '.data.id')
call 'PATCH /status（→ PENDING_POST）' 200 PATCH "/api/business/erp/shipment/$SH_N/status" '{"target_status":"PENDING_POST"}'
call 'PATCH /status（→ POSTED，库位无账）→ 409' 409 PATCH "/api/business/erp/shipment/$SH_N/status" '{"target_status":"POSTED"}'
printf '%s\n' "$LAST" | jq -r '.msg'
eq "报文点名库位" "$(printf '%s' "$LAST" | jq -r '.msg | contains("库位")')" 'true'
eq "库存没被动（整仓账仍 5）" "$(cost_qty "$MAT_A")" '5.0000'
eq "没有留下出库单" "$($PSQL -c "SELECT count(*) FROM biz_stock_out WHERE source_id='$SH_N'")" '0'

printf '\n===== 结果：%s =====\n' "$([ $FAIL -eq 0 ] && echo '全部断言通过' || echo '存在失败断言')"
exit $FAIL

#!/usr/bin/env bash
# ============================================================================
# SO-10（t_83e23c1d）发货过账 → 销售出库 → 库存扣减：真实 token curl 闭环实测
#
# 跑法（实例已在 127.0.0.1:18093，库 cloudpivot_so10）：
#   bash docs/evidence/20261106-so10-shipment-posting/curl-e2e.sh
#
# 覆盖（每条都是 HTTP 断言 + 库内断言）：
#   ① 登录（admin 首登强制改密）
#   ② 夹具复位 + 期初库存（成本账 / 库位账各 10、另一物料 0 件）
#   ③ 正常发货：订单 10 件 → 发货单 → 提交 → **过账 200**
#        · 销售出库单：status=POSTED / out_type=OTHER / source_type=SALES / 有成本结转凭证
#        · 三账：成本账 10→0、库位账 10→0、出库流水 −10（doc_type=STOCK_OUT）
#        · 回写：biz_shipment.stock_out_id 有值、订单行 delivered_qty=10、订单 OUTBOUND
#   ④ 重复过账：同一张单再 POSTED → 409，库存不再变、销售出库单仍只有 1 张
#   ⑤ 部分发货：订单 10 件先发 3 → PARTIAL_OUT（库存 10→7）→ 再发 7 → OUTBOUND（7→0）
#   ⑥ 库存不足：零库存物料要发 5 → 409，且三账 / 出库单 / delivered_qty / 订单状态一分不动
#
# 数据口径说明（为什么有的夹具直接写 SQL）：
#   · 期初库存直接写 cost/location 两本账：本卡验的是**出库侧**，入库侧（收货过账的
#     凭证 / 流水）属于别的模块；直接建两本账可以精确控制「够 / 不够」两种场景。
#     三账一致性在本脚本里按「成本账 == 库位账 == 期初 − 出库量」+「出库流水 = 出库量」判定。
#   · 订单置 APPROVED：审批链（SO-06）要「审批人 ≠ 发起人」，admin 单账号走不通三档审批；
#     本卡验的是过账，不是审批，所以直接改状态（与 SO-09 证据同一口径）。
# ============================================================================
set -uo pipefail
BASE=${BASE:-http://127.0.0.1:18093}
USER_NAME=${USER_NAME:-admin}
PASSWORD=${PASSWORD:-Admin@123456}
export PGPASSWORD=${PGPASSWORD:-alun}
PSQL="psql -h 127.0.0.1 -U alun -d ${DB:-cloudpivot_so10} -tAq"
WH=${WH:-d1370001-0000-4000-8000-000000000001}      # WH01 总仓（种子）
MAT_A=${MAT_A:-d1370003-0000-4000-8000-000000000002} # M-W1A-002 原料B（有货）
MAT_B=${MAT_B:-d1370003-0000-4000-8000-000000000005} # M-W1A-005 成品E（零库存）
CUST=${CUST:-d0260000-0000-4000-8000-000000000001}   # CUS-0001 北京云枢
TAG='SO10E2E'                                        # 夹具订单备注前缀（复位按它定位）
FAIL=0

say() { printf '\n===== %s =====\n' "$*"; }
ok()  { printf '  ✓ %s\n' "$*"; }
bad() { printf '  ✗ %s\n' "$*"; FAIL=1; }

# 断言：$1=标签 $2=实际 $3=期望
eq() { if [ "$2" = "$3" ]; then ok "$1 = $2"; else bad "$1 = $2（期望 $3）"; fi; }

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

say '① 登录（admin / Admin@123456）→ 首登强制改密'
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

say '② 夹具复位 + 期初库存（成本账 / 库位账）'
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

# 建一张订单并置 APPROVED，返回 id 写在全局 SO_ID / ITEM_IDS
new_order() { # $1=remark后缀 $2=material $3=qty
    local body="{\"customer_id\":\"$CUST\",\"order_date\":\"2026-11-06\",\"remark\":\"$TAG $1\",\
\"items\":[{\"material_id\":\"$2\",\"qty\":$3,\"price\":100,\"tax_rate\":13}]}"
    call "POST /sales-order（$1）" 200 POST /api/business/erp/sales-order "$body"
    SO_ID=$(printf '%s' "$LAST" | jq -r '.data.id')
    SO_NO=$(printf '%s' "$LAST" | jq -r '.data.so_no')
    ITEM_ID=$(printf '%s' "$LAST" | jq -r '.data.items[0].id')
    $PSQL -c "UPDATE biz_sales_order SET status='APPROVED', updated_at=now() WHERE id='$SO_ID'" > /dev/null
    printf 'order %s / %s / item %s 已置 APPROVED\n' "$SO_NO" "$SO_ID" "$ITEM_ID"
}

say '③ 正常发货：订单 10 件 → 发货单（带出未发完的行）→ 提交 → 过账'
new_order '正常' "$MAT_A" 10
call 'POST /shipment（items 留空 = 带出）' 200 POST /api/business/erp/shipment \
  "{\"so_id\":\"$SO_ID\",\"warehouse_id\":\"$WH\",\"ship_date\":\"2026-11-06\",\"remark\":\"$TAG 正常发货\"}"
SH_ID=$(printf '%s' "$LAST" | jq -r '.data.id')
SH_NO=$(printf '%s' "$LAST" | jq -r '.data.ship_no')
printf 'ship_no=%s id=%s\n' "$SH_NO" "$SH_ID"
call 'PATCH /status（→ PENDING_POST）' 200 PATCH "/api/business/erp/shipment/$SH_ID/status" \
  '{"target_status":"PENDING_POST"}'
call 'PATCH /status（→ POSTED，过账）' 200 PATCH "/api/business/erp/shipment/$SH_ID/status" \
  '{"target_status":"POSTED"}'
eq "过账后发货单状态" "$(printf '%s' "$LAST" | jq -r '.data.status')" 'POSTED'
eq "回填 stock_out_id 非空" "$(printf '%s' "$LAST" | jq -r '.data.stock_out_id != null')" 'true'
SOUT_NO=$(printf '%s' "$LAST" | jq -r '.data.stock_out_no')
printf '关联销售出库单号 = %s\n' "$SOUT_NO"
case "$SOUT_NO" in OUT-*) ok "出库单号格式 $SOUT_NO";; *) bad "出库单号格式 $SOUT_NO";; esac

say '   库内断言：销售出库单 / 三账 / delivery / 订单状态'
eq "出库单 status" "$($PSQL -c "SELECT status FROM biz_stock_out WHERE source_id='$SH_ID'")" 'POSTED'
eq "出库单 out_type" "$($PSQL -c "SELECT out_type FROM biz_stock_out WHERE source_id='$SH_ID'")" 'OTHER'
eq "出库单 source_type" "$($PSQL -c "SELECT source_type FROM biz_stock_out WHERE source_id='$SH_ID'")" 'SALES'
eq "出库单行数" "$($PSQL -c "SELECT count(*) FROM biz_stock_out_item WHERE stock_out_id=(SELECT id FROM biz_stock_out WHERE source_id='$SH_ID')")" '1'
eq "成本结转凭证已生成" "$($PSQL -c "SELECT (voucher_id IS NOT NULL)::text FROM biz_stock_out WHERE source_id='$SH_ID'")" 'true'
eq "成本账 MAT_A（10−10）" "$(cost_qty "$MAT_A")" '0.0000'
eq "库位账 MAT_A（10−10）" "$(loc_qty "$MAT_A")" '0.0000'
eq "出库流水合计 MAT_A" "$(out_txn "$MAT_A")" '10.0000'
eq "订单行 delivered_qty" "$($PSQL -c "SELECT delivered_qty FROM biz_sales_order_item WHERE id='$ITEM_ID'")" '10.0000'
eq "订单状态（全发完）" "$($PSQL -c "SELECT status FROM biz_sales_order WHERE id='$SO_ID'")" 'OUTBOUND'
eq "凭证-出库单双向一致" "$($PSQL -c "SELECT count(*) FROM biz_voucher v JOIN biz_stock_out s ON s.voucher_id=v.id WHERE s.source_id='$SH_ID'")" '1'

say '④ 重复过账：同一张单再 POSTED → 409（幂等，不再扣库存）'
call 'PATCH /status（POSTED 再来一次）→ 409' 409 PATCH "/api/business/erp/shipment/$SH_ID/status" \
  '{"target_status":"POSTED"}'
eq "成本账仍为 0（没有第二遍扣减）" "$(cost_qty "$MAT_A")" '0.0000'
eq "销售出库单仍只有 1 张" "$($PSQL -c "SELECT count(*) FROM biz_stock_out WHERE source_id='$SH_ID'")" '1'
eq "delivered_qty 没被加两遍" "$($PSQL -c "SELECT delivered_qty FROM biz_sales_order_item WHERE id='$ITEM_ID'")" '10.0000'

say '⑤ 部分发货：订单 10 件 → 先发 3（PARTIAL_OUT）→ 再发 7（OUTBOUND）'
seed_stock "$MAT_A" 10
new_order '部分' "$MAT_A" 10
PART_SO=$SO_ID; PART_ITEM=$ITEM_ID
call 'POST /shipment（显式 3 件）' 200 POST /api/business/erp/shipment \
  "{\"so_id\":\"$PART_SO\",\"warehouse_id\":\"$WH\",\"ship_date\":\"2026-11-06\",\
\"items\":[{\"so_item_id\":\"$PART_ITEM\",\"qty\":3}]}"
SH_P1=$(printf '%s' "$LAST" | jq -r '.data.id')
call 'PATCH /status（→ PENDING_POST）' 200 PATCH "/api/business/erp/shipment/$SH_P1/status" '{"target_status":"PENDING_POST"}'
call 'PATCH /status（→ POSTED）' 200 PATCH "/api/business/erp/shipment/$SH_P1/status" '{"target_status":"POSTED"}'
eq "第一次部分发货后订单状态" "$($PSQL -c "SELECT status FROM biz_sales_order WHERE id='$PART_SO'")" 'PARTIAL_OUT'
eq "成本账（10−3）" "$(cost_qty "$MAT_A")" '7.0000'
eq "库位账（10−3）" "$(loc_qty "$MAT_A")" '7.0000'
eq "订单行 delivered_qty" "$($PSQL -c "SELECT delivered_qty FROM biz_sales_order_item WHERE id='$PART_ITEM'")" '3.0000'

call 'POST /shipment（再发 7 件）' 200 POST /api/business/erp/shipment \
  "{\"so_id\":\"$PART_SO\",\"warehouse_id\":\"$WH\",\"ship_date\":\"2026-11-06\",\
\"items\":[{\"so_item_id\":\"$PART_ITEM\",\"qty\":7}]}"
SH_P2=$(printf '%s' "$LAST" | jq -r '.data.id')
call 'PATCH /status（→ PENDING_POST）' 200 PATCH "/api/business/erp/shipment/$SH_P2/status" '{"target_status":"PENDING_POST"}'
call 'PATCH /status（→ POSTED）' 200 PATCH "/api/business/erp/shipment/$SH_P2/status" '{"target_status":"POSTED"}'
eq "第二次发货后订单状态" "$($PSQL -c "SELECT status FROM biz_sales_order WHERE id='$PART_SO'")" 'OUTBOUND'
eq "成本账（7−7）" "$(cost_qty "$MAT_A")" '0.0000'
eq "订单行 delivered_qty 累计" "$($PSQL -c "SELECT delivered_qty FROM biz_sales_order_item WHERE id='$PART_ITEM'")" '10.0000'
eq "两笔出库流水累计（③10 + ⑤10）" "$(out_txn "$MAT_A")" "20.0000"

say '⑥ 库存不足：零库存物料要发 5 → 409（三账 / 出库单 / delivered_qty 一分不动）'
new_order '缺货' "$MAT_B" 5
SHORT_SO=$SO_ID; SHORT_ITEM=$ITEM_ID
call 'POST /shipment（MAT_B 5 件）' 200 POST /api/business/erp/shipment \
  "{\"so_id\":\"$SHORT_SO\",\"warehouse_id\":\"$WH\",\"ship_date\":\"2026-11-06\",\
\"items\":[{\"so_item_id\":\"$SHORT_ITEM\",\"qty\":5}]}"
SH_S=$(printf '%s' "$LAST" | jq -r '.data.id')
call 'PATCH /status（→ PENDING_POST）' 200 PATCH "/api/business/erp/shipment/$SH_S/status" '{"target_status":"PENDING_POST"}'
call 'PATCH /status（→ POSTED，库存 0）→ 409' 409 PATCH "/api/business/erp/shipment/$SH_S/status" \
  '{"target_status":"POSTED"}'
printf '%s' "$LAST" | jq -r '.msg' | head -2
eq "成本账 MAT_B 仍 0" "$(cost_qty "$MAT_B")" '0.0000'
eq "库位账 MAT_B 仍 0" "$(loc_qty "$MAT_B")" '0.0000'
eq "没有留下半张销售出库单" "$($PSQL -c "SELECT count(*) FROM biz_stock_out WHERE source_id='$SH_S'")" '0'
eq "发货单仍在 PENDING_POST" "$($PSQL -c "SELECT status FROM biz_shipment WHERE id='$SH_S'")" 'PENDING_POST'
eq "delivered_qty 未回写" "$($PSQL -c "SELECT delivered_qty FROM biz_sales_order_item WHERE id='$SHORT_ITEM'")" '0.0000'
eq "订单状态未推进" "$($PSQL -c "SELECT status FROM biz_sales_order WHERE id='$SHORT_SO'")" 'APPROVED'

printf '\n===== 结果：%s =====\n' "$([ $FAIL -eq 0 ] && echo '全部断言通过' || echo '存在失败断言')"
exit $FAIL

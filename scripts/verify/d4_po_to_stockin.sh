#!/usr/bin/env bash
# ============================================================
# D4 采购订单 → 一键生成收货单 e2e（真实 curl，跑在隔离实例上）
#
# 老板原话（2026-11-01）：「采购订单流程走完不能去做收货入库，还需要再做一次收货入库，
# 所有的物料都要重新填写，这在实际过程中会效率非常低。」
#
# 本脚本用真接口把这条链路走一遍，验的就是卡面 §三 的九步：
#   ① 建采购订单（A×100、B×50）→ 提交
#   ② GET /purchase-order/{id}/pending-items → 两行待收 = 订单（没收过）
#   ③ 从采购单生成收货单（只给 purchase_order_id）：A 收 30、B 收 50 → 落在 DRAFT
#   ④ 过账 → 库存 A +30、采购单变 PARTIAL_RECEIVED
#   ⑤ 再查待收：A 待收 70、B 已收满 0；已生成收货单可反查
#   ⑥ 试超收：A 填 100 → 400，文案点明「本次 100 / 只剩 70 / 已收 30 / 订单 100」
#   ⑦ 收货单列表按来源采购单号筛 → 命中；列表与详情都回填 purchase_order_no
#   ⑧ 收完剩余（A 70 + B 0 …）→ 采购单变 RECEIVED，待收全 0、can_receive=false
#
# 用法：
#   export D4_BASE=http://127.0.0.1:18096/api
#   export D4_DB_PASSWORD='<库口令>'
#   export D4_DB_NAME=cloudpivot_d4e2e        # 默认 cloudpivot_d4e2e
#   export D4_ADMIN_PASSWORD='Admin@123456'   # 隔离实例首启种子口令
#   bash scripts/verify/d4_po_to_stockin.sh
#
# 退出码 = 是否有断言失败（0 = 全过）。脚本自建夹具（单位 / 供应商 / 仓库 / 物料），
# 采购单与收货单**全走 API 建**（不走 SQL 造单，避免「脚本自己造的账」骗过断言）。
#
# ⚠️ 隔离实例自起方式（本机）：
#   createdb -O cloudpivot cloudpivot_d4e2e
#   NIK_PROFILE=test NIK_SERVER_LISTEN=127.0.0.1:18096 \
#   NIK_DATABASE_NAME=cloudpivot_d4e2e NIK_DATABASE_PASSWORD=... \
#   NIK_MIDDLEWARE_AUTH_JWT_SECRET=$(openssl rand -hex 32) \
#   backend/target/debug/cloudpivot      # 迁移 + 种子（admin / 权限码）自动跑完
# ============================================================
set -uo pipefail

BASE="${D4_BASE:-http://127.0.0.1:18096/api}"
DBN="${D4_DB_NAME:-cloudpivot_d4e2e}"
DBH="${D4_DB_HOST:-127.0.0.1}"
DBU="${D4_DB_USER:-cloudpivot}"
ADMIN_USER="${D4_ADMIN:-admin}"
ADMIN_PASS="${D4_ADMIN_PASSWORD:-Admin@123456}"
: "${D4_DB_PASSWORD:?请先 export D4_DB_PASSWORD（库口令）}"

UNIT="d4000001-0000-4000-8000-000000000001"
SUPPLIER="d4000001-0000-4000-8000-000000000002"
WAREHOUSE="d4000001-0000-4000-8000-000000000003"
MATERIAL_A="d4000001-0000-4000-8000-000000000011"
MATERIAL_B="d4000001-0000-4000-8000-000000000012"

PASS=0
FAIL=0
TOKEN=""

# ---------- 断言工具 ----------
ok() { PASS=$((PASS + 1)); printf '[PASS] %s\n' "$1"; }
bad() { FAIL=$((FAIL + 1)); printf '[FAIL] %s\n' "$1"; }
assert_eq() { if [ "$1" = "$2" ]; then ok "$3（$2）"; else bad "$3：期望 [$1] 实际 [$2]"; fi; }
assert_num() {
    if awk -v a="$1" -v b="$2" 'BEGIN { if (a == "" || b == "" || b == "null") exit 1; exit !(a + 0 == b + 0) }'; then
        ok "$3（$2）"
    else
        bad "$3：期望 [$1] 实际 [$2]"
    fi
}
assert_contains() {
    case "$1" in
        *"$2"*) ok "$3" ;;
        *) bad "$3：文本里没有 [$2]，实际 [$1]" ;;
    esac
}

psql_q() { PGPASSWORD="$D4_DB_PASSWORD" psql -h "$DBH" -U "$DBU" -d "$DBN" -Atc "$1" 2>&1 | head -1; }
psql_x() { PGPASSWORD="$D4_DB_PASSWORD" psql -h "$DBH" -U "$DBU" -d "$DBN" -v ON_ERROR_STOP=1 -c "$1" 2>&1 | head -3; }

# 发请求：REQ_CODE / REQ_BODY 写回全局（不要用 $(...) 包，子 shell 里赋值会丢）
REQ_CODE=0
REQ_BODY=""
call() {
    local method="$1" path="$2" body="${3:-}" tmp
    tmp=$(mktemp)
    if [ -n "$body" ]; then
        REQ_CODE=$(curl -s -o "$tmp" -w '%{http_code}' -X "$method" \
            -H "Authorization: Bearer $TOKEN" -H 'Content-Type: application/json' \
            --data-binary "$body" "$BASE$path")
    else
        REQ_CODE=$(curl -s -o "$tmp" -w '%{http_code}' -X "$method" \
            -H "Authorization: Bearer $TOKEN" "$BASE$path")
    fi
    REQ_BODY=$(cat "$tmp")
    rm -f "$tmp"
}

jqv() { printf '%s' "$REQ_BODY" | jq -r "$1" 2>/dev/null; }

echo '==> 0. 登录'
tmp=$(mktemp)
REQ_CODE=$(curl -s -o "$tmp" -w '%{http_code}' -X POST -H 'Content-Type: application/json' \
    --data-binary "{\"username\":\"$ADMIN_USER\",\"password\":\"$ADMIN_PASS\"}" \
    "$BASE/system/auth/login")
REQ_BODY=$(cat "$tmp")
rm -f "$tmp"
TOKEN=$(jqv '.data.access_token')
if [ -z "$TOKEN" ] || [ "$TOKEN" = "null" ]; then
    bad "登录失败：$REQ_BODY"
    echo "==> 汇总：PASS=$PASS FAIL=$FAIL"
    exit 1
fi
ok "登录拿到令牌（$ADMIN_USER）"
MUST_CHANGE=$(jqv '.data.must_change_password')
if [ "$MUST_CHANGE" = "true" ]; then
    # 首登强制改密：改完重新登（隔离库首次启动会走到这里）
    NEW_PASS="D4_${ADMIN_PASS}_new"
    curl -s -o /dev/null -X PUT -H "Authorization: Bearer $TOKEN" -H 'Content-Type: application/json' \
        --data-binary "{\"old_password\":\"$ADMIN_PASS\",\"new_password\":\"$NEW_PASS\"}" \
        "$BASE/system/auth/password"
    tmp=$(mktemp)
    curl -s -o "$tmp" -X POST -H 'Content-Type: application/json' \
        --data-binary "{\"username\":\"$ADMIN_USER\",\"password\":\"$NEW_PASS\"}" \
        "$BASE/system/auth/login"
    REQ_BODY=$(cat "$tmp")
    rm -f "$tmp"
    TOKEN=$(jqv '.data.access_token')
    ok "首登改密后重新登录"
fi

echo '==> 1. 清理上一轮夹具 + 造主数据（供应商 / 仓库 / 物料 / 单位）'
psql_x "DELETE FROM biz_material_serial WHERE source_type = 'PURCHASE' AND source_id IN
          (SELECT id FROM biz_stock_in WHERE supplier_id = '$SUPPLIER'::uuid)" >/dev/null
psql_x "DELETE FROM biz_inventory_txn WHERE doc_type = 'STOCK_IN' AND doc_id IN
          (SELECT id FROM biz_stock_in WHERE supplier_id = '$SUPPLIER'::uuid)" >/dev/null
psql_x "DELETE FROM biz_voucher_entry WHERE voucher_id IN
          (SELECT id FROM biz_voucher WHERE source_id IN
              (SELECT id FROM biz_stock_in WHERE supplier_id = '$SUPPLIER'::uuid))" >/dev/null
psql_x "DELETE FROM biz_voucher WHERE source_id IN
          (SELECT id FROM biz_stock_in WHERE supplier_id = '$SUPPLIER'::uuid)" >/dev/null
psql_x "DELETE FROM biz_payable WHERE source_id IN
          (SELECT id FROM biz_stock_in WHERE supplier_id = '$SUPPLIER'::uuid)" >/dev/null
psql_x "DELETE FROM biz_inventory_location WHERE material_id IN ('$MATERIAL_A'::uuid, '$MATERIAL_B'::uuid)
          AND warehouse_id = '$WAREHOUSE'::uuid" >/dev/null
psql_x "DELETE FROM biz_inventory WHERE material_id IN ('$MATERIAL_A'::uuid, '$MATERIAL_B'::uuid)
          AND warehouse_id = '$WAREHOUSE'::uuid" >/dev/null
psql_x "DELETE FROM biz_status_log WHERE entity_no LIKE 'D4-%'" >/dev/null
psql_x "DELETE FROM biz_stock_in WHERE supplier_id = '$SUPPLIER'::uuid" >/dev/null
psql_x "DELETE FROM biz_purchase_order WHERE supplier_id = '$SUPPLIER'::uuid" >/dev/null

psql_x "INSERT INTO biz_unit (id, unit_code, unit_name, is_base, base_unit_id, rate, status, sort_order)
        VALUES ('$UNIT', 'D4-E2E-PCS', '个', 1, NULL, 1.000000, 1, 1) ON CONFLICT DO NOTHING" >/dev/null
psql_x "INSERT INTO biz_supplier (id, supplier_code, supplier_name, status)
        VALUES ('$SUPPLIER', 'D4-E2E-SUP', 'D4-E2E 供应商', 1) ON CONFLICT DO NOTHING" >/dev/null
psql_x "INSERT INTO biz_warehouse (id, warehouse_code, warehouse_name, warehouse_path, level, is_leaf, status)
        VALUES ('$WAREHOUSE', 'D4-E2E-WH', 'D4-E2E 收货仓', '/$WAREHOUSE/', 1, 1, 1) ON CONFLICT DO NOTHING" >/dev/null
psql_x "INSERT INTO biz_material (id, material_code, material_name, unit_id, status, serial_required)
        VALUES ('$MATERIAL_A', 'D4-E2E-MA', '物料 A', '$UNIT'::uuid, 1, false) ON CONFLICT DO NOTHING" >/dev/null
psql_x "INSERT INTO biz_material (id, material_code, material_name, unit_id, status, serial_required)
        VALUES ('$MATERIAL_B', 'D4-E2E-MB', '物料 B', '$UNIT'::uuid, 1, false) ON CONFLICT DO NOTHING" >/dev/null
ok '主数据就绪'

echo '==> 2. ① 建采购订单：物料 A × 100、物料 B × 50 → 提交'
BODY=$(cat <<JSON
{"supplier_id":"$SUPPLIER","warehouse_id":"$WAREHOUSE","remark":"D4-E2E 采购",
 "items":[{"material_id":"$MATERIAL_A","qty":"100","price_excl":"10","tax_rate":"13"},
          {"material_id":"$MATERIAL_B","qty":"50","price_excl":"20","tax_rate":"13"}]}
JSON
)
call POST '/business/erp/purchase-order' "$BODY"
assert_eq 200 "$REQ_CODE" '建采购单'
PO_ID=$(jqv '.data.id // .data.header.id')
PO_NO=$(jqv '.data.order_no // .data.header.order_no')
if [ -z "$PO_ID" ] || [ "$PO_ID" = "null" ]; then
    # 详情接口只回单头时，从列表按单号捞一次
    PO_ID=$(psql_q "SELECT id FROM biz_purchase_order WHERE supplier_id = '$SUPPLIER'::uuid ORDER BY created_at DESC LIMIT 1")
    PO_NO=$(psql_q "SELECT order_no FROM biz_purchase_order WHERE id = '$PO_ID'::uuid")
fi
ok "采购单已建：$PO_NO（$PO_ID）"
call PATCH "/business/erp/purchase-order/$PO_ID/status" '{"status":"SUBMITTED"}'
assert_eq 200 "$REQ_CODE" '提交采购单'

echo '==> 3. ② 待收明细（没收过 → 待收 = 订单）'
call GET "/business/erp/purchase-order/$PO_ID/pending-items"
assert_eq 200 "$REQ_CODE" '查待收明细'
assert_eq 2 "$(jqv '.data.items | length')" '两行明细都返回'
assert_num 100 "$(jqv '.data.items[0].ordered_qty')" '行 1 订单数量 100'
assert_num 0 "$(jqv '.data.items[0].received_qty')" '行 1 已收 0'
assert_num 100 "$(jqv '.data.items[0].pending_qty')" '行 1 待收 = 订单（新单）'
assert_num 50 "$(jqv '.data.items[1].pending_qty')" '行 2 待收 50'
assert_eq "$SUPPLIER" "$(jqv '.data.order.supplier_id')" '带出供应商（O1）'
assert_eq "$WAREHOUSE" "$(jqv '.data.order.warehouse_id')" '带出仓库（O1）'
assert_eq true "$(jqv '.data.can_receive')" '新单可收（前端显示按钮）'
ITEM_A=$(jqv '.data.items[0].item_id')
ITEM_B=$(jqv '.data.items[1].item_id')

echo '==> 4. ③ 一键生成收货单：A 只收 30、B 收 50（草稿，不自动过账）'
BODY=$(cat <<JSON
{"purchase_order_id":"$PO_ID","supplier_id":"$SUPPLIER","warehouse_id":"$WAREHOUSE",
 "invoice_received":true,"remark":"D4-E2E 部分收货",
 "items":[{"purchase_order_item_id":"$ITEM_A","material_id":"$MATERIAL_A","qty":"30","price_excl":"10","tax_rate":"13"},
          {"purchase_order_item_id":"$ITEM_B","material_id":"$MATERIAL_B","qty":"50","price_excl":"20","tax_rate":"13"}]}
JSON
)
call POST '/business/erp/stock-in' "$BODY"
assert_eq 200 "$REQ_CODE" '建收货单（带 purchase_order_id）'
IN_ID=$(jqv '.data.header.id')
IN_NO=$(jqv '.data.header.in_no')
assert_eq DRAFT "$(jqv '.data.header.status')" '落 DRAFT（硬红线 7：不自动过账）'
assert_eq "$PO_ID" "$(jqv '.data.purchase_order_id')" '详情响应给 purchase_order_id'
assert_eq "$PO_NO" "$(jqv '.data.purchase_order_no')" '详情响应给 purchase_order_no'
assert_eq "$ITEM_A" "$(jqv '.data.items[0].source_item_id')" '行 1 落 source_item_id（按行匹配）'
assert_eq SUBMITTED "$(psql_q "SELECT status FROM biz_purchase_order WHERE id = '$PO_ID'::uuid")" '建草稿不改采购单状态'

echo '==> 5. ④ 过账 → 库存 A +30、采购单变 PARTIAL_RECEIVED'
call PATCH "/business/erp/stock-in/$IN_ID/status" '{"status":"POSTED"}'
assert_eq 200 "$REQ_CODE" '过账'
assert_eq POSTED "$(jqv '.data.header.status')" '单据变 POSTED'
assert_num 30 "$(psql_q "SELECT qty FROM biz_inventory WHERE material_id = '$MATERIAL_A'::uuid
    AND warehouse_id = '$WAREHOUSE'::uuid AND location_id IS NULL")" '库存 A +30'
assert_eq PARTIAL_RECEIVED "$(psql_q "SELECT status FROM biz_purchase_order WHERE id = '$PO_ID'::uuid")" '采购单变 PARTIAL_RECEIVED'

echo '==> 6. ⑤ 再查待收：A 待收 70、B 已收满；已生成收货单可反查'
call GET "/business/erp/purchase-order/$PO_ID/pending-items"
assert_eq 200 "$REQ_CODE" '再查待收明细'
assert_num 70 "$(jqv '.data.items[0].pending_qty')" 'A 待收 70（只算已过账）'
assert_num 30 "$(jqv '.data.items[0].received_qty')" 'A 已收 30'
assert_num 50 "$(jqv '.data.items[1].received_qty')" 'B 已收 50'
assert_num 0 "$(jqv '.data.items[1].pending_qty')" 'B 已收满（待收 0）'
assert_eq 1 "$(jqv '.data.pending_line_count')" '只剩 1 行未收完'
assert_eq "$IN_NO" "$(jqv '.data.stock_ins[0].in_no')" '已生成收货单反查得到'
assert_eq true "$(jqv '.data.stock_ins[0].posted')" '反查标记已过账'

echo '==> 7. ⑥ 防超收：A 填 100（待收只有 70）→ 400 + 精确文案'
BODY=$(cat <<JSON
{"purchase_order_id":"$PO_ID","supplier_id":"$SUPPLIER","warehouse_id":"$WAREHOUSE",
 "items":[{"purchase_order_item_id":"$ITEM_A","material_id":"$MATERIAL_A","qty":"100","price_excl":"10","tax_rate":"13"}]}
JSON
)
call POST '/business/erp/stock-in' "$BODY"
assert_eq 400 "$REQ_CODE" '超收 400'
MSG=$(jqv '.msg')
assert_contains "$MSG" '本次收 100 件' '文案给本次数量'
assert_contains "$MSG" '只剩 70 件未收' '文案给待收数量'
assert_contains "$MSG" '已收 30' '文案给已收数量'
assert_contains "$MSG" '订单 100' '文案给订单数量'
assert_contains "$MSG" 'D4-E2E-MA' '文案点名物料编码'

echo '==> 8. ⑦ 列表：按来源采购单号筛 + 回填 purchase_order_no'
call GET "/business/erp/stock-in/page?purchase_order_no=$(printf '%s' "$PO_NO" | jq -sRr @uri)&page=1&page_size=20"
assert_eq 200 "$REQ_CODE" '按采购单号筛收货单'
assert_eq 1 "$(jqv '.data.total')" '命中 1 张'
assert_eq "$PO_NO" "$(jqv '.data.list[0].source_order_no')" '列表回填来源采购单号（F3）'
assert_eq "$IN_NO" "$(jqv '.data.list[0].in_no')" '命中的就是要找的那张'

echo '==> 9. ⑧ 收完剩余 → 采购单变 RECEIVED，待收全 0'
call POST '/business/erp/stock-in' \
    "{\"purchase_order_id\":\"$PO_ID\",\"supplier_id\":\"$SUPPLIER\",\"warehouse_id\":\"$WAREHOUSE\",
      \"items\":[{\"purchase_order_item_id\":\"$ITEM_A\",\"material_id\":\"$MATERIAL_A\",\"qty\":\"70\",\"price_excl\":\"10\",\"tax_rate\":\"13\"}]}"
assert_eq 200 "$REQ_CODE" '建第二张收货单（只收 A 的剩余 70）'
IN2_ID=$(jqv '.data.header.id')
call PATCH "/business/erp/stock-in/$IN2_ID/status" '{"status":"POSTED"}'
assert_eq 200 "$REQ_CODE" '第二张过账'
assert_eq RECEIVED "$(psql_q "SELECT status FROM biz_purchase_order WHERE id = '$PO_ID'::uuid")" '采购单变 RECEIVED'
call GET "/business/erp/purchase-order/$PO_ID/pending-items"
assert_eq false "$(jqv '.data.can_receive')" '收满后 can_receive=false（前端不显示按钮）'
assert_eq 0 "$(jqv '.data.pending_line_count')" '0 行未收完'
assert_contains "$(jqv '.data.blocked_reason')" '已收满' '给出「已收满」原因'

echo '==> 10. 清理 + 残留核对'
psql_x "DELETE FROM biz_material_serial WHERE source_type = 'PURCHASE' AND source_id IN
          (SELECT id FROM biz_stock_in WHERE supplier_id = '$SUPPLIER'::uuid)" >/dev/null
psql_x "DELETE FROM biz_inventory_txn WHERE doc_type = 'STOCK_IN' AND doc_id IN
          (SELECT id FROM biz_stock_in WHERE supplier_id = '$SUPPLIER'::uuid)" >/dev/null
psql_x "DELETE FROM biz_voucher_entry WHERE voucher_id IN
          (SELECT id FROM biz_voucher WHERE source_id IN
              (SELECT id FROM biz_stock_in WHERE supplier_id = '$SUPPLIER'::uuid))" >/dev/null
psql_x "DELETE FROM biz_voucher WHERE source_id IN
          (SELECT id FROM biz_stock_in WHERE supplier_id = '$SUPPLIER'::uuid)" >/dev/null
psql_x "DELETE FROM biz_payable WHERE source_id IN
          (SELECT id FROM biz_stock_in WHERE supplier_id = '$SUPPLIER'::uuid)" >/dev/null
psql_x "DELETE FROM biz_inventory_location WHERE material_id IN ('$MATERIAL_A'::uuid, '$MATERIAL_B'::uuid)
          AND warehouse_id = '$WAREHOUSE'::uuid" >/dev/null
psql_x "DELETE FROM biz_inventory WHERE material_id IN ('$MATERIAL_A'::uuid, '$MATERIAL_B'::uuid)
          AND warehouse_id = '$WAREHOUSE'::uuid" >/dev/null
psql_x "DELETE FROM biz_status_log WHERE entity_no LIKE 'D4-%'" >/dev/null
psql_x "DELETE FROM biz_stock_in WHERE supplier_id = '$SUPPLIER'::uuid" >/dev/null
psql_x "DELETE FROM biz_purchase_order WHERE supplier_id = '$SUPPLIER'::uuid" >/dev/null
LEFT=$(psql_q "SELECT (SELECT count(*) FROM biz_stock_in WHERE supplier_id = '$SUPPLIER'::uuid)
                 + (SELECT count(*) FROM biz_purchase_order WHERE supplier_id = '$SUPPLIER'::uuid)")
assert_eq 0 "$LEFT" '夹具残留 = 0'

echo "==> 汇总：PASS=$PASS FAIL=$FAIL"
[ "$FAIL" -eq 0 ] || exit 1
exit 0

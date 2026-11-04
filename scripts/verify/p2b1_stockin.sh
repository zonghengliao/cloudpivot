#!/usr/bin/env bash
# ============================================================
# P2-B1 收货入库接口 e2e（真实 curl，跑在隔离实例上）
#
#   覆盖：建单（金额服务端算 / 来源采购单自动带出）→ **过账**（DRAFT→POSTED：库存加法 +
#         移动加权 + 库存流水 + 一物一码序列号 + 回写采购已收数量 / 推送采购单状态）→
#         幂等（重复过账 409）→ 成本口径（不含税，不是价税合计）→
#         二次入库重算移动加权（PRD §3.2.5 算例 1）→ 超收 400 → 序列号条数不符 400 →
#         改单（DRAFT ✅ / POSTED 409）→ 作废（DRAFT ✅ / POSTED 需 reason）→ 删除（DRAFT ✅）
#         → 列表分页
#
# 用法：
#   export P2B1_BASE=http://127.0.0.1:18096/api
#   export P2B1_DB_PASSWORD='<cloudpivot 库口令>'
#   export P2B1_DB_NAME=cloudpivot_p2b1        # 默认 cloudpivot_p2b1
#   export P2B1_ADMIN_PASSWORD='<admin 口令>'  # 默认 Nereus792379（dev/test 种子）
#   bash scripts/verify/p2b1_stockin.sh
#
# 退出码 = 是否有断言失败（0 = 全过）。脚本自建夹具（单位 / 供应商 / 仓库 / 物料 / 采购单），
# 开头按固定夹具 UUID 清上一轮，结尾打印清理 SQL 并核对残留。
#
# ⚠️ 前置：目标库必须已应用 V0145（收货两表）与 V0147（6 个权限码）。
#    隔离库建法见本卡证据文档：`su - postgres -c 'createdb -O cloudpivot cloudpivot_p2b1'`
#    再用 target/debug/cloudpivot 起一次让迁移自动跑完。
# ============================================================
set -uo pipefail

BASE="${P2B1_BASE:-http://127.0.0.1:18096/api}"
DBN="${P2B1_DB_NAME:-cloudpivot_p2b1}"
DBH="${P2B1_DB_HOST:-127.0.0.1}"
DBU="${P2B1_DB_USER:-cloudpivot}"
ADMIN_USER="${P2B1_ADMIN:-admin}"
ADMIN_PASS="${P2B1_ADMIN_PASSWORD:-Nereus792379}"
: "${P2B1_DB_PASSWORD:?请先 export P2B1_DB_PASSWORD（cloudpivot 库口令）}"

UNIT="e2b10001-0000-4000-8000-000000000001"
SUPPLIER="e2b10001-0000-4000-8000-000000000002"
WAREHOUSE="e2b10001-0000-4000-8000-000000000003"
MATERIAL_A="e2b10001-0000-4000-8000-000000000011"
MATERIAL_SN="e2b10001-0000-4000-8000-000000000012"

PASS=0
FAIL=0
TOKEN=""

# ---------- 断言工具（**必须在调用之前定义**：放在登录之后会导致断言被静默跳过） ----------
ok() { PASS=$((PASS + 1)); printf '[PASS] %s\n' "$1"; }
bad() { FAIL=$((FAIL + 1)); printf '[FAIL] %s\n' "$1"; }
assert_eq() { if [ "$1" = "$2" ]; then ok "$3（$2）"; else bad "$3：期望 [$1] 实际 [$2]"; fi; }
# 金额比数值而不是比字符串：库里 `NUMERIC(18,4)` 经 JSON 回来是 "200.0000"
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

psql_q() { PGPASSWORD="$P2B1_DB_PASSWORD" psql -h "$DBH" -U "$DBU" -d "$DBN" -Atc "$1" 2>&1 | head -1; }
psql_x() { PGPASSWORD="$P2B1_DB_PASSWORD" psql -h "$DBH" -U "$DBU" -d "$DBN" -v ON_ERROR_STOP=1 -c "$1" 2>&1 | head -3; }

# 发请求：REQ_CODE / REQ_BODY 写回**全局**（不要用 $(...) 包，子 shell 里赋值会丢）
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

echo "==> 0. 登录（首登强制改密的两种情况都兜住）"
login_once() {
    local pw="$1" tmp
    tmp=$(mktemp)
    REQ_CODE=$(curl -s -o "$tmp" -w '%{http_code}' -X POST \
        -H 'Content-Type: application/json' \
        --data-binary "{\"username\":\"$ADMIN_USER\",\"password\":\"$pw\"}" \
        "$BASE/system/auth/login")
    REQ_BODY=$(cat "$tmp")
    rm -f "$tmp"
    TOKEN=$(jqv '.data.access_token')
    MUST_CHANGE=$(jqv '.data.must_change_password')
}
NEW_PASS="P2b1_${ADMIN_PASS}_new"
MUST_CHANGE="false"
login_once "$NEW_PASS"
if [ -z "$TOKEN" ] || [ "$TOKEN" = "null" ]; then
    login_once "$ADMIN_PASS"
fi
if [ -z "$TOKEN" ] || [ "$TOKEN" = "null" ]; then
    bad "登录失败：$REQ_BODY"
    echo "==> 汇总：PASS=$PASS FAIL=$FAIL"
    exit 1
fi
if [ "$MUST_CHANGE" = "true" ]; then
    # 改密接口本身要认证：必须带着刚拿到的 TOKEN
    REQ_CODE=$(curl -s -o /dev/null -w '%{http_code}' -X PUT \
        -H "Authorization: Bearer $TOKEN" -H 'Content-Type: application/json' \
        --data-binary "{\"old_password\":\"$ADMIN_PASS\",\"new_password\":\"$NEW_PASS\"}" \
        "$BASE/system/auth/password")
    assert_eq 200 "$REQ_CODE" "首登改密"
    login_once "$NEW_PASS"
fi
ok "登录拿到令牌（admin）"

echo "==> 1. 清理上一轮夹具 + 造夹具"
psql_x "DELETE FROM biz_material_serial WHERE source_type = 'PURCHASE' AND source_id IN
          (SELECT id FROM biz_stock_in WHERE supplier_id = '$SUPPLIER'::uuid)" >/dev/null
psql_x "DELETE FROM biz_inventory_txn WHERE doc_type = 'STOCK_IN' AND doc_id IN
          (SELECT id FROM biz_stock_in WHERE supplier_id = '$SUPPLIER'::uuid)" >/dev/null
psql_x "DELETE FROM biz_inventory WHERE material_id IN ('$MATERIAL_A'::uuid, '$MATERIAL_SN'::uuid)
          AND warehouse_id = '$WAREHOUSE'::uuid" >/dev/null
psql_x "DELETE FROM biz_stock_in WHERE supplier_id = '$SUPPLIER'::uuid" >/dev/null
psql_x "DELETE FROM biz_purchase_order WHERE supplier_id = '$SUPPLIER'::uuid" >/dev/null
# `ON CONFLICT DO NOTHING`（不带目标列）兜住任意唯一约束，重跑不中断
psql_x "INSERT INTO biz_unit (id, unit_code, unit_name, is_base, base_unit_id, rate, status, sort_order)
        VALUES ('$UNIT', 'P2B1-E2E-PCS', '个', 1, NULL, 1.000000, 1, 1) ON CONFLICT DO NOTHING"
psql_x "INSERT INTO biz_supplier (id, supplier_code, supplier_name, status)
        VALUES ('$SUPPLIER', 'P2B1-E2E-SUP', 'P2B1-E2E 供应商', 1) ON CONFLICT DO NOTHING"
psql_x "INSERT INTO biz_warehouse (id, warehouse_code, warehouse_name, warehouse_path, level, is_leaf, status)
        VALUES ('$WAREHOUSE', 'P2B1-E2E-WH', 'P2B1-E2E 收货仓', '/$WAREHOUSE/', 1, 1, 1) ON CONFLICT DO NOTHING"
psql_x "INSERT INTO biz_material (id, material_code, material_name, unit_id, status, serial_required)
        VALUES ('$MATERIAL_A', 'P2B1-E2E-M1', '螺丝 M6x20', '$UNIT'::uuid, 1, false)
        ON CONFLICT DO NOTHING"
psql_x "INSERT INTO biz_material (id, material_code, material_name, unit_id, status, serial_required)
        VALUES ('$MATERIAL_SN', 'P2B1-E2E-M2', '伺服电机（一物一码）', '$UNIT'::uuid, 1, true)
        ON CONFLICT DO NOTHING"

echo "==> 2. 建单（DRAFT）：100@2 13% + 一物一码 2 件@40 0%"
BODY=$(cat <<JSON
{"supplier_id":"$SUPPLIER","warehouse_id":"$WAREHOUSE","invoice_received":false,
 "remark":"P2B1-E2E 收货",
 "items":[{"material_id":"$MATERIAL_A","qty":"100","price_excl":"2","tax_rate":"13"},
          {"material_id":"$MATERIAL_SN","qty":"2","price_excl":"40","tax_rate":"0",
           "serial_ids":["E2B1-SN-001","E2B1-SN-002"]}]}
JSON
)
call POST "/business/erp/stock-in" "$BODY"
assert_eq 200 "$REQ_CODE" "建收货单"
IN_ID=$(jqv '.data.header.id')
IN_NO=$(jqv '.data.header.in_no')
assert_eq "DRAFT" "$(jqv '.data.header.status')" "建单落 DRAFT"
assert_contains "$IN_NO" "IN-" "单号前缀 IN-（实际 $IN_NO）"
assert_num 280 "$(jqv '.data.header.total_amount')" "不含税合计 200+80"
assert_num 26 "$(jqv '.data.header.total_tax')" "税额合计 26"
assert_num 306 "$(jqv '.data.header.total_with_tax')" "价税合计 306（精确恒等）"
assert_num 102 "$(jqv '.data.header.total_qty')" "合计数量 102"
assert_num 2 "$(jqv '.data.items[0].cost_price')" "行 1 cost_price 落不含税单价 2"
assert_num 40 "$(jqv '.data.items[1].cost_price')" "行 2（一物一码）cost_price 40"

echo "==> 3. 过账（DRAFT → POSTED）：第 2 行用自己贴的码"
call PATCH "/business/erp/stock-in/$IN_ID/status" \
     "{\"status\":\"POSTED\",\"serials\":[{\"line_no\":2,\"serial_ids\":[\"E2B1-SN-001\",\"E2B1-SN-002\"]}]}"
assert_eq 200 "$REQ_CODE" "过账"
assert_eq "POSTED" "$(jqv '.data.header.status')" "单据变 POSTED"
assert_eq "true" "$([ -n "$(jqv '.data.header.posted_at')" ] && [ "$(jqv '.data.header.posted_at')" != "null" ] && echo true || echo false)" "写 posted_at"
assert_eq 2 "$(jqv '.data.serial_count')" "本单生成/落了 2 个序列号"

echo "==> 4. 库存 / 流水 / 序列号落库断言（psql）"
read -r QTY AMT AVG < <(psql_q "SELECT qty||' '||amount||' '||avg_cost FROM biz_inventory
    WHERE material_id = '$MATERIAL_A'::uuid AND warehouse_id = '$WAREHOUSE'::uuid AND location_id IS NULL")
assert_num 100 "$QTY" "库存数量 +100"
assert_num 200 "$AMT" "库存金额 +200（**不含税**，不是 226）"
assert_num 2 "$AVG" "移动加权单价 2.000000"

TXN=$(psql_q "SELECT txn_type||'|'||biz_type||'|'||qty||'|'||amount||'|'||before_qty||'|'||after_qty
    FROM biz_inventory_txn WHERE doc_type = 'STOCK_IN' AND doc_id = '$IN_ID'::uuid
     AND material_id = '$MATERIAL_A'::uuid")
assert_eq "IN|PURCHASE_IN|100.0000|200.0000|0.0000|100.0000" "$TXN" "流水：IN / PURCHASE_IN / before 0 → after 100"

SN=$(psql_q "SELECT count(*)||'|'||min(warehouse_id::text)||'|'||string_agg(serial_no, ',' ORDER BY serial_no)
    FROM biz_material_serial WHERE source_id = '$IN_ID'::uuid")
assert_eq "2|$WAREHOUSE|E2B1-SN-001,E2B1-SN-002" "$SN" "序列号 2 件、落在收货仓、用客户端贴的码"

echo "==> 5. 幂等：重复过账 → 409"
call PATCH "/business/erp/stock-in/$IN_ID/status" '{"status":"POSTED"}'
assert_eq 409 "$REQ_CODE" "重复过账 409"
assert_contains "$(jqv '.msg')" "已过账" "409 文案点名已过账"
assert_num 100 "$(psql_q "SELECT qty FROM biz_inventory WHERE material_id = '$MATERIAL_A'::uuid
    AND warehouse_id = '$WAREHOUSE'::uuid AND location_id IS NULL")" "重复过账没再加库存"

echo "==> 6. 已过账单不能再改 / 不能删"
call PUT "/business/erp/stock-in/$IN_ID" \
     "{\"supplier_id\":\"$SUPPLIER\",\"warehouse_id\":\"$WAREHOUSE\",\"items\":[{\"material_id\":\"$MATERIAL_A\",\"qty\":\"1\",\"price_excl\":\"1\"}]}"
assert_eq 409 "$REQ_CODE" "POSTED 改单 409"
call DELETE "/business/erp/stock-in/$IN_ID"
assert_eq 409 "$REQ_CODE" "POSTED 删除 409"

echo "==> 7. 二次入库 100@3 → 移动加权重算（PRD §3.2.5 算例 1）"
call POST "/business/erp/stock-in" \
     "{\"supplier_id\":\"$SUPPLIER\",\"warehouse_id\":\"$WAREHOUSE\",
       \"items\":[{\"material_id\":\"$MATERIAL_A\",\"qty\":\"100\",\"price_excl\":\"3\",\"tax_rate\":\"0\"}]}"
assert_eq 200 "$REQ_CODE" "建第二张收货单"
IN2=$(jqv '.data.header.id')
call PATCH "/business/erp/stock-in/$IN2/status" '{"status":"POSTED"}'
assert_eq 200 "$REQ_CODE" "第二张过账"
read -r QTY2 AMT2 AVG2 < <(psql_q "SELECT qty||' '||amount||' '||avg_cost FROM biz_inventory
    WHERE material_id = '$MATERIAL_A'::uuid AND warehouse_id = '$WAREHOUSE'::uuid AND location_id IS NULL")
assert_num 200 "$QTY2" "结存数量 200"
assert_num 500 "$AMT2" "结存金额 500（加法，不用单价×数量重算）"
assert_num 2.5 "$AVG2" "结存单价 2.500000"
BEFORE2=$(psql_q "SELECT before_qty||'|'||before_amount||'|'||before_cost FROM biz_inventory_txn
    WHERE doc_id = '$IN2'::uuid")
assert_eq "100.0000|200.0000|2.000000" "$BEFORE2" "第二笔流水的 before 快照 = 第一张单的结果"

echo "==> 8. 序列号条数与数量不符 → 400"
call POST "/business/erp/stock-in" \
     "{\"supplier_id\":\"$SUPPLIER\",\"warehouse_id\":\"$WAREHOUSE\",
       \"items\":[{\"material_id\":\"$MATERIAL_SN\",\"qty\":\"3\",\"price_excl\":\"40\",
                   \"serial_ids\":[\"E2B1-SN-003\",\"E2B1-SN-004\"]}]}"
assert_eq 400 "$REQ_CODE" "序列号条数 2 ≠ 数量 3 → 400"
assert_contains "$(jqv '.msg')" "序列号条数与数量不符" "400 文案点名条数不符"

echo "==> 9. 非一物一码物料给了 serial_ids → 400（66007）"
call POST "/business/erp/stock-in" \
     "{\"supplier_id\":\"$SUPPLIER\",\"warehouse_id\":\"$WAREHOUSE\",
       \"items\":[{\"material_id\":\"$MATERIAL_A\",\"qty\":\"2\",\"price_excl\":\"1\",
                   \"serial_ids\":[\"X1\",\"X2\"]}]}"
assert_eq 400 "$REQ_CODE" "非一物一码给码 → 400"
assert_eq 66007 "$(jqv '.code')" "业务码 66007（不静默丢弃）"

echo "==> 10. 采购单：超收 400 / 收满推送 RECEIVED / 自动带出"
PO=$(psql_q "INSERT INTO biz_purchase_order
      (id, order_no, supplier_id, order_date, warehouse_id, status, total_qty, total_amount, total_tax, total_with_tax)
      VALUES (gen_random_uuid(), 'E2B1-PO-1', '$SUPPLIER'::uuid, CURRENT_DATE, '$WAREHOUSE'::uuid,
              'SUBMITTED', 30, 60, 0, 60) RETURNING id")
PO_ITEM=$(psql_q "INSERT INTO biz_purchase_order_item
      (id, order_id, line_no, material_id, unit_id, qty, price, price_incl, tax_rate,
       amount, tax_amount, with_tax_amount)
      VALUES (gen_random_uuid(), '$PO'::uuid, 1, '$MATERIAL_A'::uuid, '$UNIT'::uuid,
              30, 2, 2, 0, 60, 0, 60) RETURNING id")
# P2-3：已收数量是派生值（只算已过账收货单），造「已收 10」要造一张 POSTED 收货单
psql_x "WITH si AS (
         INSERT INTO biz_stock_in (id, in_no, in_type, source_order_id, supplier_id, warehouse_id,
                                   in_date, status, total_qty, total_amount, total_tax, total_with_tax)
         VALUES (gen_random_uuid(), 'E2B1-IN-SEED', 'PURCHASE', '$PO'::uuid, '$SUPPLIER'::uuid,
                 '$WAREHOUSE'::uuid, CURRENT_DATE, 'POSTED', 10, 20, 0, 20) RETURNING id)
       INSERT INTO biz_stock_in_item (id, stock_in_id, line_no, source_item_id, material_id, unit_id,
                                      qty, price, price_incl, tax_rate, amount, tax_amount, with_tax_amount)
       SELECT gen_random_uuid(), si.id, 1, '$PO_ITEM'::uuid, '$MATERIAL_A'::uuid, '$UNIT'::uuid,
              10, 2, 2, 0, 20, 0, 20
         FROM si"
psql_x "SELECT 1" >/dev/null
call POST "/business/erp/stock-in" \
     "{\"source_order_id\":\"$PO\",\"supplier_id\":\"$SUPPLIER\",\"warehouse_id\":\"$WAREHOUSE\",
       \"items\":[{\"purchase_order_item_id\":\"$PO_ITEM\",\"material_id\":\"$MATERIAL_A\",
                   \"qty\":\"25\",\"price_excl\":\"2\",\"tax_rate\":\"0\"}]}"
assert_eq 400 "$REQ_CODE" "收 25 超未收 20 → 400"
assert_contains "$(jqv '.msg')" "超过采购单未收数量" "400 文案点名超收"

call POST "/business/erp/stock-in" \
     "{\"source_order_id\":\"$PO\",\"supplier_id\":\"$SUPPLIER\",\"warehouse_id\":\"$WAREHOUSE\"}"
assert_eq 200 "$REQ_CODE" "只给 source_order_id → 自动带出未收明细"
assert_num 20 "$(jqv '.data.items[0].qty')" "自动带出的数量 = 未收 20"
PO_IN=$(jqv '.data.header.id')
call PATCH "/business/erp/stock-in/$PO_IN/status" '{"status":"POSTED"}'
assert_eq 200 "$REQ_CODE" "采购收货过账"
assert_num 30 "$(psql_q "SELECT COALESCE(SUM(si.qty),0) FROM biz_stock_in s JOIN biz_stock_in_item si ON si.stock_in_id = s.id WHERE s.source_order_id = '$PO'::uuid AND si.source_item_id = '$PO_ITEM'::uuid AND s.status = 'POSTED' AND s.deleted_at IS NULL")" "已收数量（派生）= 10+20"
assert_eq "RECEIVED" "$(psql_q "SELECT status FROM biz_purchase_order WHERE id = '$PO'::uuid")" "收满推送采购单 RECEIVED"

echo "==> 11. 改单（DRAFT 能改）/ 删除（DRAFT 能删）/ 作废"
call POST "/business/erp/stock-in" \
     "{\"supplier_id\":\"$SUPPLIER\",\"warehouse_id\":\"$WAREHOUSE\",
       \"items\":[{\"material_id\":\"$MATERIAL_A\",\"qty\":\"5\",\"price_excl\":\"2\"}]}"
DRAFT=$(jqv '.data.header.id')
call PUT "/business/erp/stock-in/$DRAFT" \
     "{\"supplier_id\":\"$SUPPLIER\",\"warehouse_id\":\"$WAREHOUSE\",
       \"items\":[{\"material_id\":\"$MATERIAL_A\",\"qty\":\"7\",\"price_excl\":\"2\",\"tax_rate\":\"13\"},
                 {\"material_id\":\"$MATERIAL_SN\",\"qty\":\"1\",\"price_excl\":\"40\"}]}"
assert_eq 200 "$REQ_CODE" "DRAFT 改单"
assert_eq 2 "$(jqv '.data.items | length')" "改单后 2 行"
assert_num 54 "$(jqv '.data.header.total_amount')" "改单后汇总重算 7×2 + 1×40 = 54"

call PATCH "/business/erp/stock-in/$DRAFT/status" '{"status":"CANCELLED","reason":"E2E 作废草稿"}'
assert_eq 200 "$REQ_CODE" "DRAFT 作废"
assert_eq "CANCELLED" "$(jqv '.data.header.status')" "作废后状态"

call POST "/business/erp/stock-in" \
     "{\"supplier_id\":\"$SUPPLIER\",\"warehouse_id\":\"$WAREHOUSE\",
       \"items\":[{\"material_id\":\"$MATERIAL_A\",\"qty\":\"1\",\"price_excl\":\"2\"}]}"
DEL=$(jqv '.data.header.id')
call DELETE "/business/erp/stock-in/$DEL"
assert_eq 200 "$REQ_CODE" "DRAFT 删除"
call GET "/business/erp/stock-in/$DEL"
assert_eq 404 "$REQ_CODE" "删完详情 404"

echo "==> 12. 列表分页"
call GET "/business/erp/stock-in/page?page=1&page_size=10&status=POSTED&supplier_id=$SUPPLIER"
assert_eq 200 "$REQ_CODE" "列表 200"
assert_eq "POSTED" "$(jqv '.data.list[0].status')" "按状态筛出 POSTED"
assert_num 3 "$(jqv '.data.total')" "本夹具 POSTED 单共 3 张（普通×2 + 采购收货×1）"

echo "==> 13. 清理（只删本脚本夹具）"
psql_x "DELETE FROM biz_material_serial WHERE source_type = 'PURCHASE' AND source_id IN
          (SELECT id FROM biz_stock_in WHERE supplier_id = '$SUPPLIER'::uuid)" >/dev/null
psql_x "DELETE FROM biz_inventory_txn WHERE doc_type = 'STOCK_IN' AND doc_id IN
          (SELECT id FROM biz_stock_in WHERE supplier_id = '$SUPPLIER'::uuid)" >/dev/null
psql_x "DELETE FROM biz_stock_in WHERE supplier_id = '$SUPPLIER'::uuid" >/dev/null
psql_x "DELETE FROM biz_purchase_order WHERE supplier_id = '$SUPPLIER'::uuid" >/dev/null
psql_x "DELETE FROM biz_inventory WHERE material_id IN ('$MATERIAL_A'::uuid, '$MATERIAL_SN'::uuid)
          AND warehouse_id = '$WAREHOUSE'::uuid" >/dev/null
LEFT=$(psql_q "SELECT count(*) FROM biz_stock_in WHERE supplier_id = '$SUPPLIER'::uuid")
assert_eq 0 "$LEFT" "清理后残留收货单 0"

echo
echo "==> 汇总：PASS=$PASS FAIL=$FAIL"
[ "$FAIL" -eq 0 ] || exit 1

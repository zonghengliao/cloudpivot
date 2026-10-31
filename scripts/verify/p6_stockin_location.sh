#!/usr/bin/env bash
# ============================================================
# P6 收货过账**补写库位账**（`biz_inventory_location`）e2e
#
#   覆盖（卡面「四、验收（真机）」逐条 + 两本账一致）：
#     ① 全新库迁移到最新（本脚本只读校验 schema 已到位，建库见脚本头）
#     ② POST /erp/stock-in 200 件 → PATCH POSTED
#     ③ SELECT * FROM biz_inventory_location WHERE material_id=… → 有行、qty=200
#     ④ SELECT SUM(库位账) = biz_inventory.qty = 200（不变式③，一条 SQL 两边比）
#     ⑤ GET /erp/inventory-location/page → total=1（不用夹具了）
#     ⑥ GET /erp/inventory/page 与 /erp/inventory-location/page 两个 Tab 数字一致
#     +  带库位收货 → 加到那条库位行（不新起行）；同库位再收 → 累加（唯一键）
#     +  重复过账 409 且库位账不再加（幂等）
#     +  收尾清理并核对残留 = 0
#
# 用法（隔离实例 + 隔离库，全程不碰 prod）：
#   su - postgres -c 'createdb -O cloudpivot cloudpivot_p6'
#   # 起一次实例让迁移自动跑完（见卡面证据文档），然后：
#   export P6_BASE=http://127.0.0.1:18096/api
#   export P6_DB_PASSWORD='<cloudpivot 库口令>'
#   export P6_DB_NAME=cloudpivot_p6          # 默认 cloudpivot_p6
#   export P6_ADMIN_PASSWORD='<admin 口令>'  # 默认 Nereus792379（dev/test 种子）
#   bash scripts/verify/p6_stockin_location.sh
#
# 退出码 = 是否有断言失败（0 = 全过）。夹具全部带 `P6-` / `e6b1…` 前缀，
# 开头按固定 UUID 清上一轮，结尾打印清理结果并核对残留为 0。
# ============================================================
set -uo pipefail

BASE="${P6_BASE:-http://127.0.0.1:18096/api}"
DBN="${P6_DB_NAME:-cloudpivot_p6}"
DBH="${P6_DB_HOST:-127.0.0.1}"
DBU="${P6_DB_USER:-cloudpivot}"
ADMIN_USER="${P6_ADMIN:-admin}"
ADMIN_PASS="${P6_ADMIN_PASSWORD:-Nereus792379}"
: "${P6_DB_PASSWORD:?请先 export P6_DB_PASSWORD（cloudpivot 库口令）}"

UNIT="e6b10001-0000-4000-8000-000000000001"
SUPPLIER="e6b10001-0000-4000-8000-000000000002"
WAREHOUSE="e6b10001-0000-4000-8000-000000000003"
MATERIAL_A="e6b10001-0000-4000-8000-000000000011"
LOC_DICT="e6b10001-0000-4000-8000-000000000021"

PASS=0
FAIL=0
TOKEN=""

# ---------- 断言工具（必须在调用前定义：放到登录之后会让断言被静默跳过） ----------
ok() { PASS=$((PASS + 1)); printf '[PASS] %s\n' "$1"; }
bad() { FAIL=$((FAIL + 1)); printf '[FAIL] %s\n' "$1"; }
assert_eq() { if [ "$1" = "$2" ]; then ok "$3（$2）"; else bad "$3：期望 [$1] 实际 [$2]"; fi; }
# 金额/数量比数值不比字符串：`NUMERIC(18,4)` 经 JSON 回来是 "200.0000"
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

psql_q() { PGPASSWORD="$P6_DB_PASSWORD" psql -h "$DBH" -U "$DBU" -d "$DBN" -Atc "$1" 2>&1 | head -1; }
psql_x() { PGPASSWORD="$P6_DB_PASSWORD" psql -h "$DBH" -U "$DBU" -d "$DBN" -v ON_ERROR_STOP=1 -c "$1" 2>&1 | head -3; }

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
NEW_PASS="P6_${ADMIN_PASS}_new"
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
    # 改密接口本身要认证：必须带着刚拿到的 TOKEN；新口令不能与初始口令相同
    REQ_CODE=$(curl -s -o /dev/null -w '%{http_code}' -X PUT \
        -H "Authorization: Bearer $TOKEN" -H 'Content-Type: application/json' \
        --data-binary "{\"old_password\":\"$ADMIN_PASS\",\"new_password\":\"$NEW_PASS\"}" \
        "$BASE/system/auth/password")
    assert_eq 200 "$REQ_CODE" "首登改密"
    login_once "$NEW_PASS"
fi
assert_eq 200 "$REQ_CODE" "登录（admin）"

echo "==> 1. 清理上一轮夹具 + 造夹具"
# 顺序：序列号 → 流水 → 成本账 → 库位账（成本账的 location_id 硬引用它）→ 收货单 → 采购单
psql_x "DELETE FROM biz_material_serial WHERE source_type = 'PURCHASE' AND source_id IN
          (SELECT id FROM biz_stock_in WHERE supplier_id = '$SUPPLIER'::uuid)" >/dev/null
psql_x "DELETE FROM biz_inventory_txn WHERE doc_type = 'STOCK_IN' AND doc_id IN
          (SELECT id FROM biz_stock_in WHERE supplier_id = '$SUPPLIER'::uuid)" >/dev/null
psql_x "DELETE FROM biz_inventory WHERE material_id = '$MATERIAL_A'::uuid
          AND warehouse_id = '$WAREHOUSE'::uuid" >/dev/null
psql_x "DELETE FROM biz_inventory_location WHERE material_id = '$MATERIAL_A'::uuid
          AND warehouse_id = '$WAREHOUSE'::uuid" >/dev/null
psql_x "DELETE FROM biz_stock_in WHERE supplier_id = '$SUPPLIER'::uuid" >/dev/null

# `ON CONFLICT DO NOTHING`（不带目标列）兜住任意唯一约束，重跑不中断
psql_x "INSERT INTO biz_unit (id, unit_code, unit_name, is_base, base_unit_id, rate, status, sort_order)
        VALUES ('$UNIT', 'P6-E2E-PCS', '个', 1, NULL, 1.000000, 1, 1) ON CONFLICT DO NOTHING" >/dev/null
psql_x "INSERT INTO biz_supplier (id, supplier_code, supplier_name, status)
        VALUES ('$SUPPLIER', 'P6-E2E-SUP', 'P6-E2E 供应商', 1) ON CONFLICT DO NOTHING" >/dev/null
psql_x "INSERT INTO biz_warehouse (id, warehouse_code, warehouse_name, warehouse_path, level, is_leaf, status)
        VALUES ('$WAREHOUSE', 'P6-E2E-WH', 'P6-E2E 收货仓', '/$WAREHOUSE/', 1, 1, 1) ON CONFLICT DO NOTHING" >/dev/null
psql_x "INSERT INTO biz_material (id, material_code, material_name, unit_id, status, serial_required)
        VALUES ('$MATERIAL_A', 'P6-E2E-M1', 'P6 螺丝 M6x20', '$UNIT'::uuid, 1, false) ON CONFLICT DO NOTHING" >/dev/null
psql_x "INSERT INTO biz_location (id, location_code, location_name, warehouse_id, location_type,
                                 is_default, capacity, status, sort_order)
        VALUES ('$LOC_DICT', 'P6-E2E-L01', 'P6-E2E A 区一号', '$WAREHOUSE'::uuid, 'STORAGE', 0, NULL, 1, 1)
        ON CONFLICT DO NOTHING" >/dev/null
FIX=$(psql_q "SELECT (SELECT count(*) FROM biz_warehouse WHERE id = '$WAREHOUSE'::uuid)
                 || '|' || (SELECT count(*) FROM biz_material WHERE id = '$MATERIAL_A'::uuid)
                 || '|' || (SELECT count(*) FROM biz_location WHERE id = '$LOC_DICT'::uuid)")
assert_eq "1|1|1" "$FIX" "夹具就位（仓库 / 物料 / 库位字典）"

echo "==> 2. 不带库位收 200 件 → 过账"
call POST "/business/erp/stock-in" \
     "{\"supplier_id\":\"$SUPPLIER\",\"warehouse_id\":\"$WAREHOUSE\",
       \"remark\":\"P6-E2E 收货（不带库位）\",
       \"items\":[{\"material_id\":\"$MATERIAL_A\",\"qty\":\"200\",\"price_excl\":\"2\",\"tax_rate\":\"0\"}]}"
assert_eq 200 "$REQ_CODE" "建收货单（200 件）"
IN_ID=$(jqv '.data.header.id')
IN_NO=$(jqv '.data.header.in_no')
assert_eq "DRAFT" "$(jqv '.data.header.status')" "建单落 DRAFT"
assert_contains "$IN_NO" "IN-" "单号前缀 IN-（实际 $IN_NO）"

call PATCH "/business/erp/stock-in/$IN_ID/status" '{"status":"POSTED"}'
assert_eq 200 "$REQ_CODE" "过账 POSTED"
assert_eq "POSTED" "$(jqv '.data.header.status')" "单据变 POSTED"

echo "==> 3. 库位账落库（③ 卡面：SELECT * … → 有行、qty=200）"
LOC_ROW=$(psql_q "SELECT count(*)||'|'||COALESCE(SUM(qty), 0)||'|'||COALESCE(bool_and(location_id IS NULL)::text, 'n/a')
    FROM biz_inventory_location
   WHERE material_id = '$MATERIAL_A'::uuid AND warehouse_id = '$WAREHOUSE'::uuid")
assert_eq "1|200.0000|true" "$LOC_ROW" "库位账：1 行、qty=200、落在 location_id IS NULL 桶"
LOCKED=$(psql_q "SELECT locked_qty FROM biz_inventory_location
   WHERE material_id = '$MATERIAL_A'::uuid AND warehouse_id = '$WAREHOUSE'::uuid")
assert_num 0 "$LOCKED" "locked_qty 初始 0（收货不占用量）"

echo "==> 4. 不变式③：SUM(库位账) = 成本账 qty = 200（一条 SQL 两边比）"
INV=$(psql_q "SELECT (SELECT COALESCE(SUM(qty), 0) FROM biz_inventory_location
                        WHERE material_id = '$MATERIAL_A'::uuid AND warehouse_id = '$WAREHOUSE'::uuid)
                 || '|' ||
                 (SELECT COALESCE(SUM(qty), 0) FROM biz_inventory
                        WHERE material_id = '$MATERIAL_A'::uuid AND warehouse_id = '$WAREHOUSE'::uuid)")
assert_eq "200.0000|200.0000" "$INV" "库位账合计 == 成本账合计（不变式③）"
AMT=$(psql_q "SELECT amount FROM biz_inventory
   WHERE material_id = '$MATERIAL_A'::uuid AND warehouse_id = '$WAREHOUSE'::uuid")
assert_num 400 "$AMT" "成本金额 200×2 = 400（不含税口径）"

echo "==> 5. 两个 Tab 的接口：库位库存 /erp/inventory-location/page 与 即时库存 /erp/inventory/page"
call GET "/business/erp/inventory-location/page?material_id=$MATERIAL_A&page=1&page_size=10"
assert_eq 200 "$REQ_CODE" "GET /erp/inventory-location/page"
assert_num 1 "$(jqv '.data.total')" "库位库存 total = 1（**不用夹具**，真接口造数）"
assert_num 200 "$(jqv '.data.list[0].qty')" "库位库存 Tab 的数量 = 200"
assert_eq "NULL" "$(jqv '.data.list[0].location_id // "NULL"')" "该行的 location_id 为 NULL（未分配库位桶）"
LOC_PAGE_ID=$(jqv '.data.list[0].id')
assert_contains "$LOC_PAGE_ID" "-" "库位库存行 id 正常返回"

call GET "/business/erp/inventory/page?material_id=$MATERIAL_A&page=1&page_size=10"
assert_eq 200 "$REQ_CODE" "GET /erp/inventory/page"
assert_num 1 "$(jqv '.data.total')" "即时库存 total = 1"
INST_QTY=$(jqv '.data.list[0].qty')
assert_num 200 "$INST_QTY" "即时库存 Tab 的数量 = 200"
assert_num 200 "$(jqv '.data.list[0].qty')" "两 Tab 数字一致（库位库存 200 / 即时库存 $INST_QTY）"

echo "==> 6. 带库位收 50 件 → 加到那条库位行（不新起一行）"
# 库位库存行没有建接口（W3a/W2b 的既有口径：由一物一码/调拨流程创建），e2e 用 psql 造一条空行
LOC_ROW_ID=$(psql_q "INSERT INTO biz_inventory_location
        (id, material_id, warehouse_id, location_id, qty, locked_qty)
     VALUES (gen_random_uuid(), '$MATERIAL_A'::uuid, '$WAREHOUSE'::uuid, '$LOC_DICT'::uuid, 0, 0)
     RETURNING id")
assert_contains "$LOC_ROW_ID" "-" "造库位库存行（qty=0，指向库位字典 $LOC_DICT）"

call POST "/business/erp/stock-in" \
     "{\"supplier_id\":\"$SUPPLIER\",\"warehouse_id\":\"$WAREHOUSE\",
       \"remark\":\"P6-E2E 收货（带库位）\",
       \"items\":[{\"material_id\":\"$MATERIAL_A\",\"qty\":\"50\",\"price_excl\":\"2\",\"tax_rate\":\"0\",
                   \"location_id\":\"$LOC_ROW_ID\"}]}"
assert_eq 200 "$REQ_CODE" "建带库位收货单"
IN_ID2=$(jqv '.data.header.id')
call PATCH "/business/erp/stock-in/$IN_ID2/status" '{"status":"POSTED"}'
assert_eq 200 "$REQ_CODE" "带库位收货过账"

read -r LQTY LCOUNT < <(psql_q "SELECT qty||' '||(SELECT count(*) FROM biz_inventory_location
        WHERE material_id = '$MATERIAL_A'::uuid AND warehouse_id = '$WAREHOUSE'::uuid)
     FROM biz_inventory_location WHERE id = '$LOC_ROW_ID'::uuid")
assert_num 50 "$LQTY" "库位账：指定的那行 +50（0 → 50）"
assert_num 2 "$LCOUNT" "库位账总行数 = 2（NULL 桶 200 + 库位行 50），没多起行"

echo "==> 7. 同一库位再收 20 → 累加（uk_biz_inv_loc 生效，行数不变）"
call POST "/business/erp/stock-in" \
     "{\"supplier_id\":\"$SUPPLIER\",\"warehouse_id\":\"$WAREHOUSE\",
       \"items\":[{\"material_id\":\"$MATERIAL_A\",\"qty\":\"20\",\"price_excl\":\"2\",\"tax_rate\":\"0\",
                   \"location_id\":\"$LOC_ROW_ID\"}]}"
assert_eq 200 "$REQ_CODE" "建第三张收货单"
IN_ID3=$(jqv '.data.header.id')
call PATCH "/business/erp/stock-in/$IN_ID3/status" '{"status":"POSTED"}'
assert_eq 200 "$REQ_CODE" "第三张过账"
read -r LQTY2 LCOUNT2 < <(psql_q "SELECT qty||' '||(SELECT count(*) FROM biz_inventory_location
        WHERE material_id = '$MATERIAL_A'::uuid AND warehouse_id = '$WAREHOUSE'::uuid)
     FROM biz_inventory_location WHERE id = '$LOC_ROW_ID'::uuid")
assert_num 70 "$LQTY2" "同一库位累加到 70（50 + 20）"
assert_num 2 "$LCOUNT2" "行数仍是 2（唯一键挡住了新行）"

echo "==> 8. 不变式③（混合库位后再核一次）+ 两 Tab 数字仍一致"
INV2=$(psql_q "SELECT (SELECT COALESCE(SUM(qty), 0) FROM biz_inventory_location
                        WHERE material_id = '$MATERIAL_A'::uuid AND warehouse_id = '$WAREHOUSE'::uuid)
                 || '|' ||
                 (SELECT COALESCE(SUM(qty), 0) FROM biz_inventory
                        WHERE material_id = '$MATERIAL_A'::uuid AND warehouse_id = '$WAREHOUSE'::uuid)")
assert_eq "270.0000|270.0000" "$INV2" "库位账合计 == 成本账合计 = 270（不变式③）"
call GET "/business/erp/inventory-location/page?material_id=$MATERIAL_A&page=1&page_size=10"
LOC_SUM=$(jqv '[.data.list[].qty] | map(tonumber) | add')
assert_num 270 "$LOC_SUM" "库位库存 Tab 两行相加 = 270"
call GET "/business/erp/inventory/page?material_id=$MATERIAL_A&page=1&page_size=10"
assert_num 270 "$(jqv '[.data.list[].qty] | map(tonumber) | add')" "即时库存 Tab = 270（与库位库存一致）"

echo "==> 9. 幂等：重复过账 409，库位账不再加"
call PATCH "/business/erp/stock-in/$IN_ID/status" '{"status":"POSTED"}'
assert_eq 409 "$REQ_CODE" "重复过账 409"
assert_contains "$(jqv '.msg')" "已过账" "409 文案点名已过账"
assert_num 270 "$(psql_q "SELECT COALESCE(SUM(qty), 0) FROM biz_inventory_location
    WHERE material_id = '$MATERIAL_A'::uuid AND warehouse_id = '$WAREHOUSE'::uuid")" "库位账合计仍是 270"

echo "==> 10. 收尾清理 + 残留核对"
psql_x "DELETE FROM biz_inventory_txn WHERE doc_type = 'STOCK_IN' AND doc_id IN
          (SELECT id FROM biz_stock_in WHERE supplier_id = '$SUPPLIER'::uuid)" >/dev/null
psql_x "DELETE FROM biz_inventory WHERE material_id = '$MATERIAL_A'::uuid
          AND warehouse_id = '$WAREHOUSE'::uuid" >/dev/null
psql_x "DELETE FROM biz_inventory_location WHERE material_id = '$MATERIAL_A'::uuid
          AND warehouse_id = '$WAREHOUSE'::uuid" >/dev/null
psql_x "DELETE FROM biz_stock_in WHERE supplier_id = '$SUPPLIER'::uuid" >/dev/null
psql_x "DELETE FROM biz_location WHERE id = '$LOC_DICT'::uuid" >/dev/null
psql_x "DELETE FROM biz_material WHERE id = '$MATERIAL_A'::uuid" >/dev/null
psql_x "DELETE FROM biz_warehouse WHERE id = '$WAREHOUSE'::uuid" >/dev/null
psql_x "DELETE FROM biz_supplier WHERE id = '$SUPPLIER'::uuid" >/dev/null
psql_x "DELETE FROM biz_unit WHERE id = '$UNIT'::uuid" >/dev/null
RESIDUE=$(psql_q "SELECT (SELECT count(*) FROM biz_stock_in WHERE supplier_id = '$SUPPLIER'::uuid)
                     || '|' || (SELECT count(*) FROM biz_inventory
                                  WHERE material_id = '$MATERIAL_A'::uuid)
                     || '|' || (SELECT count(*) FROM biz_inventory_location
                                  WHERE material_id = '$MATERIAL_A'::uuid)
                     || '|' || (SELECT count(*) FROM biz_inventory_txn
                                  WHERE material_id = '$MATERIAL_A'::uuid)")
assert_eq "0|0|0|0" "$RESIDUE" "残留核对（收货单 / 成本账 / 库位账 / 流水）"

echo
echo "==> 汇总：PASS=$PASS FAIL=$FAIL"
[ "$FAIL" -eq 0 ] || exit 1

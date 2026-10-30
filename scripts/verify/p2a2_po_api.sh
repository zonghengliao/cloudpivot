#!/usr/bin/env bash
# ============================================================
# P2-A2 采购订单接口 e2e（真实 curl，跑在隔离实例上）
#
#   覆盖：建单（两种口径 + 金额服务端算 + 防篡改）→ 校验边界（空明细 / 0 数量 /
#         负单价 / 税率越界 / 缺供应商）→ 改单（DRAFT 能改 / SUBMITTED 409）→
#         状态机（DRAFT→SUBMITTED ✅ / DRAFT→CLOSED 409 / SUBMITTED→CANCELLED 需 reason）
#         → 删除（DRAFT ✅ / SUBMITTED 409）→ 列表分页
#
# 用法：
#   export P2A2_BASE=http://127.0.0.1:18197/api
#   export P2A2_DB_PASSWORD='<cloudpivot 库口令>'
#   export P2A2_DB_NAME=cloudpivot_p2a2        # 默认 cloudpivot_p2a2
#   export P2A2_ADMIN_PASSWORD='<admin 口令>'  # 默认 Nereus792379（dev/test 种子）
#   bash scripts/verify/p2a2_po_api.sh
#
# 退出码 = 是否有断言失败（0 = 全过）。脚本自建夹具（供应商 / 仓库 / 物料），
# 开头按 `P2A2-E2E` 前缀清上一轮，结尾打印清理 SQL 并核对残留。
# ============================================================
set -uo pipefail

BASE="${P2A2_BASE:-http://127.0.0.1:18197/api}"
DBN="${P2A2_DB_NAME:-cloudpivot_p2a2}"
DBH="${P2A2_DB_HOST:-127.0.0.1}"
DBU="${P2A2_DB_USER:-cloudpivot}"
ADMIN_USER="${P2A2_ADMIN:-admin}"
ADMIN_PASS="${P2A2_ADMIN_PASSWORD:-Nereus792379}"
: "${P2A2_DB_PASSWORD:?请先 export P2A2_DB_PASSWORD（cloudpivot 库口令）}"

SUPPLIER="e2a20001-0000-4000-8000-000000000001"
WAREHOUSE="e2a20001-0000-4000-8000-000000000002"
UNIT="e2a20001-0000-4000-8000-000000000003"
MATERIAL_A="e2a20001-0000-4000-8000-000000000011"
MATERIAL_B="e2a20001-0000-4000-8000-000000000012"

PASS=0
FAIL=0
TOKEN=""

psql_q() { PGPASSWORD="$P2A2_DB_PASSWORD" psql -h "$DBH" -U "$DBU" -d "$DBN" -Atc "$1" 2>&1 | head -1; }
psql_x() { PGPASSWORD="$P2A2_DB_PASSWORD" psql -h "$DBH" -U "$DBU" -d "$DBN" -v ON_ERROR_STOP=1 -c "$1" 2>&1 | head -3; }

ok() { PASS=$((PASS + 1)); printf '[PASS] %s\n' "$1"; }
bad() { FAIL=$((FAIL + 1)); printf '[FAIL] %s\n' "$1"; }
assert_eq() { if [ "$1" = "$2" ]; then ok "$3（$2）"; else bad "$3：期望 [$1] 实际 [$2]"; fi; }
# 金额比数值而不是比字符串：库里是 `NUMERIC(18,4)`，JSON 里带回 "200.0000" 这种标度
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

# 发请求：REQ_CODE / REQ_BODY 写回全局（**不要用 $(...) 包**，否则子 shell 里赋值丢失）
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

echo "==> 0. 清理上一轮 e2e 夹具（只动 $SUPPLIER 的订单）"
psql_x "DELETE FROM biz_purchase_order WHERE supplier_id = '$SUPPLIER'::uuid" >/dev/null
# `ON CONFLICT DO NOTHING`（不带目标列）能兜住任意唯一约束（unit_code / material_code 等），
# 重跑不会因为「编码已存在」中断；编码统一带 P2A2-E2E- 前缀，避免与其它卡的夹具撞车。
psql_x "INSERT INTO biz_unit (id, unit_code, unit_name, is_base, base_unit_id, rate, status, sort_order)
        VALUES ('$UNIT', 'P2A2-E2E-PCS', '个', 1, NULL, 1.000000, 1, 1) ON CONFLICT DO NOTHING"
psql_x "INSERT INTO biz_supplier (id, supplier_code, supplier_name, status)
        VALUES ('$SUPPLIER', 'P2A2-E2E-SUP', 'P2A2-E2E 供应商', 1) ON CONFLICT DO NOTHING"
psql_x "INSERT INTO biz_warehouse (id, warehouse_code, warehouse_name, warehouse_path, level, is_leaf, status)
        VALUES ('$WAREHOUSE', 'P2A2-E2E-WH', 'P2A2-E2E 仓', '/$WAREHOUSE/', 1, 1, 1)
        ON CONFLICT DO NOTHING"
psql_x "INSERT INTO biz_material (id, material_code, material_name, unit_id, status)
        VALUES ('$MATERIAL_A', 'P2A2-E2E-M1', '螺丝 M6x20', '$UNIT', 1),
               ('$MATERIAL_B', 'P2A2-E2E-M2', '垫片 8mm', '$UNIT', 1)
        ON CONFLICT DO NOTHING"
assert_eq "2" "$(psql_q "SELECT count(*) FROM biz_material WHERE material_code LIKE 'P2A2-E2E-%'")" "夹具物料就位"

echo "==> 1. 登录 admin"
carry_login() {
    local pw="$1" body tmp
    body="{\"username\":\"$ADMIN_USER\",\"password\":\"$pw\"}"
    tmp=$(mktemp)
    REQ_CODE=$(curl -s -o "$tmp" -w '%{http_code}' -X POST \
        -H 'Content-Type: application/json' --data-binary "$body" "$BASE/system/auth/login")
    REQ_BODY=$(cat "$tmp")
    rm -f "$tmp"
    TOKEN=$(jqv '.data.access_token')
    MUST=$(jqv '.data.must_change_password')
}
MUST=""
NEW_PASS="${P2A2_ADMIN_NEW_PASSWORD:-Nereus792379!P2a2}"
# 首登改密是一次性的：先试「改过之后的口令」，再退回环境里的初始口令（重跑幂等）
for pw in "$NEW_PASS" "${P2A2_ADMIN_PASSWORD:-$ADMIN_PASS}"; do
    carry_login "$pw"
    if [ -n "$TOKEN" ] && [ "$TOKEN" != "null" ]; then
        ADMIN_PASS="$pw"
        break
    fi
done
if [ -z "$TOKEN" ] || [ "$TOKEN" = "null" ]; then
    bad "登录失败（HTTP $REQ_CODE）：$REQ_BODY"
    echo "==> 汇总：PASS=$PASS FAIL=$FAIL"
    exit 1
fi
if [ "$MUST" = "true" ]; then
    tmp=$(mktemp)
    REQ_CODE=$(curl -s -o "$tmp" -w '%{http_code}' -X PUT \
        -H "Authorization: Bearer $TOKEN" -H 'Content-Type: application/json' \
        --data-binary "{\"old_password\":\"$ADMIN_PASS\",\"new_password\":\"$NEW_PASS\"}" \
        "$BASE/system/auth/password")
    rm -f "$tmp"
    assert_eq "200" "$REQ_CODE" "首登强制改密"
    carry_login "$NEW_PASS"
fi
assert_contains "$REQ_BODY" "access_token" "登录拿到令牌"

po_body() {
    # $1 = price_mode, $2 = items JSON
    printf '{"supplier_id":"%s","warehouse_id":"%s","order_date":"2026-10-31","price_mode":"%s","remark":"P2A2-E2E","items":%s}' \
        "$SUPPLIER" "$WAREHOUSE" "$1" "$2"
}

echo "==> 2. 口径 A 建单（100 × 2，13%）"
call POST /business/erp/purchase-order \
    "$(po_body EXCL "[{\"material_id\":\"$MATERIAL_A\",\"qty\":100,\"price_excl\":2,\"tax_rate\":13}]")"
assert_eq "200" "$REQ_CODE" "建单 HTTP"
PO_EXCL=$(jqv '.data.order.id')
assert_eq "DRAFT" "$(jqv '.data.order.status')" "建单落 DRAFT"
assert_eq "EXCL" "$(jqv '.data.order.price_mode')" "口径回显 EXCL"
assert_num "200" "$(jqv '.data.order.total_amount')" "口径 A 不含税金额"
assert_num "26" "$(jqv '.data.order.total_tax')" "口径 A 税额"
assert_num "226" "$(jqv '.data.order.total_with_tax')" "口径 A 价税合计"
assert_contains "$(jqv '.data.order.order_no')" "PO-" "单号前缀 PO-"
assert_num "200" "$(jqv '.data.items[0].amount')" "行不含税金额"
assert_num "26" "$(jqv '.data.items[0].tax_amount')" "行税额"

echo "==> 3. 口径 B 建单（100 × 2，13%，含税录入）"
call POST /business/erp/purchase-order \
    "$(po_body INCL "[{\"material_id\":\"$MATERIAL_A\",\"qty\":100,\"price_incl\":2,\"tax_rate\":13}]")"
assert_eq "200" "$REQ_CODE" "口径 B 建单 HTTP"
PO_INCL=$(jqv '.data.order.id')
assert_num "200" "$(jqv '.data.order.total_with_tax')" "口径 B 价税合计"
assert_num "176.99" "$(jqv '.data.order.total_amount')" "口径 B 不含税金额（倒算）"
assert_num "23.01" "$(jqv '.data.order.total_tax')" "口径 B 税额（减法倒挤）"

echo "==> 4. 防篡改：客户端塞金额 / 0 数量 / 负单价 / 税率越界"
call POST /business/erp/purchase-order "$(po_body EXCL \
    "[{\"material_id\":\"$MATERIAL_A\",\"qty\":100,\"price_excl\":2,\"tax_rate\":13,\"amount\":\"1.00\",\"with_tax_amount\":\"1.00\"}]")"
assert_eq "200" "$REQ_CODE" "带 amount 字段仍成功（未知字段忽略）"
assert_num "226" "$(jqv '.data.order.total_with_tax')" "客户端金额被忽略，仍是 226"
PO_TAMPER=$(jqv '.data.order.id')

call POST /business/erp/purchase-order "$(po_body EXCL "[]")"
assert_eq "400" "$REQ_CODE" "空明细 400"
call POST /business/erp/purchase-order \
    "$(po_body EXCL "[{\"material_id\":\"$MATERIAL_A\",\"qty\":0,\"price_excl\":2,\"tax_rate\":13}]")"
assert_eq "400" "$REQ_CODE" "0 数量 400（库约束 qty > 0）"
call POST /business/erp/purchase-order \
    "$(po_body EXCL "[{\"material_id\":\"$MATERIAL_A\",\"qty\":1,\"price_excl\":-2,\"tax_rate\":13}]")"
assert_eq "400" "$REQ_CODE" "负单价 400"
call POST /business/erp/purchase-order \
    "$(po_body EXCL "[{\"material_id\":\"$MATERIAL_A\",\"qty\":1,\"price_excl\":2,\"tax_rate\":101}]")"
assert_eq "400" "$REQ_CODE" "税率 101 400"
call POST /business/erp/purchase-order \
    '{"warehouse_id":"'"$WAREHOUSE"'","price_mode":"EXCL","items":[{"material_id":"'"$MATERIAL_A"'","qty":1,"price_excl":2,"tax_rate":13}]}'
assert_eq "400" "$REQ_CODE" "缺 supplier_id 400"

echo "==> 5. 改单：DRAFT 能改；SUBMITTED 409"
call PUT "/business/erp/purchase-order/$PO_EXCL" "$(po_body EXCL \
    "[{\"material_id\":\"$MATERIAL_A\",\"qty\":10,\"price_excl\":2,\"tax_rate\":13},{\"material_id\":\"$MATERIAL_B\",\"qty\":5,\"price_excl\":3,\"tax_rate\":6}]")"
assert_eq "200" "$REQ_CODE" "DRAFT 改单 HTTP"
assert_eq "2" "$(jqv '.data.items | length')" "明细被整单替换成 2 行"
assert_num "38.5" "$(jqv '.data.order.total_with_tax')" "改单后价税合计重算"

echo "==> 6. 状态机"
call PATCH "/business/erp/purchase-order/$PO_EXCL/status" '{"status":"CLOSED"}'
assert_eq "409" "$REQ_CODE" "DRAFT→CLOSED 跳级 409"
call PATCH "/business/erp/purchase-order/$PO_EXCL/status" '{"status":"SUBMITTED"}'
assert_eq "200" "$REQ_CODE" "DRAFT→SUBMITTED ✅"
assert_eq "SUBMITTED" "$(jqv '.data.order.status')" "状态已提交"
call PUT "/business/erp/purchase-order/$PO_EXCL" "$(po_body EXCL \
    "[{\"material_id\":\"$MATERIAL_A\",\"qty\":1,\"price_excl\":2,\"tax_rate\":13}]")"
assert_eq "409" "$REQ_CODE" "SUBMITTED 改单 409"
call DELETE "/business/erp/purchase-order/$PO_EXCL"
assert_eq "409" "$REQ_CODE" "SUBMITTED 删除 409"
call PATCH "/business/erp/purchase-order/$PO_EXCL/status" '{"status":"CANCELLED"}'
assert_eq "400" "$REQ_CODE" "SUBMITTED→CANCELLED 缺 reason 400"
call PATCH "/business/erp/purchase-order/$PO_EXCL/status" \
    '{"status":"CANCELLED","reason":"P2A2-E2E 供应商断供"}'
assert_eq "200" "$REQ_CODE" "SUBMITTED→CANCELLED 带 reason ✅"
assert_eq "CANCELLED" "$(jqv '.data.order.status')" "状态已取消"
assert_contains "$(jqv '.data.order.remark')" "取消原因" "取消原因写进 remark"

echo "==> 7. 删除：DRAFT 能删"
call DELETE "/business/erp/purchase-order/$PO_TAMPER"
assert_eq "200" "$REQ_CODE" "DRAFT 删除 ✅"
call GET "/business/erp/purchase-order/$PO_TAMPER"
assert_eq "404" "$REQ_CODE" "删完详情 404"

echo "==> 8. 列表分页带金额字段"
call GET "/business/erp/purchase-order/page?page=1&page_size=5&keyword=PO-"
assert_eq "200" "$REQ_CODE" "列表 HTTP"
assert_contains "$REQ_BODY" "total_with_tax" "列表返回 total_with_tax"
assert_contains "$REQ_BODY" "price_mode" "列表返回 price_mode"

echo "==> 9. 清理并核对残留"
PSQL_CLEAN="DELETE FROM biz_purchase_order WHERE supplier_id = '$SUPPLIER'::uuid;"
echo "    清理 SQL：$PSQL_CLEAN"
psql_x "$PSQL_CLEAN" >/dev/null
assert_eq "0" "$(psql_q "SELECT count(*) FROM biz_purchase_order WHERE supplier_id = '$SUPPLIER'::uuid")" "订单残留为 0"
echo "    夹具（供应商/仓库/物料）保留：$SUPPLIER / $WAREHOUSE / $MATERIAL_A"

echo "==> 汇总：PASS=$PASS FAIL=$FAIL"
[ "$FAIL" -eq 0 ] || exit 1

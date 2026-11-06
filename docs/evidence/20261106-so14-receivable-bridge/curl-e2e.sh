#!/usr/bin/env bash
# ============================================================================
# SO-14 出库 → 应收挂账联动：真实 token 的端到端 curl 实测
#
# 用法（先起一个指向**隔离演示库**的实例）：
#   BASE=http://127.0.0.1:18117/api DB=cloudpivot_so14 \
#     bash docs/evidence/20261106-so14-receivable-bridge/curl-e2e.sh
#
# 链路（走 HTTP handler，不绕过任何一道闸）：
#   POST /customer（带 credit_days=45）
#     → POST /sales-order（10 × 100，13% 税 → 含税 1130）
#     → 夹具快捷键：SQL 把订单置 APPROVED（审批流不属本卡，演示库无审批人）
#     → POST /shipment（从订单带出未发完的行）
#     → PATCH /shipment/{id}/status POSTED
#         ⇒ 销售出库单 + 三本账负向 + 成本结转凭证 + **应收自动挂账**（同一事务）
#     → GET /receivable/page + /receivable/{id} 验证金额 / 来源 / 期间 / 到期日
#     → 重复过账（409）后应收仍然只有 1 张（幂等）
#
# 脚本自带清场（只清 `SO14E2E` 前缀的夹具），可连跑多轮，每轮断言数量相同。
# ============================================================================
set -uo pipefail

BASE=${BASE:-http://127.0.0.1:18117/api}
DB=${DB:-cloudpivot_so14}
PGHOST=${PGHOST:-127.0.0.1}
PGUSER=${PGUSER:-alun}
export PGPASSWORD=${PGPASSWORD:-alun}
ADMIN_USER=${ADMIN_USER:-admin}
ADMIN_PASS=${ADMIN_PASS:-Admin@123456}

UNIT='d5140000-0000-4000-8000-000000000001'
WH='d5140000-0000-4000-8000-000000000002'
MAT='d5140000-0000-4000-8000-000000000011'
CCODE='SO14E2E-CUST'

PASS=0
FAIL=0
ok()    { PASS=$((PASS+1)); echo "  PASS  $*"; }
bad()   { FAIL=$((FAIL+1)); echo "  FAIL  $*"; }
check() { if [ "$2" = "$3" ]; then ok "$1（$3）"; else bad "$1：期望 $3，实际 $2"; fi }

psql_q() { psql -h "$PGHOST" -U "$PGUSER" -d "$DB" -Atc "$1"; }

echo "==> 0. 清场 + 夹具（只碰 SO14E2E 前缀与三张夹具主数据）"
psql_q "DELETE FROM biz_receivable WHERE source_id IN
          (SELECT o.id FROM biz_stock_out o JOIN biz_shipment s ON s.id = o.source_id
            JOIN biz_sales_order so ON so.id = s.so_id
            JOIN biz_customer c ON c.id = so.customer_id WHERE c.code LIKE 'SO14E2E%')" >/dev/null
psql_q "DELETE FROM biz_voucher WHERE source_id IN
          (SELECT o.id FROM biz_stock_out o JOIN biz_shipment s ON s.id = o.source_id
            JOIN biz_sales_order so ON so.id = s.so_id
            JOIN biz_customer c ON c.id = so.customer_id WHERE c.code LIKE 'SO14E2E%')" >/dev/null
psql_q "DELETE FROM biz_status_log WHERE entity_id IN
          (SELECT o.id FROM biz_stock_out o WHERE o.source_id IN
             (SELECT s.id FROM biz_shipment s JOIN biz_sales_order so ON so.id = s.so_id
               JOIN biz_customer c ON c.id = so.customer_id WHERE c.code LIKE 'SO14E2E%'))" >/dev/null
psql_q "DELETE FROM biz_status_log WHERE entity_id IN
          (SELECT s.id FROM biz_shipment s JOIN biz_sales_order so ON so.id = s.so_id
            JOIN biz_customer c ON c.id = so.customer_id WHERE c.code LIKE 'SO14E2E%')" >/dev/null
psql_q "DELETE FROM biz_status_log WHERE entity_id IN
          (SELECT so.id FROM biz_sales_order so JOIN biz_customer c ON c.id = so.customer_id
            WHERE c.code LIKE 'SO14E2E%')" >/dev/null
psql_q "DELETE FROM biz_stock_out WHERE source_id IN
          (SELECT s.id FROM biz_shipment s JOIN biz_sales_order so ON so.id = s.so_id
            JOIN biz_customer c ON c.id = so.customer_id WHERE c.code LIKE 'SO14E2E%')" >/dev/null
psql_q "DELETE FROM biz_shipment WHERE so_id IN
          (SELECT so.id FROM biz_sales_order so JOIN biz_customer c ON c.id = so.customer_id
            WHERE c.code LIKE 'SO14E2E%')" >/dev/null
psql_q "DELETE FROM biz_sales_order WHERE customer_id IN
          (SELECT id FROM biz_customer WHERE code LIKE 'SO14E2E%')" >/dev/null
psql_q "DELETE FROM biz_customer WHERE code LIKE 'SO14E2E%'" >/dev/null
psql_q "DELETE FROM biz_inventory_txn WHERE material_id = '$MAT'::uuid" >/dev/null
psql_q "DELETE FROM biz_inventory WHERE material_id = '$MAT'::uuid" >/dev/null
psql_q "DELETE FROM biz_inventory_location WHERE material_id = '$MAT'::uuid" >/dev/null

psql_q "INSERT INTO biz_unit (id, unit_code, unit_name, is_base, base_unit_id, rate, status, sort_order)
        VALUES ('$UNIT'::uuid, 'SO14E-PCS', '个', 1, NULL, 1.000000, 1, 1) ON CONFLICT DO NOTHING" >/dev/null
psql_q "INSERT INTO biz_warehouse (id, warehouse_code, warehouse_name, warehouse_path, level, is_leaf, status, allow_negative)
        VALUES ('$WH'::uuid, 'SO14E-WH', 'SO-14 挂账仓', '/$WH/', 1, 1, 1, 0) ON CONFLICT DO NOTHING" >/dev/null
psql_q "INSERT INTO biz_material (id, material_code, material_name, unit_id, status, serial_required)
        VALUES ('$MAT'::uuid, 'SO14E-M1', 'SO-14 挂账物料', '$UNIT'::uuid, 1, false) ON CONFLICT DO NOTHING" >/dev/null
# 库存夹具：成本账 + 库位账各 20（未分配桶，与单测 seed_stock 同口径；只为本卡演示过账可通过负库存闸）
psql_q "INSERT INTO biz_inventory_location (material_id, warehouse_id, location_id, qty)
        VALUES ('$MAT'::uuid, '$WH'::uuid, NULL, 20)" >/dev/null
psql_q "INSERT INTO biz_inventory (material_id, warehouse_id, location_id, qty, amount, avg_cost)
        VALUES ('$MAT'::uuid, '$WH'::uuid, NULL, 20, 200, 10)" >/dev/null
ok "夹具就位（客户经 API 建，库存 20）"

echo
echo "==> 1. 登录（admin，首登强制改密自动处理）"
login() {
  curl -s -X POST -H 'Content-Type: application/json' \
    --data-binary "{\"username\":\"$ADMIN_USER\",\"password\":\"$1\"}" \
    "$BASE/system/auth/login"
}
RESP=""; TOKEN=""
for candidate in "$ADMIN_PASS" "${ADMIN_PASS}_so14"; do
  for attempt in 1 2 3; do
    RESP=$(login "$candidate")
    TOKEN=$(echo "$RESP" | jq -r '.data.access_token // empty')
    [ -n "$TOKEN" ] && break
    # 10005 = 登录尝试过于频繁（连跑多轮会踩到），等一会再试，别把失败算成脚本错误
    if [ "$(echo "$RESP" | jq -r '.code')" = "10005" ]; then sleep 25; else break; fi
  done
  if [ -n "$TOKEN" ]; then ADMIN_PASS="$candidate"; break; fi
done
if [ -z "$TOKEN" ]; then echo "  !! 登录失败：$RESP"; exit 1; fi
MUST=$(echo "$RESP" | jq -r '.data.must_change_password // false')
if [ "$MUST" = "true" ]; then
  NEW_PASS="${ADMIN_PASS}_so14"
  curl -s -o /dev/null -X PUT -H "Authorization: Bearer ${TOKEN}" -H 'Content-Type: application/json' \
    --data-binary "{\"old_password\":\"$ADMIN_PASS\",\"new_password\":\"$NEW_PASS\"}" \
    "$BASE/system/auth/password"
  ADMIN_PASS="$NEW_PASS"
  TOKEN=$(login "$ADMIN_PASS" | jq -r '.data.access_token // empty')
fi
[ -n "$TOKEN" ] && ok "拿到 admin access_token" || { bad "登录失败"; exit 1; }
AUTH="Authorization: Bearer ${TOKEN}"

echo
echo "==> 2. 客户：credit_days 落库并经接口回显（V0289 新列）"
CJSON=$(curl -s -X POST -H "$AUTH" -H 'Content-Type: application/json' \
  --data-binary "{\"code\":\"$CCODE\",\"name\":\"SO-14 挂账演示客户\",\"credit_days\":45,\"credit_limit\":0}" \
  "$BASE/business/erp/customer")
CUST=$(echo "$CJSON" | jq -r '.data.id // empty')
[ -n "$CUST" ] && ok "建客户成功" || { bad "建客户失败：$CJSON"; exit 1; }
check "客户 credit_days = 45" "$(echo "$CJSON" | jq -r '.data.credit_days')" "45"
NEG=$(curl -s -X POST -H "$AUTH" -H 'Content-Type: application/json' \
  --data-binary "{\"code\":\"SO14E2E-BAD\",\"name\":\"负账期客户\",\"credit_days\":-5}" \
  "$BASE/business/erp/customer")
check "credit_days = -5 被拒（10001）" "$(echo "$NEG" | jq -r '.code')" "10001"

echo
echo "==> 3. 销售订单：10 × 100，税率 13% → 含税 1130"
SOJSON=$(curl -s -X POST -H "$AUTH" -H 'Content-Type: application/json' \
  --data-binary "{\"customer_id\":\"$CUST\",\"order_date\":\"2026-11-06\",\"items\":[{\"material_id\":\"$MAT\",\"qty\":10,\"price\":100,\"tax_rate\":13}]}" \
  "$BASE/business/erp/sales-order")
SO=$(echo "$SOJSON" | jq -r '.data.id // empty')
[ -n "$SO" ] && ok "建销售订单成功（$(echo "$SOJSON" | jq -r '.data.so_no')）" || { bad "建单失败：$SOJSON"; exit 1; }
check "订单不含税合计" "$(echo "$SOJSON" | jq -r '.data.total_amount')" "1000.0000"
check "订单价税合计" "$(echo "$SOJSON" | jq -r '.data.total_with_tax')" "1130.0000"
# 审批流不属本卡：演示库没有审批人，夹具快捷键把订单置 APPROVED（真实环境由 so_approve 流程推进）
psql_q "UPDATE biz_sales_order SET status='APPROVED', updated_at=now() WHERE id='$SO'::uuid" >/dev/null
ok "夹具：订单置 APPROVED（审批流由 SO-05/SO-06 覆盖，本卡不测）"

echo
echo "==> 4. 发货单 → 过账（一把梭：出库 + 三本账 + 成本凭证 + 应收挂账）"
SHJSON=$(curl -s -X POST -H "$AUTH" -H 'Content-Type: application/json' \
  --data-binary "{\"so_id\":\"$SO\",\"warehouse_id\":\"$WH\",\"ship_date\":\"2026-11-06\"}" \
  "$BASE/business/erp/shipment")
SHIP=$(echo "$SHJSON" | jq -r '.data.id // empty')
[ -n "$SHIP" ] && ok "建发货单成功（$(echo "$SHJSON" | jq -r '.data.ship_no')）" || { bad "建发货单失败：$SHJSON"; exit 1; }
check "发货单数量合计" "$(echo "$SHJSON" | jq -r '.data.total_qty')" "10.0000"

# 状态机是 DRAFT → PENDING_POST → POSTED（DRAFT 不能直接跳 POSTED，边表拦）
PENDJSON=$(curl -s -X PATCH -H "$AUTH" -H 'Content-Type: application/json' \
  --data-binary '{"target_status":"PENDING_POST"}' \
  "$BASE/business/erp/shipment/$SHIP/status")
check "发货单提交待过账" "$(echo "$PENDJSON" | jq -r '.data.status')" "PENDING_POST"

POSTJSON=$(curl -s -X PATCH -H "$AUTH" -H 'Content-Type: application/json' \
  --data-binary '{"target_status":"POSTED"}' \
  "$BASE/business/erp/shipment/$SHIP/status")
check "发货单过账后状态" "$(echo "$POSTJSON" | jq -r '.data.status')" "POSTED"
OUTNO=$(echo "$POSTJSON" | jq -r '.data.stock_out_no // empty')
[ -n "$OUTNO" ] && ok "生成销售出库单 $OUTNO" || { bad "未回填出库单号：$POSTJSON"; exit 1; }
OUT=$(psql_q "SELECT id FROM biz_stock_out WHERE source_id='$SHIP'::uuid")

echo
echo "==> 5. 应收：自动挂账结果（分页 + 详情）"
PJSON=$(curl -s -H "$AUTH" "$BASE/business/erp/receivable/page?ar_no=$OUTNO")
check "按出库单号能查到应收（1 张）" "$(echo "$PJSON" | jq -r '.data.total')" "1"
AR=$(echo "$PJSON" | jq -r '.data.list[0].id // empty')
check "应收金额 = 出库金额含税" "$(echo "$PJSON" | jq -r '.data.list[0].amount')" "1130.0000"
check "来源类型 SALES_OUT" "$(echo "$PJSON" | jq -r '.data.list[0].source_type')" "SALES_OUT"
check "来源单号 = 出库单号" "$(echo "$PJSON" | jq -r '.data.list[0].source_no')" "$OUTNO"
check "来源单据 id = 出库单 id" "$(echo "$PJSON" | jq -r '.data.list[0].source_id')" "$OUT"
check "状态 OPEN" "$(echo "$PJSON" | jq -r '.data.list[0].status')" "OPEN"
check "账期天数快照 45" "$(echo "$PJSON" | jq -r '.data.list[0].credit_days')" "45"

DJSON=$(curl -s -H "$AUTH" "$BASE/business/erp/receivable/$AR")
check "详情客户名" "$(echo "$DJSON" | jq -r '.data.customer_name')" "SO-14 挂账演示客户"
check "未收金额 = 应收金额" "$(echo "$DJSON" | jq -r '.data.unsettled_amount')" "1130.0000"
OUTDATE=$(psql_q "SELECT out_date::text FROM biz_stock_out WHERE id='$OUT'::uuid")
DUE_EXPECT=$(psql_q "SELECT (DATE '$OUTDATE' + 45)::text")
check "到期日 = 出库日 + 45" "$(echo "$DJSON" | jq -r '.data.due_date')" "$DUE_EXPECT"
PERIOD_EXPECT=$(psql_q "SELECT to_char(DATE '$OUTDATE', 'YYYY-MM')")
check "period_code = 过账日所属期间" "$(echo "$DJSON" | jq -r '.data.period_code')" "$PERIOD_EXPECT"

echo
echo "==> 6. 库层留痕：建单日志 + 台账唯一"
REASON=$(psql_q "SELECT reason FROM biz_status_log WHERE entity_type='RECEIVABLE' AND entity_id='$AR'::uuid ORDER BY created_at LIMIT 1")
check "建单日志说明 = 自动挂账" "$REASON" "销售出库过账自动挂账"
check "库内该出库单只有 1 张应收" "$(psql_q "SELECT count(*) FROM biz_receivable WHERE source_type='SALES_OUT' AND source_id='$OUT'::uuid")" "1"
check "库存已扣 10（20-10）" "$(psql_q "SELECT qty::text FROM biz_inventory WHERE material_id='$MAT'::uuid AND warehouse_id='$WH'::uuid AND location_id IS NULL")" "10.0000"
check "成本结转凭证已生成" "$(psql_q "SELECT count(*) FROM biz_voucher WHERE source_id='$OUT'::uuid")" "1"

echo
echo "==> 7. 幂等：重复过账被拒，应收不增加"
AGAIN=$(curl -s -X PATCH -H "$AUTH" -H 'Content-Type: application/json' \
  --data-binary '{"target_status":"POSTED"}' \
  "$BASE/business/erp/shipment/$SHIP/status")
[ "$(echo "$AGAIN" | jq -r '.code')" != "0" ] && ok "重复过账被拒（code=$(echo "$AGAIN" | jq -r '.code')）" || bad "重复过账竟然成功：$AGAIN"
check "重复过账后应收仍 1 张" "$(psql_q "SELECT count(*) FROM biz_receivable WHERE source_type='SALES_OUT' AND source_id='$OUT'::uuid")" "1"
check "库存仍 10（只扣一次）" "$(psql_q "SELECT qty::text FROM biz_inventory WHERE material_id='$MAT'::uuid AND warehouse_id='$WH'::uuid AND location_id IS NULL")" "10.0000"

echo
echo "==> 8. 客户账期可改，历史应收是快照（不回写）"
UJSON=$(curl -s -X PUT -H "$AUTH" -H 'Content-Type: application/json' \
  --data-binary '{"credit_days":30}' "$BASE/business/erp/customer/$CUST")
check "客户 credit_days 改为 30" "$(echo "$UJSON" | jq -r '.data.credit_days')" "30"
check "已挂应收的账期仍是 45（快照）" "$(curl -s -H "$AUTH" "$BASE/business/erp/receivable/$AR" | jq -r '.data.credit_days')" "45"
check "已挂应收的到期日不变" "$(curl -s -H "$AUTH" "$BASE/business/erp/receivable/$AR" | jq -r '.data.due_date')" "$DUE_EXPECT"
check "客户其它字段未被部分更新冲掉（name 仍在）" "$(echo "$UJSON" | jq -r '.data.name')" "SO-14 挂账演示客户"

echo
echo "==> 汇总：PASS=$PASS FAIL=$FAIL"
[ "$FAIL" = "0" ] || exit 1

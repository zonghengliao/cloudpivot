#!/usr/bin/env bash
# ============================================================================
# SO-14 遗留（t_b33adc45）：销售出库单作废 → 应收冲销 端到端 curl 实测
#
# 用法（先起一个指向**隔离演示库**的实例）：
#   BASE=http://127.0.0.1:18118/api DB=cloudpivot_so14 \
#     bash docs/evidence/20261106-so14-receivable-cancel/curl-e2e.sh
#
# 链路（走 HTTP handler，不绕过任何一道闸）：
#   ① 客户（credit_days=45）→ 销售订单（10×100@13%）→ 发货单 → 过账
#        ⇒ 销售出库单 + 三本账负向 + 成本结转凭证 + 应收自动挂账（OPEN 1130）
#   ② 作废那张出库单（PATCH /stock-out/{id}/status → CANCELLED）
#        ⇒ 三本账回滚 + 出库红字凭证 + **应收 OPEN → CANCELLED**（同一事务）
#   ③ 验：应收状态/标签/金额/封账时间、台账筛选、账龄不再计提、
#          库存回滚、红字凭证、状态日志、行仍在（不是软删）
#   ④ 幂等：重复作废 409，应收与库存都不再动
#   ⑤ 已收款拦截：应收收到 100（PARTIAL）后再作废 → 409，整笔回滚
#          （出库单仍 POSTED / 库存不退回 / 没有红字凭证 / 应收仍 PARTIAL）
#   ⑥ 状态白名单：CANCELLED 可筛，payable 的 PARTIAL_PAID 明确 400
#
# 脚本自带清场（只清 `SO14C` 前缀的夹具），可连跑多轮，每轮断言数量相同。
# ============================================================================
set -uo pipefail

BASE=${BASE:-http://127.0.0.1:18118/api}
DB=${DB:-cloudpivot_so14}
PGHOST=${PGHOST:-127.0.0.1}
PGUSER=${PGUSER:-alun}
export PGPASSWORD=${PGPASSWORD:-alun}
ADMIN_USER=${ADMIN_USER:-admin}
ADMIN_PASS=${ADMIN_PASS:-Admin@123456}

UNIT='d5150000-0000-4000-8000-000000000001'
WH='d5150000-0000-4000-8000-000000000002'
MAT='d5150000-0000-4000-8000-000000000011'
CCODE='SO14C-CUST'
REASON='客户取消订单，货退回仓库'

PASS=0
FAIL=0
ok()    { PASS=$((PASS+1)); echo "  PASS  $*"; }
bad()   { FAIL=$((FAIL+1)); echo "  FAIL  $*"; }
check() { if [ "$2" = "$3" ]; then ok "$1（$3）"; else bad "$1：期望 $3，实际 $2"; fi }

psql_q() { psql -h "$PGHOST" -U "$PGUSER" -d "$DB" -Atc "$1"; }

echo "==> 0. 清场 + 夹具（只碰 SO14C 前缀与三张夹具主数据）"
psql_q "DELETE FROM biz_receivable WHERE source_id IN
          (SELECT o.id FROM biz_stock_out o JOIN biz_shipment s ON s.id = o.source_id
            JOIN biz_sales_order so ON so.id = s.so_id
            JOIN biz_customer c ON c.id = so.customer_id WHERE c.code LIKE 'SO14C%')" >/dev/null
psql_q "DELETE FROM biz_voucher WHERE source_id IN
          (SELECT o.id FROM biz_stock_out o JOIN biz_shipment s ON s.id = o.source_id
            JOIN biz_sales_order so ON so.id = s.so_id
            JOIN biz_customer c ON c.id = so.customer_id WHERE c.code LIKE 'SO14C%')" >/dev/null
psql_q "DELETE FROM biz_status_log WHERE entity_type = 'RECEIVABLE' AND entity_id IN
          (SELECT r.id FROM biz_receivable r JOIN biz_stock_out o ON o.id = r.source_id
            JOIN biz_shipment s ON s.id = o.source_id
            JOIN biz_sales_order so ON so.id = s.so_id
            JOIN biz_customer c ON c.id = so.customer_id WHERE c.code LIKE 'SO14C%')" >/dev/null
psql_q "DELETE FROM biz_status_log WHERE entity_type = 'STOCK_OUT' AND entity_id IN
          (SELECT o.id FROM biz_stock_out o JOIN biz_shipment s ON s.id = o.source_id
            JOIN biz_sales_order so ON so.id = s.so_id
            JOIN biz_customer c ON c.id = so.customer_id WHERE c.code LIKE 'SO14C%')" >/dev/null
psql_q "DELETE FROM biz_status_log WHERE entity_type = 'SHIPMENT' AND entity_id IN
          (SELECT s.id FROM biz_shipment s JOIN biz_sales_order so ON so.id = s.so_id
            JOIN biz_customer c ON c.id = so.customer_id WHERE c.code LIKE 'SO14C%')" >/dev/null
psql_q "DELETE FROM biz_status_log WHERE entity_type = 'SALE_ORDER' AND entity_id IN
          (SELECT so.id FROM biz_sales_order so JOIN biz_customer c ON c.id = so.customer_id
            WHERE c.code LIKE 'SO14C%')" >/dev/null
psql_q "DELETE FROM biz_stock_out WHERE source_id IN
          (SELECT s.id FROM biz_shipment s JOIN biz_sales_order so ON so.id = s.so_id
            JOIN biz_customer c ON c.id = so.customer_id WHERE c.code LIKE 'SO14C%')" >/dev/null
psql_q "DELETE FROM biz_shipment WHERE so_id IN
          (SELECT so.id FROM biz_sales_order so JOIN biz_customer c ON c.id = so.customer_id
            WHERE c.code LIKE 'SO14C%')" >/dev/null
psql_q "DELETE FROM biz_sales_order WHERE customer_id IN
          (SELECT id FROM biz_customer WHERE code LIKE 'SO14C%')" >/dev/null
psql_q "DELETE FROM biz_customer WHERE code LIKE 'SO14C%'" >/dev/null
psql_q "DELETE FROM biz_inventory_txn WHERE material_id = '$MAT'::uuid" >/dev/null
psql_q "DELETE FROM biz_inventory WHERE material_id = '$MAT'::uuid" >/dev/null
psql_q "DELETE FROM biz_inventory_location WHERE material_id = '$MAT'::uuid" >/dev/null

psql_q "INSERT INTO biz_unit (id, unit_code, unit_name, is_base, base_unit_id, rate, status, sort_order)
        VALUES ('$UNIT'::uuid, 'SO14C-PCS', '个', 1, NULL, 1.000000, 1, 1) ON CONFLICT DO NOTHING" >/dev/null
psql_q "INSERT INTO biz_warehouse (id, warehouse_code, warehouse_name, warehouse_path, level, is_leaf, status, allow_negative)
        VALUES ('$WH'::uuid, 'SO14C-WH', 'SO-14C 作废仓', '/$WH/', 1, 1, 1, 0) ON CONFLICT DO NOTHING" >/dev/null
psql_q "INSERT INTO biz_material (id, material_code, material_name, unit_id, status, serial_required)
        VALUES ('$MAT'::uuid, 'SO14C-M1', 'SO-14C 冲销物料', '$UNIT'::uuid, 1, false) ON CONFLICT DO NOTHING" >/dev/null
# 库存夹具：成本账 + 库位账各 20（未分配桶，与单测 seed_stock 同口径）
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
for candidate in "$ADMIN_PASS" "${ADMIN_PASS}_so14" "${ADMIN_PASS}_so14c"; do
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
  NEW_PASS="${ADMIN_PASS}_so14c"
  curl -s -o /dev/null -X PUT -H "Authorization: Bearer ${TOKEN}" -H 'Content-Type: application/json' \
    --data-binary "{\"old_password\":\"$ADMIN_PASS\",\"new_password\":\"$NEW_PASS\"}" \
    "$BASE/system/auth/password"
  ADMIN_PASS="$NEW_PASS"
  TOKEN=$(login "$ADMIN_PASS" | jq -r '.data.access_token // empty')
fi
[ -n "$TOKEN" ] && ok "拿到 admin access_token" || { bad "登录失败"; exit 1; }
AUTH="Authorization: Bearer ${TOKEN}"

echo
echo "==> 2. 客户 → 销售订单（10 × 100，13% 税 → 含税 1130）"
CJSON=$(curl -s -X POST -H "$AUTH" -H 'Content-Type: application/json' \
  --data-binary "{\"code\":\"$CCODE\",\"name\":\"SO-14C 冲销演示客户\",\"credit_days\":45,\"credit_limit\":0}" \
  "$BASE/business/erp/customer")
CUST=$(echo "$CJSON" | jq -r '.data.id // empty')
[ -n "$CUST" ] && ok "建客户成功" || { bad "建客户失败：$CJSON"; exit 1; }
check "客户 credit_days = 45" "$(echo "$CJSON" | jq -r '.data.credit_days')" "45"

SOJSON=$(curl -s -X POST -H "$AUTH" -H 'Content-Type: application/json' \
  --data-binary "{\"customer_id\":\"$CUST\",\"order_date\":\"2026-11-06\",\"items\":[{\"material_id\":\"$MAT\",\"qty\":10,\"price\":100,\"tax_rate\":13}]}" \
  "$BASE/business/erp/sales-order")
SO=$(echo "$SOJSON" | jq -r '.data.id // empty')
[ -n "$SO" ] && ok "建销售订单成功（$(echo "$SOJSON" | jq -r '.data.so_no')）" || { bad "建单失败：$SOJSON"; exit 1; }
check "订单价税合计" "$(echo "$SOJSON" | jq -r '.data.total_with_tax')" "1130.0000"
# 审批流不属本卡：演示库没有审批人，夹具快捷键把订单置 APPROVED
psql_q "UPDATE biz_sales_order SET status='APPROVED', updated_at=now() WHERE id='$SO'::uuid" >/dev/null
ok "夹具：订单置 APPROVED（审批流由 SO-05/SO-06 覆盖）"

echo
echo "==> 3. 发货单 → 过账（出库 + 三本账 + 成本凭证 + 应收挂账）"
SHJSON=$(curl -s -X POST -H "$AUTH" -H 'Content-Type: application/json' \
  --data-binary "{\"so_id\":\"$SO\",\"warehouse_id\":\"$WH\",\"ship_date\":\"2026-11-06\"}" \
  "$BASE/business/erp/shipment")
SHIP=$(echo "$SHJSON" | jq -r '.data.id // empty')
[ -n "$SHIP" ] && ok "建发货单成功（$(echo "$SHJSON" | jq -r '.data.ship_no')）" || { bad "建发货单失败：$SHJSON"; exit 1; }

curl -s -o /dev/null -X PATCH -H "$AUTH" -H 'Content-Type: application/json' \
  --data-binary '{"target_status":"PENDING_POST"}' "$BASE/business/erp/shipment/$SHIP/status"
POSTJSON=$(curl -s -X PATCH -H "$AUTH" -H 'Content-Type: application/json' \
  --data-binary '{"target_status":"POSTED"}' "$BASE/business/erp/shipment/$SHIP/status")
check "发货单过账后状态" "$(echo "$POSTJSON" | jq -r '.data.status')" "POSTED"
OUTNO=$(echo "$POSTJSON" | jq -r '.data.stock_out_no // empty')
[ -n "$OUTNO" ] && ok "生成销售出库单 $OUTNO" || { bad "未回填出库单号：$POSTJSON"; exit 1; }
OUT=$(psql_q "SELECT id FROM biz_stock_out WHERE source_id='$SHIP'::uuid")

PJSON=$(curl -s -H "$AUTH" "$BASE/business/erp/receivable/page?ar_no=$OUTNO")
check "过账后应收 1 张" "$(echo "$PJSON" | jq -r '.data.total')" "1"
AR=$(echo "$PJSON" | jq -r '.data.list[0].id // empty')
check "应收金额 1130（含税）" "$(echo "$PJSON" | jq -r '.data.list[0].amount')" "1130.0000"
check "应收初始状态 OPEN" "$(echo "$PJSON" | jq -r '.data.list[0].status')" "OPEN"
check "库存已扣到 10" "$(psql_q "SELECT qty::text FROM biz_inventory WHERE material_id='$MAT'::uuid AND warehouse_id='$WH'::uuid AND location_id IS NULL")" "10.0000"

ASOF=$(psql_q "SELECT CURRENT_DATE::text")
AGING_BEFORE=$(curl -s -H "$AUTH" "$BASE/business/erp/receivable/aging?as_of=$ASOF")
DOCS_BEFORE=$(echo "$AGING_BEFORE" | jq -r '.data.doc_count')
check "作废前账龄含本单" "$(echo "$AGING_BEFORE" | jq -r --arg no "$OUTNO" '[.data.details[] | select(.source_no == $no)] | length')" "1"

echo
echo "==> 4. 作废出库单（POSTED → CANCELLED，与三本账回滚 / 红字凭证同一事务）"
CANJSON=$(curl -s -X PATCH -H "$AUTH" -H 'Content-Type: application/json' \
  --data-binary "{\"target_status\":\"CANCELLED\",\"reason\":\"$REASON\"}" \
  "$BASE/business/erp/stock-out/$OUT/status")
check "出库单作废成功" "$(echo "$CANJSON" | jq -r '.data.status')" "CANCELLED"

echo
echo "==> 5. 应收侧结果：状态 / 金额 / 封账时间 / 标签"
DJSON=$(curl -s -H "$AUTH" "$BASE/business/erp/receivable/$AR")
check "应收状态 CANCELLED" "$(echo "$DJSON" | jq -r '.data.status')" "CANCELLED"
check "状态中文标签「已作废」" "$(echo "$DJSON" | jq -r '.data.status_label')" "已作废"
check "已收金额仍为 0" "$(echo "$DJSON" | jq -r '.data.received_amount')" "0.0000"
check "未收金额仍等于应收金额（恒等式）" "$(echo "$DJSON" | jq -r '.data.unsettled_amount')" "1130.0000"
check "settled_at 已落（封账时间）" "$(echo "$DJSON" | jq -r 'if .data.settled_at then "set" else "null" end')" "set"
check "作废单不算逾期" "$(echo "$DJSON" | jq -r '.data.overdue')" "false"
check "备注留冲销前缀 + 原因" "$(psql_q "SELECT CASE WHEN remark LIKE '%[出库作废冲销]%$REASON%' THEN 'yes' ELSE 'no' END FROM biz_receivable WHERE id='$AR'::uuid")" "yes"
check "行仍在（不是软删）" "$(psql_q "SELECT count(*) FROM biz_receivable WHERE id='$AR'::uuid AND deleted_at IS NULL")" "1"
check "库层状态与金额" "$(psql_q "SELECT status || '|' || received_amount::text FROM biz_receivable WHERE id='$AR'::uuid")" "CANCELLED|0.0000"

echo
echo "==> 6. 台账筛选 + 账龄不再计提"
check "page?status=CANCELLED 能筛到" \
  "$(curl -s -H "$AUTH" "$BASE/business/erp/receivable/page?status=CANCELLED&ar_no=$OUTNO" | jq -r '.data.total')" "1"
check "page?status=OPEN 已不含本单" \
  "$(curl -s -H "$AUTH" "$BASE/business/erp/receivable/page?status=OPEN&ar_no=$OUTNO" | jq -r '.data.total')" "0"
AGING_AFTER=$(curl -s -H "$AUTH" "$BASE/business/erp/receivable/aging?as_of=$ASOF")
check "账龄明细已排除本单" "$(echo "$AGING_AFTER" | jq -r --arg no "$OUTNO" '[.data.details[] | select(.source_no == $no)] | length')" "0"
DOCS_AFTER=$(echo "$AGING_AFTER" | jq -r '.data.doc_count')
check "账龄单据数减少 1" "$((DOCS_BEFORE - DOCS_AFTER))" "1"
check "账龄未收合计减少 1130" \
  "$(psql_q "SELECT (($(echo "$AGING_BEFORE" | jq -r '.data.total_unsettled')::numeric) - ($(echo "$AGING_AFTER" | jq -r '.data.total_unsettled')::numeric))::text")" "1130.0000"

echo
echo "==> 7. 三本账 / 凭证 / 状态日志"
check "库存回滚到 20" "$(psql_q "SELECT qty::text FROM biz_inventory WHERE material_id='$MAT'::uuid AND warehouse_id='$WH'::uuid AND location_id IS NULL")" "20.0000"
check "库位账同步回滚" "$(psql_q "SELECT qty::text FROM biz_inventory_location WHERE material_id='$MAT'::uuid AND warehouse_id='$WH'::uuid AND location_id IS NULL")" "20.0000"
check "流水借贷净额为 0（一出一进）" "$(psql_q "SELECT COALESCE(sum(CASE WHEN txn_type='OUT' THEN -qty ELSE qty END),0)::text FROM biz_inventory_txn WHERE material_id='$MAT'::uuid")" "0.0000"
check "出库红字凭证 1 张" "$(psql_q "SELECT count(*) FROM biz_voucher WHERE source_type='STOCK_OUT' AND source_id='$OUT'::uuid AND is_red=1")" "1"
check "原凭证已被标记红冲" "$(psql_q "SELECT count(*) FROM biz_voucher WHERE source_type='STOCK_OUT' AND source_id='$OUT'::uuid AND is_red=0 AND reversed_by IS NOT NULL")" "1"
check "应收状态日志 OPEN→CANCELLED + 原因" \
  "$(psql_q "SELECT from_status || '>' || to_status || '|' || coalesce(reason,'') FROM biz_status_log WHERE entity_type='RECEIVABLE' AND entity_id='$AR'::uuid AND to_status='CANCELLED' ORDER BY created_at DESC LIMIT 1")" \
  "OPEN>CANCELLED|$REASON"

echo
echo "==> 8. 幂等：重复作废 409，应收 / 库存都不再动"
AGAIN=$(curl -s -X PATCH -H "$AUTH" -H 'Content-Type: application/json' \
  --data-binary "{\"target_status\":\"CANCELLED\",\"reason\":\"$REASON\"}" \
  "$BASE/business/erp/stock-out/$OUT/status")
[ "$(echo "$AGAIN" | jq -r '.code')" != "0" ] && ok "重复作废被拒（code=$(echo "$AGAIN" | jq -r '.code')）" || bad "重复作废竟然成功：$AGAIN"
check "应收仍只有 1 张且为 CANCELLED" "$(psql_q "SELECT count(*) FROM biz_receivable WHERE source_id='$OUT'::uuid AND status='CANCELLED'")" "1"
check "作废轨迹只 1 条" "$(psql_q "SELECT count(*) FROM biz_status_log WHERE entity_type='RECEIVABLE' AND entity_id='$AR'::uuid AND to_status='CANCELLED'")" "1"
check "库存仍 20（不会被退两次）" "$(psql_q "SELECT qty::text FROM biz_inventory WHERE material_id='$MAT'::uuid AND warehouse_id='$WH'::uuid AND location_id IS NULL")" "20.0000"

echo
echo "==> 9. 已收款拦截：应收 PARTIAL（收 100）后作废 → 409 且整笔回滚"
SO2JSON=$(curl -s -X POST -H "$AUTH" -H 'Content-Type: application/json' \
  --data-binary "{\"customer_id\":\"$CUST\",\"order_date\":\"2026-11-06\",\"items\":[{\"material_id\":\"$MAT\",\"qty\":5,\"price\":100,\"tax_rate\":13}]}" \
  "$BASE/business/erp/sales-order")
SO2=$(echo "$SO2JSON" | jq -r '.data.id // empty')
psql_q "UPDATE biz_sales_order SET status='APPROVED', updated_at=now() WHERE id='$SO2'::uuid" >/dev/null
SH2JSON=$(curl -s -X POST -H "$AUTH" -H 'Content-Type: application/json' \
  --data-binary "{\"so_id\":\"$SO2\",\"warehouse_id\":\"$WH\",\"ship_date\":\"2026-11-06\"}" \
  "$BASE/business/erp/shipment")
SHIP2=$(echo "$SH2JSON" | jq -r '.data.id // empty')
curl -s -o /dev/null -X PATCH -H "$AUTH" -H 'Content-Type: application/json' \
  --data-binary '{"target_status":"PENDING_POST"}' "$BASE/business/erp/shipment/$SHIP2/status"
P2=$(curl -s -X PATCH -H "$AUTH" -H 'Content-Type: application/json' \
  --data-binary '{"target_status":"POSTED"}' "$BASE/business/erp/shipment/$SHIP2/status")
OUT2NO=$(echo "$P2" | jq -r '.data.stock_out_no // empty')
OUT2=$(psql_q "SELECT id FROM biz_stock_out WHERE source_id='$SHIP2'::uuid")
AR2=$(psql_q "SELECT id FROM biz_receivable WHERE source_type='SALES_OUT' AND source_id='$OUT2'::uuid")
[ -n "$AR2" ] && ok "第二张应收已挂（$OUT2NO，565）" || { bad "第二张应收未挂：$P2"; exit 1; }
check "第二张应收金额 565" "$(psql_q "SELECT amount::text FROM biz_receivable WHERE id='$AR2'::uuid")" "565.0000"
# 模拟已收 100（收款模块尚未落地，直接造 PARTIAL 形态）
psql_q "UPDATE biz_receivable SET received_amount=100, unsettled_amount=465, status='PARTIAL', updated_at=now() WHERE id='$AR2'::uuid" >/dev/null
ok "夹具：第二张应收造为 PARTIAL（已收 100 / 未收 465）"
check "第二张出库后库存 15（20-5）" "$(psql_q "SELECT qty::text FROM biz_inventory WHERE material_id='$MAT'::uuid AND warehouse_id='$WH'::uuid AND location_id IS NULL")" "15.0000"

BLOCK=$(curl -s -X PATCH -H "$AUTH" -H 'Content-Type: application/json' \
  --data-binary "{\"target_status\":\"CANCELLED\",\"reason\":\"$REASON\"}" \
  "$BASE/business/erp/stock-out/$OUT2/status")
[ "$(echo "$BLOCK" | jq -r '.code')" != "0" ] && ok "已收款 → 作废被拒（code=$(echo "$BLOCK" | jq -r '.code')）" || bad "已收款竟然能作废：$BLOCK"
MSG=$(echo "$BLOCK" | jq -r '.message // .msg // ""')
echo "$MSG" | grep -q "收款" && ok "报文点名先冲收款（$MSG）" || bad "报文没提收款：$MSG"
check "出库单仍 POSTED" "$(psql_q "SELECT status FROM biz_stock_out WHERE id='$OUT2'::uuid")" "POSTED"
check "库存仍 15（三本账未回滚）" "$(psql_q "SELECT qty::text FROM biz_inventory WHERE material_id='$MAT'::uuid AND warehouse_id='$WH'::uuid AND location_id IS NULL")" "15.0000"
check "没有红字凭证（同一事务回滚）" "$(psql_q "SELECT count(*) FROM biz_voucher WHERE source_type='STOCK_OUT' AND source_id='$OUT2'::uuid AND is_red=1")" "0"
check "应收仍 PARTIAL / 已收 100" "$(psql_q "SELECT status || '|' || received_amount::text FROM biz_receivable WHERE id='$AR2'::uuid")" "PARTIAL|100.0000"

echo
echo "==> 10. 状态白名单（V0290 后 5 值）"
check "page?status=CANCELLED 合法" \
  "$(curl -s -o /dev/null -w '%{http_code}' -H "$AUTH" "$BASE/business/erp/receivable/page?status=CANCELLED")" "200"
BADF=$(curl -s -H "$AUTH" "$BASE/business/erp/receivable/page?status=PARTIAL_PAID")
[ "$(echo "$BADF" | jq -r '.code')" != "0" ] && ok "payable 的 PARTIAL_PAID 仍被拒（code=$(echo "$BADF" | jq -r '.code')）" || bad "PARTIAL_PAID 竟然通过：$BADF"

echo
echo "==> 汇总：PASS=$PASS FAIL=$FAIL"
[ "$FAIL" = "0" ] || exit 1

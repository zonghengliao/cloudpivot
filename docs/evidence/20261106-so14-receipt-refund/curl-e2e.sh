#!/usr/bin/env bash
# ============================================================================
# SO-14 遗留（t_573bfe5b）：已收款的应收 + 出库作废 → 收款退款单（路线 A）端到端 curl 实测
#
# 用法（先起一个指向**隔离演示库**的实例，见 README §六）：
#   BASE=http://127.0.0.1:18121/api DB=cloudpivot_rr14 \
#     bash docs/evidence/20261106-so14-receipt-refund/curl-e2e.sh
#
# 链路（全部走 HTTP handler，不绕过任何一道闸）：
#   ① 客户（credit_days=45）→ 销售订单（10×100@13%）→ 发货单 → 过账
#        ⇒ 销售出库单 + 三本账负向 + 成本结转凭证 + 应收自动挂账（OPEN 1130）
#   ② 夹具：按 V0292 表结构造一张**已过账**的收款单（RC）+ 核销明细 100
#        ⇒ 应收 PARTIAL / 已收 100 / 未收 1030（等价于 SO-17 核销后的账）
#   ③ 基线：作废出库单 → 409（已收过款不能直接冲销，报文要求先冲收款）
#   ④ 登记收款退款单（RR）→ 核销 100（应收回到 OPEN / 已收 0）
#   ⑤ 确认 → 过账（借 1122 应收 / 贷 1002 银行存款 各 100，凭证号回填）
#   ⑥ 再作废出库单 → 200 CANCELLED，库存回滚、应收 CANCELLED（走过的就是既有 OPEN→CANCELLED）
#   ⑦ 幂等 / 边界：重复作废 409、已过账退款不能作废 409、退款原因必填 400、
#        超冲减 400、一分没收的应收不能退款 409（页面/详情的 status 白名单也对齐）
#
# 脚本自带清场（只在本卡隔离演示库里按表清），可连跑多轮，每轮断言数量相同。
# ============================================================================
set -uo pipefail

BASE=${BASE:-http://127.0.0.1:18121/api}
DB=${DB:-cloudpivot_rr14}
PGHOST=${PGHOST:-127.0.0.1}
PGUSER=${PGUSER:-alun}
export PGPASSWORD=${PGPASSWORD:-alun}
ADMIN_USER=${ADMIN_USER:-admin}
ADMIN_PASS=${ADMIN_PASS:-Admin@123456}

UNIT='d5950000-0000-4000-8000-000000000001'
WH='d5950000-0000-4000-8000-000000000002'
MAT='d5950000-0000-4000-8000-000000000011'
CCODE='RR14C-CUST'
REASON='客户取消订单，货款原路退回'

PASS=0
FAIL=0
ok()    { PASS=$((PASS+1)); echo "  PASS  $*"; }
bad()   { FAIL=$((FAIL+1)); echo "  FAIL  $*"; }
check() { if [ "$2" = "$3" ]; then ok "$1（$3）"; else bad "$1：期望 $3，实际 $2"; fi }
nonz()  { if [ "$2" != "0" ] && [ -n "$2" ]; then ok "$1（code=$2）"; else bad "$1：期望非 0，实际 $2"; fi }

psql_q() { psql -h "$PGHOST" -U "$PGUSER" -d "$DB" -Atc "$1"; }

echo "==> 0. 清场（本卡隔离演示库按表清）+ 夹具"
psql_q "DELETE FROM biz_receipt_refund_allocation" >/dev/null
psql_q "DELETE FROM biz_receipt_refund" >/dev/null
psql_q "DELETE FROM biz_receipt_allocation" >/dev/null
psql_q "DELETE FROM biz_receipt" >/dev/null
psql_q "DELETE FROM biz_status_log" >/dev/null
psql_q "DELETE FROM biz_voucher_entry" >/dev/null
psql_q "DELETE FROM biz_voucher" >/dev/null
psql_q "DELETE FROM biz_receivable" >/dev/null
psql_q "DELETE FROM biz_stock_out" >/dev/null
psql_q "DELETE FROM biz_shipment" >/dev/null
psql_q "DELETE FROM biz_sales_order" >/dev/null
psql_q "DELETE FROM biz_customer WHERE code LIKE 'RR14C%'" >/dev/null
psql_q "DELETE FROM biz_inventory_txn WHERE material_id = '$MAT'::uuid" >/dev/null
psql_q "DELETE FROM biz_inventory WHERE material_id = '$MAT'::uuid" >/dev/null
psql_q "DELETE FROM biz_inventory_location WHERE material_id = '$MAT'::uuid" >/dev/null

psql_q "INSERT INTO biz_unit (id, unit_code, unit_name, is_base, base_unit_id, rate, status, sort_order)
        VALUES ('$UNIT'::uuid, 'RR14C-PCS', '个', 1, NULL, 1.000000, 1, 1) ON CONFLICT DO NOTHING" >/dev/null
psql_q "INSERT INTO biz_warehouse (id, warehouse_code, warehouse_name, warehouse_path, level, is_leaf, status, allow_negative)
        VALUES ('$WH'::uuid, 'RR14C-WH', 'RR14C 退款仓', '/$WH/', 1, 1, 1, 0) ON CONFLICT DO NOTHING" >/dev/null
psql_q "INSERT INTO biz_material (id, material_code, material_name, unit_id, status, serial_required)
        VALUES ('$MAT'::uuid, 'RR14C-M1', 'RR14C 退款物料', '$UNIT'::uuid, 1, false) ON CONFLICT DO NOTHING" >/dev/null
psql_q "INSERT INTO biz_inventory_location (material_id, warehouse_id, location_id, qty)
        VALUES ('$MAT'::uuid, '$WH'::uuid, NULL, 20)" >/dev/null
psql_q "INSERT INTO biz_inventory (material_id, warehouse_id, location_id, qty, amount, avg_cost)
        VALUES ('$MAT'::uuid, '$WH'::uuid, NULL, 20, 200, 10)" >/dev/null
ok "夹具就位（库存 20 / 成本 10）"

echo
echo "==> 1. 登录（admin，首登强制改密自动处理）"
login() {
  curl -s -X POST -H 'Content-Type: application/json' \
    --data-binary "{\"username\":\"$ADMIN_USER\",\"password\":\"$1\"}" \
    "$BASE/system/auth/login"
}
RESP=""; TOKEN=""
for candidate in "$ADMIN_PASS" "${ADMIN_PASS}_rr14"; do
  for attempt in 1 2 3; do
    RESP=$(login "$candidate")
    TOKEN=$(echo "$RESP" | jq -r '.data.access_token // empty')
    [ -n "$TOKEN" ] && break
    if [ "$(echo "$RESP" | jq -r '.code')" = "10005" ]; then sleep 25; else break; fi
  done
  if [ -n "$TOKEN" ]; then ADMIN_PASS="$candidate"; break; fi
done
if [ -z "$TOKEN" ]; then echo "  !! 登录失败：$RESP"; exit 1; fi
MUST=$(echo "$RESP" | jq -r '.data.must_change_password // false')
if [ "$MUST" = "true" ]; then
  NEW_PASS="${ADMIN_PASS}_rr14"
  curl -s -o /dev/null -X PUT -H "Authorization: Bearer $TOKEN" -H 'Content-Type: application/json' \
    --data-binary "{\"old_password\":\"$ADMIN_PASS\",\"new_password\":\"$NEW_PASS\"}" \
    "$BASE/system/auth/password"
  ADMIN_PASS="$NEW_PASS"
  TOKEN=$(login "$ADMIN_PASS" | jq -r '.data.access_token // empty')
fi
[ -n "$TOKEN" ] && ok "拿到 admin access_token" || { bad "登录失败"; exit 1; }
AUTH="Authorization: Bearer $TOKEN"

echo
echo "==> 2. 客户 → 销售订单（10 × 100，13% 税 → 含税 1130）→ 发货过账"
CJSON=$(curl -s -X POST -H "$AUTH" -H 'Content-Type: application/json' \
  --data-binary "{\"code\":\"$CCODE\",\"name\":\"RR14C 退款演示客户\",\"credit_days\":45,\"credit_limit\":0}" \
  "$BASE/business/erp/customer")
CUST=$(echo "$CJSON" | jq -r '.data.id // empty')
[ -n "$CUST" ] && ok "建客户成功" || { bad "建客户失败：$CJSON"; exit 1; }

SOJSON=$(curl -s -X POST -H "$AUTH" -H 'Content-Type: application/json' \
  --data-binary "{\"customer_id\":\"$CUST\",\"order_date\":\"2026-11-06\",\"items\":[{\"material_id\":\"$MAT\",\"qty\":10,\"price\":100,\"tax_rate\":13}]}" \
  "$BASE/business/erp/sales-order")
SO=$(echo "$SOJSON" | jq -r '.data.id // empty')
[ -n "$SO" ] && ok "建销售订单成功（$(echo "$SOJSON" | jq -r '.data.so_no')）" || { bad "建单失败：$SOJSON"; exit 1; }
# 审批流不属本卡：演示库没有审批人，夹具把订单置 APPROVED
psql_q "UPDATE biz_sales_order SET status='APPROVED', updated_at=now() WHERE id='$SO'::uuid" >/dev/null

SHJSON=$(curl -s -X POST -H "$AUTH" -H 'Content-Type: application/json' \
  --data-binary "{\"so_id\":\"$SO\",\"warehouse_id\":\"$WH\",\"ship_date\":\"2026-11-06\"}" \
  "$BASE/business/erp/shipment")
SHIP=$(echo "$SHJSON" | jq -r '.data.id // empty')
[ -n "$SHIP" ] && ok "建发货单成功" || { bad "建发货单失败：$SHJSON"; exit 1; }
curl -s -o /dev/null -X PATCH -H "$AUTH" -H 'Content-Type: application/json' \
  --data-binary '{"target_status":"PENDING_POST"}' "$BASE/business/erp/shipment/$SHIP/status"
POSTJSON=$(curl -s -X PATCH -H "$AUTH" -H 'Content-Type: application/json' \
  --data-binary '{"target_status":"POSTED"}' "$BASE/business/erp/shipment/$SHIP/status")
check "发货单过账后状态" "$(echo "$POSTJSON" | jq -r '.data.status')" "POSTED"
OUTNO=$(echo "$POSTJSON" | jq -r '.data.stock_out_no // empty')
OUT=$(psql_q "SELECT id FROM biz_stock_out WHERE source_id='$SHIP'::uuid")
[ -n "$OUT" ] && ok "生成销售出库单 $OUTNO" || { bad "未生成出库单"; exit 1; }

PJSON=$(curl -s -H "$AUTH" "$BASE/business/erp/receivable/page?ar_no=$OUTNO")
AR=$(echo "$PJSON" | jq -r '.data.list[0].id // empty')
check "过账后应收 1 张" "$(echo "$PJSON" | jq -r '.data.total')" "1"
check "应收金额 1130（含税）" "$(echo "$PJSON" | jq -r '.data.list[0].amount')" "1130.0000"
check "应收初始 OPEN" "$(echo "$PJSON" | jq -r '.data.list[0].status')" "OPEN"

echo
echo "==> 3. 夹具：造一张已过账的收款单（V0292 表结构）+ 核销 100（SO-17 在途，按表造账）"
RC='d5950fff-0000-4000-8000-000000000001'
psql_q "INSERT INTO biz_receipt (id, receipt_no, customer_id, receipt_date, amount, allocated_amount, method, status)
        VALUES ('$RC'::uuid, 'RC-20261106-900001', '$CUST'::uuid, '2026-11-06', 100, 100, 'BANK_TRANSFER', 'POSTED')" >/dev/null
psql_q "INSERT INTO biz_receipt_allocation (receipt_id, receivable_id, amount, remark)
        VALUES ('$RC'::uuid, '$AR'::uuid, 100, '夹具：RR14C 已收款 100')" >/dev/null
psql_q "UPDATE biz_receivable SET received_amount=100, unsettled_amount=1030, status='PARTIAL', updated_at=now() WHERE id='$AR'::uuid" >/dev/null
check "夹具后应收 = PARTIAL / 已收 100" \
  "$(psql_q "SELECT status || '|' || received_amount::text FROM biz_receivable WHERE id='$AR'::uuid")" \
  "PARTIAL|100.0000"

echo
echo "==> 4. 基线：已收款的应收 → 作废出库单被 409 拦住（本卡要解开的那一步）"
BLOCK=$(curl -s -X PATCH -H "$AUTH" -H 'Content-Type: application/json' \
  --data-binary "{\"target_status\":\"CANCELLED\",\"reason\":\"$REASON\"}" \
  "$BASE/business/erp/stock-out/$OUT/status")
nonz "已收款 → 作废被拒" "$(echo "$BLOCK" | jq -r '.code')"
echo "$BLOCK" | jq -r '.message // .msg // ""' | grep -q "收款" && ok "报文点名先冲收款" || bad "报文没提收款"
check "整笔回滚：出库单仍 POSTED" "$(psql_q "SELECT status FROM biz_stock_out WHERE id='$OUT'::uuid")" "POSTED"
check "库存仍 10（未回滚）" "$(psql_q "SELECT qty::text FROM biz_inventory WHERE material_id='$MAT'::uuid AND warehouse_id='$WH'::uuid AND location_id IS NULL")" "10.0000"
check "没有出库红字凭证" "$(psql_q "SELECT count(*) FROM biz_voucher WHERE source_type='STOCK_OUT' AND source_id='$OUT'::uuid AND is_red=1")" "0"

echo
echo "==> 5. 登记收款退款单（RR-yyyyMMdd-000001，原因必填）"
RJSON=$(curl -s -X POST -H "$AUTH" -H 'Content-Type: application/json' \
  --data-binary "{\"customer_id\":\"$CUST\",\"source_receipt_id\":\"$RC\",\"refund_date\":\"2026-11-06\",\"amount\":100,\"method\":\"BANK_TRANSFER\",\"bank_account\":\"6222 0000 1111 2222\",\"reason\":\"$REASON\"}" \
  "$BASE/business/erp/receipt-refund")
RID=$(echo "$RJSON" | jq -r '.data.id // empty')
RNO=$(echo "$RJSON" | jq -r '.data.refund_no // empty')
[ -n "$RID" ] && ok "登记退款成功（$RNO）" || { bad "登记退款失败：$RJSON"; exit 1; }
check "退款单初始状态 DRAFT" "$(echo "$RJSON" | jq -r '.data.status')" "DRAFT"
check "退款单号前缀 RR-" "$(echo "$RNO" | cut -c1-3)" "RR-"
check "退款原因落库" "$(echo "$RJSON" | jq -r '.data.reason')" "$REASON"
check "已冲减 0" "$(echo "$RJSON" | jq -r '.data.allocated_amount')" "0.0000"
check "page 能查到 1 张" "$(curl -s -H "$AUTH" "$BASE/business/erp/receipt-refund/page?refund_no=$RNO" | jq -r '.data.total')" "1"

echo
echo "==> 6. 边界：原因必填（400）、一分没收的应收不能退款（409）"
NOR=$(curl -s -X POST -H "$AUTH" -H 'Content-Type: application/json' \
  --data-binary "{\"customer_id\":\"$CUST\",\"amount\":10,\"reason\":\"退\"}" \
  "$BASE/business/erp/receipt-refund")
nonz "原因太短被拒" "$(echo "$NOR" | jq -r '.code')"
echo "$NOR" | jq -r '.message // .msg // ""' | grep -q "reason" && ok "报文点名 reason" || bad "报文没提 reason"
# 造一张一分没收的 OPEN 应收：退款不能冲它（那是出库作废的活）
AROPEN=$(psql_q "INSERT INTO biz_receivable (ar_no, customer_id, source_type, source_id, source_no, amount, received_amount, unsettled_amount, credit_days, period_code, due_date, status)
   VALUES ('AR-RR14C-OPEN', '$CUST'::uuid, 'MANUAL', gen_random_uuid(), 'MANUAL', 500, 0, 500, 30, '2026-11', '2026-12-06', 'OPEN') RETURNING id")
OPENR=$(curl -s -X POST -H "$AUTH" -H 'Content-Type: application/json' \
  --data-binary "{\"allocations\":[{\"receivable_id\":\"$AROPEN\",\"amount\":10}]}" \
  "$BASE/business/erp/receipt-refund/$RID/allocate")
nonz "一分没收的应收不能退款" "$(echo "$OPENR" | jq -r '.code')"

echo
echo "==> 7. 退款核销 100 → 应收回到 OPEN / 已收 0"
ALJSON=$(curl -s -X POST -H "$AUTH" -H 'Content-Type: application/json' \
  --data-binary "{\"allocations\":[{\"receivable_id\":\"$AR\",\"amount\":100,\"remark\":\"冲减退货应收的已收款\"}]}" \
  "$BASE/business/erp/receipt-refund/$RID/allocate")
check "核销后已冲减 100" "$(echo "$ALJSON" | jq -r '.data.allocated_amount')" "100.0000"
check "明细 1 行" "$(echo "$ALJSON" | jq -r '.data.allocations | length')" "1"
check "明细里应收状态 OPEN" "$(echo "$ALJSON" | jq -r '.data.allocations[0].ar_status')" "OPEN"
check "应收已收归零 / 未收恢复 1130" \
  "$(psql_q "SELECT status || '|' || received_amount::text || '|' || unsettled_amount::text FROM biz_receivable WHERE id='$AR'::uuid")" \
  "OPEN|0.0000|1130.0000"
check "应收状态日志有退款冲减边" \
  "$(psql_q "SELECT from_status || '>' || to_status FROM biz_status_log WHERE entity_type='RECEIVABLE' AND entity_id='$AR'::uuid AND to_status='OPEN' ORDER BY created_at DESC LIMIT 1")" \
  "PARTIAL>OPEN"
# 同一条 (退款, 应收) 再报一个数 = 改冲减金额（UPSERT，合法）；
# 上限 = 该应收已收 + 本退款此前已冲减它的金额 = 0 + 100，所以 100.0001 必须被拒。
OVER=$(curl -s -X POST -H "$AUTH" -H 'Content-Type: application/json' \
  --data-binary "{\"allocations\":[{\"receivable_id\":\"$AR\",\"amount\":100.0001}]}" \
  "$BASE/business/erp/receipt-refund/$RID/allocate")
nonz "冲减超过可退金额（100.0001 > 已收 100）被拒" "$(echo "$OVER" | jq -r '.code')"
echo "$OVER" | jq -r '.message // .msg // ""' | grep -q "可退金额" && ok "报文点名可退金额" || bad "报文没提可退金额"
check "被拒后应收回到 OPEN / 已收 0（整笔回滚）" \
  "$(psql_q "SELECT status || '|' || received_amount::text FROM biz_receivable WHERE id='$AR'::uuid")" \
  "OPEN|0.0000"

echo
echo "==> 8. 确认 → 过账（借 1122 应收 / 贷 1002 银行存款）"
CONF=$(curl -s -X PATCH -H "$AUTH" -H 'Content-Type: application/json' \
  --data-binary '{"status":"CONFIRMED"}' "$BASE/business/erp/receipt-refund/$RID/status")
check "确认后 CONFIRMED" "$(echo "$CONF" | jq -r '.data.status')" "CONFIRMED"
POSTR=$(curl -s -X PATCH -H "$AUTH" -H 'Content-Type: application/json' \
  --data-binary '{"status":"POSTED"}' "$BASE/business/erp/receipt-refund/$RID/status")
check "过账后 POSTED" "$(echo "$POSTR" | jq -r '.data.status')" "POSTED"
VNO=$(echo "$POSTR" | jq -r '.data.voucher_no')
[ -n "$VNO" ] && ok "退款凭证已生成（$VNO）" || bad "过账没生成凭证号"
check "凭证借方 1122 = 100" \
  "$(psql_q "SELECT count(*) FROM biz_voucher_entry e JOIN biz_voucher v ON v.id=e.voucher_id WHERE v.source_type='RECEIPT_REFUND' AND v.source_id='$RID'::uuid AND e.account_code='1122' AND e.debit_amount=100 AND e.credit_amount=0")" "1"
check "凭证贷方 1002 = 100" \
  "$(psql_q "SELECT count(*) FROM biz_voucher_entry e JOIN biz_voucher v ON v.id=e.voucher_id WHERE v.source_type='RECEIPT_REFUND' AND v.source_id='$RID'::uuid AND e.account_code='1002' AND e.credit_amount=100 AND e.debit_amount=0")" "1"
check "凭证来源登记一致（source_no = 退款单号）" \
  "$(psql_q "SELECT source_no FROM biz_voucher WHERE source_type='RECEIPT_REFUND' AND source_id='$RID'::uuid")" "$RNO"
check "状态日志 3 条（建单/确认/过账）" \
  "$(psql_q "SELECT count(*) FROM biz_status_log WHERE entity_type='RECEIPT_REFUND' AND entity_id='$RID'::uuid")" "3"
CANCELR=$(curl -s -X PATCH -H "$AUTH" -H 'Content-Type: application/json' \
  --data-binary "{\"status\":\"CANCELLED\",\"reason\":\"已过账不该能作废\"}" "$BASE/business/erp/receipt-refund/$RID/status")
nonz "已过账的退款不能作废（终态）" "$(echo "$CANCELR" | jq -r '.code')"

echo
echo "==> 9. 再作废出库单 → 走既有 OPEN → CANCELLED（本卡的目标）"
CANJSON=$(curl -s -X PATCH -H "$AUTH" -H 'Content-Type: application/json' \
  --data-binary "{\"target_status\":\"CANCELLED\",\"reason\":\"$REASON\"}" \
  "$BASE/business/erp/stock-out/$OUT/status")
check "出库单作废成功" "$(echo "$CANJSON" | jq -r '.data.status')" "CANCELLED"
check "应收 CANCELLED / 已收仍 0（CANCELLED 语义未变）" \
  "$(psql_q "SELECT status || '|' || received_amount::text FROM biz_receivable WHERE id='$AR'::uuid")" "CANCELLED|0.0000"
check "库存回滚到 20" "$(psql_q "SELECT qty::text FROM biz_inventory WHERE material_id='$MAT'::uuid AND warehouse_id='$WH'::uuid AND location_id IS NULL")" "20.0000"
check "出库红字凭证 1 张" \
  "$(psql_q "SELECT count(*) FROM biz_voucher WHERE source_type='STOCK_OUT' AND source_id='$OUT'::uuid AND is_red=1")" "1"
AGAIN=$(curl -s -X PATCH -H "$AUTH" -H 'Content-Type: application/json' \
  --data-binary "{\"target_status\":\"CANCELLED\",\"reason\":\"$REASON\"}" \
  "$BASE/business/erp/stock-out/$OUT/status")
nonz "重复作废被拒（幂等）" "$(echo "$AGAIN" | jq -r '.code')"

echo
echo "==> 10. 台账读侧（详情 / 分页筛选 / 白名单）"
DJSON=$(curl -s -H "$AUTH" "$BASE/business/erp/receipt-refund/$RID")
check "详情状态 POSTED" "$(echo "$DJSON" | jq -r '.data.status')" "POSTED"
check "详情凭证号回填" "$(echo "$DJSON" | jq -r '.data.voucher_no')" "$VNO"
check "详情明细 1 行" "$(echo "$DJSON" | jq -r '.data.allocations | length')" "1"
check "page?status=POSTED 能筛到" \
  "$(curl -s -H "$AUTH" "$BASE/business/erp/receipt-refund/page?status=POSTED&refund_no=$RNO" | jq -r '.data.total')" "1"
BADF=$(curl -s -H "$AUTH" "$BASE/business/erp/receipt-refund/page?status=PARTIAL")
nonz "应收的 PARTIAL 在退款状态域里不合法（400）" "$(echo "$BADF" | jq -r '.code')"

echo
echo "==> 汇总：PASS=$PASS FAIL=$FAIL"
[ "$FAIL" = "0" ] || exit 1

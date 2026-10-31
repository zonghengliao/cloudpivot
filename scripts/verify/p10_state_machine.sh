#!/usr/bin/env bash
# ============================================================
# P10 验收脚本：状态机统一 + 入账后只读 + 状态变更日志（铁律 ③④）
#
# 用法（在仓根跑，后端必须已起、库已迁移到 V0159）：
#   BASE=http://127.0.0.1:18090 \
#   P10_USER=admin P10_PASS='P10Admin@123456' \
#   DB_HOST=127.0.0.1 DB_USER=p10role DB_NAME=cloudpivot_p10 DB_PASS=p10pass \
#   bash scripts/verify/p10_state_machine.sh
#
# 断言口径（与卡面 §五 一一对应）：
#   ① 建单 + 过账 → biz_status_log 两行（null→DRAFT、DRAFT→POSTED）
#   ② PATCH/PUT 已过账单据改金额 → 409 + 文案点「单据已入账」
#   ③ 已过账单据只改 remark → 200（白名单）
#   ④ 采购单 SUBMITTED → 收货过账 → 订单变 RECEIVED，日志 biz_ref_no = 收货单号
#   ⑤ 部分收货 → PARTIAL_RECEIVED
#   ⑥ 数据清理：本脚本造的行全部删净（按 P10E- 前缀）
# ============================================================
set -u

BASE="${BASE:-http://127.0.0.1:18090}"
P10_USER="${P10_USER:-admin}"
P10_PASS="${P10_PASS:-P10Admin@123456}"
DB_HOST="${DB_HOST:-127.0.0.1}"
DB_PORT="${DB_PORT:-5432}"
DB_USER="${DB_USER:-p10role}"
DB_NAME="${DB_NAME:-cloudpivot_p10}"
DB_PASS="${DB_PASS:-p10pass}"
PASS=0
FAIL=0

psql_q() { echo "$DB_PASS" | true; PGPASSWORD="$DB_PASS" psql -h "$DB_HOST" -p "$DB_PORT" -U "$DB_USER" -d "$DB_NAME" -tAF '~' -c "$1" 2>&1 | head -1; }
psql_x() { PGPASSWORD="$DB_PASS" psql -v ON_ERROR_STOP=1 -h "$DB_HOST" -p "$DB_PORT" -U "$DB_USER" -d "$DB_NAME" -qAt -c "$1" 2>&1; }

ok()   { PASS=$((PASS+1)); echo "[PASS] $1"; }
bad()  { FAIL=$((FAIL+1)); echo "[FAIL] $1"; }
check() { if [ "$2" = "$3" ]; then ok "$1（$2）"; else bad "$1（期望 [$3] 实际 [$2]）"; fi; }
check_contains() { case "$2" in *"$3"*) ok "$1";; *) bad "$1（未包含 [$3] 实际 [$2]）";; esac; }
check_num() { if awk -v a="$2" -v b="$3" 'BEGIN{exit !(a+0==b+0)}'; then ok "$1（$2）"; else bad "$1（期望 [$3] 实际 [$2]）"; fi; }

# ---------- 0. 登录 ----------
login() {
  curl -s -m 10 -X POST "$BASE/api/system/auth/login" \
    -H 'Content-Type: application/json' \
    -d "{\"username\":\"$P10_USER\",\"password\":\"$1\"}"
}
LOGIN_BODY=""
for CAND in "$P10_PASS" "${P10_NEWPASS:-P10Rotated@123456}"; do
  LOGIN_BODY=$(login "$CAND")
  TOKEN=$(printf '%s' "$LOGIN_BODY" | python3 -c 'import sys,json;d=json.load(sys.stdin);print((d.get("data") or {}).get("access_token",""))' 2>/dev/null)
  if [ -n "$TOKEN" ]; then P10_PASS="$CAND"; break; fi
done
MUST=$(printf '%s' "$LOGIN_BODY" | python3 -c 'import sys,json;d=json.load(sys.stdin);print(str((d.get("data") or {}).get("must_change_password",False)).lower())' 2>/dev/null)
if [ "$MUST" = "true" ]; then
  NEWPASS="${P10_NEWPASS:-P10Rotated@123456}"
  curl -s -m 10 -X PUT "$BASE/api/system/auth/password" -H "Authorization: Bearer $TOKEN" \
    -H 'Content-Type: application/json' \
    -d "{\"old_password\":\"$P10_PASS\",\"new_password\":\"$NEWPASS\"}" > /dev/null
  P10_PASS="$NEWPASS"
  LOGIN_BODY=$(curl -s -m 10 -X POST "$BASE/api/system/auth/login" \
    -H 'Content-Type: application/json' \
    -d "{\"username\":\"$P10_USER\",\"password\":\"$P10_PASS\"}")
  TOKEN=$(printf '%s' "$LOGIN_BODY" | python3 -c 'import sys,json;d=json.load(sys.stdin);print((d.get("data") or {}).get("access_token",""))' 2>/dev/null)
fi
if [ -z "$TOKEN" ]; then
  echo "[FATAL] 登录失败：$LOGIN_BODY"
  exit 1
fi
ok "登录成功（$P10_USER）"

api() { # api METHOD PATH [BODY] → 打印 "HTTP码~响应体"
  local method="$1" path="$2" body="${3:-}"
  if [ -n "$body" ]; then
    curl -s -m 20 -o /tmp/p10_resp.json -w '%{http_code}' -X "$method" "$BASE$path" \
      -H "Authorization: Bearer $TOKEN" -H 'Content-Type: application/json' -d "$body"
  else
    curl -s -m 20 -o /tmp/p10_resp.json -w '%{http_code}' -X "$method" "$BASE$path" \
      -H "Authorization: Bearer $TOKEN"
  fi
  echo "~$(cat /tmp/p10_resp.json)"
}
body_of() { printf '%s' "${1#*~}"; }
code_of() { printf '%s' "${1%%~*}"; }
jget() { printf '%s' "$1" | python3 -c "import sys,json;d=json.load(sys.stdin);print(eval('d'+sys.argv[1]))" "$2" 2>/dev/null; }

# ---------- 0.5 夹具（单位 / 供应商 / 仓库 / 物料 / 库位）----------
SUP='a10e0001-0000-4000-8000-000000000001'
WH='a10e0001-0000-4000-8000-000000000002'
UNIT='a10e0001-0000-4000-8000-000000000003'
MAT='a10e0001-0000-4000-8000-000000000011'
MAT2='a10e0001-0000-4000-8000-000000000012'

cleanup() {
  psql_x "DELETE FROM biz_status_log WHERE entity_no LIKE 'IN-P10E%' OR entity_no LIKE 'PO-P10E%'" >/dev/null 2>&1
  psql_x "DELETE FROM biz_voucher_entry WHERE voucher_id IN (SELECT id FROM biz_voucher WHERE source_id IN (SELECT id FROM biz_stock_in WHERE supplier_id='$SUP'::uuid))" >/dev/null 2>&1
  psql_x "DELETE FROM biz_voucher WHERE source_id IN (SELECT id FROM biz_stock_in WHERE supplier_id='$SUP'::uuid)" >/dev/null 2>&1
  psql_x "DELETE FROM biz_payable WHERE source_id IN (SELECT id FROM biz_stock_in WHERE supplier_id='$SUP'::uuid)" >/dev/null 2>&1
  psql_x "DELETE FROM biz_material_serial WHERE source_id IN (SELECT id FROM biz_stock_in WHERE supplier_id='$SUP'::uuid)" >/dev/null 2>&1
  psql_x "DELETE FROM biz_inventory_txn WHERE doc_id IN (SELECT id FROM biz_stock_in WHERE supplier_id='$SUP'::uuid)" >/dev/null 2>&1
  psql_x "DELETE FROM biz_inventory_location WHERE material_id IN ('$MAT'::uuid,'$MAT2'::uuid) AND warehouse_id='$WH'::uuid" >/dev/null 2>&1
  psql_x "DELETE FROM biz_inventory WHERE material_id IN ('$MAT'::uuid,'$MAT2'::uuid) AND warehouse_id='$WH'::uuid" >/dev/null 2>&1
  psql_x "DELETE FROM biz_stock_in WHERE supplier_id='$SUP'::uuid" >/dev/null 2>&1
  psql_x "DELETE FROM biz_purchase_order WHERE supplier_id='$SUP'::uuid" >/dev/null 2>&1
  psql_x "DELETE FROM biz_material WHERE id IN ('$MAT'::uuid,'$MAT2'::uuid)" >/dev/null 2>&1
  psql_x "DELETE FROM biz_warehouse WHERE id='$WH'::uuid" >/dev/null 2>&1
  psql_x "DELETE FROM biz_supplier WHERE id='$SUP'::uuid" >/dev/null 2>&1
  psql_x "DELETE FROM biz_unit WHERE id='$UNIT'::uuid" >/dev/null 2>&1
}

echo "==> 0.5 夹具（P10E- 前缀）"
cleanup
psql_x "INSERT INTO biz_unit (id, unit_code, unit_name, is_base, base_unit_id, rate, status, sort_order)
        VALUES ('$UNIT','P10E-PCS','个',1,NULL,1.000000,1,1)" >/dev/null || { echo "[FATAL] 单位夹具失败"; exit 1; }
psql_x "INSERT INTO biz_supplier (id, supplier_code, supplier_name, status)
        VALUES ('$SUP','P10E-SUP','P10E 验收供应商',1)" >/dev/null || { echo "[FATAL] 供应商夹具失败"; exit 1; }
psql_x "INSERT INTO biz_warehouse (id, warehouse_code, warehouse_name, warehouse_path, level, is_leaf, status)
        VALUES ('$WH','P10E-WH','P10E 收货仓','/$WH/',1,1,1)" >/dev/null || { echo "[FATAL] 仓库夹具失败"; exit 1; }
psql_x "INSERT INTO biz_material (id, material_code, material_name, unit_id, status, serial_required)
        VALUES ('$MAT','P10E-M1','P10E 螺丝','$UNIT',1,false),
               ('$MAT2','P10E-M2','P10E 垫片','$UNIT',1,false)" >/dev/null || { echo "[FATAL] 物料夹具失败"; exit 1; }
ok "夹具就位"

# ---------- ① 建单 → 过账 → 两行状态日志 ----------
echo "==> ① 建单 + 过账 → biz_status_log 两行"
R=$(api POST /api/business/erp/stock-in "{\"supplier_id\":\"$SUP\",\"warehouse_id\":\"$WH\",\"items\":[{\"material_id\":\"$MAT\",\"qty\":\"5\",\"price_excl\":\"2\",\"tax_rate\":\"0\"}]}")
check "建单 HTTP" "$(code_of "$R")" "200"
IN_ID=$(jget "$(body_of "$R")" "['data']['header']['id']")
IN_NO=$(jget "$(body_of "$R")" "['data']['header']['in_no']")
if [ -z "$IN_ID" ]; then echo "[FATAL] 拿不到收货单 id：$(body_of "$R")"; cleanup; exit 1; fi

R=$(api PATCH "/api/business/erp/stock-in/$IN_ID/status" '{"status":"POSTED"}')
check "过账 HTTP" "$(code_of "$R")" "200"

LOGN=$(psql_q "SELECT count(*) FROM biz_status_log WHERE entity_no='$IN_NO'")
check_num "状态日志行数" "$LOGN" "2"
FIRST=$(psql_q "SELECT coalesce(from_status,'NULL')||'→'||to_status FROM biz_status_log WHERE entity_no='$IN_NO' ORDER BY created_at, id LIMIT 1")
check "第一行日志" "$FIRST" "NULL→DRAFT"
SECOND=$(psql_q "SELECT coalesce(from_status,'NULL')||'→'||to_status FROM biz_status_log WHERE entity_no='$IN_NO' ORDER BY created_at DESC, id DESC LIMIT 1")
check "第二行日志" "$SECOND" "DRAFT→POSTED"
OPNAME=$(psql_q "SELECT coalesce(operator_name,'') FROM biz_status_log WHERE entity_no='$IN_NO' ORDER BY created_at DESC LIMIT 1")
check "操作人姓名落库" "$OPNAME" "$P10_USER"

# ---------- ② 入账后只读：改金额 → 409 ----------
echo "==> ② 已过账单据改金额 → 409"
R=$(api PUT "/api/business/erp/stock-in/$IN_ID" "{\"supplier_id\":\"$SUP\",\"warehouse_id\":\"$WH\",\"items\":[{\"material_id\":\"$MAT\",\"qty\":\"5\",\"price_excl\":\"3\",\"tax_rate\":\"0\"}]}")
check "改金额 HTTP" "$(code_of "$R")" "409"
check_contains "文案点「已入账」" "$(body_of "$R")" "单据已入账"

# ---------- ②b 卡面 §五-3 原文：body 只给 total_amount ----------
# 这一条专门防「未知字段被 serde 静默丢掉 → 200」的假绿（P10 实测踩到过）：
# `CreateStockInReq` 必须显式收下 total_*，否则改金额的请求会变成「什么都没改」。
echo "==> ②b PUT {\"total_amount\":999}（只有金额字段）→ 409"
R=$(api PUT "/api/business/erp/stock-in/$IN_ID" '{"total_amount":999}')
check "只有 total_amount 的改单 HTTP" "$(code_of "$R")" "409"
check_contains "文案点「已入账」" "$(body_of "$R")" "单据已入账"
check_contains "文案点名 total_amount" "$(body_of "$R")" "total_amount"
AMT0=$(psql_q "SELECT total_amount FROM biz_stock_in WHERE id='$IN_ID'::uuid")
check_num "库里金额一毫未动" "$AMT0" "10"

# ---------- ③ 白名单：只改 remark → 200 ----------
echo "==> ③ 只改 remark → 200（白名单）"
R=$(api PUT "/api/business/erp/stock-in/$IN_ID" "{\"supplier_id\":\"$SUP\",\"warehouse_id\":\"$WH\",\"remark\":\"P10E 只改备注\",\"items\":[{\"material_id\":\"$MAT\",\"qty\":\"5\",\"price_excl\":\"2\",\"tax_rate\":\"0\"}]}")
check "改备注 HTTP" "$(code_of "$R")" "200"
AMT=$(psql_q "SELECT total_amount FROM biz_stock_in WHERE id='$IN_ID'::uuid")
check_num "金额未被顺手改掉" "$AMT" "10"
RM=$(psql_q "SELECT coalesce(remark,'') FROM biz_stock_in WHERE id='$IN_ID'::uuid")
check "备注已落库" "$RM" "P10E 只改备注"

# ---------- ④ 采购单追溯：SUBMITTED → 收货 → RECEIVED + 日志带收货单号 ----------
echo "==> ④ 采购单收货追溯（RECEIVED + biz_ref_no）"
R=$(api POST /api/business/erp/purchase-order "{\"supplier_id\":\"$SUP\",\"warehouse_id\":\"$WH\",\"price_mode\":\"EXCL\",\"items\":[{\"material_id\":\"$MAT\",\"qty\":\"10\",\"price_excl\":\"2\",\"tax_rate\":\"0\"}]}")
check "建采购单 HTTP" "$(code_of "$R")" "200"
PO_ID=$(jget "$(body_of "$R")" "['data']['order']['id']")
PO_NO=$(jget "$(body_of "$R")" "['data']['order']['order_no']")
if [ -z "$PO_ID" ]; then echo "[FATAL] 拿不到采购单 id：$(body_of "$R")"; cleanup; exit 1; fi
R=$(api PATCH "/api/business/erp/purchase-order/$PO_ID/status" '{"status":"SUBMITTED"}')
check "提交采购单 HTTP" "$(code_of "$R")" "200"
POL1=$(psql_q "SELECT coalesce(from_status,'NULL')||'→'||to_status FROM biz_status_log WHERE entity_no='$PO_NO' ORDER BY created_at LIMIT 1")
check "采购单提交日志" "$POL1" "NULL→DRAFT"

R=$(api POST /api/business/erp/stock-in "{\"source_order_id\":\"$PO_ID\",\"supplier_id\":\"$SUP\",\"warehouse_id\":\"$WH\"}")
check "按采购单建收货单 HTTP" "$(code_of "$R")" "200"
IN2_ID=$(jget "$(body_of "$R")" "['data']['header']['id']")
IN2_NO=$(jget "$(body_of "$R")" "['data']['header']['in_no']")
R=$(api PATCH "/api/business/erp/stock-in/$IN2_ID/status" '{"status":"POSTED"}')
check "收货过账 HTTP" "$(code_of "$R")" "200"

POSTATUS=$(psql_q "SELECT status FROM biz_purchase_order WHERE id='$PO_ID'::uuid")
check "采购单状态" "$POSTATUS" "RECEIVED"
POLOG=$(psql_q "SELECT from_status||'→'||to_status||'|'||coalesce(biz_ref_no,'')||'|'||coalesce(biz_ref_type,'') FROM biz_status_log WHERE entity_no='$PO_NO' ORDER BY created_at DESC LIMIT 1")
check "采购单推进日志" "$POLOG" "SUBMITTED→RECEIVED|$IN2_NO|STOCK_IN"

# ---------- ⑤ 部分收货 → PARTIAL_RECEIVED ----------
echo "==> ⑤ 部分收货 → PARTIAL_RECEIVED"
R=$(api POST /api/business/erp/purchase-order "{\"supplier_id\":\"$SUP\",\"warehouse_id\":\"$WH\",\"price_mode\":\"EXCL\",\"items\":[{\"material_id\":\"$MAT\",\"qty\":\"10\",\"price_excl\":\"2\",\"tax_rate\":\"0\"}]}")
PO2_ID=$(jget "$(body_of "$R")" "['data']['order']['id']")
PO2_NO=$(jget "$(body_of "$R")" "['data']['order']['order_no']")
PO2_ITEM=$(jget "$(body_of "$R")" "['data']['items'][0]['id']")
R=$(api PATCH "/api/business/erp/purchase-order/$PO2_ID/status" '{"status":"SUBMITTED"}')
check "部分收货：提交采购单 HTTP" "$(code_of "$R")" "200"
R=$(api POST /api/business/erp/stock-in "{\"source_order_id\":\"$PO2_ID\",\"supplier_id\":\"$SUP\",\"warehouse_id\":\"$WH\",\"items\":[{\"purchase_order_item_id\":\"$PO2_ITEM\",\"material_id\":\"$MAT\",\"qty\":\"4\",\"price_excl\":\"2\",\"tax_rate\":\"0\"}]}")
check "部分收货：建收货单 HTTP" "$(code_of "$R")" "200"
IN3_ID=$(jget "$(body_of "$R")" "['data']['header']['id']")
R=$(api PATCH "/api/business/erp/stock-in/$IN3_ID/status" '{"status":"POSTED"}')
check "部分收货：过账 HTTP" "$(code_of "$R")" "200"
POSTATUS2=$(psql_q "SELECT status FROM biz_purchase_order WHERE id='$PO2_ID'::uuid")
check "部分收货后采购单状态" "$POSTATUS2" "PARTIAL_RECEIVED"
POLOG2=$(psql_q "SELECT from_status||'→'||to_status FROM biz_status_log WHERE entity_no='$PO2_NO' ORDER BY created_at DESC, id DESC LIMIT 1")
check "部分收货推进日志" "$POLOG2" "SUBMITTED→PARTIAL_RECEIVED"

# ---------- ⑥ 清理 ----------
echo "==> ⑥ 清理夹具"
cleanup
LEFT=$(psql_q "SELECT count(*) FROM biz_status_log WHERE entity_no LIKE 'IN-P10E%' OR entity_no LIKE 'PO-P10E%'")
check_num "状态日志零残留" "$LEFT" "0"
LEFT2=$(psql_q "SELECT count(*) FROM biz_stock_in WHERE supplier_id='$SUP'::uuid")
check_num "收货单零残留" "$LEFT2" "0"

echo "==> 汇总：PASS=$PASS FAIL=$FAIL"
[ "$FAIL" -eq 0 ] || exit 1

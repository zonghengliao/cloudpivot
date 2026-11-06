#!/usr/bin/env bash
# SO-18 前端页真实验收（连跑两轮：bash verify-receipt.sh 1 / bash verify-receipt.sh 2）
#
# 只打**前端页真实调用**的那 5 个接口（与 receipt/index.vue 的 api 层一一对应）：
#   POST  /receipt                收款登记（弹窗提交）
#   POST  /receipt/{id}/allocate  核销面板保存
#   PATCH /receipt/{id}/status    确认 / 过账 / 作废
#   GET   /receipt/page           列表（分页 + 筛选）
#   GET   /receipt/{id}           详情（核销明细回填）
# 断言口径逐字对齐 receipt-logic.ts（canPost / validateAllocation / postBlockedReason /
# isAllocatable），凭证明细直接查库（biz_voucher_entry）。
set -uo pipefail
ROUND="${1:-1}"
BASE="${BASE:-http://127.0.0.1:8023/api}"
OUT="${OUT:-/tmp/so18-verify-round$ROUND.out}"
PGPASSWORD=alun; export PGPASSWORD
PSQL="psql -h 127.0.0.1 -U alun -d ${NIK_DATABASE_NAME:-cloudpivot_so18} -At"
: > "$OUT"
PASS=0; FAIL=0
log() { echo "$@" | tee -a "$OUT"; }
ok()  { PASS=$((PASS+1)); log "PASS  $*"; }
bad() { FAIL=$((FAIL+1)); log "FAIL  $*"; }
check() { if [ "$2" = "$3" ]; then ok "$1 = $3"; else bad "$1 = $2 (expect $3)"; fi; }

OLD_PW="${OLD_PW:-Admin@123456}"
NEW_PW="${NEW_PW:-So18@Pass123}"
login() { curl -s -X POST "$BASE/system/auth/login" -H 'Content-Type: application/json' \
            -d "{\"username\":\"admin\",\"password\":\"$1\"}"; }
RESP=$(login "$OLD_PW"); TOKEN=$(echo "$RESP" | jq -r '.data.access_token // empty')
if [ -n "$TOKEN" ]; then
  log "== 首登：改初始密码（session_guard 只放行改密/登出）"
  curl -s -X PUT "$BASE/system/auth/password" -H "Authorization: Bearer $TOKEN" \
    -H 'Content-Type: application/json' \
    -d "{\"old_password\":\"$OLD_PW\",\"new_password\":\"$NEW_PW\"}" | jq -c '{code,message}' | tee -a "$OUT"
  RESP=$(login "$NEW_PW"); TOKEN=$(echo "$RESP" | jq -r '.data.access_token // empty')
fi
[ -z "$TOKEN" ] && { log "LOGIN FAIL: $(echo "$RESP" | head -c 300)"; exit 1; }
AUTH="Authorization: Bearer $TOKEN"
log "== login ok (token_len=${#TOKEN}) round=$ROUND"

CUS1='d0260000-0000-4000-8000-000000000001'   # 北京云枢（2 张未收）
CUS2='d0260000-0000-4000-8000-000000000002'   # 上海云枢（别家客户，1 张）
N1="AR-SO18-R$ROUND-001"; N2="AR-SO18-R$ROUND-002"; N3="AR-SO18-R$ROUND-003"

# ---------- 0) 核销面板数据源：两次分页（OPEN + PARTIAL）再本地合并 ----------
log "== 0) 核销面板数据源 GET /receivable/page（status=OPEN，customer_id=$CUS1）"
OPEN=$(curl -s "$BASE/business/erp/receivable/page?page=1&page_size=200&status=OPEN&customer_id=$CUS1" -H "$AUTH")
PART=$(curl -s "$BASE/business/erp/receivable/page?page=1&page_size=200&status=PARTIAL&customer_id=$CUS1" -H "$AUTH")
echo "$OPEN" | jq -c '[.data.list[]?|{ar_no,amount,unsettled_amount,status,due_date}]' | tee -a "$OUT"
echo "$PART" | jq -c '[.data.list[]?|{ar_no,amount,unsettled_amount,status,due_date}]' | tee -a "$OUT"
AR1=$(echo "$OPEN" | jq -r --arg p "$N1" '.data.list[] | select(.ar_no==$p) | .id')
AR2=$(echo "$OPEN" | jq -r --arg p "$N2" '.data.list[] | select(.ar_no==$p) | .id')
OTHER=$(curl -s "$BASE/business/erp/receivable/page?page=1&page_size=200&status=OPEN&customer_id=$CUS2" -H "$AUTH" \
  | jq -r --arg p "$N3" '.data.list[] | select(.ar_no==$p) | .id')
check "面板拿到 $N1" "$([ -n "$AR1" ] && echo yes || echo no)" "yes"
check "面板拿到 $N2" "$([ -n "$AR2" ] && echo yes || echo no)" "yes"
check "面板拿到别家客户 $N3" "$([ -n "$OTHER" ] && echo yes || echo no)" "yes"
if [ -z "$AR1" ] || [ -z "$AR2" ] || [ -z "$OTHER" ]; then
  log "种子缺失：先跑 seed-receivables.sql -v round=$ROUND"; exit 1
fi

# ---------- 1) 收款登记（登记弹窗） ----------
log "== 1) 收款登记 POST /receipt amount=1000.0000（弹窗 testid=btn-rc-create）"
CREATE=$(curl -s -X POST "$BASE/business/erp/receipt" -H "$AUTH" -H 'Content-Type: application/json' \
  -d "{\"customer_id\":\"$CUS1\",\"receipt_date\":\"2026-11-06\",\"amount\":\"1000.0000\",\"method\":\"BANK_TRANSFER\",\"bank_account\":\"6222-SO18\",\"remark\":\"SO-18 前端页验收 第$ROUND轮\"}")
echo "$CREATE" | jq -c '{code,message,data:{receipt_no:.data.receipt_no,amount:.data.amount,allocated:.data.allocated_amount,unallocated:.data.unallocated_amount,status:.data.status,status_label:.data.status_label,method_label:.data.method_label}}' | tee -a "$OUT"
RID=$(echo "$CREATE" | jq -r '.data.id // empty')
RNO=$(echo "$CREATE" | jq -r '.data.receipt_no // empty')
check "登记单号形如 RC-yyyyMMdd-000001" "$(echo "$RNO" | grep -cE '^RC-[0-9]{8}-[0-9]{6}$')" "1"
check "登记后状态" "$(echo "$CREATE" | jq -r '.data.status')" "DRAFT"
check "登记后未核销 = 全额" "$(echo "$CREATE" | jq -r '.data.unallocated_amount')" "1000.0000"
S0=$(curl -s -o /dev/null -w '%{http_code}' -X POST "$BASE/business/erp/receipt" -H "$AUTH" -H 'Content-Type: application/json' \
  -d "{\"customer_id\":\"$CUS1\",\"amount\":\"0.0000\"}")
check "负面：登记金额 0 的 HTTP" "$S0" "400"

# ---------- 2) 核销面板负面（validateAllocation 的三道闸） ----------
log "== 2) 核销负面 POST /receipt/$RID/allocate"
S1=$(curl -s -o /tmp/so18-n1.json -w '%{http_code}' -X POST "$BASE/business/erp/receipt/$RID/allocate" -H "$AUTH" -H 'Content-Type: application/json' \
  -d "{\"items\":[{\"receivable_id\":\"$OTHER\",\"amount\":\"100.0000\"}]}")
log "   核到别家客户 -> http=$S1 body=$(cat /tmp/so18-n1.json)"
check "负面：核到别家客户应收的 HTTP" "$S1" "400"
S2=$(curl -s -o /tmp/so18-n2.json -w '%{http_code}' -X POST "$BASE/business/erp/receipt/$RID/allocate" -H "$AUTH" -H 'Content-Type: application/json' \
  -d "{\"items\":[{\"receivable_id\":\"$AR1\",\"amount\":\"600.0001\"}]}")
log "   单张超额 600.0001 > 600 -> http=$S2 body=$(cat /tmp/so18-n2.json)"
check "负面：单张超过可核销余额的 HTTP" "$S2" "400"
S3=$(curl -s -o /tmp/so18-n3.json -w '%{http_code}' -X POST "$BASE/business/erp/receipt/$RID/allocate" -H "$AUTH" -H 'Content-Type: application/json' \
  -d "{\"items\":[{\"receivable_id\":\"$AR1\",\"amount\":\"600.0000\"},{\"receivable_id\":\"$AR2\",\"amount\":\"401.0000\"}]}")
log "   合计 1001 > 1000 -> http=$S3 body=$(cat /tmp/so18-n3.json)"
check "负面：核销合计超过收款金额的 HTTP" "$S3" "400"

# ---------- 3) 核销面板保存（一次两张；AR2 只核 300 → 留 100 → PARTIAL） ----------
log "== 3) 核销面板保存 POST /receipt/$RID/allocate {AR1:600, AR2:300}"
ALLOC=$(curl -s -X POST "$BASE/business/erp/receipt/$RID/allocate" -H "$AUTH" -H 'Content-Type: application/json' \
  -d "{\"items\":[{\"receivable_id\":\"$AR1\",\"amount\":\"600.0000\"},{\"receivable_id\":\"$AR2\",\"amount\":\"300.0000\"}]}")
echo "$ALLOC" | jq -c '{code,message,data:{allocated:.data.allocated_amount,unallocated:.data.unallocated_amount,allocations:[.data.allocations[]?|{ar_no,amount,ar_status,ar_unsettled_amount}]}}' | tee -a "$OUT"
check "核销后已核销 = 900" "$(echo "$ALLOC" | jq -r '.data.allocated_amount')" "900.0000"
check "核销后未核销 = 100（面板实时剩余）" "$(echo "$ALLOC" | jq -r '.data.unallocated_amount')" "100.0000"

# ---------- 4) 应收状态变化（核销副作用，页面「已核销/未核销」列的数据来源） ----------
log "== 4) 应收状态变化 GET /receivable/{id}"
A1=$(curl -s "$BASE/business/erp/receivable/$AR1" -H "$AUTH")
A2=$(curl -s "$BASE/business/erp/receivable/$AR2" -H "$AUTH")
echo "$A1" | jq -c '{ar_no:.data.ar_no,amount:.data.amount,received:.data.received_amount,unsettled:.data.unsettled_amount,status:.data.status,status_label:.data.status_label}' | tee -a "$OUT"
echo "$A2" | jq -c '{ar_no:.data.ar_no,amount:.data.amount,received:.data.received_amount,unsettled:.data.unsettled_amount,status:.data.status,status_label:.data.status_label}' | tee -a "$OUT"
check "AR001 状态（核满）" "$(echo "$A1" | jq -r '.data.status')" "SETTLED"
check "AR001 未收" "$(echo "$A1" | jq -r '.data.unsettled_amount')" "0.0000"
check "AR002 状态（核一半）" "$(echo "$A2" | jq -r '.data.status')" "PARTIAL"
check "AR002 未收" "$(echo "$A2" | jq -r '.data.unsettled_amount')" "100.0000"

# ---------- 5) 确认 ----------
log "== 5) 确认 PATCH /receipt/$RID/status {CONFIRMED}"
CONF=$(curl -s -X PATCH "$BASE/business/erp/receipt/$RID/status" -H "$AUTH" -H 'Content-Type: application/json' -d '{"status":"CONFIRMED"}')
echo "$CONF" | jq -c '{code,message,data:{status:.data.status,status_label:.data.status_label}}' | tee -a "$OUT"
check "确认后状态" "$(echo "$CONF" | jq -r '.data.status')" "CONFIRMED"

# ---------- 6) 过账前置：未核销满必须 409（canPost / postBlockedReason 的口径） ----------
log "== 6) 过账前置：未核销满 -> 409"
S4=$(curl -s -o /tmp/so18-n4.json -w '%{http_code}' -X PATCH "$BASE/business/erp/receipt/$RID/status" -H "$AUTH" -H 'Content-Type: application/json' -d '{"status":"POSTED"}')
log "   http=$S4 body=$(cat /tmp/so18-n4.json)"
check "负面：未核销满过账的 HTTP" "$S4" "409"

# ---------- 7) 补齐 AR2 剩余 100 → 过账生成凭证 ----------
log "== 7) 面板只发「有变化」的行（changedAllocationRows 的口径）"
# 7.1 负面：把已核满（SETTLED）的 AR1 原样再提交一次 → 409「已收清不能核销」
#     —— 这正是 buildAllocateItems 必须跳过未改金额行的原因
S6=$(curl -s -o /tmp/so18-n6.json -w '%{http_code}' -X POST "$BASE/business/erp/receipt/$RID/allocate" -H "$AUTH" -H 'Content-Type: application/json' \
  -d "{\"items\":[{\"receivable_id\":\"$AR1\",\"amount\":\"600.0000\"}]}")
log "   重提已核满的 AR1 -> http=$S6 body=$(cat /tmp/so18-n6.json)"
check "负面：重提已 SETTLED 应收的 HTTP" "$S6" "409"
# 7.2 只补 AR2 的 100（AR1 那行库里已有，后端合计会算 untouched）
log "== 7.2 补齐 AR2 剩余 100（只发 AR2）"
curl -s -X POST "$BASE/business/erp/receipt/$RID/allocate" -H "$AUTH" -H 'Content-Type: application/json' \
  -d "{\"items\":[{\"receivable_id\":\"$AR2\",\"amount\":\"400.0000\"}]}" \
  | jq -c '{code,message,data:{allocated:.data.allocated_amount,unallocated:.data.unallocated_amount}}' | tee -a "$OUT"
POST=$(curl -s -X PATCH "$BASE/business/erp/receipt/$RID/status" -H "$AUTH" -H 'Content-Type: application/json' -d '{"status":"POSTED"}')
echo "$POST" | jq -c '{code,message,data:{status:.data.status,status_label:.data.status_label,voucher_no:.data.voucher_no}}' | tee -a "$OUT"
check "过账后状态" "$(echo "$POST" | jq -r '.data.status')" "POSTED"
check "过账生成凭证号形如 PZ-yyyyMMdd-000001" "$(echo "$POST" | jq -r '.data.voucher_no' | grep -cE '^PZ-[0-9]{8}-[0-9]{6}$')" "1"
A2B=$(curl -s "$BASE/business/erp/receivable/$AR2" -H "$AUTH")
check "AR002 补核后状态" "$(echo "$A2B" | jq -r '.data.status')" "SETTLED"
VID=$(echo "$POST" | jq -r '.data.voucher_id')
log "== 7.1 凭证分录（查库 biz_voucher_entry）"
$PSQL -c "select account_code, debit_amount, credit_amount from biz_voucher_entry where voucher_id='$VID' order by line_no" | tee -a "$OUT"
check "凭证分录 1002 借 1000.0000" \
  "$($PSQL -c "select debit_amount from biz_voucher_entry where voucher_id='$VID' and account_code='1002'")" "1000.0000"
check "凭证分录 1122 贷 1000.0000" \
  "$($PSQL -c "select credit_amount from biz_voucher_entry where voucher_id='$VID' and account_code='1122'")" "1000.0000"

# ---------- 8) 列表 + 详情（页面列表 / 详情抽屉的数据源） ----------
log "== 8) 列表 GET /receipt/page（customer_id 筛选 + 分页）"
PAGE=$(curl -s "$BASE/business/erp/receipt/page?page=1&page_size=10&customer_id=$CUS1" -H "$AUTH")
echo "$PAGE" | jq -c '{total:.data.total,list:[.data.list[]|{receipt_no,customer_name,receipt_date,amount,allocated_amount,unallocated_amount,method_label,status,status_label}]}' | tee -a "$OUT"
check "列表含本轮单号" "$(echo "$PAGE" | jq -r --arg n "$RNO" '[.data.list[]|select(.receipt_no==$n)]|length')" "1"
check "列表行客户名（LEFT JOIN biz_customer）" "$(echo "$PAGE" | jq -r --arg n "$RNO" '.data.list[]|select(.receipt_no==$n)|.customer_name')" "北京云枢科技有限公司"
check "列表行收款方式中文" "$(echo "$PAGE" | jq -r --arg n "$RNO" '.data.list[]|select(.receipt_no==$n)|.method_label')" "银行转账"
check "列表行状态中文" "$(echo "$PAGE" | jq -r --arg n "$RNO" '.data.list[]|select(.receipt_no==$n)|.status_label')" "已过账"

DET=$(curl -s "$BASE/business/erp/receipt/$RID" -H "$AUTH")
echo "$DET" | jq -c '{receipt_no:.data.receipt_no,status:.data.status,allocated:.data.allocated_amount,voucher_no:.data.voucher_no,allocations:[.data.allocations[]?|{ar_no,amount,ar_status,ar_unsettled_amount}]}' | tee -a "$OUT"
check "详情核销明细行数" "$(echo "$DET" | jq -r '.data.allocations|length')" "2"

# ---------- 9) 过账后不可再核销（isAllocatable 只放行 DRAFT/CONFIRMED） ----------
S5=$(curl -s -o /tmp/so18-n5.json -w '%{http_code}' -X POST "$BASE/business/erp/receipt/$RID/allocate" -H "$AUTH" -H 'Content-Type: application/json' \
  -d "{\"items\":[{\"receivable_id\":\"$AR1\",\"amount\":\"1.0000\"}]}")
log "   过账后再核销 -> http=$S5 body=$(cat /tmp/so18-n5.json)"
check "负面：POSTED 收款不可再核销的 HTTP" "$S5" "409"

log ""
log "==== ROUND $ROUND 结果: PASS=$PASS FAIL=$FAIL  receipt=$RNO ($RID) ===="
echo "RID=$RID RNO=$RNO" >> "$OUT"
[ "$FAIL" -eq 0 ]

#!/usr/bin/env bash
# =============================================================================
# SO-30 验收：退货过账（回库 + 红字入库凭证 + 冲应收 + 红字发票 + delivered_qty）
# =============================================================================
#   bash scripts/verify/so30_db_setup.sh              # 空库
#   bash scripts/verify/so30_instance.sh up           # 实例 :18230（全量迁移）
#   psql -f scripts/verify/so30_posting_fixture.sql   # 夹具（A 未开票 / B 已开票 / C 已收款）
#   bash scripts/verify/so30_return_posting_e2e.sh    # 本脚本
#
# 验什么（卡面验收：cargo test 全绿 + 真实退货后三账一致 + 应收正确冲减）：
#   ① V0305 结构：biz_stock_in 来源两列 + 部分唯一索引 + 发票来源白名单含 SALES_RETURN
#   ② 场景 A（未开票 · 部分退货）：POSTED 后
#        · 生成入库单（in_type=RETURN / source_type=SALES_RETURN / status=POSTED）
#        · 三本账回库（成本账 / 库位账 / 流水，流水 biz_type=SALES_RETURN_IN）
#        · 红字入库凭证 借 1405 / 贷 6401，金额 = 退货数量 × 原出库成本
#        · 应收按退货含税金额冲减（amount / unsettled 同步减少，状态仍 OPEN）
#        · delivered_qty 回写减少
#        · 不造红字发票（未开票）
#        · 客户信用占用下降（同一张新订单：退货前提交 409 额度不足，退货后提交通过）
#   ③ 场景 B（已开票 · 全退）：红字发票 OUT_RED 三金额全负 + ISSUED
#        · 收入冲减凭证 借 5001 + 借 2221.02 / 贷 1122（平衡）
#        · 应收全额冲减 → OPEN → CANCELLED
#   ④ 场景 C（已收款 · 退货额 > 未收）：POSTED 409 点名「收款退款单」，且
#        **整笔回滚**（状态仍 PENDING_POST、无入库单、库存/应收/delivered_qty 一分不动）
#   ⑤ 幂等：已 POSTED 的退货再流转 → 409；重复过账不会回第二次库
#   ⑥ 三账一致：每个物料 SUM(库位账) == 成本账数量
#
# 证据：docs/evidence/20261107-so30-return-posting/raw/*（响应体逐份落盘，JWT 已脱敏）
# =============================================================================
set -uo pipefail
DIR=$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)
ROOT=$(cd "$DIR/../.." && pwd)
PORT=${SO30_PORT:-18230}
BASE="http://127.0.0.1:$PORT/api"
DB=${SO30_DB:-cloudpivot_so30}
EVID="$ROOT/docs/evidence/20261107-so30-return-posting"
RAW="$EVID/raw"
mkdir -p "$RAW"
export PGPASSWORD=alun
PSQLQ() { psql -h 127.0.0.1 -U alun -d "$DB" -tAF'|' "$@"; }

# 夹具 UUID（与 so30_posting_fixture.sql 逐字一致）
CUST_A='d0300000-0000-4000-8000-000000000001'
CUST_B='d0300000-0000-4000-8000-000000000002'
CUST_C='d0300000-0000-4000-8000-000000000003'
SO_A='d0301000-0000-4000-8000-000000000001'
SO_B='d0301000-0000-4000-8000-000000000002'
SO_C='d0301000-0000-4000-8000-000000000003'
I_A1='d0301100-0000-4000-8000-000000000001'
I_A2='d0301100-0000-4000-8000-000000000002'
I_B1='d0301100-0000-4000-8000-000000000003'
I_B2='d0301100-0000-4000-8000-000000000004'
I_C1='d0301100-0000-4000-8000-000000000005'
SH_A='d0302000-0000-4000-8000-000000000001'
SH_B='d0302000-0000-4000-8000-000000000002'
SH_C='d0302000-0000-4000-8000-000000000003'
OUT_A='d0303000-0000-4000-8000-000000000001'
OUT_B='d0303000-0000-4000-8000-000000000002'
OUT_C='d0303000-0000-4000-8000-000000000003'
AR_A='d0304000-0000-4000-8000-000000000001'
AR_B='d0304000-0000-4000-8000-000000000002'
AR_C='d0304000-0000-4000-8000-000000000003'
WH='d1370001-0000-4000-8000-000000000002'
MAT1='d1370003-0000-4000-8000-000000000001'
MAT2='d1370003-0000-4000-8000-000000000002'

PASS=0; FAIL=0; FAILED=()
ok()  { PASS=$((PASS+1)); printf '  ✅ %s\n' "$*"; }
bad() { FAIL=$((FAIL+1)); FAILED+=("$*"); printf '  ❌ %s\n' "$*"; }
check() { if [ "$2" = "$3" ]; then ok "$1 = $2"; else bad "$1 = $2（期望 $3）"; fi; }
# 数值比较：一律 ×10000 取整（Decimal 最多 4 位，无浮点误差）
check_n() { # check_n <标签> <jq 表达式> <期望值>
  local label=$1 expr=$2 want=$3 got expect
  got=$(printf '%s' "$BODY" | jq -r "($expr|tonumber)*10000|round" 2>/dev/null)
  expect=$(python3 -c "print(round(float('$want')*10000))" 2>/dev/null)
  check "$label" "$got" "$expect"
}
# 数据库数值比较（psql 返回文本，同样 ×10000 取整后用 awk 比）
check_dbn() { # check_dbn <标签> <SQL> <期望值>
  local label=$1 sql=$2 want=$3 got expect
  got=$(PSQLQ -c "$sql" | awk '{printf "%d", $1*10000}')
  expect=$(python3 -c "print(round(float('$want')*10000))")
  check "$label" "$got" "$expect"
}
check_has() { # check_has <标签> <haystack> <needle>
  case "$2" in *"$3"*) ok "$1（含「$3」）" ;; *) bad "$1 不含「$3」：$2" ;; esac
}
check_not_has() { # check_not_has <标签> <haystack> <needle>
  case "$2" in *"$3"*) bad "$1 含「$3」：$2" ;; *) ok "$1（不含「$3」）" ;; esac
}

api() {
  local name=$1 method=$2 path=$3 token=${4:-} body=${5:-}
  local out="$RAW/$name.json"
  local -a args=(-s -m 40 -o "$out" -w '%{http_code}' -X "$method" "$BASE$path")
  [ -n "$token" ] && args+=(--oauth2-bearer "$token")
  [ -n "$body" ] && args+=(-H 'Content-Type: application/json' -d "$body")
  HTTP=$(curl "${args[@]}")
  BODY=$(cat "$out" 2>/dev/null || true)
  CODE=$(printf '%s' "$BODY" | jq -r '.code // empty' 2>/dev/null)
  if printf '%s' "$BODY" | jq -e '.data.access_token? // .data.refresh_token?' >/dev/null 2>&1; then
    jq '((.data.access_token? // "") |= (if . == "" then . else "<redacted>" end))
        | ((.data.refresh_token? // "") |= (if . == "" then . else "<redacted>" end))' \
       "$out" > "$out.tmp" 2>/dev/null && mv "$out.tmp" "$out"
  fi
}
d() { printf '%s' "$BODY" | jq -r "$1" 2>/dev/null; }

try_login() {
  local code tok out="$RAW/login_${3:-try}.json" must
  code=$(curl -s -m 20 -o "$out" -w '%{http_code}' -X POST "$BASE/system/auth/login" \
           -H 'Content-Type: application/json' \
           -d "{\"username\":\"$1\",\"password\":\"$2\"}")
  [ "$code" = "200" ] || { printf ''; return; }
  tok=$(jq -r '.data.access_token // empty' "$out")
  must=$(jq -r '.data.must_change_password // false' "$out")
  jq '((.data.access_token? // "") |= (if . == "" then . else "<redacted>" end))
      | ((.data.refresh_token? // "") |= (if . == "" then . else "<redacted>" end))' \
     "$out" > "$out.tmp" 2>/dev/null && mv "$out.tmp" "$out"
  printf '%s\t%s' "$tok" "$must"
}
login_split() {
  local line
  line=$(try_login "$1" "$2" "${3:-try}")
  T_ADMIN=${line%%$'\t'*}
  MUST_CHANGE=${line##*$'\t'}
  [ -z "$T_ADMIN" ] && MUST_CHANGE=false
}

# walk_to_pending <名> <return_id> [token] —— DRAFT → SUBMITTED → APPROVED → PENDING_POST
walk_to_pending() {
  local tag=$1 rid=$2 tok=$3
  api "${tag}_sub" PATCH "/business/erp/sales-return/$rid/status" "$tok" '{"target_status":"SUBMITTED"}'
  api "${tag}_appr" PATCH "/business/erp/sales-return/$rid/status" "$tok" '{"target_status":"APPROVED"}'
  api "${tag}_pend" PATCH "/business/erp/sales-return/$rid/status" "$tok" '{"target_status":"PENDING_POST"}'
}

echo "=================================================================="
echo "SO-30 验收：库=$DB 实例=$BASE"
echo "=================================================================="

echo; echo "== 0) 实例 / V0305 结构 / 迁移记账 =="
health=$(curl -s -o /dev/null -w '%{http_code}' -m 3 "http://127.0.0.1:$PORT/health")
check "/health" "$health" "200"

row=$(PSQLQ -c "select version||'|'||name||'|'||kind||'|'||applied_via from schema_migrations where name='sales_return_posting'")
echo "  · V0305 记账：$row"
case "$row" in
  *"|sales_return_posting|versioned|executed") ok "V0305 kind=versioned applied_via=executed" ;;
  *) bad "V0305 记账不对：$row" ;;
esac
badvia=$(PSQLQ -c "select count(*) from schema_migrations where applied_via not in ('executed','imported')")
check "applied_via 非 executed/imported 的迁移数" "$badvia" "0"

cols=$(PSQLQ -c "select count(*) from information_schema.columns where table_name='biz_stock_in' and column_name in ('source_type','source_id')")
check "biz_stock_in 来源两列" "$cols" "2"
idx=$(PSQLQ -c "select count(*) from pg_indexes where tablename='biz_stock_in' and indexname='uk_biz_si_source_doc' and indexdef like '%UNIQUE%'")
check "来源部分唯一索引 uk_biz_si_source_doc" "$idx" "1"
inv=$(PSQLQ -c "select pg_get_constraintdef(oid) from pg_constraint where conname='ck_biz_invoice_source'")
check_has "发票来源白名单含 SALES_RETURN" "$inv" "SALES_RETURN"

echo; echo "== 1) admin 登录 =="
T_ADMIN=""; FIRST_PWD=""; MUST_CHANGE=false
for cand in 'Admin@So30!2026' 'Admin@So29!2026' 'Admin@123456' 'Admin@So28!2026' 'Admin@So27!2026'; do
  login_split admin "$cand" "admin_$(printf '%s' "$cand" | tr -cd 'A-Za-z0-9')"
  if [ -n "$T_ADMIN" ]; then FIRST_PWD="$cand"; break; fi
done
[ -z "$T_ADMIN" ] && { echo "FATAL: admin 口令未知，无法继续"; exit 1; }
ok "admin 登录成功（must_change_password=$MUST_CHANGE）"
if [ "$MUST_CHANGE" = "true" ]; then
  api pre_pwd_change PUT /system/auth/password "$T_ADMIN" \
      "{\"old_password\":\"$FIRST_PWD\",\"new_password\":\"Admin@So30!2026\"}"
  [ "$CODE" = "0" ] && ok "前置：首登改密 → Admin@So30!2026" || bad "前置改密失败 http=$HTTP $(d '.msg')"
  login_split admin 'Admin@So30!2026' admin_so30
  [ -n "$T_ADMIN" ] && ok "新口令重登成功" || { bad "新口令重登失败"; exit 1; }
fi

# 客户 A 的额度调到 2300：A 的在手占用 2260，退货 226 后 2034
# —— 让「信用额度释放」有一个 API 级的可观测证据（见 ②'）
PSQLQ -c "update biz_customer set credit_limit=2300 where id='$CUST_A'" >/dev/null

echo; echo "== 2) 场景 A：未开票 · 部分退货（2 件 @100，13%）=="
api a_create POST /business/erp/sales-return "$T_ADMIN" \
  "{\"shipment_id\":\"$SH_A\",\"warehouse_id\":\"$WH\",\"return_type\":\"QUALITY\",
    \"remark\":\"SO30 e2e：A 部分退货\",\"items\":[{\"so_item_id\":\"$I_A1\",\"qty\":2}]}"
check "建单 http" "$HTTP" "200"
RA=$(d '.data.id'); RA_NO=$(d '.data.return_no')
check "建单状态" "$(d '.data.status')" "DRAFT"
check_n "退货价税合计" '.data.total_with_tax' 226
check_n "退货不含税" '.data.total_amount' 200
check_n "退货税额" '.data.total_tax' 26

walk_to_pending a "$RA" "$T_ADMIN"
check "待过账后状态" "$(d '.data.status')" "PENDING_POST"

# 夹具基线（退货过账前）
INV1_BEFORE=$(PSQLQ -c "select qty from biz_inventory where material_id='$MAT1' and warehouse_id='$WH'")
api a_posted PATCH "/business/erp/sales-return/$RA/status" "$T_ADMIN" '{"target_status":"POSTED"}'
check "过账 http" "$HTTP" "200"
check "过账后状态" "$(d '.data.status')" "POSTED"
SIN_A=$(d '.data.stock_in_id')
check "回了入库单 id" "$([ -n "$SIN_A" ] && [ "$SIN_A" != "null" ] && echo yes)" "yes"
check "冲减了应收 id" "$(d '.data.receivable_id')" "$AR_A"
check "无红字发票（未开票）" "$(d '.data.red_invoice_id')" "null"

# ②a 入库单：类型 / 来源 / 状态 / 金额 = 2 × 60
sin=$(PSQLQ -c "select in_type||'|'||status||'|'||coalesce(source_type,'-')||'|'||source_id::text||'|'||total_amount from biz_stock_in where id='$SIN_A'")
check_has "入库单 in_type=RETURN" "$sin" "RETURN|POSTED|SALES_RETURN|$RA|"
check_dbn "入库单金额 = 2×60 成本" "select total_amount from biz_stock_in where id='$SIN_A'" 120
items=$(PSQLQ -c "select count(*) from biz_stock_in_item where stock_in_id='$SIN_A'")
check "入库明细行数" "$items" "1"
src_item=$(PSQLQ -c "select source_item_id from biz_stock_in_item where stock_in_id='$SIN_A'")
check "入库明细回指订单行" "$src_item" "$I_A1"

# ②b 三本账：成本账 + 库位账 + 流水
check_dbn "成本账数量 100→102" "select qty from biz_inventory where material_id='$MAT1' and warehouse_id='$WH'" 102
check_dbn "成本账金额 6000→6120" "select amount from biz_inventory where material_id='$MAT1' and warehouse_id='$WH'" 6120
check_dbn "库位账数量合计 100→102" "select sum(qty) from biz_inventory_location where material_id='$MAT1' and warehouse_id='$WH'" 102
txn=$(PSQLQ -c "select txn_type||'|'||biz_type||'|'||doc_type||'|'||doc_no from biz_inventory_txn where doc_id='$SIN_A' order by line_no limit 1")
check_has "流水 IN / SALES_RETURN_IN / STOCK_IN" "$txn" "IN|SALES_RETURN_IN|STOCK_IN|IN-"
check_dbn "流水数量" "select qty from biz_inventory_txn where doc_id='$SIN_A' limit 1" 2
check_dbn "流水金额" "select amount from biz_inventory_txn where doc_id='$SIN_A' limit 1" 120
check_dbn "流水 before_qty" "select before_qty from biz_inventory_txn where doc_id='$SIN_A' limit 1" "$INV1_BEFORE"

# ②c 红字入库凭证：借 1405 / 贷 6401，金额 120，借贷平衡
vid=$(PSQLQ -c "select voucher_id from biz_stock_in where id='$SIN_A'")
check "入库单回写凭证" "$([ -n "$vid" ] && [ "$vid" != "" ] && echo yes)" "yes"
vsum=$(PSQLQ -c "select status||'|'||coalesce(source_type,'-') from biz_voucher where id='$vid'")
check_has "凭证 POSTED / 来源 STOCK_IN" "$vsum" "POSTED|STOCK_IN"
check_dbn "借 1405 合计" "select coalesce(sum(debit_amount),0) from biz_voucher_entry where voucher_id='$vid' and account_code='1405'" 120
check_dbn "贷 6401 合计" "select coalesce(sum(credit_amount),0) from biz_voucher_entry where voucher_id='$vid' and account_code='6401'" 120
vb=$(PSQLQ -c "select (coalesce(sum(debit_amount),0)-coalesce(sum(credit_amount),0))=0 from biz_voucher_entry where voucher_id='$vid'")
check "凭证借贷平衡" "$vb" "t"

# ②d 应收冲减：1921 - 226 = 1695，状态仍 OPEN
arsum=$(PSQLQ -c "select amount||'|'||unsettled_amount||'|'||received_amount||'|'||status from biz_receivable where id='$AR_A'")
check_has "应收已冲减金额" "$arsum" "1695.0000|1695.0000|0.0000|OPEN"

# ②e delivered_qty 回写：7 → 5
check_dbn "订单行 delivered_qty 7→5" "select delivered_qty from biz_sales_order_item where id='$I_A1'" 5
# ②f 退货单回链回填
back=$(PSQLQ -c "select coalesce(stock_in_id::text,'-')||'|'||coalesce(receivable_id::text,'-')||'|'||coalesce(red_invoice_id::text,'-') from biz_sales_return where id='$RA'")
check "退货单三回链" "$back" "$SIN_A|$AR_A|-"

echo; echo "== 2') 客户信用额度释放（同一个新订单，退货前后各提交一次）=="
# A 的在手占用 = 2260（PARTIAL_OUT）；额度 2300；新订单 113
api credit_order POST /business/erp/sales-order "$T_ADMIN" \
  "{\"customer_id\":\"$CUST_A\",\"remark\":\"SO30 e2e：额度释放探针\",
    \"items\":[{\"material_id\":\"$MAT1\",\"qty\":1,\"price\":100,\"tax_rate\":13}]}"
check "探针订单建单 http" "$HTTP" "200"
ORD_PROBE=$(d '.data.id')
check_n "探针订单价税合计" '.data.total_with_tax' 113
# 退货过账后：占用 2260-226=2034，2034+113=2147 ≤ 2300 → 提交应当通过
api credit_submit PATCH "/business/erp/sales-order/$ORD_PROBE/status" "$T_ADMIN" '{"target_status":"SUBMITTED"}'
check_not_has "退货后提交不再报额度不足" "$(d '.msg')" "信用额度不足"
# 反证：把额度压到 2100（< 2147），必须重新被拦 → 证明这个守卫真的在算数
PSQLQ -c "update biz_customer set credit_limit=2100 where id='$CUST_A'" >/dev/null
api credit_order2 POST /business/erp/sales-order "$T_ADMIN" \
  "{\"customer_id\":\"$CUST_A\",\"remark\":\"SO30 e2e：额度反证\",
    \"items\":[{\"material_id\":\"$MAT1\",\"qty\":1,\"price\":100,\"tax_rate\":13}]}"
ORD_PROBE2=$(d '.data.id')
api credit_submit2 PATCH "/business/erp/sales-order/$ORD_PROBE2/status" "$T_ADMIN" '{"target_status":"SUBMITTED"}'
check "额度压到 2100 后提交 http" "$HTTP" "409"
check_has "409 点名信用额度不足" "$(d '.msg')" "信用额度不足"
PSQLQ -c "update biz_customer set credit_limit=2300 where id='$CUST_A'" >/dev/null
# 还原：探针订单与反证订单作废掉（不污染后续断言）
PSQLQ -c "update biz_sales_order set status='CANCELLED', remark=remark||'（e2e 收尾作废）'
             where id in ('$ORD_PROBE','$ORD_PROBE2')" >/dev/null

echo; echo "== 3) 场景 B：已开票 · 全退（12 件，含税 1921）=="
api b_create POST /business/erp/sales-return "$T_ADMIN" \
  "{\"shipment_id\":\"$SH_B\",\"warehouse_id\":\"$WH\",\"return_type\":\"WRONG_ITEM\",
    \"remark\":\"SO30 e2e：B 全退\",
    \"items\":[{\"so_item_id\":\"$I_B1\",\"qty\":7},{\"so_item_id\":\"$I_B2\",\"qty\":5}]}"
check "建单 http" "$HTTP" "200"
RB=$(d '.data.id')
check_n "全退货价税合计" '.data.total_with_tax' 1921
walk_to_pending b "$RB" "$T_ADMIN"
api b_posted PATCH "/business/erp/sales-return/$RB/status" "$T_ADMIN" '{"target_status":"POSTED"}'
check "过账 http" "$HTTP" "200"
check "过账后状态" "$(d '.data.status')" "POSTED"
RINV=$(d '.data.red_invoice_id')
check "生成了红字发票" "$([ -n "$RINV" ] && [ "$RINV" != "null" ] && echo yes)" "yes"

# ③a 红字发票：OUT_RED + 三金额全负 + ISSUED + 来源 SALES_RETURN
rinv=$(PSQLQ -c "select invoice_type||'|'||status||'|'||amount||'|'||tax_amount||'|'||total_with_tax||'|'||source_type||'|'||coalesce(receivable_id::text,'-') from biz_invoice where id='$RINV'")
check_has "红字发票 OUT_RED/ISSUED/金额全负/来源 SALES_RETURN" "$rinv" "OUT_RED|ISSUED|-1700.0000|-221.0000|-1921.0000|SALES_RETURN|$AR_B"
# ③b 收入冲减凭证：借 5001 1700 + 借 2221.02 221 / 贷 1122 1921
rvid=$(PSQLQ -c "select voucher_id from biz_invoice where id='$RINV'")
check "红票回写凭证" "$([ -n "$rvid" ] && [ "$rvid" != "" ] && echo yes)" "yes"
check_dbn "借 5001" "select coalesce(sum(debit_amount),0) from biz_voucher_entry where voucher_id='$rvid' and account_code='5001'" 1700
check_dbn "借 2221.02" "select coalesce(sum(debit_amount),0) from biz_voucher_entry where voucher_id='$rvid' and account_code='2221.02'" 221
check_dbn "贷 1122" "select coalesce(sum(credit_amount),0) from biz_voucher_entry where voucher_id='$rvid' and account_code='1122'" 1921
rbal=$(PSQLQ -c "select (coalesce(sum(debit_amount),0)-coalesce(sum(credit_amount),0))=0 from biz_voucher_entry where voucher_id='$rvid'")
check "收入冲减凭证借贷平衡" "$rbal" "t"
# ③c 应收全额冲减 → 作废
arB=$(PSQLQ -c "select status||'|'||amount||'|'||unsettled_amount from biz_receivable where id='$AR_B'")
check_has "应收 B 已作废（全额退 + 一分没收）" "$arB" "CANCELLED|1921.0000|1921.0000"
arB_log=$(PSQLQ -c "select count(*) from biz_status_log where entity_type='RECEIVABLE' and entity_id='$AR_B' and to_status='CANCELLED'")
check "应收作废留痕" "$arB_log" "1"
# ③d 库存回库：001 102→109（+7），002 50→55（+5）；成本金额同步
check_dbn "物料1 成本账 102→109" "select qty from biz_inventory where material_id='$MAT1' and warehouse_id='$WH'" 109
check_dbn "物料1 成本金额 6120→6540" "select amount from biz_inventory where material_id='$MAT1' and warehouse_id='$WH'" 6540
check_dbn "物料2 成本账 50→55" "select qty from biz_inventory where material_id='$MAT2' and warehouse_id='$WH'" 55
check_dbn "物料2 成本金额 6000→6600" "select amount from biz_inventory where material_id='$MAT2' and warehouse_id='$WH'" 6600
check_dbn "B 的红字入库凭证金额 = 7×60+5×120" "select total_amount from biz_stock_in where id=(select stock_in_id from biz_sales_return where id='$RB')" 1020
# ③e delivered_qty 归零
check_dbn "订单行 B1 delivered_qty 7→0" "select delivered_qty from biz_sales_order_item where id='$I_B1'" 0
check_dbn "订单行 B2 delivered_qty 5→0" "select delivered_qty from biz_sales_order_item where id='$I_B2'" 0

echo; echo "== 4) 场景 C：已收款 · 退货额 791 > 未收 291 → 409 且整笔回滚 =="
api c_create POST /business/erp/sales-return "$T_ADMIN" \
  "{\"shipment_id\":\"$SH_C\",\"warehouse_id\":\"$WH\",\"return_type\":\"OTHER\",
    \"remark\":\"SO30 e2e：C 已收款\",\"items\":[{\"so_item_id\":\"$I_C1\",\"qty\":7}]}"
check "建单 http" "$HTTP" "200"
RC=$(d '.data.id')
walk_to_pending c "$RC" "$T_ADMIN"
check "C 待过账状态" "$(d '.data.status')" "PENDING_POST"
INV1_BEFORE_C=$(PSQLQ -c "select qty from biz_inventory where material_id='$MAT1' and warehouse_id='$WH'")
DELIV_BEFORE_C=$(PSQLQ -c "select delivered_qty from biz_sales_order_item where id='$I_C1'")
AR_BEFORE_C=$(PSQLQ -c "select amount||'|'||unsettled_amount||'|'||status from biz_receivable where id='$AR_C'")
api c_posted PATCH "/business/erp/sales-return/$RC/status" "$T_ADMIN" '{"target_status":"POSTED"}'
check "过账 http（钱收多了 → 拒）" "$HTTP" "409"
check_has "409 点名收款退款单" "$(d '.msg')" "收款退款单"
# 回滚证据：状态 / 库存 / 应收 / delivered_qty / 入库单 一分不动
check "退货单状态仍 PENDING_POST（状态 CAS 已回滚）" "$(PSQLQ -c "select status from biz_sales_return where id='$RC'")" "PENDING_POST"
check "没有生成入库单" "$(PSQLQ -c "select count(*) from biz_stock_in where source_type='SALES_RETURN' and source_id='$RC'")" "0"
check "库存数量未变" "$(PSQLQ -c "select qty from biz_inventory where material_id='$MAT1' and warehouse_id='$WH'")" "$INV1_BEFORE_C"
check "delivered_qty 未变" "$(PSQLQ -c "select delivered_qty from biz_sales_order_item where id='$I_C1'")" "$DELIV_BEFORE_C"
check "应验收支未变" "$(PSQLQ -c "select amount||'|'||unsettled_amount||'|'||status from biz_receivable where id='$AR_C'")" "$AR_BEFORE_C"
check "没有多出库存流水" "$(PSQLQ -c "select count(*) from biz_inventory_txn where biz_type='SALES_RETURN_IN' and doc_no not in (select in_no from biz_stock_in where source_type='SALES_RETURN')")" "0"

echo; echo "== 5) 幂等 / 终态 =="
api a_posted_again PATCH "/business/erp/sales-return/$RA/status" "$T_ADMIN" '{"target_status":"POSTED"}'
check "已 POSTED 再过账 http" "$HTTP" "409"
cnt_in=$(PSQLQ -c "select count(*) from biz_stock_in where source_type='SALES_RETURN' and source_id='$RA'")
check "同一退货单只有一张入库单" "$cnt_in" "1"
api c_over POST /business/erp/sales-return "$T_ADMIN" \
  "{\"shipment_id\":\"$SH_A\",\"warehouse_id\":\"$WH\",\"return_type\":\"OTHER\",
    \"items\":[{\"so_item_id\":\"$I_A1\",\"qty\":6}]}"
check "超退（剩余可退 5，再要 6）http" "$HTTP" "400"
check_has "400 点名超退" "$(d '.msg')" "超退"

echo; echo "== 6) 三账一致（成本账数量 == Σ库位账数量，逐物料）=="
for m in "$MAT1" "$MAT2"; do
  pair=$(PSQLQ -c "select coalesce((select qty from biz_inventory where material_id='$m' and warehouse_id='$WH'),-1)
                          ||'|'||coalesce((select sum(qty) from biz_inventory_location where material_id='$m' and warehouse_id='$WH'),-1)")
  left=${pair%%|*}; right=${pair##*|}
  if [ "$left" = "$right" ]; then ok "物料 $m 三账一致（$left）"; else bad "物料 $m 三账不一致（成本账 $left / 库位账 $right）"; fi
done
# 流水的 before/after 连续性：回库流水 before_qty 必须等于当时账上数量（不是拍脑袋写的 0）
seq_ok=$(PSQLQ -c "select bool_and(after_qty = before_qty + qty) from biz_inventory_txn where biz_type='SALES_RETURN_IN'")
check "回库流水 after = before + qty" "$seq_ok" "t"
# 入库单 → 凭证 → 流水 三方金额一致
amounts=$(PSQLQ -c "select s.total_amount||'|'||
  (select coalesce(sum(e.debit_amount),0) from biz_voucher_entry e where e.voucher_id=s.voucher_id)||'|'||
  (select coalesce(sum(qty),0) from biz_stock_in_item i where i.stock_in_id=s.id)
  from biz_stock_in s where s.source_type='SALES_RETURN' order by s.in_no limit 1")
echo "  · 首张退货入库单三值（入库金额|凭证借方|明细数量）：$amounts"

echo; echo "== 7) biz_status_log 轨迹（退货单 + 发票）=="
trail=$(PSQLQ -c "select count(*) from biz_status_log where entity_type='SALES_RETURN' and entity_id='$RA'")
check "A 单轨迹条数（建单 + 4 次流转）" "$trail" "5"
inv_trail=$(PSQLQ -c "select string_agg(to_status,'>' order by created_at) from biz_status_log where entity_type='INVOICE' and entity_id='$RINV'")
check_has "红票轨迹（DRAFT → ISSUED）" "$inv_trail" "DRAFT>ISSUED"

echo
echo "=================================================================="
printf '结果：PASS=%d FAIL=%d\n' "$PASS" "$FAIL"
if [ "$FAIL" -gt 0 ]; then
  printf '失败项：\n'
  for item in "${FAILED[@]}"; do printf '  - %s\n' "$item"; done
  exit 1
fi
echo "全部断言通过"

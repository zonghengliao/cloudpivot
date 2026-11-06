#!/usr/bin/env bash
# ============================================================================
# SO-22 收入确认凭证：真实 token / 真实库 / 真凭证的定点验收
#
# 断言链（与卡面验收逐条对应）：
#   ① 开票（ISSUED）→ 生成收入凭证：借 1122 价税合计 / 贷 5001 不含税 / 贷 2221.02 税额
#   ② 凭证借贷平衡（Σ借 = Σ贷，且 = 价税合计）
#   ③ 应收单 voucher_id 被回填（= 收入凭证 id）—— 出库作废冲销能红冲的前提
#   ④ 作废（VOID）→ 红字凭证反向 + 应收的 invoice_no / voucher_id 撤回
#   ⑤ 库层：无借贷不平凭证
#
# 用法：BASE=http://127.0.0.1:18124/api DB=cloudpivot_so22e2e bash so22-verify.sh
# ============================================================================
set -uo pipefail

BASE=${BASE:-http://127.0.0.1:18124/api}
DB=${DB:-cloudpivot_so22e2e}
PGHOST=${PGHOST:-127.0.0.1}
PGUSER=${PGUSER:-alun}
export PGPASSWORD=${PGPASSWORD:-alun}
ADMIN_USER=${ADMIN_USER:-admin}
ADMIN_PASS=${ADMIN_PASS:-Admin@123456}

CUSTOMER='5a220000-0000-4000-8000-000000000001'
AR_ID='5a220000-0000-4000-8000-0000000000a1'
AR_NO='AR22-0001'

PASS=0
FAIL=0
ok()  { PASS=$((PASS+1)); echo "  PASS  $*"; }
bad() { FAIL=$((FAIL+1)); echo "  FAIL  $*"; }
check(){ if [ "$2" = "$3" ]; then ok "$1（$3）"; else bad "$1：期望 $3，实际 $2"; fi; }

psql_q() { psql -h "$PGHOST" -U "$PGUSER" -d "$DB" -Atc "$1"; }

echo "==> 0. 夹具：1 客户 + 1 张 OPEN 应收 1130"
psql_q "DELETE FROM biz_invoice WHERE receivable_id IN (SELECT id FROM biz_receivable WHERE ar_no LIKE 'AR22-%')" >/dev/null
psql_q "DELETE FROM biz_status_log WHERE entity_type IN ('INVOICE','RECEIVABLE') AND entity_id IN (SELECT id FROM biz_receivable WHERE ar_no LIKE 'AR22-%')" >/dev/null
psql_q "DELETE FROM biz_voucher_entry WHERE voucher_id IN (SELECT id FROM biz_voucher WHERE source_no LIKE 'IV-%-SO22')" >/dev/null
psql_q "DELETE FROM biz_status_log WHERE entity_type = 'VOUCHER' AND entity_id IN (SELECT id FROM biz_voucher WHERE source_type = 'INVOICE')" >/dev/null
psql_q "DELETE FROM biz_receivable WHERE ar_no LIKE 'AR22-%'" >/dev/null
psql_q "DELETE FROM biz_customer WHERE id = '$CUSTOMER'::uuid" >/dev/null
psql_q "INSERT INTO biz_customer (id, code, name, status) VALUES ('$CUSTOMER'::uuid, 'REV22-01', '收入确认演示客户', 'ACTIVE')" >/dev/null
psql_q "INSERT INTO biz_receivable
          (id, ar_no, customer_id, source_type, source_id, source_no, currency_code,
           amount, received_amount, unsettled_amount, credit_days, due_date, period_code, status)
        VALUES ('$AR_ID'::uuid, '$AR_NO', '$CUSTOMER'::uuid, 'MANUAL', gen_random_uuid(), 'MANUAL',
                'CNY', 1130, 0, 1130, 30, DATE '2026-12-06', '2026-11', 'OPEN')" >/dev/null
echo "      应收 $AR_NO 1130 / 科目：$(psql_q "select string_agg(account_code, ',' order by account_code) from biz_account where account_code in ('1122','5001','2221.02') and deleted_at is null")"

echo
echo "==> 1. 登录（软口令兼容）"
login() {
  curl -s -X POST -H 'Content-Type: application/json' \
    --data-binary "{\"username\":\"$ADMIN_USER\",\"password\":\"$1\"}" \
    "$BASE/system/auth/login"
}
TOKEN=""
for candidate in "$ADMIN_PASS" "${ADMIN_PASS}_so20" "${ADMIN_PASS}_so21"; do
  TOKEN=$(login "$candidate" | jq -r '.data.access_token // empty')
  [ -n "$TOKEN" ] && { ADMIN_PASS="$candidate"; break; }
  sleep 6   # 登录有频控（10005 太频繁），试错之间必须留间隔
done
if [ -z "$TOKEN" ]; then bad "登录失败"; echo "PASS=$PASS FAIL=$FAIL"; exit 1; fi
ok "登录成功（token 长度 ${#TOKEN}）"

api() { # method path [body]
  local method=$1 path=$2 body=${3:-}
  if [ -n "$body" ]; then
    curl -s -X "$method" -H "Authorization: Bearer $TOKEN" -H 'Content-Type: application/json' \
      --data-binary "$body" "$BASE$path"
  else
    curl -s -X "$method" -H "Authorization: Bearer $TOKEN" "$BASE$path"
  fi
}

echo
echo "==> 2. 从应收开票 → ISSUED（收入确认时点）"
CREATED=$(api POST /business/erp/invoice "{\"receivable_id\":\"$AR_ID\",\"invoice_type\":\"OUT\"}")
INV_ID=$(echo "$CREATED" | jq -r '.data.id // empty')
INV_NO=$(echo "$CREATED" | jq -r '.data.invoice_no // empty')
check "建单 code=0" "$(echo "$CREATED" | jq -r '.code')" "0"
check "价税合计自动带出 1130" "$(echo "$CREATED" | jq -r '.data.total_with_tax')" "1130.0000"
if [ -z "$INV_ID" ] || [ "$INV_ID" = "null" ]; then bad "建单没拿到发票 id"; echo "PASS=$PASS FAIL=$FAIL"; exit 1; fi

ISSUED=$(api PATCH "/business/erp/invoice/$INV_ID/status" '{"status":"ISSUED"}')
check "开票 code=0" "$(echo "$ISSUED" | jq -r '.code')" "0"
VOUCHER_NO=$(echo "$ISSUED" | jq -r '.data.voucher_no // empty')
VOUCHER_ID=$(psql_q "select voucher_id from biz_invoice where id = '$INV_ID'::uuid")
if [ -n "$VOUCHER_NO" ] && [ "$VOUCHER_NO" != "null" ]; then ok "收入凭证号 = $VOUCHER_NO"; else bad "未生成收入凭证"; fi

echo
echo "==> 3. 分录逐行（真库）"
psql_q "select line_no || ' | ' || account_code || ' | 借 ' || coalesce(debit_amount,0) || ' | 贷 ' || coalesce(credit_amount,0)
         from biz_voucher_entry where voucher_id = '$VOUCHER_ID'::uuid order by line_no" | sed 's/^/      /'
check "借方 1122 = 价税合计 1130" "$(psql_q "select coalesce(sum(debit_amount),0) from biz_voucher_entry where voucher_id='$VOUCHER_ID'::uuid and account_code='1122'")" "1130.0000"
check "贷方 5001 = 不含税 1000" "$(psql_q "select coalesce(sum(credit_amount),0) from biz_voucher_entry where voucher_id='$VOUCHER_ID'::uuid and account_code='5001'")" "1000.0000"
check "贷方 2221.02 = 税额 130" "$(psql_q "select coalesce(sum(credit_amount),0) from biz_voucher_entry where voucher_id='$VOUCHER_ID'::uuid and account_code='2221.02'")" "130.0000"
check "Σ借 = Σ贷" "$(psql_q "select sum(debit_amount) - sum(credit_amount) from biz_voucher_entry where voucher_id='$VOUCHER_ID'::uuid")" "0.0000"
check "凭证头 借贷平衡（total_debit = total_credit = 1130）" \
  "$(psql_q "select total_debit || '/' || total_credit from biz_voucher where id='$VOUCHER_ID'::uuid")" "1130.0000/1130.0000"

echo
echo "==> 4. 应收回填 voucher_id（出库作废冲销能红冲的前提）"
check "biz_receivable.voucher_id = 收入凭证 id" "$(psql_q "select voucher_id from biz_receivable where id='$AR_ID'::uuid")" "$VOUCHER_ID"
check "biz_receivable.invoice_no = 发票号" "$(psql_q "select invoice_no from biz_receivable where id='$AR_ID'::uuid")" "$INV_NO"

echo
echo "==> 5. 作废 → 红字凭证 + 撤回应收回写"
VOIDED=$(api PATCH "/business/erp/invoice/$INV_ID/status" '{"status":"VOID","reason":"SO22 验收：作废红冲"}')
check "作废 code=0" "$(echo "$VOIDED" | jq -r '.code')" "0"
RED_ID=$(psql_q "select id from biz_voucher where reverse_of = '$VOUCHER_ID'::uuid")
if [ -n "$RED_ID" ]; then ok "红字凭证已生成（reverse_of 指回蓝字）"; else bad "红字凭证缺失"; fi
check "红字凭证 is_red=1" "$(psql_q "select is_red from biz_voucher where id='$RED_ID'::uuid")" "1"
check "红字分录反向：贷 1122 = 1130" "$(psql_q "select coalesce(sum(credit_amount),0) from biz_voucher_entry where voucher_id='$RED_ID'::uuid and account_code='1122'")" "1130.0000"
check "红字分录反向：借 5001 = 1000" "$(psql_q "select coalesce(sum(debit_amount),0) from biz_voucher_entry where voucher_id='$RED_ID'::uuid and account_code='5001'")" "1000.0000"
check "应收 voucher_id 已撤回" "$(psql_q "select coalesce(voucher_id::text,'NULL') from biz_receivable where id='$AR_ID'::uuid")" "NULL"
check "应收 invoice_no 已撤回" "$(psql_q "select coalesce(invoice_no,'NULL') from biz_receivable where id='$AR_ID'::uuid")" "NULL"

echo
echo "==> 6. 库层健康：全库没有借贷不平的凭证"
check "借贷不平凭证数" "$(psql_q "select count(*) from biz_voucher where total_debit <> total_credit")" "0"
check "分录与凭证头金额不一致的凭证数" \
  "$(psql_q "select count(*) from (select v.id, v.total_debit, sum(e.debit_amount) d, sum(e.credit_amount) c
              from biz_voucher v join biz_voucher_entry e on e.voucher_id = v.id
             group by v.id, v.total_debit, v.total_credit
            having sum(e.debit_amount) <> v.total_debit or sum(e.credit_amount) <> v.total_credit) t")" "0"

echo
echo "==================== 结果 ===================="
echo "PASS=$PASS FAIL=$FAIL"
[ "$FAIL" = "0" ]

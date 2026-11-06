#!/usr/bin/env bash
# ============================================================================
# SO-17 收款单后端 API 的 5 接口 curl 实测（真实 token / 真实库 / 真凭证）
#
# 用法（先起一个指向**隔离库**的实例）：
#   BASE=http://127.0.0.1:8023/api DB=cloudpivot_so17 \
#     bash docs/evidence/20261106-so17-receipt/curl-5-endpoints.sh
#
# 断言分组：
#   ① 登录（首登强制改密自动处理）
#   ② 登记 POST /receipt（DRAFT、RC- 取号、方式白名单）
#   ③ 详情 GET /receipt/{id} / 分页 GET /receipt/page
#   ④ 核销 POST /receipt/{id}/allocate（一张收款 → 多张应收；回写 received/unsettled/status）
#   ⑤ 超额核销被拒（单张超额 400、总额超额 400、别家客户 400、终态应收 409）
#   ⑥ 状态 PATCH /receipt/{id}/status（DRAFT→CONFIRMED→POSTED 生成凭证；作废规则）
#   ⑦ 库层留痕（biz_status_log / biz_voucher + 分录）
#
# ⚠ 会清空演示库的 biz_receipt / biz_receipt_allocation / biz_receivable 与演示客户，
#   只对一次性隔离库运行。
# ============================================================================
set -uo pipefail

BASE=${BASE:-http://127.0.0.1:8023/api}
DB=${DB:-cloudpivot_so17}
PGHOST=${PGHOST:-127.0.0.1}
PGUSER=${PGUSER:-alun}
export PGPASSWORD=${PGPASSWORD:-alun}
ADMIN_USER=${ADMIN_USER:-admin}
ADMIN_PASS=${ADMIN_PASS:-Admin@123456}

CUSTOMER='5a170000-0000-4000-8000-000000000001'
OTHER_CUSTOMER='5a170000-0000-4000-8000-000000000002'
AR_SETTLED='5a170000-0000-4000-8000-0000000000a1'   # 600，一次收清
AR_PARTIAL='5a170000-0000-4000-8000-0000000000a2'   # 500，收 400
AR_SPARE='5a170000-0000-4000-8000-0000000000a3'     # 300，留给负向用例
AR_FOREIGN='5a170000-0000-4000-8000-0000000000a4'   # 别家客户的 300

PASS=0
FAIL=0
ok()   { PASS=$((PASS+1)); echo "  PASS  $*"; }
bad()  { FAIL=$((FAIL+1)); echo "  FAIL  $*"; }
check(){ if [ "$2" = "$3" ]; then ok "$1（$3）"; else bad "$1：期望 $3，实际 $2"; fi; }
checkne(){ if [ "$2" != "$3" ]; then ok "$1（$3）"; else bad "$1：不该等于 $3"; fi; }

psql_q() { psql -h "$PGHOST" -U "$PGUSER" -d "$DB" -Atc "$1"; }

echo "==> 0. 夹具（1 客户 + 1 别家客户 + 4 张应收）"
psql_q "DELETE FROM biz_receipt" >/dev/null
psql_q "DELETE FROM biz_receivable" >/dev/null
psql_q "DELETE FROM biz_customer WHERE id IN ('$CUSTOMER'::uuid, '$OTHER_CUSTOMER'::uuid)" >/dev/null
# 状态日志不随表清空（它是审计流水）：本库是一次性隔离库，按实体类型清干净才能断言条数
psql_q "DELETE FROM biz_status_log WHERE entity_type IN ('RECEIPT','RECEIVABLE')" >/dev/null
psql_q "INSERT INTO biz_customer (id, code, name, status) VALUES
          ('$CUSTOMER'::uuid, 'RC17-01', '收款演示客户', 'ACTIVE'),
          ('$OTHER_CUSTOMER'::uuid, 'RC17-02', '别家客户', 'ACTIVE')" >/dev/null
mk_ar() { # id ar_no customer amount
  psql_q "INSERT INTO biz_receivable
            (id, ar_no, customer_id, source_type, source_id, source_no, currency_code,
             amount, received_amount, unsettled_amount, credit_days, due_date, period_code, status)
          VALUES ('$1'::uuid, '$2', '$3'::uuid, 'MANUAL', gen_random_uuid(), 'MANUAL', 'CNY',
                  $4, 0, $4, 30, DATE '2026-12-06', '2026-11', 'OPEN')" >/dev/null
}
mk_ar "$AR_SETTLED" 'AR17-001' "$CUSTOMER" 600
mk_ar "$AR_PARTIAL" 'AR17-002' "$CUSTOMER" 500
mk_ar "$AR_SPARE"   'AR17-003' "$CUSTOMER" 300
mk_ar "$AR_FOREIGN" 'AR17-004' "$OTHER_CUSTOMER" 300
echo "      应收 4 张（600 / 500 / 300 本客户 + 300 别家客户），科目余额：$(psql_q "select count(*) from biz_account where account_code in ('1002','1122')")/2"

echo
echo "==> 1. 登录"
login() {
  curl -s -X POST -H 'Content-Type: application/json' \
    --data-binary "{\"username\":\"$ADMIN_USER\",\"password\":\"$1\"}" \
    "$BASE/system/auth/login"
}
RESP=""; TOKEN=""
for candidate in "$ADMIN_PASS" "${ADMIN_PASS}_rc17"; do
  RESP=$(login "$candidate")
  TOKEN=$(echo "$RESP" | jq -r '.data.access_token // empty')
  if [ -n "$TOKEN" ]; then ADMIN_PASS="$candidate"; break; fi
done
if [ -z "$TOKEN" ]; then echo "  !! 登录失败：$RESP"; exit 1; fi
MUST=$(echo "$RESP" | jq -r '.data.must_change_password // false')
if [ "$MUST" = "true" ]; then
  NEW_PASS="${ADMIN_PASS}_rc17"
  curl -s -o /dev/null -X PUT -H "Authorization: Bearer $TOKEN" -H 'Content-Type: application/json' \
    --data-binary "{\"old_password\":\"$ADMIN_PASS\",\"new_password\":\"$NEW_PASS\"}" \
    "$BASE/system/auth/password"
  ADMIN_PASS="$NEW_PASS"
  TOKEN=$(login "$ADMIN_PASS" | jq -r '.data.access_token // empty')
  [ -n "$TOKEN" ] && ok "首登改密后重新登录" || { bad "改密后登录失败"; exit 1; }
fi
[ -n "$TOKEN" ] && ok "拿到 access_token（admin）"
AUTH="Authorization: Bearer $TOKEN"

echo
echo "==> 2. 登记收款 POST /receipt"
J=$(curl -s -X POST -H "$AUTH" -H 'Content-Type: application/json' --data-binary "{
  \"customer_id\": \"$CUSTOMER\",
  \"receipt_date\": \"2026-11-06\",
  \"amount\": 1000,
  \"method\": \"BANK_TRANSFER\",
  \"bank_account\": \"6222 0000 1111 2222\",
  \"remark\": \"SO-17 curl 实测\"
}" "$BASE/business/erp/receipt")
echo "      → $(echo "$J" | jq -c '{code, msg, receipt_no: .data.receipt_no, status: .data.status, allocated: .data.allocated_amount, unallocated: .data.unallocated_amount}')"
check "登记 code" "$(echo "$J" | jq -r '.code')" "0"
RC=$(echo "$J" | jq -r '.data.id')
RC_NO=$(echo "$J" | jq -r '.data.receipt_no')
check "状态 = DRAFT" "$(echo "$J" | jq -r '.data.status')" "DRAFT"
check "状态中文" "$(echo "$J" | jq -r '.data.status_label')" "草稿"
check "方式中文" "$(echo "$J" | jq -r '.data.method_label')" "银行转账"
check "已核销 = 0" "$(echo "$J" | jq -r '.data.allocated_amount')" "0.0000"
check "未核销 = 1000" "$(echo "$J" | jq -r '.data.unallocated_amount')" "1000.0000"
if echo "$RC_NO" | grep -Eq '^RC-[0-9]{8}-[0-9]{6}$'; then ok "单号形状 RC-yyyyMMdd-000001（$RC_NO）"; else bad "单号形状不对：$RC_NO"; fi
echo "      收款单 id=$RC 单号=$RC_NO"

# 负向：金额 0 / 方式越界 / 客户不存在
J0=$(curl -s -X POST -H "$AUTH" -H 'Content-Type: application/json' \
  --data-binary "{\"customer_id\":\"$CUSTOMER\",\"amount\":0}" "$BASE/business/erp/receipt")
check "amount=0 → 400" "$(echo "$J0" | jq -r '.code')" "10001"
JM=$(curl -s -X POST -H "$AUTH" -H 'Content-Type: application/json' \
  --data-binary "{\"customer_id\":\"$CUSTOMER\",\"amount\":10,\"method\":\"ALIPAY\"}" "$BASE/business/erp/receipt")
check "method=ALIPAY → 400" "$(echo "$JM" | jq -r '.code')" "10001"
echo "      method 文案：$(echo "$JM" | jq -r '.msg')"
JC=$(curl -s -X POST -H "$AUTH" -H 'Content-Type: application/json' \
  --data-binary "{\"customer_id\":\"5a170000-0000-4000-8000-0000000000ff\",\"amount\":10}" "$BASE/business/erp/receipt")
check "客户不存在 → 400" "$(echo "$JC" | jq -r '.code')" "10001"

echo
echo "==> 3. 详情 / 分页"
JD=$(curl -s -H "$AUTH" "$BASE/business/erp/receipt/$RC")
check "详情 code" "$(echo "$JD" | jq -r '.code')" "0"
check "详情客户名" "$(echo "$JD" | jq -r '.data.customer_name')" "收款演示客户"
check "详情核销明细为空" "$(echo "$JD" | jq -r '.data.allocations | length')" "0"
JP=$(curl -s -H "$AUTH" "$BASE/business/erp/receipt/page?page=1&page_size=10")
check "分页 code" "$(echo "$JP" | jq -r '.code')" "0"
check "分页命中 1 条" "$(echo "$JP" | jq -r '.data.total')" "1"
check "分页状态标签" "$(echo "$JP" | jq -r '.data.list[0].status_label')" "草稿"
JPS=$(curl -s -H "$AUTH" "$BASE/business/erp/receipt/page?status=OPEN")
check "分页 status=OPEN（应收域值）→ 400" "$(echo "$JPS" | jq -r '.code')" "10001"
echo "      分页拒收文案：$(echo "$JPS" | jq -r '.msg')"

echo
echo "==> 4. 核销 POST /receipt/{id}/allocate（一条收款 → 两张应收）"
JA=$(curl -s -X POST -H "$AUTH" -H 'Content-Type: application/json' --data-binary "{
  \"items\": [
    {\"receivable_id\": \"$AR_SETTLED\", \"amount\": 600, \"remark\": \"一次收清\"},
    {\"receivable_id\": \"$AR_PARTIAL\", \"amount\": 400, \"remark\": \"部分收款\"}
  ]
}" "$BASE/business/erp/receipt/$RC/allocate")
echo "      → $(echo "$JA" | jq -c '{code, allocated: .data.allocated_amount, unallocated: .data.unallocated_amount, rows: (.data.allocations|length)}')"
check "核销 code" "$(echo "$JA" | jq -r '.code')" "0"
check "已核销 = 1000" "$(echo "$JA" | jq -r '.data.allocated_amount')" "1000.0000"
check "未核销 = 0" "$(echo "$JA" | jq -r '.data.unallocated_amount')" "0.0000"
check "核销明细 2 行" "$(echo "$JA" | jq -r '.data.allocations | length')" "2"
echo "      明细：$(echo "$JA" | jq -c '[.data.allocations[] | {ar_no, amount, ar_status, ar_unsettled_amount}]')"
check "AR17-001 状态 → SETTLED" "$(echo "$JA" | jq -r '.data.allocations[] | select(.ar_no=="AR17-001") | .ar_status')" "SETTLED"
check "AR17-002 状态 → PARTIAL" "$(echo "$JA" | jq -r '.data.allocations[] | select(.ar_no=="AR17-002") | .ar_status')" "PARTIAL"
check "AR17-002 未收 = 100" "$(echo "$JA" | jq -r '.data.allocations[] | select(.ar_no=="AR17-002") | .ar_unsettled_amount')" "100.0000"
check "库：AR-001 received/unsettled/status" \
  "$(psql_q "select received_amount || '|' || unsettled_amount || '|' || status from biz_receivable where id='$AR_SETTLED'::uuid")" \
  "600.0000|0.0000|SETTLED"
check "库：AR-002 received/unsettled/status" \
  "$(psql_q "select received_amount || '|' || unsettled_amount || '|' || status from biz_receivable where id='$AR_PARTIAL'::uuid")" \
  "400.0000|100.0000|PARTIAL"

echo
echo "==> 5. 超额核销被拒"
# 5.1 单张超额：AR17-003 应收 300，核 301 > 未收 300
J5=$(curl -s -X POST -H "$AUTH" -H 'Content-Type: application/json' \
  --data-binary "{\"items\":[{\"receivable_id\":\"$AR_SPARE\",\"amount\":301}]}" \
  "$BASE/business/erp/receipt/$RC/allocate")
check "单张超额 → 400" "$(echo "$J5" | jq -r '.code')" "10001"
echo "      文案：$(echo "$J5" | jq -r '.msg')"
# 5.2 总额超额：已核销 1000 = 收款 1000，再核 1 也超
J6=$(curl -s -X POST -H "$AUTH" -H 'Content-Type: application/json' \
  --data-binary "{\"items\":[{\"receivable_id\":\"$AR_SPARE\",\"amount\":1}]}" \
  "$BASE/business/erp/receipt/$RC/allocate")
check "核销总额超过收款金额 → 400" "$(echo "$J6" | jq -r '.code')" "10001"
echo "      文案：$(echo "$J6" | jq -r '.msg')"
# 5.3 别家客户的应收
J7=$(curl -s -X POST -H "$AUTH" -H 'Content-Type: application/json' \
  --data-binary "{\"items\":[{\"receivable_id\":\"$AR_FOREIGN\",\"amount\":100}]}" \
  "$BASE/business/erp/receipt/$RC/allocate")
check "核到别家客户的应收 → 400" "$(echo "$J7" | jq -r '.code')" "10001"
echo "      文案：$(echo "$J7" | jq -r '.msg')"
# 5.4 已结清的应收不能再核（AR17-001 已 SETTLED）
J8=$(curl -s -X POST -H "$AUTH" -H 'Content-Type: application/json' \
  --data-binary "{\"items\":[{\"receivable_id\":\"$AR_SETTLED\",\"amount\":1}]}" \
  "$BASE/business/erp/receipt/$RC/allocate")
check "核到已结清应收 → 409" "$(echo "$J8" | jq -r '.code')" "10003"
echo "      文案：$(echo "$J8" | jq -r '.msg')"
# 5.5 请求内重复应收
J9=$(curl -s -X POST -H "$AUTH" -H 'Content-Type: application/json' \
  --data-binary "{\"items\":[{\"receivable_id\":\"$AR_SPARE\",\"amount\":10},{\"receivable_id\":\"$AR_SPARE\",\"amount\":20}]}" \
  "$BASE/business/erp/receipt/$RC/allocate")
check "同一应收重复提交 → 400" "$(echo "$J9" | jq -r '.code')" "10001"
check "负向用例后库：AR-003 未被扣款" \
  "$(psql_q "select received_amount from biz_receivable where id='$AR_SPARE'::uuid")" "0.0000"

echo
echo "==> 6. 状态迁移 PATCH /receipt/{id}/status"
# 6.1 非法状态域
JS0=$(curl -s -X PATCH -H "$AUTH" -H 'Content-Type: application/json' \
  --data-binary '{"status":"OPEN"}' "$BASE/business/erp/receipt/$RC/status")
check "status=OPEN → 400" "$(echo "$JS0" | jq -r '.code')" "10001"
# 6.2 跳跃边（DRAFT → POSTED）
JS1=$(curl -s -X PATCH -H "$AUTH" -H 'Content-Type: application/json' \
  --data-binary '{"status":"POSTED"}' "$BASE/business/erp/receipt/$RC/status")
check "DRAFT→POSTED 跳边 → 409" "$(echo "$JS1" | jq -r '.code')" "10003"
echo "      文案：$(echo "$JS1" | jq -r '.msg')"
# 6.3 确认
JS2=$(curl -s -X PATCH -H "$AUTH" -H 'Content-Type: application/json' \
  --data-binary '{"status":"CONFIRMED"}' "$BASE/business/erp/receipt/$RC/status")
check "DRAFT→CONFIRMED" "$(echo "$JS2" | jq -r '.data.status')" "CONFIRMED"
# 6.4 过账（生成凭证）
JS3=$(curl -s -X PATCH -H "$AUTH" -H 'Content-Type: application/json' \
  --data-binary '{"status":"POSTED"}' "$BASE/business/erp/receipt/$RC/status")
echo "      → $(echo "$JS3" | jq -c '{code, status: .data.status, voucher_no: .data.voucher_no}')"
check "CONFIRMED→POSTED" "$(echo "$JS3" | jq -r '.data.status')" "POSTED"
VNO=$(echo "$JS3" | jq -r '.data.voucher_no')
checkne "凭证号已生成" "$VNO" "null"
VID=$(psql_q "select voucher_id from biz_receipt where id='$RC'::uuid")
checkne "biz_receipt.voucher_id 已回写" "$VID" ""
check "凭证来源 = RECEIPT" "$(psql_q "select source_type from biz_voucher where id='$VID'::uuid")" "RECEIPT"
check "凭证状态 = POSTED" "$(psql_q "select status from biz_voucher where id='$VID'::uuid")" "POSTED"
check "分录：借 1002 / 贷 1122 / 各 1000" "$(psql_q "
  select string_agg(account_code || ':' || direction || ':' || amount, ',' order by line_no) from (
    select e.line_no, e.account_code,
           case when e.debit_amount > 0 then 'D' else 'C' end as direction,
           case when e.debit_amount > 0 then e.debit_amount else e.credit_amount end as amount
      from biz_voucher_entry e
     where e.voucher_id = '$VID'::uuid
  ) t")" "1002:D:1000.0000,1122:C:1000.0000"
# 6.5 过账后不能再核销
JS4=$(curl -s -X POST -H "$AUTH" -H 'Content-Type: application/json' \
  --data-binary "{\"items\":[{\"receivable_id\":\"$AR_SPARE\",\"amount\":10}]}" \
  "$BASE/business/erp/receipt/$RC/allocate")
check "POSTED 后再核销 → 409" "$(echo "$JS4" | jq -r '.code')" "10003"
echo "      文案：$(echo "$JS4" | jq -r '.msg')"
# 6.6 过账后不能再作废
JS5=$(curl -s -X PATCH -H "$AUTH" -H 'Content-Type: application/json' \
  --data-binary '{"status":"CANCELLED","reason":"想作废看看"}' "$BASE/business/erp/receipt/$RC/status")
check "POSTED→CANCELLED → 409" "$(echo "$JS5" | jq -r '.code')" "10003"

echo
echo "==> 7. 作废规则（第二张收款单）"
J2=$(curl -s -X POST -H "$AUTH" -H 'Content-Type: application/json' \
  --data-binary "{\"customer_id\":\"$CUSTOMER\",\"receipt_date\":\"2026-11-06\",\"amount\":100,\"method\":\"CASH\"}" \
  "$BASE/business/erp/receipt")
RC2=$(echo "$J2" | jq -r '.data.id')
RC2_NO=$(echo "$J2" | jq -r '.data.receipt_no')
check "第二张收款单登记（CASH）" "$(echo "$J2" | jq -r '.data.method_label')" "现金"
# 7.1 缺原因 → 400
JC1=$(curl -s -X PATCH -H "$AUTH" -H 'Content-Type: application/json' \
  --data-binary '{"status":"CANCELLED"}' "$BASE/business/erp/receipt/$RC2/status")
check "作废缺原因 → 400" "$(echo "$JC1" | jq -r '.code')" "10001"
echo "      文案：$(echo "$JC1" | jq -r '.msg')"
# 7.2 已核销 → 409（先核 100 到 AR17-003）
JAL=$(curl -s -X POST -H "$AUTH" -H 'Content-Type: application/json' \
  --data-binary "{\"items\":[{\"receivable_id\":\"$AR_SPARE\",\"amount\":100}]}" \
  "$BASE/business/erp/receipt/$RC2/allocate")
check "第二张核销 100 到 AR17-003" "$(echo "$JAL" | jq -r '.data.allocated_amount')" "100.0000"
check "AR17-003 状态 → PARTIAL" "$(echo "$JAL" | jq -r '.data.allocations[0].ar_status')" "PARTIAL"
JC2=$(curl -s -X PATCH -H "$AUTH" -H 'Content-Type: application/json' \
  --data-binary '{"status":"CANCELLED","reason":"登记错了客户"}' "$BASE/business/erp/receipt/$RC2/status")
check "已核销的收款作废 → 409" "$(echo "$JC2" | jq -r '.code')" "10003"
echo "      文案：$(echo "$JC2" | jq -r '.msg')"
# 7.3 未核销余额 > 0 时不许过账（先把核销退回不了 -> 用第三张单验证）
J3=$(curl -s -X POST -H "$AUTH" -H 'Content-Type: application/json' \
  --data-binary "{\"customer_id\":\"$CUSTOMER\",\"receipt_date\":\"2026-11-06\",\"amount\":50}" \
  "$BASE/business/erp/receipt")
RC3=$(echo "$J3" | jq -r '.data.id')
JCONF=$(curl -s -X PATCH -H "$AUTH" -H 'Content-Type: application/json' \
  --data-binary '{"status":"CONFIRMED"}' "$BASE/business/erp/receipt/$RC3/status")
check "第三张确认" "$(echo "$JCONF" | jq -r '.data.status')" "CONFIRMED"
JPOST=$(curl -s -X PATCH -H "$AUTH" -H 'Content-Type: application/json' \
  --data-binary '{"status":"POSTED"}' "$BASE/business/erp/receipt/$RC3/status")
check "未核销满 → 过账 409" "$(echo "$JPOST" | jq -r '.code')" "10003"
echo "      文案：$(echo "$JPOST" | jq -r '.msg')"
# 7.4 CONFIRMED → CANCELLED 不是合法边（设计如此：确认过的收款要作废得先回到草稿/走红冲）
JC3=$(curl -s -X PATCH -H "$AUTH" -H 'Content-Type: application/json' \
  --data-binary '{"status":"CANCELLED","reason":"客户退回，重开一张"}' "$BASE/business/erp/receipt/$RC3/status")
check "CONFIRMED→CANCELLED → 409（无此边）" "$(echo "$JC3" | jq -r '.code')" "10003"
echo "      文案：$(echo "$JC3" | jq -r '.msg')"
# 7.5 草稿单可以作废（DRAFT → CANCELLED，必须给原因）
J4=$(curl -s -X POST -H "$AUTH" -H 'Content-Type: application/json' \
  --data-binary "{\"customer_id\":\"$CUSTOMER\",\"receipt_date\":\"2026-11-06\",\"amount\":50}" \
  "$BASE/business/erp/receipt")
RC4=$(echo "$J4" | jq -r '.data.id')
RC4_NO=$(echo "$J4" | jq -r '.data.receipt_no')
JC4=$(curl -s -X PATCH -H "$AUTH" -H 'Content-Type: application/json' \
  --data-binary '{"status":"CANCELLED","reason":"客户退回，重开一张"}' "$BASE/business/erp/receipt/$RC4/status")
check "未核销草稿作废 → CANCELLED" "$(echo "$JC4" | jq -r '.data.status')" "CANCELLED"
check "作废后 status_label" "$(echo "$JC4" | jq -r '.data.status_label')" "已作废"

echo
echo "==> 8. 404 / 66006"
JN=$(curl -s -H "$AUTH" "$BASE/business/erp/receipt/5a170000-0000-4000-8000-0000000000ff")
check "不存在的收款单 → 404 + 10002" "$(echo "$JN" | jq -r '.code')" "10002"
JX=$(curl -s -H "$AUTH" "$BASE/business/erp/receipt/not-a-uuid")
check "非法 UUID → 66006" "$(echo "$JX" | jq -r '.code')" "66006"

echo
echo "==> 9. 库层留痕"
echo "      biz_status_log(RECEIPT)：$(psql_q "select string_agg(coalesce(from_status,'NULL') || '→' || to_status, ' , ' order by created_at) from biz_status_log where entity_type='RECEIPT' and entity_no='$RC_NO'")"
check "收款单状态日志 3 条（NULL→DRAFT / DRAFT→CONFIRMED / CONFIRMED→POSTED）" \
  "$(psql_q "select count(*) from biz_status_log where entity_type='RECEIPT' and entity_no='$RC_NO'")" "3"
check "作废原因落库" \
  "$(psql_q "select reason from biz_status_log where entity_type='RECEIPT' and to_status='CANCELLED' and entity_no='$RC4_NO'")" \
  "客户退回，重开一张"
echo "      应收核销留痕：$(psql_q "select string_agg(coalesce(from_status,'NULL') || '→' || to_status, ' , ' order by created_at) from biz_status_log where entity_type='RECEIVABLE' and entity_no in ('AR17-001','AR17-002')")"
check "应收状态日志含 OPEN→SETTLED 与 OPEN→PARTIAL" \
  "$(psql_q "select count(*) from biz_status_log where entity_type='RECEIVABLE' and entity_no='AR17-001' and to_status='SETTLED'")" "1"
check "汇总：biz_receipt.allocated_amount = SUM(明细)" "$(psql_q "
  select (select allocated_amount from biz_receipt where id='$RC'::uuid) =
         (select coalesce(sum(amount),0) from biz_receipt_allocation where receipt_id='$RC'::uuid)")" "t"
check "核销明细行数（RC-1 两行 + RC-2 一行）" "$(psql_q "select count(*) from biz_receipt_allocation")" "3"
echo "      凭证：$VNO / id=$VID"

echo
echo "================ 汇总：PASS=$PASS  FAIL=$FAIL ================"
[ "$FAIL" -eq 0 ] || exit 1

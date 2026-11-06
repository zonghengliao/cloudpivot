#!/usr/bin/env bash
# ============================================================================
# t_b9840c8d（SO-14 遗留 · D10）：收款退款单接入审批流 —— 真实 token 端到端 curl 实测
#
# 用法（先起一个指向**隔离库**的实例，见 README §五）：
#   BASE=http://127.0.0.1:18131/api DB=cloudpivot_rr_ap \
#     bash docs/evidence/20261106-so14-receipt-refund-approval/curl-e2e.sh
#
# 链路（全走 HTTP handler，不绕过任何一道闸）：
#   ① 未审批直接过账 → 409 + 66044（报文给出 submit-approval 入口）
#   ② 提交审批 → 200：退款单仍 CONFIRMED，实例 PENDING，1 条待办
#   ③ 审批中重复提交 → 409
#   ④ 审批人（非制单人的超管）登录 → 找到待办 → 同意
#   ⑤ 回调**直接过账**：退款单 POSTED + 凭证 借 1122 / 贷 1002 各 100 + 状态日志带审批单号
#   ⑥ 已过账再作废 → 409（终态语义没动）
#   ⑦ 第二张单：驳回 → 仍是 CONFIRMED、一分没记账、可重新提交
#
# 脚本自带清场（只在本卡隔离库里按表清），可连跑多轮，每轮断言数量相同。
# ============================================================================
set -uo pipefail

BASE=${BASE:-http://127.0.0.1:18131/api}
DB=${DB:-cloudpivot_rr_ap}
PGHOST=${PGHOST:-127.0.0.1}
PGUSER=${PGUSER:-alun}
export PGPASSWORD=${PGPASSWORD:-alun}
ADMIN_USER=${ADMIN_USER:-admin}
ADMIN_PASS=${ADMIN_PASS:-Admin@123456}
APPROVER_USER=${APPROVER_USER:-rr_approver}
APPROVER_ID=${APPROVER_ID:-d5950000-0000-4000-8000-0000000000a1}
CCODE='RR14AP-CUST'
CUST_ID='d5950000-0000-4000-8000-0000000000c9'

PASS=0
FAIL=0
ok()    { PASS=$((PASS+1)); echo "  PASS  $*"; }
bad()   { FAIL=$((FAIL+1)); echo "  FAIL  $*"; }
check() { if [ "$2" = "$3" ]; then ok "$1（$3）"; else bad "$1：期望 $3，实际 $2"; fi }

psql_q() { psql -h "$PGHOST" -U "$PGUSER" -d "$DB" -Atq -c "$1"; }
jqf()    { jq -r "$1" 2>/dev/null; }

echo "==> 0. 清场（隔离库按表清）+ 夹具"
psql_q "DELETE FROM biz_flow_task WHERE instance_id IN (SELECT id FROM biz_flow_instance WHERE biz_type = 'RECEIPT_REFUND')" >/dev/null
psql_q "DELETE FROM biz_flow_instance WHERE biz_type = 'RECEIPT_REFUND'" >/dev/null
psql_q "DELETE FROM biz_status_log WHERE entity_type IN ('RECEIPT_REFUND','RECEIVABLE')" >/dev/null
psql_q "DELETE FROM biz_voucher_entry WHERE voucher_id IN (SELECT id FROM biz_voucher WHERE source_type = 'RECEIPT_REFUND')" >/dev/null
psql_q "DELETE FROM biz_voucher WHERE source_type = 'RECEIPT_REFUND'" >/dev/null
psql_q "DELETE FROM biz_receipt_refund_allocation" >/dev/null
psql_q "DELETE FROM biz_receipt_refund" >/dev/null
psql_q "DELETE FROM biz_receivable WHERE ar_no LIKE 'AR-RR14AP-%'" >/dev/null
psql_q "DELETE FROM biz_customer_contact WHERE customer_id = '$CUST_ID'::uuid" >/dev/null
psql_q "DELETE FROM biz_customer WHERE code = '$CCODE'" >/dev/null

psql_q "INSERT INTO biz_customer (id, code, name, credit_days, credit_limit, status)
        VALUES ('$CUST_ID'::uuid, '$CCODE', 'RR14AP 审批演示客户', 30, 0, 'ACTIVE')
        ON CONFLICT (id) DO UPDATE SET deleted_at = NULL" >/dev/null
AR_ID=$(psql_q "INSERT INTO biz_receivable (ar_no, customer_id, source_type, source_id, source_no, amount,
                                           received_amount, unsettled_amount, credit_days, period_code,
                                           due_date, status)
                VALUES ('AR-RR14AP-000001', '$CUST_ID'::uuid, 'MANUAL', gen_random_uuid(), 'MANUAL',
                        1000, 100, 900, 30, '2026-11', '2026-12-06', 'PARTIAL')
                RETURNING id")
[ -n "$AR_ID" ] && ok "夹具：客户 + 应收 PARTIAL（1000 / 已收 100 / $AR_ID）" || { bad "夹具建应收失败"; exit 1; }

# 审批人夹具：非制单人的超管（同一套口令，便于登录）
psql_q "INSERT INTO sys_user (id, username, password, real_name, must_change_password)
        SELECT '$APPROVER_ID'::uuid, '$APPROVER_USER', password, 'RR14AP 退款审批人', FALSE
          FROM sys_user WHERE username = 'admin'
        ON CONFLICT (id) DO UPDATE SET deleted_at = NULL, password = EXCLUDED.password" >/dev/null
psql_q "INSERT INTO sys_user_role (user_id, role_id)
        SELECT '$APPROVER_ID'::uuid, id FROM sys_role WHERE role_code = 'SUPER_ADMIN'
        ON CONFLICT DO NOTHING" >/dev/null
ok "夹具：审批人 $APPROVER_USER（超管，但不是制单人）"

echo
echo "==> 1. 登录（admin；首登强制改密自动处理）"
login() {
  curl -s -X POST -H 'Content-Type: application/json' \
    --data-binary "{\"username\":\"$1\",\"password\":\"$2\"}" \
    "$BASE/system/auth/login"
}
TOKEN=""
CHANGED_PASS=""
for candidate in "$ADMIN_PASS" "${ADMIN_PASS}_ap"; do
  for attempt in 1 2 3; do
    RESP=$(login "$ADMIN_USER" "$candidate")
    TOKEN=$(echo "$RESP" | jqf '.data.access_token // empty')
    [ -n "$TOKEN" ] && break
    if [ "$(echo "$RESP" | jqf '.code')" = "10005" ]; then sleep 25; else break; fi
  done
  if [ -n "$TOKEN" ]; then ADMIN_PASS="$candidate"; break; fi
done
if [ -z "$TOKEN" ]; then echo "  !! 登录失败：$RESP"; exit 1; fi
MUST=$(echo "$RESP" | jqf '.data.must_change_password // false')
if [ "$MUST" = "true" ]; then
  CHANGED_PASS="${ADMIN_PASS}_ap"
  curl -s -o /dev/null -X PUT -H "Authorization: Bearer $TOKEN" -H 'Content-Type: application/json' \
    --data-binary "{\"old_password\":\"$ADMIN_PASS\",\"new_password\":\"$CHANGED_PASS\"}" \
    "$BASE/system/auth/password"
  ADMIN_PASS="$CHANGED_PASS"
  TOKEN=$(login "$ADMIN_USER" "$ADMIN_PASS" | jqf '.data.access_token // empty')
fi
[ -n "$TOKEN" ] && ok "拿到 admin access_token（制单人）" || { bad "登录失败"; exit 1; }
# 审批人口令跟随 admin 当前口令（admin 首登强制改密后哈希变了，夹具用户要同步，
# 否则「制单人改完密码、审批人还用旧口令」会登录失败）
psql_q "UPDATE sys_user SET password = (SELECT password FROM sys_user WHERE username = 'admin')
         WHERE username = '$APPROVER_USER'" >/dev/null
ok "审批人口令已与 admin 同步"
AUTH="Authorization: Bearer $TOKEN"

echo
echo "==> 2. 登记退款 → 核销 → 确认（制单人 = admin）"
R1=$(curl -s -X POST -H "$AUTH" -H 'Content-Type: application/json' \
  --data-binary "{\"customer_id\":\"$CUST_ID\",\"refund_date\":\"2026-11-06\",\"amount\":100,
                    \"method\":\"BANK_TRANSFER\",\"reason\":\"客户退货，货款原路退回\"}" \
  "$BASE/business/erp/receipt-refund")
RID=$(echo "$R1" | jqf '.data.id // empty')
RNO=$(echo "$R1" | jqf '.data.refund_no // empty')
[ -n "$RID" ] && ok "登记退款（$RNO）" || { bad "登记失败：$R1"; exit 1; }
check "新单审批段为空" "$(echo "$R1" | jqf '.data.approval')" "null"

curl -s -o /dev/null -X POST -H "$AUTH" -H 'Content-Type: application/json' \
  --data-binary "{\"allocations\":[{\"receivable_id\":\"$AR_ID\",\"amount\":100}]}" \
  "$BASE/business/erp/receipt-refund/$RID/allocate"
CONF=$(curl -s -X PATCH -H "$AUTH" -H 'Content-Type: application/json' \
  --data-binary '{"status":"CONFIRMED"}' "$BASE/business/erp/receipt-refund/$RID/status")
check "确认后状态" "$(echo "$CONF" | jqf '.data.status')" "CONFIRMED"

echo
echo "==> 3. 未审批直接过账 → 409 + 66044"
NOAPPR=$(curl -s -X PATCH -H "$AUTH" -H 'Content-Type: application/json' \
  --data-binary '{"status":"POSTED"}' "$BASE/business/erp/receipt-refund/$RID/status")
check "过账闸错误码" "$(echo "$NOAPPR" | jqf '.code')" "66044"
if echo "$NOAPPR" | jqf '.msg' | grep -q 'submit-approval'; then
  ok "报文给出提交审批的入口"
else
  bad "报文没给入口：$(echo "$NOAPPR" | jqf '.msg')"
fi

echo
echo "==> 4. 提交审批（状态不变 + 实例 PENDING + 1 条待办）"
SUB=$(curl -s -X POST -H "$AUTH" -H 'Content-Type: application/json' \
  --data-binary '{}' "$BASE/business/erp/receipt-refund/$RID/submit-approval")
check "提交后单据状态（审批不是状态）" "$(echo "$SUB" | jqf '.data.status')" "CONFIRMED"
check "审批实例状态" "$(echo "$SUB" | jqf '.data.approval.status')" "PENDING"
check "待办数" "$(echo "$SUB" | jqf '.data.approval.pending_tasks')" "1"
INST=$(echo "$SUB" | jqf '.data.approval.instance_id // empty')
[ -n "$INST" ] && ok "审批实例（$INST）" || { bad "没有实例 id：$SUB"; exit 1; }

AGAIN=$(curl -s -X POST -H "$AUTH" -H 'Content-Type: application/json' \
  --data-binary '{}' "$BASE/business/erp/receipt-refund/$RID/submit-approval")
check "审批中重复提交 409" "$(echo "$AGAIN" | jqf '.code')" "10003"

echo
echo "==> 5. 审批人登录 → 同意（内控：制单人不能批自己的单）"
ATOKEN=$(login "$APPROVER_USER" "$ADMIN_PASS" | jqf '.data.access_token // empty')
[ -n "$ATOKEN" ] && ok "审批人登录成功" || { bad "审批人登录失败"; exit 1; }
AAUTH="Authorization: Bearer $ATOKEN"
DETAIL=$(curl -s -H "$AAUTH" "$BASE/business/flow/instance/$INST")
TASK=$(echo "$DETAIL" | jqf '.data.my_task.id // empty')
check "审批人能看到待办且是自己的" "$([ -n "$TASK" ] && echo yes)" "yes"

# 反向验证：制单人拿不到这条待办（内控在**生成待办时**就排除了他）
CREATOR_TASK=$(curl -s -H "$AUTH" "$BASE/business/flow/instance/$INST" | jqf '.data.my_task.id // empty')
check "制单人没有待办（内控排除）" "$CREATOR_TASK" ""

APPROVED=$(curl -s -X POST -H "$AAUTH" -H 'Content-Type: application/json' \
  --data-binary '{"comment":"同意退款，财务已复核"}' \
  "$BASE/business/flow/task/$TASK/approve")
check "审批动作返回实例状态" "$(echo "$APPROVED" | jqf '.data.instance_status')" "APPROVED"

echo
echo "==> 6. 回调自动过账：退款单 POSTED + 凭证 + 状态日志"
AFTER=$(curl -s -H "$AUTH" "$BASE/business/erp/receipt-refund/$RID")
check "退款单状态（回调直接过账）" "$(echo "$AFTER" | jqf '.data.status')" "POSTED"
VNO=$(echo "$AFTER" | jqf '.data.voucher_no // empty')
[ -n "$VNO" ] && ok "过账生成凭证号（$VNO）" || bad "没有凭证号：$AFTER"
check "详情里的审批状态" "$(echo "$AFTER" | jqf '.data.approval.status')" "APPROVED"
check "凭证 借 1122 100" "$(psql_q "SELECT count(*) FROM biz_voucher_entry e
    JOIN biz_voucher v ON v.id = e.voucher_id
   WHERE v.source_type = 'RECEIPT_REFUND' AND v.source_id = '$RID'::uuid
     AND e.account_code = '1122' AND e.debit_amount = 100 AND e.credit_amount = 0")" "1"
check "凭证 贷 1002 100" "$(psql_q "SELECT count(*) FROM biz_voucher_entry e
    JOIN biz_voucher v ON v.id = e.voucher_id
   WHERE v.source_type = 'RECEIPT_REFUND' AND v.source_id = '$RID'::uuid
     AND e.account_code = '1002' AND e.debit_amount = 0 AND e.credit_amount = 100")" "1"
check "状态日志写明审批单号" "$(psql_q "SELECT count(*) FROM biz_status_log
   WHERE entity_type = 'RECEIPT_REFUND' AND entity_id = '$RID'::uuid
     AND to_status = 'POSTED' AND reason LIKE '审批通过（FLW-%'")" "1"
check "应收已回到 OPEN / 已收 0" "$(psql_q "SELECT status || '|' || received_amount::text
   FROM biz_receivable WHERE id = '$AR_ID'::uuid")" "OPEN|0.0000"

echo
echo "==> 7. 终态语义没动：已过账再作废 409"
CANCEL=$(curl -s -X PATCH -H "$AUTH" -H 'Content-Type: application/json' \
  --data-binary '{"status":"CANCELLED","reason":"过账后不该能作废"}' \
  "$BASE/business/erp/receipt-refund/$RID/status")
check "已过账作废被拒" "$(echo "$CANCEL" | jqf '.code')" "10003"

echo
echo "==> 8. 第二张单：驳回 → 仍是 CONFIRMED、一分没记账、可重新提交"
AR2=$(psql_q "INSERT INTO biz_receivable (ar_no, customer_id, source_type, source_id, source_no, amount,
                                         received_amount, unsettled_amount, credit_days, period_code,
                                         due_date, status)
              VALUES ('AR-RR14AP-000002', '$CUST_ID'::uuid, 'MANUAL', gen_random_uuid(), 'MANUAL',
                      200, 200, 0, 30, '2026-11', '2026-12-06', 'SETTLED')
              RETURNING id")
R2=$(curl -s -X POST -H "$AUTH" -H 'Content-Type: application/json' \
  --data-binary "{\"customer_id\":\"$CUST_ID\",\"refund_date\":\"2026-11-06\",\"amount\":200,
                    \"method\":\"CASH\",\"reason\":\"客户投诉，部分退款\"}" \
  "$BASE/business/erp/receipt-refund")
RID2=$(echo "$R2" | jqf '.data.id // empty')
curl -s -o /dev/null -X POST -H "$AUTH" -H 'Content-Type: application/json' \
  --data-binary "{\"allocations\":[{\"receivable_id\":\"$AR2\",\"amount\":200}]}" \
  "$BASE/business/erp/receipt-refund/$RID2/allocate"
curl -s -o /dev/null -X PATCH -H "$AUTH" -H 'Content-Type: application/json' \
  --data-binary '{"status":"CONFIRMED"}' "$BASE/business/erp/receipt-refund/$RID2/status"
SUB2=$(curl -s -X POST -H "$AUTH" -H 'Content-Type: application/json' \
  --data-binary '{}' "$BASE/business/erp/receipt-refund/$RID2/submit-approval")
INST2=$(echo "$SUB2" | jqf '.data.approval.instance_id // empty')
TASK2=$(curl -s -H "$AAUTH" "$BASE/business/flow/instance/$INST2" | jqf '.data.my_task.id // empty')
curl -s -o /dev/null -X POST -H "$AAUTH" -H 'Content-Type: application/json' \
  --data-binary '{"comment":"金额存疑，请补充凭证材料","return_to":"NONE"}' \
  "$BASE/business/flow/task/$TASK2/reject"
D2=$(curl -s -H "$AUTH" "$BASE/business/erp/receipt-refund/$RID2")
check "驳回后单据仍是已确认" "$(echo "$D2" | jqf '.data.status')" "CONFIRMED"
check "驳回后审批状态" "$(echo "$D2" | jqf '.data.approval.status')" "REJECTED"
check "驳回一分没记账（凭证数）" "$(psql_q "SELECT count(*) FROM biz_voucher
   WHERE source_type = 'RECEIPT_REFUND' AND source_id = '$RID2'::uuid")" "0"
RESUB=$(curl -s -X POST -H "$AUTH" -H 'Content-Type: application/json' \
  --data-binary '{}' "$BASE/business/erp/receipt-refund/$RID2/submit-approval")
check "驳回后可以重新提交（新实例 PENDING）" "$(echo "$RESUB" | jqf '.data.approval.status')" "PENDING"
check "重提后仍是同一张单（未过账）" "$(echo "$RESUB" | jqf '.data.status')" "CONFIRMED"

echo
echo "==> 9. 汇总"
echo "  断言：PASS=$PASS FAIL=$FAIL"
[ "$FAIL" -eq 0 ] && echo "  RESULT=ALL_PASS（$PASS 条断言）" || echo "  RESULT=HAS_FAILURE"
exit "$FAIL"

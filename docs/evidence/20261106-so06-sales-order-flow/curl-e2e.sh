#!/usr/bin/env bash
# ============================================================================
# SO-06（t_860ea3d4）销售订单审批流 —— 真实 token curl 实测
#
# 跑法（实例在 127.0.0.1:18107，库 cloudpivot_so06，已应用 V0281）：
#   bash docs/evidence/20261106-so06-sales-order-flow/curl-e2e.sh
#
# 覆盖：
#   ① 登录（首登强制改密）
#   ② 建客户 + 三档金额建单（5000 / 50000 / 150000）
#   ③ 内控（默认开）：制单人 = 唯一可审批人（超管 fallback）→ 提交 422（"审批人全是制单人"）
#   ④ 关掉 erp.separation_of_duties.approve + 重载 → 三档分别提交成功（各建一条审批实例）
#   ⑤ 人工 PATCH APPROVED / DRAFT → 400（审批结论只能由审批中心写）
#   ⑥ 内控再打开：审批入口闸 → 自审 403（separation.rs，事务之前拦）
#   ⑦ 内控关掉：逐跳审批 → 1 / 2 / 3 跳 → 全部 APPROVED
#   ⑧ 驳回 → 退回 DRAFT（remark 带审批意见、实例清空、可重新提交）
#   ⑨ 重复提交 → 409
# ============================================================================
set -uo pipefail
BASE=${BASE:-http://127.0.0.1:18107}
DB=${ERP_DB:-cloudpivot_so06}
USER_NAME=${USER_NAME:-admin}
PASSWORD=${PASSWORD:-Admin@123456}
PASSWORD_ALT=${PASSWORD_ALT:-Admin@20261106}
FAIL=0

say() { printf '\n===== %s =====\n' "$*" >&2; }
ok()  { printf '  ✅ %s\n' "$*" >&2; }
bad() { printf '  ❌ %s\n' "$*" >&2; FAIL=1; }

# $1=label $2=expected_http $3=method $4=path [$5=json body]
call() {
    local label=$1 expect=$2 method=$3 path=$4 body=${5:-}
    local out code payload
    if [ -n "$body" ]; then
        out=$(curl -s -w '\n%{http_code}' -X "$method" "$BASE$path" \
              -H "Authorization: Bearer $TOKEN" -H 'Content-Type: application/json' -d "$body")
    else
        out=$(curl -s -w '\n%{http_code}' -X "$method" "$BASE$path" \
              -H "Authorization: Bearer $TOKEN")
    fi
    code=$(printf '%s' "$out" | tail -1)
    payload=$(printf '%s' "$out" | sed '$d')
    printf -- '--- %s\nHTTP %s (期望 %s)\n%s\n' "$label" "$code" "$expect" "$payload" >&2
    if [ "$code" != "$expect" ]; then
        bad "$label 状态码不符：实际 $code 期望 $expect"
    fi
    LAST_PAYLOAD=$payload
}

# 只发请求、不打印（给 $(...) 取数用）
raw_get() {
    curl -s -X GET "$BASE$1" -H "Authorization: Bearer $TOKEN"
}

# 内控开关：psql 改 sys_config + 调重载接口（进程内快照 TTL 60s，重载立刻生效）
set_separation() {
    local value=$1
    PGPASSWORD=alun psql -h 127.0.0.1 -U alun -d "$DB" -Atc \
      "UPDATE sys_config SET config_value = '$value' WHERE config_key = 'erp.separation_of_duties.approve'" \
      > /dev/null
    call "POST /support/config/reload（separation=$value）" 200 POST /api/support/config/reload '{}'
    printf '  erp.separation_of_duties.approve = %s\n' "$value"
}

# 取某单据当前的待办 id（我的待办里按 form_data.order_no 过滤）
todo_of() {
    local so_no=$1
    raw_get "/api/business/flow/task/todo?page=1&page_size=50" \
      | jq -r --arg no "$so_no" '.data.list[] | select((.form_data.order_no // "") == $no) | .id' \
      | head -1
}

# 逐跳审批直到没有待办；打印跳数
approve_all() {
    local so_no=$1 hops=0 task_id
    while [ "$hops" -lt 6 ]; do
        task_id=$(todo_of "$so_no")
        if [ -z "$task_id" ] || [ "$task_id" = "null" ]; then break; fi
        hops=$((hops + 1))
        call "POST /business/flow/task/$task_id/approve（$so_no 第 $hops 跳）" 200 POST \
          "/api/business/flow/task/$task_id/approve" '{"comment":"SO-06 实测同意"}'
    done
    echo "$hops"
}

# ---------------------------------------------------------------------------
say "① 登录（admin）"
LOGIN=""
for try in "$PASSWORD" "$PASSWORD_ALT"; do
    LOGIN=$(curl -s -X POST "$BASE/api/system/auth/login" -H 'Content-Type: application/json' \
            -d "{\"username\":\"$USER_NAME\",\"password\":\"$try\"}")
    if [ "$(printf '%s' "$LOGIN" | jq -r '.code')" = "0" ]; then PASSWORD="$try"; break; fi
done
TOKEN=$(printf '%s' "$LOGIN" | jq -r '.data.access_token')
printf 'code=%s token_len=%s must_change_password=%s\n' \
  "$(printf '%s' "$LOGIN" | jq -r '.code')" "${#TOKEN}" \
  "$(printf '%s' "$LOGIN" | jq -r '.data.must_change_password')"
[ -n "$TOKEN" ] && [ "$TOKEN" != "null" ] || { echo '!!! 登录失败'; exit 1; }

if [ "$(printf '%s' "$LOGIN" | jq -r '.data.must_change_password')" = "true" ]; then
    PASSWORD_NEW="Admin@20261106"
    CHG=$(curl -s -w '\n%{http_code}' -X PUT "$BASE/api/system/auth/password" \
          -H "Authorization: Bearer $TOKEN" -H 'Content-Type: application/json' \
          -d "{\"old_password\":\"$PASSWORD\",\"new_password\":\"$PASSWORD_NEW\"}")
    printf '改密 HTTP %s\n' "$(printf '%s' "$CHG" | tail -1)"
    LOGIN=$(curl -s -X POST "$BASE/api/system/auth/login" -H 'Content-Type: application/json' \
            -d "{\"username\":\"$USER_NAME\",\"password\":\"$PASSWORD_NEW\"}")
    TOKEN=$(printf '%s' "$LOGIN" | jq -r '.data.access_token')
    [ -n "$TOKEN" ] && [ "$TOKEN" != "null" ] || { echo '!!! 改密后登录失败'; exit 1; }
    printf '重新登录 token_len=%s\n' "${#TOKEN}"
fi

# ---------------------------------------------------------------------------
say "② 建客户（额度 0 = 不限）+ 三档金额建单"
CUST_CODE="E2E-SO6-$$"
call "POST /business/erp/customer" 200 POST /api/business/erp/customer \
  "{\"code\":\"$CUST_CODE\",\"name\":\"SO-06 审批实测客户\",\"credit_limit\":0,\"status\":\"ACTIVE\"}"
CUST_ID=$(printf '%s' "$LAST_PAYLOAD" | jq -r '.data.id')
printf 'customer_id=%s\n' "$CUST_ID"

create_order() {
    local price=$1 label=$2
    call "POST /sales-order（$label：1 × $price，税率 0）" 200 POST /api/business/erp/sales-order \
      "{\"customer_id\":\"$CUST_ID\",\"order_date\":\"2026-11-06\",\"remark\":\"SO-06 $label\",\"items\":[{\"material_id\":\"d5050000-0000-4000-8000-000000000001\",\"qty\":1,\"price\":$price,\"tax_rate\":0}]}"
    ORDER_ID=$(printf '%s' "$LAST_PAYLOAD" | jq -r '.data.id')
    ORDER_NO=$(printf '%s' "$LAST_PAYLOAD" | jq -r '.data.so_no')
    printf '  → so_id=%s so_no=%s total_with_tax=%s\n' "$ORDER_ID" "$ORDER_NO" \
      "$(printf '%s' "$LAST_PAYLOAD" | jq -r '.data.total_with_tax')"
}

create_order 5000   "tier1-5000";   TIER1_ID=$ORDER_ID; TIER1_NO=$ORDER_NO
create_order 50000  "tier2-50000";  TIER2_ID=$ORDER_ID; TIER2_NO=$ORDER_NO
create_order 150000 "tier3-150000"; TIER3_ID=$ORDER_ID; TIER3_NO=$ORDER_NO

# ---------------------------------------------------------------------------
say "③ 内控（默认开）：提交 → 422（唯一的可审批人是制单人本人，内控不留可用审批人）"
set_separation true
call "POST /submit-approval（tier1，内控开）" 422 POST \
  "/api/business/erp/sales-order/$TIER1_ID/submit-approval" '{}'
printf '%s' "$LAST_PAYLOAD" | jq -r '.msg' | grep -q "制单人" \
  && ok "422 报文点名「审批人全是制单人 / 发起人」（internal_control 落 SKIPPED 留痕）" \
  || bad "422 报文没有说明内控原因"
call "GET /sales-order/{id}（被内控拒绝后）" 200 GET "/api/business/erp/sales-order/$TIER1_ID"
[ "$(printf '%s' "$LAST_PAYLOAD" | jq -r '.data.status')" = "DRAFT" ] \
  && ok "内控拒绝后仍是 DRAFT（事务整体回滚，没留下半张审批单）" \
  || bad "内控拒绝后状态漂移"

# ---------------------------------------------------------------------------
say "④ 关掉内控（内控例外，脚本留痕）→ 三档分别提交成功"
set_separation false
for pair in "$TIER1_ID:< 1 万" "$TIER2_ID:1–10 万" "$TIER3_ID:≥ 10 万"; do
    oid=${pair%%:*}; label=${pair##*:}
    call "POST /submit-approval（$label）" 200 POST \
      "/api/business/erp/sales-order/$oid/submit-approval" '{}'
    [ "$(printf '%s' "$LAST_PAYLOAD" | jq -r '.data.status')" = "SUBMITTED" ] \
      && ok "$label → SUBMITTED（已发起 so_approve 实例）" || bad "$label 没有进入 SUBMITTED"
done

# ---------------------------------------------------------------------------
say "⑤ 人工推审批结论 → 400（只能由审批中心写）"
call "PATCH /status（APPROVED）" 400 PATCH "/api/business/erp/sales-order/$TIER1_ID/status" \
  '{"target_status":"APPROVED"}'
call "PATCH /status（DRAFT）" 400 PATCH "/api/business/erp/sales-order/$TIER1_ID/status" \
  '{"target_status":"DRAFT"}'

# ---------------------------------------------------------------------------
say "⑥ 内控再打开：审批入口闸 → 自审 403（separation.rs，事务之前拦）"
set_separation true
SELF_TASK=$(todo_of "$TIER1_NO")
printf '  待办 task_id=%s\n' "$SELF_TASK"
call "POST /business/flow/task/{id}/approve（自己审自己）" 403 POST \
  "/api/business/flow/task/$SELF_TASK/approve" '{"comment":"自己批自己"}'

# ---------------------------------------------------------------------------
say "⑦ 内控关掉后逐跳审批：跳数必须分别是 1 / 2 / 3"
set_separation false
HOPS1=$(approve_all "$TIER1_NO")
HOPS2=$(approve_all "$TIER2_NO")
HOPS3=$(approve_all "$TIER3_NO")
printf '  跳数：tier1=%s tier2=%s tier3=%s（期望 1 / 2 / 3）\n' "$HOPS1" "$HOPS2" "$HOPS3"
[ "$HOPS1" = "1" ] && ok "tier1 全链 = 销售主管（1 跳）" || bad "tier1 跳数 $HOPS1 ≠ 1"
[ "$HOPS2" = "2" ] && ok "tier2 全链 = 销售主管 → 财务经理（2 跳）" || bad "tier2 跳数 $HOPS2 ≠ 2"
[ "$HOPS3" = "3" ] && ok "tier3 全链 = 销售主管 → 财务经理 → 总经理（3 跳）" || bad "tier3 跳数 $HOPS3 ≠ 3"

for pair in "$TIER1_ID:tier1" "$TIER2_ID:tier2" "$TIER3_ID:tier3"; do
    oid=${pair%%:*}; name=${pair##*:}
    call "GET /sales-order/{id}（$name 审批后）" 200 GET "/api/business/erp/sales-order/$oid"
    st=$(printf '%s' "$LAST_PAYLOAD" | jq -r '.data.status')
    [ "$st" = "APPROVED" ] && ok "$name 审批通过 → APPROVED" || bad "$name 状态 $st ≠ APPROVED"
done

# ---------------------------------------------------------------------------
say "⑧ 驳回 → 退回 DRAFT（remark 带审批意见 + 实例清空 + 可重新提交）"
create_order 5000 "reject-case"
REJ_ID=$ORDER_ID; REJ_NO=$ORDER_NO
call "POST /submit-approval（驳回用例）" 200 POST \
  "/api/business/erp/sales-order/$REJ_ID/submit-approval" '{}'
REJ_TASK=$(todo_of "$REJ_NO")
call "POST /business/flow/task/{id}/reject（return_to=NONE 终止）" 200 POST \
  "/api/business/flow/task/$REJ_TASK/reject" \
  '{"comment":"金额与合同不符，退回重报","return_to":"NONE"}'
call "GET /sales-order/{id}（驳回后）" 200 GET "/api/business/erp/sales-order/$REJ_ID"
printf '%s' "$LAST_PAYLOAD" | jq -r '.data | "  status=\(.status) remark=\(.remark)"'
[ "$(printf '%s' "$LAST_PAYLOAD" | jq -r '.data.status')" = "DRAFT" ] \
  && ok "驳回 → DRAFT" || bad "驳回后状态不是 DRAFT"
printf '%s' "$LAST_PAYLOAD" | jq -r '.data.remark // ""' | grep -q "审批驳回" \
  && ok "驳回意见写进 remark" || bad "remark 里没有审批意见"
call "POST /submit-approval（驳回后重新提交）" 200 POST \
  "/api/business/erp/sales-order/$REJ_ID/submit-approval" '{}'
[ "$(printf '%s' "$LAST_PAYLOAD" | jq -r '.data.status')" = "SUBMITTED" ] \
  && ok "回草稿后可重新提交（实例已清空，没有撞唯一索引）" || bad "重新提交失败"

# ---------------------------------------------------------------------------
say "⑨ 重复提交 → 409"
call "POST /submit-approval（已在审批中）" 409 POST \
  "/api/business/erp/sales-order/$REJ_ID/submit-approval" '{}'

# ---------------------------------------------------------------------------
# 收尾：把内控开关恢复默认（true），别把隔离库留在「内控关闭」的状态
PGPASSWORD=alun psql -h 127.0.0.1 -U alun -d "$DB" -Atc \
  "UPDATE sys_config SET config_value = 'true' WHERE config_key = 'erp.separation_of_duties.approve'" \
  > /dev/null

printf '\n'
if [ "$FAIL" = "0" ]; then
    echo "=== 全部断言通过 ==="
else
    echo "!!! 有断言失败（见上面 ❌）"
fi
exit "$FAIL"

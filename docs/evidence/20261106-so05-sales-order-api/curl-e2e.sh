#!/usr/bin/env bash
# ============================================================================
# SO-05（t_9832ecaf）销售订单 API 真实 token curl 实测
#
# 跑法（实例已在 127.0.0.1:18099，库 cloudpivot_so05）：
#   bash docs/evidence/20261106-so05-sales-order-api/curl-e2e.sh
#
# 覆盖：登录 → 建客户 → 建单（取号）→ 详情 → 改单 → 分页 → 状态机
#       （非法迁移 409 / 域外值 400 / 缺原因 400 / 终态 409 / 主链放行）
#       → 提交三道守卫（0 行明细 / 客户冻结 / 信用额度超）
# ============================================================================
set -uo pipefail
BASE=${BASE:-http://127.0.0.1:18099}
USER_NAME=${USER_NAME:-admin}
PASSWORD=${PASSWORD:-Admin@123456}
FAIL=0

say() { printf '\n===== %s =====\n' "$*"; }

# $1=label $2=expected_http $3=method $4=path [$5=json body]
call() {
    local label=$1 expect=$2 method=$3 path=$4 body=${5:-}
    local out code
    if [ -n "$body" ]; then
        out=$(curl -s -w '\n%{http_code}' -X "$method" "$BASE$path" \
              -H "Authorization: Bearer $TOKEN" -H 'Content-Type: application/json' -d "$body")
    else
        out=$(curl -s -w '\n%{http_code}' -X "$method" "$BASE$path" \
              -H "Authorization: Bearer $TOKEN")
    fi
    code=$(printf '%s' "$out" | tail -1)
    local payload
    payload=$(printf '%s' "$out" | sed '$d')
    printf -- '--- %s\nHTTP %s (期望 %s)\n%s\n' "$label" "$code" "$expect" "$payload"
    if [ "$code" != "$expect" ]; then
        printf '!!! %s 状态码不符：实际 %s 期望 %s\n' "$label" "$code" "$expect"
        FAIL=1
    fi
    LAST_PAYLOAD=$payload
}

say "① 登录（admin / Admin@123456）→ 首登强制改密（bootstrap 种子的 must_change_password）"
PASSWORD_ALT=${PASSWORD_ALT:-Admin@20261106}   # 上一轮跑过时，admin 的口令已经是这个
LOGIN=""
for try in "$PASSWORD" "$PASSWORD_ALT"; do
    LOGIN=$(curl -s -X POST "$BASE/api/system/auth/login" -H 'Content-Type: application/json' \
            -d "{\"username\":\"$USER_NAME\",\"password\":\"$try\"}")
    if [ "$(printf '%s' "$LOGIN" | jq -r '.code')" = "0" ]; then
        PASSWORD="$try"
        break
    fi
done
TOKEN=$(printf '%s' "$LOGIN" | jq -r '.data.access_token')
printf 'code=%s token_len=%s must_change_password=%s\n' \
  "$(printf '%s' "$LOGIN" | jq -r '.code')" "${#TOKEN}" \
  "$(printf '%s' "$LOGIN" | jq -r '.data.must_change_password')"
[ "$TOKEN" != "null" ] && [ -n "$TOKEN" ] || { echo '!!! 登录失败'; exit 1; }

# 首登必须先改密，否则 session_guard 对**所有**业务接口返回
#   403 {"msg":"首次登录必须先修改初始密码"}
# 这是 bootstrap 的既定行为（不是本卡的），实测第一步就撞上了 —— 这里显式改密再重登。
if [ "$(printf '%s' "$LOGIN" | jq -r '.data.must_change_password')" = "true" ]; then
    PASSWORD_NEW="Admin@20261106"
    CHG=$(curl -s -w '\n%{http_code}' -X PUT "$BASE/api/system/auth/password" \
          -H "Authorization: Bearer $TOKEN" -H 'Content-Type: application/json' \
          -d "{\"old_password\":\"$PASSWORD\",\"new_password\":\"$PASSWORD_NEW\"}")
    printf '改密 HTTP %s %s\n' "$(printf '%s' "$CHG" | tail -1)" "$(printf '%s' "$CHG" | sed '$d')"
    LOGIN=$(curl -s -X POST "$BASE/api/system/auth/login" -H 'Content-Type: application/json' \
            -d "{\"username\":\"$USER_NAME\",\"password\":\"$PASSWORD_NEW\"}")
    TOKEN=$(printf '%s' "$LOGIN" | jq -r '.data.access_token')
    PASSWORD="$PASSWORD_NEW"
    printf '重新登录 code=%s token_len=%s must_change_password=%s\n' \
      "$(printf '%s' "$LOGIN" | jq -r '.code')" "${#TOKEN}" \
      "$(printf '%s' "$LOGIN" | jq -r '.data.must_change_password')"
    [ -n "$TOKEN" ] && [ "$TOKEN" != "null" ] || { echo '!!! 改密后登录失败'; exit 1; }
fi

say "② 建客户（SO-02 接口，额度 1000）"
CUST_CODE="E2E-SO5-$$"
call "POST /business/erp/customer" 200 POST /api/business/erp/customer \
  "{\"code\":\"$CUST_CODE\",\"name\":\"SO-05 实测客户\",\"credit_limit\":1000,\"status\":\"ACTIVE\"}"
CUST_ID=$(printf '%s' "$LAST_PAYLOAD" | jq -r '.data.id')
printf 'customer_id=%s\n' "$CUST_ID"

say "③ 建单：0 行明细 → 400"
call "POST /sales-order（空明细）" 400 POST /api/business/erp/sales-order \
  "{\"customer_id\":\"$CUST_ID\",\"items\":[]}"

say "④ 建单：正常 2 行（金额由服务端算：2×100 + 13% 税 = 226）"
call "POST /sales-order" 200 POST /api/business/erp/sales-order \
  "{\"customer_id\":\"$CUST_ID\",\"order_date\":\"2026-11-06\",\"delivery_date\":\"2026-11-20\",\"remark\":\"curl 实测\",\"items\":[{\"material_id\":\"d5050000-0000-4000-8000-000000000001\",\"qty\":2,\"price\":100,\"tax_rate\":13},{\"material_id\":\"d5050000-0000-4000-8000-000000000002\",\"qty\":5,\"price\":1,\"tax_rate\":0}]}"
SO_ID=$(printf '%s' "$LAST_PAYLOAD" | jq -r '.data.id')
SO_NO=$(printf '%s' "$LAST_PAYLOAD" | jq -r '.data.so_no')
printf 'so_id=%s so_no=%s status=%s total_with_tax=%s\n' "$SO_ID" "$SO_NO" \
  "$(printf '%s' "$LAST_PAYLOAD" | jq -r '.data.status')" \
  "$(printf '%s' "$LAST_PAYLOAD" | jq -r '.data.total_with_tax')"

say "⑤ 详情"
call "GET /sales-order/{id}" 200 GET "/api/business/erp/sales-order/$SO_ID"

say "⑥ 改单（明细整单替换为 1 行）"
call "PUT /sales-order/{id}" 200 PUT "/api/business/erp/sales-order/$SO_ID" \
  "{\"remark\":\"改单：单行 3×50\",\"items\":[{\"material_id\":\"d5050000-0000-4000-8000-000000000001\",\"qty\":3,\"price\":50,\"tax_rate\":13}]}"

say "⑦ 分页（单号模糊 + 客户精确）"
call "GET /sales-order/page" 200 GET "/api/business/erp/sales-order/page?so_no=$SO_NO&customer_id=$CUST_ID&page=1&page_size=10"

say "⑧ 非法迁移：DRAFT → APPROVED（跳步）→ 409"
call "PATCH /sales-order/{id}/status（跳步）" 409 PATCH "/api/business/erp/sales-order/$SO_ID/status" \
  '{"target_status":"APPROVED"}'

say "⑨ 域外状态值：SHIPPED（技术架构旧稿）→ 400"
call "PATCH /sales-order/{id}/status（SHIPPED）" 400 PATCH "/api/business/erp/sales-order/$SO_ID/status" \
  '{"target_status":"SHIPPED"}'

say "⑩ 合法边：DRAFT → SUBMITTED"
call "PATCH /sales-order/{id}/status（提交）" 200 PATCH "/api/business/erp/sales-order/$SO_ID/status" \
  '{"target_status":"SUBMITTED"}'

say "⑪ 提交后改单 → 409（只有 DRAFT 能改）"
call "PUT /sales-order/{id}（非草稿）" 409 PUT "/api/business/erp/sales-order/$SO_ID" \
  '{"remark":"改不动"}'

say "⑫ SUBMITTED → CANCELLED 缺原因 → 400"
call "PATCH /status（缺原因）" 400 PATCH "/api/business/erp/sales-order/$SO_ID/status" \
  '{"target_status":"CANCELLED"}'

say "⑬ SUBMITTED → APPROVED"
call "PATCH /status（审批通过）" 200 PATCH "/api/business/erp/sales-order/$SO_ID/status" \
  '{"target_status":"APPROVED"}'

say "⑭ APPROVED → CANCELLED 带原因 → 200"
call "PATCH /status（审批后作废）" 200 PATCH "/api/business/erp/sales-order/$SO_ID/status" \
  '{"target_status":"CANCELLED","reason":"curl 实测：客户改主意"}'

say "⑮ 终态 CANCELLED → SUBMITTED → 409（终态不可出边）"
call "PATCH /status（终态出边）" 409 PATCH "/api/business/erp/sales-order/$SO_ID/status" \
  '{"target_status":"SUBMITTED"}'

say "⑯ 客户冻结 → 建单 409"
call "PUT /customer/{id}（冻结）" 200 PUT "/api/business/erp/customer/$CUST_ID" '{"status":"FROZEN"}'
call "POST /sales-order（冻结客户）" 409 POST /api/business/erp/sales-order \
  "{\"customer_id\":\"$CUST_ID\",\"items\":[{\"material_id\":\"d5050000-0000-4000-8000-000000000001\",\"qty\":1,\"price\":10,\"tax_rate\":0}]}"
call "PUT /customer/{id}（恢复）" 200 PUT "/api/business/erp/customer/$CUST_ID" '{"status":"ACTIVE"}'

say "⑰ 信用额度：额度改成 50，建单 113 → 提交 409"
call "PUT /customer/{id}（额度 50）" 200 PUT "/api/business/erp/customer/$CUST_ID" '{"credit_limit":50}'
call "POST /sales-order（113）" 200 POST /api/business/erp/sales-order \
  "{\"customer_id\":\"$CUST_ID\",\"items\":[{\"material_id\":\"d5050000-0000-4000-8000-000000000001\",\"qty\":1,\"price\":100,\"tax_rate\":13}]}"
TIGHT_ID=$(printf '%s' "$LAST_PAYLOAD" | jq -r '.data.id')
call "PATCH /status（超额提交）" 409 PATCH "/api/business/erp/sales-order/$TIGHT_ID/status" \
  '{"target_status":"SUBMITTED"}'

say "⑱ 提交成功后释放额度：额度调回 0（不控额度）再提交 → 200"
call "PUT /customer/{id}（额度 0 = 不控）" 200 PUT "/api/business/erp/customer/$CUST_ID" '{"credit_limit":0}'
call "PATCH /status（不控额度提交）" 200 PATCH "/api/business/erp/sales-order/$TIGHT_ID/status" \
  '{"target_status":"SUBMITTED"}'

if [ "$FAIL" = "0" ]; then
    printf '\n=== 全部断言通过（含 409 非法迁移 / 400 域外值 / 400 缺原因 / 409 守卫）===\n'
else
    printf '\n!!! 有用例不符合预期，见上方 !!! 行\n'
fi
exit "$FAIL"

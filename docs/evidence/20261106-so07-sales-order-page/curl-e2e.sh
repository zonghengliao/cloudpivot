#!/usr/bin/env bash
# ============================================================================
# SO-07（t_1ae4c684）销售订单前端页面 —— 真实 token curl 实测
#
# 跑法（后端实例在 127.0.0.1:18109，库 cloudpivot_so07a = cloudpivot_so06 的克隆，
# 启动时自动应用 V0282）：
#   bash docs/evidence/20261106-so07-sales-order-page/curl-e2e.sh
#
# 覆盖（本卡交付的前端所依赖的每一个接口 + 每个错误口径）：
#   ① 登录（首登强制改密兜底）
#   ② 建客户 → 建单（2 行明细，13% 税率）→ 断言服务端算的金额三项恒等
#   ③ 列表分页：单号模糊 + 状态精确过滤（`''` 不发）
#   ④ 详情：**草稿的 approval_instance_id 必须是 null**（审批进度区块不渲染的唯一依据）
#   ⑤ 内控关 → 提交审批 → SUBMITTED + 详情 approval_instance_id 非空（本卡新增的出参）→ 流程图节点可读
#   ⑥ 撤回走审批引擎（`POST /business/flow/instance/{id}/withdraw`）→ 回 DRAFT + 实例 id 清空
#   ⑦ 人工 PATCH 审批结论（APPROVED / DRAFT）→ 400（审批结论只由审批中心写）
#   ⑧ 审批中作废：不给原因 400 / 给原因 200 → CANCELLED（`SUBMITTED→CANCELLED` 是需要原因的边）
#   ⑨ 编辑只在 DRAFT 生效：CANCELLED 上 PUT → 409
# ============================================================================
set -uo pipefail
BASE=${BASE:-http://127.0.0.1:18109}
DB=${ERP_DB:-cloudpivot_so07a}
USER_NAME=${USER_NAME:-admin}
PASSWORD=${PASSWORD:-Admin@123456}
PASSWORD_ALT=${PASSWORD_ALT:-Admin@20261106}
FAIL=0
TOKEN=""

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

# 内控开关：psql 改 sys_config + 调重载接口（进程内快照 TTL 60s，重载立刻生效）
set_separation() {
    local value=$1
    PGPASSWORD=alun psql -h 127.0.0.1 -U alun -d "$DB" -Atc \
      "UPDATE sys_config SET config_value = '$value' WHERE config_key = 'erp.separation_of_duties.approve'" \
      > /dev/null
    call "POST /support/config/reload（separation=$value）" 200 POST /api/support/config/reload '{}'
    printf '  erp.separation_of_duties.approve = %s\n' "$value" >&2
}

# ---------------------------------------------------------------------------
say "① 登录（$USER_NAME）"
LOGIN=""
for try in "$PASSWORD" "$PASSWORD_ALT"; do
    LOGIN=$(curl -s -X POST "$BASE/api/system/auth/login" -H 'Content-Type: application/json' \
            -d "{\"username\":\"$USER_NAME\",\"password\":\"$try\"}")
    if [ "$(printf '%s' "$LOGIN" | jq -r '.code')" = "0" ]; then PASSWORD="$try"; break; fi
done
TOKEN=$(printf '%s' "$LOGIN" | jq -r '.data.access_token')
printf 'code=%s token_len=%s must_change_password=%s\n' \
  "$(printf '%s' "$LOGIN" | jq -r '.code')" "${#TOKEN}" \
  "$(printf '%s' "$LOGIN" | jq -r '.data.must_change_password')" >&2
[ -n "$TOKEN" ] && [ "$TOKEN" != "null" ] || { echo '!!! 登录失败' >&2; exit 1; }

if [ "$(printf '%s' "$LOGIN" | jq -r '.data.must_change_password')" = "true" ]; then
    PASSWORD_NEW="Admin@20261106"
    CHG=$(curl -s -w '\n%{http_code}' -X PUT "$BASE/api/system/auth/password" \
          -H "Authorization: Bearer $TOKEN" -H 'Content-Type: application/json' \
          -d "{\"old_password\":\"$PASSWORD\",\"new_password\":\"$PASSWORD_NEW\"}")
    printf '改密 HTTP %s\n' "$(printf '%s' "$CHG" | tail -1)" >&2
    LOGIN=$(curl -s -X POST "$BASE/api/system/auth/login" -H 'Content-Type: application/json' \
            -d "{\"username\":\"$USER_NAME\",\"password\":\"$PASSWORD_NEW\"}")
    TOKEN=$(printf '%s' "$LOGIN" | jq -r '.data.access_token')
    [ -n "$TOKEN" ] && [ "$TOKEN" != "null" ] || { echo '!!! 改密后登录失败' >&2; exit 1; }
    printf '重新登录 token_len=%s\n' "${#TOKEN}" >&2
fi

# ---------------------------------------------------------------------------
say "② 建客户 + 建单（2 行明细 / 13% 税率）"
CUST_CODE="E2E-SO7-$$-$(date +%s)"
call "POST /business/erp/customer" 200 POST /api/business/erp/customer \
  "{\"code\":\"$CUST_CODE\",\"name\":\"SO-07 前端实测客户\",\"credit_limit\":0,\"status\":\"ACTIVE\"}"
CUST_ID=$(printf '%s' "$LAST_PAYLOAD" | jq -r '.data.id')
printf 'customer_id=%s\n' "$CUST_ID" >&2

# 行 1：2 × 100 @13%  → 200 / 26
# 行 2：3 × 0.3333 @13% → 0.9999 / 0.13（4 位定标）
call "POST /sales-order（2 行明细）" 200 POST /api/business/erp/sales-order \
  "{\"customer_id\":\"$CUST_ID\",\"order_date\":\"2026-11-06\",\"delivery_date\":\"2026-11-10\",\"remark\":\"SO-07 建单实测\",\"items\":[{\"material_id\":\"d5050000-0000-4000-8000-000000000001\",\"qty\":2,\"price\":100,\"tax_rate\":13},{\"material_id\":\"d5050000-0000-4000-8000-000000000002\",\"qty\":3,\"price\":0.3333,\"tax_rate\":13}]}"
ORDER_ID=$(printf '%s' "$LAST_PAYLOAD" | jq -r '.data.id')
ORDER_NO=$(printf '%s' "$LAST_PAYLOAD" | jq -r '.data.so_no')
printf '  → so_id=%s so_no=%s\n' "$ORDER_ID" "$ORDER_NO" >&2

check_total() {
    local label=$1
    local res so_no status amount tax with_tax sum
    res=$(call "GET /sales-order/{id}（$label）" 200 GET "/api/business/erp/sales-order/$ORDER_ID")
    so_no=$(printf '%s' "$LAST_PAYLOAD" | jq -r '.data.so_no')
    status=$(printf '%s' "$LAST_PAYLOAD" | jq -r '.data.status')
    amount=$(printf '%s' "$LAST_PAYLOAD" | jq -r '.data.total_amount')
    tax=$(printf '%s' "$LAST_PAYLOAD" | jq -r '.data.total_tax')
    with_tax=$(printf '%s' "$LAST_PAYLOAD" | jq -r '.data.total_with_tax')
    printf '  status=%s amount=%s tax=%s with_tax=%s\n' "$status" "$amount" "$tax" "$with_tax" >&2
    sum=$(printf '%s %s\n' "$amount" "$tax" | awk '{printf "%.4f", $1 + $2}')
    if [ "$sum" = "$(printf '%.4f' "$with_tax")" ]; then
        ok "$label：total_with_tax = total_amount + total_tax（$with_tax）"
    else
        bad "$label：金额恒等被破坏（$amount + $tax ≠ $with_tax）"
    fi
    echo "$status"
}

STATUS=$(check_total "建单后")
[ "$STATUS" = "DRAFT" ] && ok "建单落在 DRAFT" || bad "建单状态不是 DRAFT：$STATUS"

# 明细金额由服务端算（前端只发 qty/price/tax_rate）
LINE1_AMOUNT=$(printf '%s' "$LAST_PAYLOAD" | jq -r '.data.items[0].amount')
LINE1_TAX=$(printf '%s' "$LAST_PAYLOAD" | jq -r '.data.items[0].tax_amount')
LINE2_AMOUNT=$(printf '%s' "$LAST_PAYLOAD" | jq -r '.data.items[1].amount')
LINE2_TAX=$(printf '%s' "$LAST_PAYLOAD" | jq -r '.data.items[1].tax_amount')
printf '  行 1 amount=%s tax=%s；行 2 amount=%s tax=%s\n' \
  "$LINE1_AMOUNT" "$LINE1_TAX" "$LINE2_AMOUNT" "$LINE2_TAX" >&2
[ "$(printf '%.4f' "$LINE1_AMOUNT")" = "200.0000" ] && [ "$(printf '%.4f' "$LINE1_TAX")" = "26.0000" ] \
  && ok "行 1 = 2 × 100 @13% → 200.0000 / 26.0000" || bad "行 1 金额不符"
[ "$(printf '%.4f' "$LINE2_AMOUNT")" = "0.9999" ] && [ "$(printf '%.4f' "$LINE2_TAX")" = "0.1300" ] \
  && ok "行 2 = 3 × 0.3333 @13% → 0.9999 / 0.1300（4 位定标）" || bad "行 2 金额不符"

# 草稿的审批实例 id 必须为空（前端据此决定画不画审批进度）
DRAFT_INS=$(printf '%s' "$LAST_PAYLOAD" | jq -r '.data.approval_instance_id')
[ "$DRAFT_INS" = "null" ] && ok "草稿 approval_instance_id = null（不画审批进度）" || bad "草稿竟然挂了实例：$DRAFT_INS"

# ---------------------------------------------------------------------------
say "③ 列表分页 / 过滤"
call "GET /sales-order/page?so_no=$ORDER_NO" 200 GET "/api/business/erp/sales-order/page?so_no=$ORDER_NO&page=1&page_size=20"
LIST_TOTAL=$(printf '%s' "$LAST_PAYLOAD" | jq -r '.data.total')
LIST_FIRST=$(printf '%s' "$LAST_PAYLOAD" | jq -r '.data.list[0].so_no')
[ "$LIST_TOTAL" -ge 1 ] && [ "$LIST_FIRST" = "$ORDER_NO" ] \
  && ok "单号模糊过滤命中 $ORDER_NO（total=$LIST_TOTAL）" || bad "单号过滤没命中"
# 列表行的 items 恒为空、实例 id 恒为 null（前端不在列表画明细 / 进度）
LIST_ITEMS=$(printf '%s' "$LAST_PAYLOAD" | jq -r '.data.list[0].items | length')
LIST_INS=$(printf '%s' "$LAST_PAYLOAD" | jq -r '.data.list[0].approval_instance_id')
[ "$LIST_ITEMS" = "0" ] && ok "列表行 items = []（明细只在详情）" || bad "列表行竟然带明细"
[ "$LIST_INS" = "null" ] && ok "列表行 approval_instance_id = null" || bad "列表行带了实例 id"

call "GET /sales-order/page?status=DRAFT" 200 GET "/api/business/erp/sales-order/page?status=DRAFT&page=1&page_size=20"
printf '  状态=DRAFT 命中 %s 条\n' "$(printf '%s' "$LAST_PAYLOAD" | jq -r '.data.total')" >&2
call "GET /sales-order/page?status=WHATEVER（非法状态）" 400 GET "/api/business/erp/sales-order/page?status=WHATEVER"
call "GET /sales-order/page?status=（空串）" 200 GET "/api/business/erp/sales-order/page?status=&page=1&page_size=20"

# ---------------------------------------------------------------------------
say "④ 编辑（DRAFT 可改：整单覆盖明细）"
call "PUT /sales-order/{id}（改交期 + 换明细）" 200 PUT "/api/business/erp/sales-order/$ORDER_ID" \
  "{\"delivery_date\":\"2026-11-12\",\"remark\":\"SO-07 改单实测\",\"items\":[{\"material_id\":\"d5050000-0000-4000-8000-000000000001\",\"qty\":4,\"price\":50,\"tax_rate\":13}]}"
NEW_TOTAL=$(printf '%s' "$LAST_PAYLOAD" | jq -r '.data.total_with_tax')
NEW_LINES=$(printf '%s' "$LAST_PAYLOAD" | jq -r '.data.items | length')
[ "$(printf '%.4f' "$NEW_TOTAL")" = "226.0000" ] && ok "改单后价税合计 226.0000（4×50 + 13%）" || bad "改单金额不符：$NEW_TOTAL"
[ "$NEW_LINES" = "1" ] && ok "明细整单替换为 1 行" || bad "明细没被整单替换：$NEW_LINES 行"

# ---------------------------------------------------------------------------
say "⑤ 提交审批（内控关，脚本留痕）→ SUBMITTED + 实例 id 非空"
set_separation false
call "POST /submit-approval" 200 POST "/api/business/erp/sales-order/$ORDER_ID/submit-approval" '{"reason":"SO-07 前端实测提交"}'
SUB_STATUS=$(printf '%s' "$LAST_PAYLOAD" | jq -r '.data.status')
INSTANCE_ID=$(printf '%s' "$LAST_PAYLOAD" | jq -r '.data.approval_instance_id')
[ "$SUB_STATUS" = "SUBMITTED" ] && ok "提交后 SUBMITTED" || bad "提交后状态 $SUB_STATUS"
[ "$INSTANCE_ID" != "null" ] && [ -n "$INSTANCE_ID" ] \
  && ok "提交出参带回 approval_instance_id=$INSTANCE_ID（本卡新增字段）" || bad "提交出参没有实例 id"

call "GET /sales-order/{id}（审批中）" 200 GET "/api/business/erp/sales-order/$ORDER_ID"
[ "$(printf '%s' "$LAST_PAYLOAD" | jq -r '.data.approval_instance_id')" = "$INSTANCE_ID" ] \
  && ok "详情 approval_instance_id 与提交时一致" || bad "详情实例 id 不一致"

call "GET /business/flow/instance/{id}（审批进度数据源）" 200 GET "/api/business/flow/instance/$INSTANCE_ID"
NODE_COUNT=$(printf '%s' "$LAST_PAYLOAD" | jq -r '.data.nodes | length')
ACTIVE_NODES=$(printf '%s' "$LAST_PAYLOAD" | jq -r '[.data.nodes[] | select(.status=="ACTIVE")] | length')
printf '  节点数=%s 进行中节点=%s\n' "$NODE_COUNT" "$ACTIVE_NODES" >&2
[ "$NODE_COUNT" -ge 2 ] && ok "FlowProgress 有节点可画（$NODE_COUNT 个）" || bad "实例没有节点（进度条空白）"
printf '%s' "$LAST_PAYLOAD" | jq -r '.data.nodes[] | "    node=\(.node_key) name=\(.node_name) status=\(.status)"' >&2

# 重复提交 → 409（审批中不能再提交）
call "POST /submit-approval（重复提交）" 409 POST "/api/business/erp/sales-order/$ORDER_ID/submit-approval" '{}'

# ---------------------------------------------------------------------------
say "⑥ 人工 PATCH 审批结论 → 400（审批结论只由审批中心写）"
for target in APPROVED DRAFT; do
    call "PATCH /status {target_status:$target}" 400 PATCH "/api/business/erp/sales-order/$ORDER_ID/status" \
      "{\"target_status\":\"$target\"}"
done

# ---------------------------------------------------------------------------
say "⑦ 撤回审批（走审批引擎，不是 PATCH 状态）"
call "POST /business/flow/instance/{id}/withdraw" 200 POST "/api/business/flow/instance/$INSTANCE_ID/withdraw" \
  '{"reason":"SO-07 撤回实测"}'
call "GET /sales-order/{id}（撤回后）" 200 GET "/api/business/erp/sales-order/$ORDER_ID"
WD_STATUS=$(printf '%s' "$LAST_PAYLOAD" | jq -r '.data.status')
WD_INS=$(printf '%s' "$LAST_PAYLOAD" | jq -r '.data.approval_instance_id')
[ "$WD_STATUS" = "DRAFT" ] && ok "撤回后回 DRAFT" || bad "撤回后状态 $WD_STATUS"
[ "$WD_INS" = "null" ] && ok "撤回后实例 id 清空（进度区块自动消失）" || bad "撤回后实例 id 还在：$WD_INS"

# ---------------------------------------------------------------------------
say "⑧ 审批中作废：不给原因 400 / 给原因 200（需要原因的边）"
call "POST /submit-approval（撤回后重新提交）" 200 POST "/api/business/erp/sales-order/$ORDER_ID/submit-approval" '{}'
INSTANCE_ID2=$(printf '%s' "$LAST_PAYLOAD" | jq -r '.data.approval_instance_id')
printf '  新实例 id=%s（与上一轮不同：%s）\n' "$INSTANCE_ID2" "$([ "$INSTANCE_ID2" != "$INSTANCE_ID" ] && echo yes || echo no)" >&2

call "PATCH /status {CANCELLED}（无原因）" 400 PATCH "/api/business/erp/sales-order/$ORDER_ID/status" \
  '{"target_status":"CANCELLED"}'
call "PATCH /status {CANCELLED}（带原因）" 200 PATCH "/api/business/erp/sales-order/$ORDER_ID/status" \
  '{"target_status":"CANCELLED","reason":"SO-07 审批中作废实测"}'
CANCEL_STATUS=$(printf '%s' "$LAST_PAYLOAD" | jq -r '.data.status')
[ "$CANCEL_STATUS" = "CANCELLED" ] && ok "审批中作废 → CANCELLED（同一事务撤掉审批实例）" || bad "作废后状态 $CANCEL_STATUS"

# 审批中作废后，那个实例应该已经被撤（不再有孤儿待办）
call "GET /business/flow/instance/{id}（作废后）" 200 GET "/api/business/flow/instance/$INSTANCE_ID2"
printf '  实例状态=%s\n' "$(printf '%s' "$LAST_PAYLOAD" | jq -r '.data.status')" >&2

# ---------------------------------------------------------------------------
say "⑨ 终态护栏"
call "PUT /sales-order/{id}（CANCELLED 上改单）" 409 PUT "/api/business/erp/sales-order/$ORDER_ID" \
  "{\"remark\":\"终态不该能改\"}"
call "PATCH /status {SUBMITTED}（CANCELLED → SUBMITTED 不是合法边）" 409 PATCH \
  "/api/business/erp/sales-order/$ORDER_ID/status" '{"target_status":"SUBMITTED"}'
call "GET /sales-order/{id}（不存在）" 404 GET "/api/business/erp/sales-order/d5050000-0000-4000-8000-0000000009ff"

# ---------------------------------------------------------------------------
printf '\n===== 结果：%s =====\n' "$([ "$FAIL" = 0 ] && echo '全部断言通过' || echo '有断言失败')" >&2
exit "$FAIL"

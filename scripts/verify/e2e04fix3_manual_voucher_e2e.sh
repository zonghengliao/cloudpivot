#!/usr/bin/env bash
# E2E04-FIX-3 ① 真机验收（145 dev，真 token curl）
#
# ① 手工凭证记到**已结账**期间（2026-09 CLOSED）→ 必须是 409 + code 10003 + 文案点名「已结账」
# ② 建单落库是 **DRAFT**（不是 POSTED）—— 用未结账期间（2026-11 OPEN）建一张再查状态，
#    然后把它删掉（本脚本自己的夹具，用 remark 精确定位，不做通配删除）
set -uo pipefail

BASE="${BASE:-http://127.0.0.1:8033}"
ADMIN_PW="${NIK_ADMIN_PASSWORD:-Admin@123456}"
DB_URL="${DB_URL:-}"
PASS=0; FAIL=0; FAILED_NAMES=()

check() { # check <名> <实际> <期望>
  if [ "$2" = "$3" ]; then echo "[PASS] $1  ← $2"; PASS=$((PASS+1));
  else echo "[FAIL] $1  ← 实际 $2 / 期望 $3"; FAIL=$((FAIL+1)); FAILED_NAMES+=("$1"); fi
}

say() { printf '\n%s\n' "$*"; }

say "登录（admin）"
LOGIN=$(curl -s -m 30 -X POST "$BASE/api/system/auth/login" \
  -H 'Content-Type: application/json' \
  -d "{\"username\":\"admin\",\"password\":\"$ADMIN_PW\"}")
TOKEN=$(printf '%s' "$LOGIN" | jq -r '.data.access_token // .data.token // empty')
if [ -z "$TOKEN" ]; then echo "[FAIL] 登录失败：$(printf '%s' "$LOGIN" | head -c 300)"; exit 1; fi
echo "[PASS] token 已取得（${#TOKEN} 字符）"

ACC_D="d1370004-0000-4000-8000-000000000001"   # 1405 库存商品
ACC_C="d1370004-0000-4000-8000-000000000002"   # 2202 应付账款

post_voucher() { # post_voucher <date> <summary> <remark>
  curl -s -m 30 -o /tmp/e2e04fix3_body.json -w '%{http_code}' \
    -X POST "$BASE/api/business/erp/voucher" \
    -H "Authorization: Bearer $TOKEN" -H 'Content-Type: application/json' \
    -d "{\"voucher_date\":\"$1\",\"summary\":\"$2\",\"remark\":\"$3\",\"entries\":[
          {\"account_id\":\"$ACC_D\",\"debit\":\"10.00\"},
          {\"account_id\":\"$ACC_C\",\"credit\":\"10.00\"}]}"
}

say "① 记到已结账期间 2026-09（封账中）→ 期望 409 + 10003"
HTTP=$(post_voucher "2026-09-10" "E2E04FIX3 已结账期间记账" "E2E04-FIX-3 真机验收")
CODE=$(jq -r '.code // empty' /tmp/e2e04fix3_body.json 2>/dev/null)
MSG=$(jq -r '.msg // empty' /tmp/e2e04fix3_body.json 2>/dev/null)
check "HTTP 状态" "$HTTP" "409"
check "业务码" "$CODE" "10003"
case "$MSG" in
  *已结账*) echo "[PASS] 文案点名「已结账」← $MSG"; PASS=$((PASS+1)) ;;
  *) echo "[FAIL] 文案未提「已结账」← $MSG"; FAIL=$((FAIL+1)); FAILED_NAMES+=("文案");;
esac
case "$MSG" in
  *2026-09*) echo "[PASS] 文案点名期间 2026-09"; PASS=$((PASS+1)) ;;
  *) echo "[FAIL] 文案未点名期间 ← $MSG"; FAIL=$((FAIL+1)); FAILED_NAMES+=("期间");;
esac

say "② 记到未结账期间 2026-11（OPEN）→ 期望 200，且落库是 DRAFT"
REMARK="E2E04-FIX-3 真机验收（可删）"
HTTP2=$(post_voucher "2026-11-10" "E2E04FIX3 DRAFT 落库确认" "$REMARK")
STATUS=$(jq -r '.data.status // empty' /tmp/e2e04fix3_body.json 2>/dev/null)
VNO=$(jq -r '.data.voucher_no // empty' /tmp/e2e04fix3_body.json 2>/dev/null)
check "HTTP 状态" "$HTTP2" "200"
check "落库状态（不是 POSTED）" "$STATUS" "DRAFT"
echo "  · 新凭证号：${VNO:-<空>}"

if [ -n "$DB_URL" ]; then
  say "清理：删掉本脚本建的那张（按 remark 精确定位，只删 DRAFT）"
  export PGPASSWORD="${PGPASSWORD:-}"
  DEL=$(psql "$DB_URL" -qtA -c "WITH d AS (
        DELETE FROM biz_voucher
         WHERE remark = '$REMARK' AND status = 'DRAFT'
      RETURNING voucher_no)
      SELECT coalesce(string_agg(voucher_no, ','), '') FROM d;")
  echo "  · 已删：${DEL:-<无命中>}"
  LEFT=$(psql "$DB_URL" -qtA -c "SELECT count(*) FROM biz_voucher WHERE remark = '$REMARK';")
  check "夹具零残留" "$LEFT" "0"
fi

say "--------------------------------------------------------------"
echo "PASS=$PASS FAIL=$FAIL"
if [ "$FAIL" -ne 0 ]; then printf '失败项：%s\n' "${FAILED_NAMES[*]}"; exit 1; fi
exit 0

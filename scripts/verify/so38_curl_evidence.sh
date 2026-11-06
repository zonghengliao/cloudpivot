#!/usr/bin/env bash
# ============================================================================
# SO-38 信用额度占用/释放/重算 · 真实 token curl 证据
#
# 覆盖本卡**接口面**的三件事：
#   ① 客户详情接口暴露 credit_used / credit_available（V0313 新列 + S9 口径）
#   ② 漂移检测：绕过业务写库改 credit_used → S9 巡检转 BLOCKER（真实 python 巡检）
#   ③ 兜底重算：POST /support/job/{id}/trigger 跑 erp_credit_recompute →
#      修正漂移 + 记 biz_credit_log(cause=RECOMPUTE) + S9 回到绿
#
# 前置：bash scripts/verify/so38_instance.sh up（库 cloudpivot_so38 / 端口 18245）
# 用法：bash scripts/verify/so38_curl_evidence.sh
# 输出：逐条 PASS/FAIL；末尾 ALL CHECKS PASSED (n)，任一失败 exit 1
# ============================================================================
set -uo pipefail
BASE=${SO38_BASE:-http://127.0.0.1:18245}
DB=${SO38_DB:-cloudpivot_so38}
BODY_FILE=$(mktemp)
PASS=0
FAIL=0

psqlv() { PGPASSWORD=alun psql -h 127.0.0.1 -U alun -d "$DB" -tAc "$1"; }

LOGIN=$(curl -s -X POST "$BASE/api/system/auth/login" -H 'Content-Type: application/json' \
  -d '{"username":"admin","password":"Admin@123456"}')
TOKEN=$(printf '%s' "$LOGIN" | jq -r '.data.access_token // empty')
if [ -z "$TOKEN" ]; then
  echo "FATAL: 登录失败：$LOGIN"
  exit 1
fi

BODY=''
STATUS=''
req() {
  local method=$1 path=$2 body=${3:-}
  if [ -n "$body" ]; then
    STATUS=$(curl -s -o "$BODY_FILE" -w '%{http_code}' -X "$method" "$BASE$path" \
      -H "Authorization: Bearer $TOKEN" -H 'Content-Type: application/json' -d "$body")
  else
    STATUS=$(curl -s -o "$BODY_FILE" -w '%{http_code}' -X "$method" "$BASE$path" \
      -H "Authorization: Bearer $TOKEN")
  fi
  BODY=$(cat "$BODY_FILE")
}

jv() { printf '%s' "$BODY" | jq -r "$1"; }

check() {
  if [ "$2" = "$3" ]; then
    PASS=$((PASS + 1)); printf 'PASS  %-58s = %s\n' "$1" "$3"
  else
    FAIL=$((FAIL + 1)); printf 'FAIL  %-58s expected=%s actual=%s\n' "$1" "$2" "$3"
  fi
}

echo "== ① 客户详情暴露 credit_used / credit_available =="
# 取一个有有效订单占用的客户（夹具里总有一个）
CUST=$(psqlv "select c.id from biz_customer c join biz_sales_order o on o.customer_id = c.id
               where o.deleted_at is null and o.status in ('APPROVED','PARTIAL_OUT','OUTBOUND')
                 and c.deleted_at is null and c.credit_limit > 0 limit 1")
if [ -z "$CUST" ]; then
  # 没有控额度的客户就自己造一个（不依赖夹具）
  CUST=$(psqlv "select id from biz_customer where deleted_at is null and credit_limit > 0 limit 1")
fi
echo "使用客户: $CUST"
req GET "/api/business/erp/customer/$CUST"
check "GET customer HTTP" "200" "$STATUS"
LIMIT=$(jv '.data.credit_limit')
USED=$(jv '.data.credit_used')
AVAIL=$(jv '.data.credit_available')
echo "  credit_limit=$LIMIT credit_used=$USED credit_available=$AVAIL"
if [ "$USED" != "null" ] && [ -n "$USED" ]; then
  PASS=$((PASS + 1)); printf 'PASS  %-58s = %s\n' "客户详情带 credit_used（V0313 新列）" "$USED"
else
  FAIL=$((FAIL + 1)); printf 'FAIL  %-58s\n' "客户详情缺 credit_used 字段"
fi
WANT=$(psqlv "select greatest(0, credit_limit - credit_used)::text from biz_customer where id = '$CUST'")
check "credit_available = limit - used" "$WANT" "$AVAIL"

echo
echo "== ② 漂移检测：绕过业务写库改 credit_used → S9 应转 BLOCKER =="
DB_USED_BEFORE=$(psqlv "select credit_used::text from biz_customer where id = '$CUST'")
psqlv "update biz_customer set credit_used = 999 where id = '$CUST'" > /dev/null
echo "  注入漂移：credit_used → 999（真值 $DB_USED_BEFORE）"
S9_RED=$(NIK_DATABASE_HOST=127.0.0.1 NIK_DATABASE_PORT=5432 NIK_DATABASE_USER=alun \
  NIK_DATABASE_NAME="$DB" NIK_DATABASE_PASSWORD=alun \
  python3 /opt/cloudpivot/scripts/verify/erp_invariants.py --group 销售 2>&1 | grep "S9")
printf '%s\n' "$S9_RED"
case "$S9_RED" in
  *"🚨"*) PASS=$((PASS + 1)); printf 'PASS  %-58s\n' "S9 发现漂移并判 BLOCKER" ;;
  *) FAIL=$((FAIL + 1)); printf 'FAIL  %-58s\n' "S9 没报漂移（巡检失职）" ;;
esac

echo
echo "== ③ 兜底重算：定时任务「执行一次」修正漂移 + 记台账 =="
JOB_ROWS=$(psqlv "select job_name || '|' || id::text from sys_job where job_name = 'erp_credit_recompute' and deleted_at is null")
echo "  sys_job: $JOB_ROWS"
JOB_NAME=${JOB_ROWS%%|*}
JOB_ID=${JOB_ROWS##*|}
check "sys_job 种子行存在" "erp_credit_recompute" "$JOB_NAME"
req POST "/api/support/job/$JOB_ID/trigger" '{}'
check "POST job trigger HTTP" "200" "$STATUS"
# `/trigger` 是**发起即返回**（空包体）：任务体在后台跑，明细落 sys_job_log.message。
sleep 3
SUMMARY=$(psqlv "select coalesce(message,'') from sys_job_log where job_id = '$JOB_ID' order by created_at desc limit 1")
printf '  任务日志：%s\n' "$SUMMARY"
case "$SUMMARY" in
  *修正*) PASS=$((PASS + 1)); printf 'PASS  %-58s\n' "重算报告含「修正」（发现并修正漂移）" ;;
  *) FAIL=$((FAIL + 1)); printf 'FAIL  %-58s\n' "重算报告没提到修正：$SUMMARY" ;;
esac
case "$SUMMARY" in
  *"→"*) PASS=$((PASS + 1)); printf 'PASS  %-58s\n' "重算报告带「前 → 后」明细（可定位）" ;;
  *) FAIL=$((FAIL + 1)); printf 'FAIL  %-58s\n' "重算报告没带前后值：$SUMMARY" ;;
esac
AFTER=$(psqlv "select credit_used::text from biz_customer where id = '$CUST'")
check "重算后 credit_used 回到真值" "$DB_USED_BEFORE" "$AFTER"
LEDGER=$(psqlv "select count(*) from biz_credit_log where customer_id = '$CUST' and cause = 'RECOMPUTE' and trigger_source = 'JOB'")
if [ "${LEDGER:-0}" -ge 1 ]; then
  PASS=$((PASS + 1)); printf 'PASS  %-58s = %s\n' "biz_credit_log 记了 RECOMPUTE/JOB" "$LEDGER"
else
  FAIL=$((FAIL + 1)); printf 'FAIL  %-58s\n' "biz_credit_log 没有 RECOMPUTE 行"
fi
LEDGER_LINE=$(psqlv "select before_used::text || '→' || after_used::text || ' Δ' || delta::text from biz_credit_log where customer_id = '$CUST' and cause = 'RECOMPUTE' order by created_at desc limit 1")
echo "  台账明细: $LEDGER_LINE"

echo
echo "== ④ 修正后 S9 回到绿 =="
S9_GREEN=$(NIK_DATABASE_HOST=127.0.0.1 NIK_DATABASE_PORT=5432 NIK_DATABASE_USER=alun \
  NIK_DATABASE_NAME="$DB" NIK_DATABASE_PASSWORD=alun \
  python3 /opt/cloudpivot/scripts/verify/erp_invariants.py --group 销售 2>&1 | grep "S9")
printf '%s\n' "$S9_GREEN"
case "$S9_GREEN" in
  *"✅"*) PASS=$((PASS + 1)); printf 'PASS  %-58s\n' "S9 修正后通过" ;;
  *) FAIL=$((FAIL + 1)); printf 'FAIL  %-58s\n' "S9 修正后仍红" ;;
esac

echo
echo "============================================================"
if [ "$FAIL" -eq 0 ]; then
  echo "ALL CHECKS PASSED ($PASS)"
  exit 0
else
  echo "FAILED: $FAIL / $((PASS + FAIL))"
  exit 1
fi

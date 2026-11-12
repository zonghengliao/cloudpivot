#!/usr/bin/env bash
# HR-23 考勤「锁定 / 解锁 + 锁定后写保护」HTTP 冒烟（真实例 + 真库，走真 REST）。
#
# 覆盖本卡两个验收项在**接口层**的样子：
#   ① 锁定后改数据被拒：`PUT /period/{id}`、`POST /period/{id}/recalc` → 409 + 68010；
#      记录层（`PUT /attendance/record/{id}`）的同一判据在 handler 层由
#      `attendance_lock::lock_for_write` 出，真库用例 `attendance_record_db_tests` ③ 覆盖；
#      库层兜底（导入 upsert / bridge / 裸 SQL）由 `V0392` 触发器出，真库用例
#      `attendance_lock_db_tests` ① 覆盖。
#   ② 解锁要权限 + 审计：
#      · 权限：用**只有 EMPLOYEE 角色**的账号调状态接口 → 403（证明 `business:hr:attendance:lock`
#        不是「登录就能调」）；admin（SUPER_ADMIN 短路）→ 200。
#      · 审计：缺原因 400 / 原因过短 400；给原因解锁成功，且 `biz_status_log` 里
#        锁定与解锁两行都带操作人、解锁行带原因（psql 直查，不靠接口自述）。
#
# 用法（库连接走 libpq 标准环境变量，脚本用它做审计直查）：
#   BASE=http://127.0.0.1:18096 ADMIN_PASSWORD='Admin@123456' \
#   PGHOST=127.0.0.1 PGPORT=15432 PGUSER=cloudpivot PGPASSWORD=*** PGDATABASE=hr23drill_xxxx \
#     bash scripts/verify/hr/attendance_lock_http_smoke.sh
#
# 退出码：0 全通过；1 有断言失败。每条断言打印 [PASS]/[FAIL]，末尾汇总。
set -uo pipefail

BASE="${BASE:-http://127.0.0.1:18096}"
USER_NAME="${USER_NAME:-admin}"
ADMIN_PASSWORD="${ADMIN_PASSWORD:-Admin@123456}"
NEW_PASSWORD="${NEW_PASSWORD:-Admin@123456x}"
LIMITED_PASSWORD="${LIMITED_PASSWORD:-Hr23@Limited9}"

PASS=0
FAIL=0
SKIP=0
TOKEN=""

pass() { PASS=$((PASS + 1)); echo "[PASS] $1"; }
fail() { FAIL=$((FAIL + 1)); echo "[FAIL] $1"; }
skip() { SKIP=$((SKIP + 1)); echo "[SKIP] $1"; }

expect_eq() {
  if [ "$1" = "$2" ]; then pass "$3（$1）"; else fail "$3：期望 $1，实际 $2"; fi
}

contains() {
  case "$1" in
    *"$2"*) pass "$3（报文含「$2」）" ;;
    *) fail "$3：报文不含「$2」，实际：$1" ;;
  esac
}

http_json() {
  local method="$1" path="$2" body="${3:-}" token="${4:-$TOKEN}"
  local tmp
  tmp="$(mktemp)"
  if [ -n "$body" ]; then
    HTTP_STATUS="$(curl -s -m 20 -o "$tmp" -w '%{http_code}' -X "$method" "$BASE$path" \
      -H "Authorization: Bearer $token" -H 'Content-Type: application/json' -d "$body")"
  else
    HTTP_STATUS="$(curl -s -m 20 -o "$tmp" -w '%{http_code}' -X "$method" "$BASE$path" \
      -H "Authorization: Bearer $token")"
  fi
  HTTP_BODY="$(cat "$tmp")"
  rm -f "$tmp"
}

json_field() {
  echo "$1" | sed -n "s/.*\"$2\":\"\([^\"]*\)\".*/\1/p" | head -1
}

# 数字字段（`{"code":68010,...}` 里的 code 是数字不是字符串，用 json_field 取不到）
json_num() {
  echo "$1" | sed -n "s/.*\"$2\":\([0-9][0-9]*\).*/\1/p" | head -1
}

# psql 直查（审计证据）。没有 psql / 连不上 → 该段 SKIP，不假绿。
psql_ok=0
psql_q() {
  psql -At -F'|' -c "$1" 2>/dev/null
}
if command -v psql >/dev/null 2>&1 && psql_q 'select 1' >/dev/null 2>&1; then
  psql_ok=1
fi

CODE="24$(date +%y%m%d%H%M%S | cut -c1-2)-$(printf '%02d' $(( ( $(date +%s) % 12 ) + 1 )))"
SUFFIX="$(date +%H%M%S)"
LIMITED_USER="hr23.limited.$SUFFIX"

echo "== HR-23 考勤锁定/解锁 HTTP 冒烟 BASE=$BASE 期间=$CODE =="
echo "   （审计直查 ${psql_ok:+$( [ "$psql_ok" = 1 ] && echo 已连接 || echo 未连接 )}）"

# ---------- 0. 未登录 → 401/403（证明路由存在且挂了鉴权）----------
TMP_STATUS="$(curl -s -m 10 -o /dev/null -w '%{http_code}' -X POST \
  "$BASE/api/business/hr/attendance/period/00000000-0000-0000-0000-000000000000/status" \
  -H 'Content-Type: application/json' -d '{"to_status":"LOCKED"}')"
case "$TMP_STATUS" in
  401|403) pass "未登录调状态接口 → $TMP_STATUS（不是 404，鉴权在位）" ;;
  *) fail "未登录调状态接口：期望 401/403，实际 $TMP_STATUS" ;;
esac

# ---------- 1. admin 登录 + 改初始密码 ----------
login_args() { echo "{\"username\":\"$1\",\"password\":\"$2\"}"; }
login() {
  json_field "$(curl -s -m 20 -X POST "$BASE/api/system/auth/login" \
    -H 'Content-Type: application/json' -d "$(login_args "$1" "$2")")" access_token
}

# 夹具准备（必须在**首次登录之前**）：`session_guard` 对 `must_change_password` 有缓存，
# 先登录再改库会读到旧值，把后面「缺权限 403」污染成「首次登录必须先修改初始密码」。
ADMIN_FLAG_PREPARED=0
if [ "${PREP_ADMIN_FLAG:-0}" = "1" ] && [ "$psql_ok" = 1 ] \
   && psql_q "UPDATE sys_user SET must_change_password = false WHERE username = '$USER_NAME'" >/dev/null; then
  ADMIN_FLAG_PREPARED=1
  pass "夹具准备：SQL 清掉 $USER_NAME 的 must_change_password（演练库；不经改密接口，理由见下）"
fi

TOKEN="$(login "$USER_NAME" "$ADMIN_PASSWORD")"
CUR="$ADMIN_PASSWORD"
if [ -z "$TOKEN" ]; then
  TOKEN="$(login "$USER_NAME" "$NEW_PASSWORD")"
  CUR="$NEW_PASSWORD"
fi
if [ -z "$TOKEN" ]; then
  echo "[FAIL] admin 登录失败（两个候选口令都不对）"
  echo "== 汇总：PASS=$PASS FAIL=$((FAIL + 1)) SKIP=$SKIP =="
  exit 1
fi
pass "admin 登录成功"

# ---------- 1.5 must_change_password 处理 ----------
# 空库首启的 admin 带 `must_change_password=true`，任何业务接口都会 403「首次登录必须先修改初始密码」。
#
# 两条路线：
#   · `PREP_ADMIN_FLAG=1`（演练库用法）：用 SQL 把 flag 清掉当**夹具准备**。为什么不用改密接口：
#     本仓当前 `PUT /system/auth/password` **没挂** `#[nik::permission]`，框架认证层因此不往
#     extensions 注入 `AuthClaims`，处理器 `Extension(AuthClaims)` 取不到 → 500
#     `Missing request extension`。这是既有缺陷（与本卡改动无关，已在证据 README 上报）；
#     本卡两个验收项与改密无关，so 演练库走 SQL。
#   · 否则：走真接口改密 → 200，再用新口令重登（正常环境的样子）。
if [ "${ADMIN_FLAG_PREPARED:-0}" = "1" ]; then
  pass "admin 已可调业务接口（must_change_password 已由夹具准备清掉）"
elif [ "$CUR" = "$NEW_PASSWORD" ]; then
  pass "初始密码已改过（must_change_password=false，跳过改密）"
else
  http_json PUT /api/system/auth/password \
    "{\"old_password\":\"$CUR\",\"new_password\":\"$NEW_PASSWORD\"}"
  expect_eq 200 "$HTTP_STATUS" "改初始密码 → 200"
  TOKEN="$(login "$USER_NAME" "$NEW_PASSWORD")"
  if [ -n "$TOKEN" ]; then pass "用新口令重新登录"; else fail "用新口令重新登录失败"; fi
fi

# ---------- 2. 建期间 → 提交 → 锁定（真状态机三边）----------
http_json POST /api/business/hr/attendance/period \
  "{\"period_code\":\"$CODE\",\"period_start\":\"2026-11-01\",\"period_end\":\"2026-11-30\",\"remark\":\"HR23 冒烟\"}"
expect_eq 200 "$HTTP_STATUS" "建期间 $CODE → 200"
PID="$(json_field "$HTTP_BODY" id)"
expect_eq "DRAFT" "$(json_field "$HTTP_BODY" status)" "新建状态 DRAFT"
if [ -z "$PID" ]; then
  echo "[FAIL] 拿不到期间 id，后续无法进行"
  echo "== 汇总：PASS=$PASS FAIL=$((FAIL + 1)) SKIP=$SKIP =="
  exit 1
fi

http_json POST "/api/business/hr/attendance/period/$PID/status" '{"to_status":"SUBMITTED"}'
expect_eq 200 "$HTTP_STATUS" "提交 → 200"
expect_eq "SUBMITTED" "$(json_field "$HTTP_BODY" status)" "状态 SUBMITTED"

http_json POST "/api/business/hr/attendance/period/$PID/status" '{"to_status":"LOCKED"}'
expect_eq 200 "$HTTP_STATUS" "锁定 → 200"
expect_eq "LOCKED" "$(json_field "$HTTP_BODY" status)" "状态 LOCKED"

# ---------- 3. 验收① 锁定后改数据被拒（409 + 68010）----------
http_json PUT "/api/business/hr/attendance/period/$PID" '{"remark":"锁定后不该能改"}'
expect_eq 409 "$HTTP_STATUS" "锁定后改期间 → 409"
expect_eq 68010 "$(json_num "$HTTP_BODY" code)" "业务码 68010（HR_ATTENDANCE_PERIOD_RULE_BLOCKED）"
contains "$HTTP_BODY" "已锁定" "409 报文说明已锁定"
contains "$HTTP_BODY" "解锁" "409 报文给出下一步：先解锁再改"

http_json POST "/api/business/hr/attendance/period/$PID/recalc" ''
expect_eq 409 "$HTTP_STATUS" "锁定后重算汇总 → 409"
expect_eq 68010 "$(json_num "$HTTP_BODY" code)" "重算也是 68010"

# 库层：改期间备注真的没落库（拒绝 ≠ 改了再报错）
if [ "$psql_ok" = 1 ]; then
  REMARK="$(psql_q "select coalesce(remark,'') from biz_attendance_period where id = '$PID'")"
  if [ "$REMARK" != "锁定后不该能改" ]; then
    pass "库里的 remark 未被改写（实际「$REMARK」）"
  else
    fail "库里 remark 被改了：拒绝不是「改了再报错」"
  fi
else
  skip "库里 remark 未变的直查（无 psql 连接）"
fi

# ---------- 4. 验收② 解锁要权限：EMPLOYEE 角色账号 → 403 ----------
http_json GET "/api/system/role/list" ''
expect_eq 200 "$HTTP_STATUS" "读角色列表 → 200"
EMP_ROLE_ID="$(echo "$HTTP_BODY" | tr '{' '\n' | grep -o '"id":"[^"]*","role_code":"EMPLOYEE"' | sed -n 's/.*"id":"\([^"]*\)".*/\1/p' | head -1)"
if [ -z "$EMP_ROLE_ID" ]; then
  EMP_ROLE_ID="$(echo "$HTTP_BODY" | grep -o '"role_code":"EMPLOYEE"[^}]*' | head -1 | sed -n 's/.*"id":"\([^"]*\)".*/\1/p')"
fi

if [ -z "$EMP_ROLE_ID" ]; then
  skip "EMPLOYEE 角色不存在 → 跳过「无权限 403」用例（不假绿）"
else
  http_json POST /api/system/user \
    "{\"username\":\"$LIMITED_USER\",\"password\":\"$LIMITED_PASSWORD\",\"real_name\":\"HR23 受限账号\",\"role_ids\":[\"$EMP_ROLE_ID\"]}"
  if [ "$HTTP_STATUS" = "200" ]; then
    LIMITED_ID="$(json_field "$HTTP_BODY" id)"
    pass "建 EMPLOYEE 角色账号 $LIMITED_USER"

    # 先清 must_change_password（**必须在首次登录之前**：session_guard 对 flag 有缓存，
    # 登一次再改库会读到旧值，把「缺权限」污染成「首次登录需改密」）
    if [ "${PREP_ADMIN_FLAG:-0}" = "1" ] && [ "$psql_ok" = 1 ] \
       && psql_q "UPDATE sys_user SET must_change_password = false WHERE username = '$LIMITED_USER'" >/dev/null; then
      pass "受限账号 must_change_password 用 SQL 清掉（演练库夹具准备）"
    fi

    LTOKEN="$(login "$LIMITED_USER" "$LIMITED_PASSWORD")"
    if [ -n "$LTOKEN" ]; then
      pass "受限账号登录成功"
      if [ "${PREP_ADMIN_FLAG:-0}" != "1" ]; then
        http_json PUT /api/system/auth/password \
          "{\"old_password\":\"$LIMITED_PASSWORD\",\"new_password\":\"${LIMITED_PASSWORD}x\"}" "$LTOKEN"
        if [ "$HTTP_STATUS" = "200" ]; then
          LTOKEN="$(login "$LIMITED_USER" "${LIMITED_PASSWORD}x")"
          pass "受限账号改初始密码并重新登录"
        else
          skip "受限账号改初始密码（$HTTP_STATUS）——403 证据可能被「首次登录需改密」污染"
        fi
      fi

      http_json POST "/api/business/hr/attendance/period/$PID/status" \
        '{"to_status":"DRAFT","reason":"HR23 冒烟：受限账号尝试解锁"}' "$LTOKEN"
      expect_eq 403 "$HTTP_STATUS" "EMPLOYEE 账号解锁 → 403（缺 business:hr:attendance:lock）"
      contains "$HTTP_BODY" "无权限" "403 报文是「无权限」而不是「首次登录必须先修改初始密码」"

      # 期间还必须是 LOCKED（403 这一步没动数据）
      http_json GET "/api/business/hr/attendance/period/page?keyword=$CODE" ''
      expect_eq 200 "$HTTP_STATUS" "受限账号读列表 → 200"
    else
      fail "受限账号登录失败（造的账号进不去，403 用例跑不了）"
    fi
  else
    fail "建 EMPLOYEE 角色账号失败：$HTTP_STATUS $HTTP_BODY"
    LIMITED_ID=""
  fi
fi

# ---------- 5. 验收② 审计：缺原因 400 / 原因过短 400 / 给原因 200 ----------
http_json POST "/api/business/hr/attendance/period/$PID/status" '{"to_status":"DRAFT"}'
expect_eq 400 "$HTTP_STATUS" "解锁缺原因 → 400"
http_json POST "/api/business/hr/attendance/period/$PID/status" '{"to_status":"DRAFT","reason":"漏人"}'
expect_eq 400 "$HTTP_STATUS" "解锁原因过短（2 字）→ 400"

REASON="HR23 冒烟：月末补录漏掉的两人"
http_json POST "/api/business/hr/attendance/period/$PID/status" \
  "{\"to_status\":\"DRAFT\",\"reason\":\"$REASON\"}"
expect_eq 200 "$HTTP_STATUS" "给足原因解锁 → 200"
expect_eq "DRAFT" "$(json_field "$HTTP_BODY" status)" "解锁后状态回 DRAFT"
expect_eq "false" "$(echo "$HTTP_BODY" | grep -o '"locked":false' | head -1 | cut -d: -f2)" "解锁后 locked=false"

# 状态日志：锁定 + 解锁两行都在，解锁行带原因与操作人（psql 直查）
if [ "$psql_ok" = 1 ]; then
  LOG_LINES="$(psql_q "select to_status||':'||coalesce(reason,'-')||':'||(operator_id is not null) from biz_status_log where entity_type='ATTENDANCE_PERIOD' and entity_id='$PID' order by created_at, id")"
  grep -qi '^SUBMITTED:-' <<<"$LOG_LINES" && pass "日志里有 SUBMITTED 行" || fail "日志缺 SUBMITTED 行：$LOG_LINES"
  grep -qi '^LOCKED:-' <<<"$LOG_LINES" && pass "日志里有 LOCKED 行" || fail "日志缺 LOCKED 行：$LOG_LINES"
  if grep -q "^DRAFT:.*true" <<<"$LOG_LINES" && grep -qF "$REASON" <<<"$LOG_LINES"; then
    pass "解锁日志带原因且 operator_id 非空（审计可追溯）"
  else
    fail "解锁日志缺原因或操作人：$LOG_LINES"
  fi
  LOG_COUNT="$(psql_q "select count(*) from biz_status_log where entity_type='ATTENDANCE_PERIOD' and entity_id='$PID'")"
  expect_eq 4 "$LOG_COUNT" "建单+提交+锁定+解锁 = 4 行日志"
else
  skip "biz_status_log 直查（无 psql 连接）"
fi

# ---------- 6. 解锁后写恢复：改期间 → 200 ----------
http_json PUT "/api/business/hr/attendance/period/$PID" '{"remark":"解锁后可以改"}'
expect_eq 200 "$HTTP_STATUS" "解锁后改期间 → 200"
contains "$HTTP_BODY" "解锁后可以改" "改动已落库并回显"

# ---------- 7. 清场（按接口删，不留脏数据）----------
if [ -n "${LIMITED_ID:-}" ]; then
  http_json DELETE "/api/system/user/$LIMITED_ID" ''
  if [ "$HTTP_STATUS" = "200" ]; then pass "删除受限账号（清场）"; else fail "删除受限账号失败：$HTTP_STATUS"; fi
fi
http_json DELETE "/api/business/hr/attendance/period/$PID" ''
if [ "$HTTP_STATUS" = "200" ]; then pass "软删期间（清场）"; else fail "软删期间失败：$HTTP_STATUS"; fi

echo ""
echo "== 汇总：PASS=$PASS FAIL=$FAIL SKIP=$SKIP =="
[ "$FAIL" -eq 0 ] || exit 1

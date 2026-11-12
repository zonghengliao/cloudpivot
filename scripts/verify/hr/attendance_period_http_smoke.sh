#!/usr/bin/env bash
# HR-19 考勤期间 HTTP 冒烟（真实例 + 真库，走真 REST，不经 handler 直调）。
#
# 覆盖 6 条路由的全部对外行为：
#   GET  /business/hr/attendance/period/page       列表（关键字 / 状态 / 年份）
#   POST /business/hr/attendance/period             建期间（含 400 期间码、409 撞码）
#   PUT  /business/hr/attendance/period/{id}        改（含"无改动"409）
#   POST /business/hr/attendance/period/{id}/status 提交 / 锁定 / 解锁（解锁缺原因 400）
#   POST /business/hr/attendance/period/{id}/recalc 汇总重算（幂等：第二次 changed=false）
#   DELETE /business/hr/attendance/period/{id}      软删（软删后列表查不到、期间码可重建）
#
# 用法：
#   BASE=http://127.0.0.1:18095 ADMIN_PASSWORD='Admin@123456' bash scripts/verify/hr/attendance_period_http_smoke.sh
#
# 前提：后端实例已起、库已迁移（V0383 的 10 张表在位）、admin 密码已知。
# 退出码：0 全通过；1 有断言失败。每条断言打印 [PASS]/[FAIL] 行，末尾汇总。

set -uo pipefail

BASE="${BASE:-http://127.0.0.1:18095}"
USER_NAME="${USER_NAME:-admin}"
ADMIN_PASSWORD="${ADMIN_PASSWORD:-Admin@123456}"
PERIOD_CODE="${PERIOD_CODE:-2026-11}"

PASS=0
FAIL=0
TOKEN=""

pass() { PASS=$((PASS + 1)); echo "[PASS] $1"; }
fail() { FAIL=$((FAIL + 1)); echo "[FAIL] $1"; }

# 断言：<期望> <实际> <说明>
expect_eq() {
  if [ "$1" = "$2" ]; then pass "$3（$1）"; else fail "$3：期望 $1，实际 $2"; fi
}

contains() {
  case "$1" in
    *"$2"*) pass "$3（报文含「$2」）" ;;
    *) fail "$3：报文不含「$2」，实际：$1" ;;
  esac
}

not_contains() {
  case "$1" in
    *"$2"*) fail "$3：报文不应含「$2」，实际：$1" ;;
    *) pass "$3" ;;
  esac
}

# HTTP 调用：http_json <method> <path> [body] → 全局 HTTP_STATUS / HTTP_BODY
http_json() {
  local method="$1" path="$2" body="${3:-}"
  local tmp
  tmp="$(mktemp)"
  if [ -n "$body" ]; then
    HTTP_STATUS="$(curl -s -m 20 -o "$tmp" -w '%{http_code}' -X "$method" "$BASE$path" \
      -H "Authorization: Bearer $TOKEN" -H 'Content-Type: application/json' -d "$body")"
  else
    HTTP_STATUS="$(curl -s -m 20 -o "$tmp" -w '%{http_code}' -X "$method" "$BASE$path" \
      -H "Authorization: Bearer $TOKEN")"
  fi
  HTTP_BODY="$(cat "$tmp")"
  rm -f "$tmp"
}

# 从 JSON 里取一个浅层字段（够用即可，不引 jq 依赖）
json_field() {
  echo "$1" | sed -n "s/.*\"$2\":\"\([^\"]*\)\".*/\1/p" | head -1
}

echo "== HR-19 考勤期间 HTTP 冒烟 BASE=$BASE 期间=$PERIOD_CODE =="

# ---------- 0. 未登录必须 401（证明路由存在且挂了鉴权，而不是 404 静默）----------
TMP_STATUS="$(curl -s -m 10 -o /dev/null -w '%{http_code}' -X GET "$BASE/api/business/hr/attendance/period/page")"
expect_eq 401 "$TMP_STATUS" "未登录访问列表 → 401（不是 404）"

# ---------- 1. 登录（初始口令 / 上次改过的口令都试）----------
NEW_PASSWORD="${NEW_PASSWORD:-Admin@123456x}"
login_args() { echo "{\"username\":\"$USER_NAME\",\"password\":\"$1\"}"; }

TOKEN="$(json_field "$(curl -s -m 20 -X POST "$BASE/api/system/auth/login" -H 'Content-Type: application/json' \
  -d "$(login_args "$ADMIN_PASSWORD")")" access_token)"
CUR_PASSWORD="$ADMIN_PASSWORD"
if [ -z "$TOKEN" ]; then
  # 上一次跑冒烟把口令改成了 NEW_PASSWORD（脚本可反复跑）
  TOKEN="$(json_field "$(curl -s -m 20 -X POST "$BASE/api/system/auth/login" -H 'Content-Type: application/json' \
    -d "$(login_args "$NEW_PASSWORD")")" access_token)"
  CUR_PASSWORD="$NEW_PASSWORD"
fi
if [ -z "$TOKEN" ]; then
  echo "[FAIL] 登录失败：两个候选口令都不对（ADMIN_PASSWORD / NEW_PASSWORD）"
  echo "== 汇总：PASS=0 FAIL=1 =="
  exit 1
fi
pass "登录成功（$USER_NAME）"

# ---------- 1.5 首次登录必须先改初始密码（否则 session_guard 把业务接口全 403）----------
# 空库首启的 admin 带 `must_change_password=true`，任何业务接口都会 403「首次登录必须先修改初始密码」。
# 这不是权限码问题，但会让整段冒烟看起来「全红像缺权限」—— 所以在最前面显式处理掉。
if [ "$CUR_PASSWORD" = "$NEW_PASSWORD" ]; then
  pass "当前口令已是改过的那一个（must_change_password 已为 false；本轮跳过改密）"
else
  http_json PUT /api/system/auth/password \
    "{\"old_password\":\"$CUR_PASSWORD\",\"new_password\":\"$NEW_PASSWORD\"}"
  expect_eq 200 "$HTTP_STATUS" "改初始密码 → 200"
  TOKEN="$(json_field "$(curl -s -m 20 -X POST "$BASE/api/system/auth/login" -H 'Content-Type: application/json' \
    -d "$(login_args "$NEW_PASSWORD")")" access_token)"
  if [ -n "$TOKEN" ]; then pass "用新口令重新登录"; else fail "用新口令重新登录失败"; fi
fi

# ---------- 2. 建期间：非法期间码 → 400 ----------
http_json POST /api/business/hr/attendance/period \
  '{"period_code":"2026-13","period_start":"2026-12-01","period_end":"2026-12-31"}'
expect_eq 400 "$HTTP_STATUS" "期间码 2026-13 → 400"
contains "$HTTP_BODY" "01~12" "400 报文点名月份范围"

# ---------- 3. 建期间：区间倒置 → 400 ----------
http_json POST /api/business/hr/attendance/period \
  "{\"period_code\":\"$PERIOD_CODE\",\"period_start\":\"2026-11-25\",\"period_end\":\"2026-10-26\"}"
expect_eq 400 "$HTTP_STATUS" "止早于起 → 400"
contains "$HTTP_BODY" "不能早于" "400 报文点名区间问题"

# ---------- 4. 建期间：正常（26-25 计薪周期）----------
http_json POST /api/business/hr/attendance/period \
  "{\"period_code\":\"$PERIOD_CODE\",\"period_start\":\"2026-10-26\",\"period_end\":\"2026-11-25\",\"remark\":\"HTTP 冒烟\"}"
expect_eq 200 "$HTTP_STATUS" "建期间 $PERIOD_CODE → 200"
PERIOD_ID="$(json_field "$HTTP_BODY" id)"
STATUS="$(json_field "$HTTP_BODY" status)"
expect_eq "DRAFT" "$STATUS" "新建状态是 DRAFT"
contains "$HTTP_BODY" '"status_label":"草稿"' "回执带中文状态名"
contains "$HTTP_BODY" '"employee_count":0' "新建期间汇总为空"

# ---------- 5. 撞码 → 409 + 68009 ----------
http_json POST /api/business/hr/attendance/period \
  "{\"period_code\":\"$PERIOD_CODE\",\"period_start\":\"2026-10-26\",\"period_end\":\"2026-11-25\"}"
expect_eq 409 "$HTTP_STATUS" "同期间码再建 → 409"
contains "$HTTP_BODY" "68009" "撞码业务码是 68009"
contains "$HTTP_BODY" "一个月只能有一批" "撞码报文给下一步"

# ---------- 6. 列表：关键字 + 状态 + 年份 ----------
http_json GET "/api/business/hr/attendance/period/page?keyword=$PERIOD_CODE&status=DRAFT&year=2026"
expect_eq 200 "$HTTP_STATUS" "列表（关键字+状态+年份）→ 200"
contains "$HTTP_BODY" "\"total\":1" "过滤后只命中本期"
contains "$HTTP_BODY" "$PERIOD_ID" "列表里就是刚建的那一期"

# 非法状态值 → 400（不是静默空列表）
http_json GET "/api/business/hr/attendance/period/page?status=APPROVED"
expect_eq 400 "$HTTP_STATUS" "列表 status=APPROVED → 400"

# ---------- 7. 改：换备注 + 原样重放 409 ----------
http_json PUT "/api/business/hr/attendance/period/$PERIOD_ID" '{"remark":"HTTP 冒烟（已改）"}'
expect_eq 200 "$HTTP_STATUS" "改备注 → 200"
contains "$HTTP_BODY" "HTTP 冒烟（已改）" "备注已落库并回显"

http_json PUT "/api/business/hr/attendance/period/$PERIOD_ID" '{"remark":"HTTP 冒烟（已改）"}'
expect_eq 409 "$HTTP_STATUS" "原样重放改 → 409"
contains "$HTTP_BODY" "没有任何改动" "409 报文说明没改动"

# ---------- 8. 状态机：提交 → 锁定 → 非法边 → 解锁缺原因 → 解锁 ----------
http_json POST "/api/business/hr/attendance/period/$PERIOD_ID/status" '{"to_status":"SUBMITTED"}'
expect_eq 200 "$HTTP_STATUS" "提交 → 200"
contains "$HTTP_BODY" '"status":"SUBMITTED"' "状态变成 SUBMITTED"
contains "$HTTP_BODY" '"submitted_at":"' "提交时间已落库"

http_json POST "/api/business/hr/attendance/period/$PERIOD_ID/status" '{"to_status":"LOCKED"}'
expect_eq 200 "$HTTP_STATUS" "锁定 → 200"
contains "$HTTP_BODY" '"locked":true' "回执 locked=true"

# 锁定后：改 / 重算 / 删 全被挡（68010）
http_json PUT "/api/business/hr/attendance/period/$PERIOD_ID" '{"remark":"锁定后改"}'
expect_eq 409 "$HTTP_STATUS" "锁定后改 → 409"
contains "$HTTP_BODY" "68010" "锁定后改业务码 68010"
contains "$HTTP_BODY" "解锁" "报文点名下一步（先解锁）"

http_json POST "/api/business/hr/attendance/period/$PERIOD_ID/recalc"
expect_eq 409 "$HTTP_STATUS" "锁定后重算 → 409"

http_json DELETE "/api/business/hr/attendance/period/$PERIOD_ID"
expect_eq 409 "$HTTP_STATUS" "锁定后删 → 409"

# 解锁缺原因 → 400
http_json POST "/api/business/hr/attendance/period/$PERIOD_ID/status" '{"to_status":"DRAFT"}'
expect_eq 400 "$HTTP_STATUS" "解锁不给原因 → 400"
contains "$HTTP_BODY" "原因" "400 报文点名要原因"

# 解锁带原因 → 200
http_json POST "/api/business/hr/attendance/period/$PERIOD_ID/status" \
  '{"to_status":"DRAFT","reason":"HTTP 冒烟：重导考勤"}'
expect_eq 200 "$HTTP_STATUS" "解锁（带原因）→ 200"
contains "$HTTP_BODY" '"status":"DRAFT"' "解锁回到 DRAFT"

# ---------- 9. 汇总重算：幂等（两次 changed=true→false）----------
http_json POST "/api/business/hr/attendance/period/$PERIOD_ID/recalc"
expect_eq 200 "$HTTP_STATUS" "重算（无记录）→ 200"
contains "$HTTP_BODY" '"scanned":0' "无记录时 scanned=0"
contains "$HTTP_BODY" '"changed":false' "无记录时快照没变化"

http_json POST "/api/business/hr/attendance/period/$PERIOD_ID/recalc"
expect_eq 200 "$HTTP_STATUS" "重算第二次 → 200"
contains "$HTTP_BODY" '"changed":false' "第二次重算仍然 changed=false（幂等）"

# ---------- 10. 删（空草稿）→ 软删；列表查不到；期间码可重建 ----------
http_json DELETE "/api/business/hr/attendance/period/$PERIOD_ID"
expect_eq 200 "$HTTP_STATUS" "删空草稿 → 200"
contains "$HTTP_BODY" "$PERIOD_ID" "回执是删除前的快照"

http_json GET "/api/business/hr/attendance/period/page?keyword=$PERIOD_CODE"
contains "$HTTP_BODY" '"total":0' "软删后列表不显示（total=0）"

http_json POST /api/business/hr/attendance/period \
  "{\"period_code\":\"$PERIOD_CODE\",\"period_start\":\"2026-10-26\",\"period_end\":\"2026-11-25\",\"remark\":\"重建\"}"
expect_eq 200 "$HTTP_STATUS" "软删后同期间码可重建（唯一索引带 deleted_at IS NULL）"
REBUILT_ID="$(json_field "$HTTP_BODY" id)"

# 收尾：把重建的这条也删掉（冒烟不留数据）
http_json DELETE "/api/business/hr/attendance/period/$REBUILT_ID"
expect_eq 200 "$HTTP_STATUS" "收尾：删掉重建的期间"

# ---------- 11. 路由表对照：未注册路径 404 / 已注册路径错方法 405 ----------
# 两个都要看：404 证明「路径不在路由表」，405 证明「路径在、但这个方法没挂」。
# 只测 404 的话，把 `GET /period/{id}` 漏注册也发现不了（PUT/DELETE 会把它变成 405）。
TMP_STATUS="$(curl -s -m 10 -o /dev/null -w '%{http_code}' -X GET "$BASE/api/business/hr/attendance/periodx" \
  -H "Authorization: Bearer $TOKEN")"
expect_eq 404 "$TMP_STATUS" "对照组 /attendance/periodx → 404（真未注册）"

TMP_STATUS="$(curl -s -m 10 -o /dev/null -w '%{http_code}' -X GET "$BASE/api/business/hr/attendance/period/nope" \
  -H "Authorization: Bearer $TOKEN")"
expect_eq 405 "$TMP_STATUS" "已存在的 /period/{id} 用 GET → 405（本卡不提供单条读，列表就够）"

echo "== 汇总：PASS=$PASS FAIL=$FAIL =="
[ "$FAIL" -eq 0 ] || exit 1

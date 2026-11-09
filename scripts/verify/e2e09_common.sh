#!/usr/bin/env bash
# E2E-09 系统管理域演练公共库（145 测试机 · cloudpivot_dev · 真后端 127.0.0.1:8033）
# 被 e2e09_sys_{a,b}.sh / e2e09_escalation.sh / e2e09_db_evidence.sh source。
# 状态落 /tmp/e2e09/state.sh，报文落 /tmp/e2e09/raw/，断言追加 /tmp/e2e09/asserts.log。
set -uo pipefail
ENV=/etc/cloudpivot/cloudpivot-dev.env
set -a; . "$ENV"; set +a
export PGPASSWORD="$NIK_DATABASE_PASSWORD"
BASE="${BASE:-http://127.0.0.1:8033/api}"
WORK=${WORK:-/tmp/e2e09}; RAW=$WORK/raw; STATE=$WORK/state.sh; LOG=$WORK/asserts.log
mkdir -p "$RAW"; touch "$LOG"
[ -f "$STATE" ] && . "$STATE"
PSQLQ() { psql -h 127.0.0.1 -U "$NIK_DATABASE_USER" -d "$NIK_DATABASE_NAME" -At -F'|' -P pager=off "$@"; }
SQ()    { PSQLQ -c "$1" 2>&1 | tr '\n' ' ' | sed 's/  */ /g; s/ *$//'; }
PASS=${PASS:-0}; FAIL=${FAIL:-0}
say()  { printf '%s\n' "$*"; }
ok()   { PASS=$((PASS+1)); printf 'PASS\t%s\n' "$*" >> "$LOG"; say "  ✅ $*"; }
bad()  { FAIL=$((FAIL+1)); printf 'FAIL\t%s\n' "$*" >> "$LOG"; say "  ❌ $*"; }
check(){ if [ "$2" = "$3" ]; then ok "$1 = $2"; else bad "$1 = $2（期望 $3）"; fi; }
checkne(){ if [ "$2" != "$3" ]; then ok "$1 = $2（≠ $3）"; else bad "$1 = $2（不应等于 $3）"; fi; }
contains(){ case "$2" in *"$3"*) ok "$1 含「$3」";; *) bad "$1 不含「$3」（实际：$(printf '%s' "$2" | head -c 260)）";; esac; }
matches(){ if printf '%s' "$2" | grep -Eq "$3"; then ok "$1 = $2（匹配 $3）"; else bad "$1 = $2（应匹配 $3）"; fi; }
matches_i(){ if printf '%s' "$2" | grep -Eqi "$3"; then ok "$1 命中（不区分大小写：$3）"; else bad "$1 未命中（应匹配 $3，实际：$(printf '%s' "$2" | head -c 300)）"; fi; }
summary(){ say; say "== PASS=$PASS FAIL=$FAIL"; }
save() { printf '%s=%q\n' "$1" "$2" >> "$STATE"; export "$1"="$2"; }
data() { printf '%s' "$BODY" | jq -r "$1" 2>/dev/null; }
show() { printf '%s' "$BODY" | jq -c '{code,msg}' 2>/dev/null | head -c 500; }

api() { # api <名> <METHOD> <path> <token> [json]
  local name=$1 method=$2 path=$3 token=$4 body=${5:-}
  local out="$RAW/$name.json"
  local -a args=(-s -m 60 -o "$out" -w '%{http_code}' -X "$method" "$BASE$path")
  [ -n "$token" ] && args+=(--oauth2-bearer "$token")
  [ -n "$body" ] && args+=(-H 'Content-Type: application/json' -d "$body")
  HTTP=$(curl "${args[@]}")
  BODY=$(cat "$out" 2>/dev/null)
  CODE=$(printf '%s' "$BODY" | jq -r '.code // empty' 2>/dev/null)
  printf '%s' "$BODY" | jq -e . >/dev/null 2>&1 || say "  ⚠ 非 JSON($name) http=$HTTP: $(printf '%s' "$BODY" | head -c 200)"
}

# 登录（自动处理「首登强制改密」）：login <user> <pass> [newpass] → 打印 token
login() {
  local user=$1 pass=$2 newpass=${3:-} i must tok
  for i in 1 2 3 4 5; do
    api "login_$user" POST /system/auth/login "" "{\"username\":\"$user\",\"password\":\"$pass\"}"
    [ "$HTTP" = 200 ] && [ "$CODE" = 0 ] && break
    sleep 2
  done
  [ "$HTTP" = 200 ] && [ "$CODE" = 0 ] || { say "FATAL: 登录失败 $user http=$HTTP $(show)" >&2; return 1; }
  tok=$(data '.data.access_token')
  must=$(data '.data.must_change_password')
  if [ "$must" = "true" ] && [ -n "$newpass" ]; then
    api "chpwd_$user" PUT /system/auth/password "$tok" "{\"old_password\":\"$pass\",\"new_password\":\"$newpass\"}"
    [ "$HTTP" = 200 ] || { say "FATAL: 首登改密失败 $user http=$HTTP $(show)" >&2; return 1; }
    api "relogin_$user" POST /system/auth/login "" "{\"username\":\"$user\",\"password\":\"$newpass\"}"
    tok=$(data '.data.access_token')
  fi
  printf '%s' "$tok"
}

# 按 auth_code 从菜单树里取节点的 id（可能多个：页面 + 按钮）
menu_ids_by_code() { # menu_ids_by_code <token> <auth_code> → 空格分隔 id
  local tok=$1 code=$2
  api "tree_for_$code" GET '/system/menu/tree' "$tok"
  printf '%s' "$BODY" | jq -r --arg c "$code" '[.. | objects | select(.auth_code==$c) | .id] | unique | join(" ")' 2>/dev/null
}

# 页面列表是否含某用户名（system/user/page 的 data.list）
page_has_user() { # page_has_user <token> <username>
  api "pg_user_$2" GET "/system/user/page?page=1&page_size=50&username=$2" "$1"
  printf '%s' "$BODY" | jq -r --arg u "$2" 'if ([.data.list[]?.username] | index($u)) != null then "1" else "" end' 2>/dev/null
}

json_probe_admin() { curl -s -m 30 -X POST "$BASE/system/auth/login" -H 'Content-Type: application/json' \
  -d "{\"username\":\"admin\",\"password\":\"$NIK_ADMIN_PASSWORD\"}" | jq -r "$1"; }

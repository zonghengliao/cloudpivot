#!/usr/bin/env bash
# ============================================================================
# P3-07 证据脚本 · 公共函数（source 使用，不单独执行）
#
# 与 P3-05 的 lib.sh 同口径：
#   · 隔离实例：独立库 + 独立端口 + 独立二进制快照（`/root/p307/bin/cloudpivot`）；
#   · 三把密钥与 admin 口令只从**环境变量/600 权限的环境文件**来（仓库不落任何口令）；
#   · 所有断言同时打印到 stdout 并落 raw/ 下的文件，README 引用真实输出。
# ============================================================================
set -uo pipefail
export PATH=/root/.cargo/bin:$PATH

EV="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
RAW="$EV/raw"
mkdir -p "$RAW"
PGHOST=127.0.0.1
PGUSER=alun
export PGPASSWORD="${PGPASSWORD:?运行前先 export PGPASSWORD=<本机测试库口令>}"

PASS=0
FAIL=0
say() { printf '%s\n' "$*"; }
ok()  { PASS=$((PASS + 1)); say "  ✅ $*"; }
bad() { FAIL=$((FAIL + 1)); say "  ❌ $*"; }
eq()  { if [ "$2" = "$3" ]; then ok "$1 = $2"; else bad "$1 = $2（期望 $3）"; fi; }
contains() { case "$2" in *"$3"*) ok "$1 含「$3」" ;; *) bad "$1 不含「$3」：$2" ;; esac; }
not_contains() { case "$2" in *"$3"*) bad "$1 不该含「$3」：$2" ;; *) ok "$1 不含「$3」" ;; esac; }
is_uuid() { printf '%s' "$1" | grep -Eq '^[0-9a-fA-F-]{36}$'; }
summary() {
  say ""
  say "===== 结果：PASS=$PASS FAIL=$FAIL ====="
  [ "$FAIL" -eq 0 ] || exit 1
}

q() { psql -h "$PGHOST" -U "$PGUSER" -d "$1" -tAc "$2"; }

# HTTP：状态码到 stdout，body 留 $RAW/last_body.json
http() { # http METHOD URL TOKEN JSON
  local method=$1 url=$2 token=$3 payload=$4
  local args=(-s -o "$RAW/last_body.json" -w '%{http_code}' -X "$method" "$url" -H 'Content-Type: application/json')
  if [ -n "$token" ]; then args+=(-H "Authorization: Bearer ${token}"); fi
  if [ -n "$payload" ]; then args+=(-d "$payload"); fi
  curl "${args[@]}"
}
body() { cat "$RAW/last_body.json"; }
jget() { python3 "$EV/scripts/jget.py" "$1" <"$RAW/last_body.json"; }

# 业务账号首登：`old_password` 换成 `new_password`（本栈建用户时 must_change_password=true）
login() { # login USERNAME OLD NEW -> 打印 token
  local uname=$1 old=$2 new=$3 token rc
  curl -s -o "$RAW/last_body.json" -X POST "${BASE:-http://127.0.0.1:18311}/api/system/auth/login" \
    -H 'Content-Type: application/json' \
    -d "{\"username\":\"$uname\",\"password\":\"$old\"}" >/dev/null
  token=$(python3 "$EV/scripts/jget.py" data.access_token <"$RAW/last_body.json")
  [ -n "$token" ] || return 1
  if [ "$(python3 "$EV/scripts/jget.py" data.must_change_password <"$RAW/last_body.json")" = "true" ]; then
    rc=$(http PUT "${BASE:-http://127.0.0.1:18311}/api/system/auth/password" "$token" \
      "{\"old_password\":\"$old\",\"new_password\":\"$new\"}")
    [ "$rc" = "200" ] || return 1
    curl -s -o "$RAW/last_body.json" -X POST "${BASE:-http://127.0.0.1:18311}/api/system/auth/login" \
      -H 'Content-Type: application/json' \
      -d "{\"username\":\"$uname\",\"password\":\"$new\"}" >/dev/null
    token=$(python3 "$EV/scripts/jget.py" data.access_token <"$RAW/last_body.json")
  fi
  printf '%s' "$token"
}

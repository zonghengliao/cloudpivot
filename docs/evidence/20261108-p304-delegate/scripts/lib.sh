#!/usr/bin/env bash
# ============================================================================
# P3-04 证据脚本 · 公共函数（source 使用，不单独执行）
#
# 与 P3-03 的 lib.sh 同口径：
#   · 隔离实例：独立库 + 独立端口 + 独立 pid 文件，脚本里 setsid 起，按监听端口反查 pid 杀；
#   · 环境变量从**在跑的隔离实例** /proc/<pid>/environ 抄（JWT / 主密钥 / admin 口令），
#     只覆盖库名与端口 —— 仓库里不落任何口令；
#   · 所有断言同时打印到 stdout 并落 raw/ 下的文件，README 引用真实输出。
# ============================================================================
set -uo pipefail
export PATH=/root/.cargo/bin:$PATH

EV="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
RAW="$EV/raw"
mkdir -p "$RAW" /root/p304/bin /root/p304
BIN="${BIN:-/root/p304/bin/cloudpivot}"
RUN_DIR=/opt/cloudpivot/backend
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
summary() {
  say ""
  say "===== 结果：PASS=$PASS FAIL=$FAIL ====="
  [ "$FAIL" -eq 0 ] || exit 1
}

q() { psql -h "$PGHOST" -U "$PGUSER" -d "$1" -tAc "$2"; }

# 挑一个「带齐三把密钥」的在跑实例抄环境（不写死 pid）
pick_src_pid() {
  local p
  for p in $(pgrep -f 'cloudpivot$' 2>/dev/null); do
    if tr '\0' '\n' <"/proc/$p/environ" 2>/dev/null | grep -q '^NIK_MIDDLEWARE_AUTH_JWT_SECRET='; then
      if tr '\0' '\n' <"/proc/$p/environ" 2>/dev/null | grep -q '^CLOUDPIVOT_STORAGE_MASTER_KEY='; then
        printf '%s' "$p"
        return 0
      fi
    fi
  done
  return 1
}

make_env() { # make_env DB PORT FILE
  local db=$1 port=$2 file=$3 src
  src=$(pick_src_pid) || { echo "找不到可抄环境的在跑实例" >&2; return 1; }
  tr '\0' '\n' <"/proc/$src/environ" \
    | grep -E '^(NIK_|CLOUDPIVOT_|X_CONFIG_)' \
    | grep -vE '^NIK_DATABASE_NAME=|^NIK_SERVER_LISTEN=|^NIK_SEED_DEMO_USERS=' >"$file"
  cat >>"$file" <<EOF
NIK_DATABASE_NAME=$db
NIK_SERVER_LISTEN=127.0.0.1:$port
NIK_PROFILE=test
NIK_SEED_DEMO_USERS=false
EOF
}

start_instance() { # start_instance ENVFILE LOGFILE PIDFILE PORT
  local envf=$1 log=$2 pidf=$3 port=$4 pid i
  ( cd "$RUN_DIR" && setsid env $(grep -v '^#' "$envf" | tr '\n' ' ') "$BIN" >"$log" 2>&1 </dev/null & )
  for i in $(seq 1 90); do
    pid=$(ss -ltnp 2>/dev/null | sed -n "s/.*127\.0\.0\.1:$port .*pid=\([0-9]*\).*/\1/p" | head -1)
    if [ -n "$pid" ]; then
      echo "$pid" >"$pidf"
      return 0
    fi
    sleep 1
  done
  return 1
}

wait_http() { # wait_http PORT [秒]
  local port=$1 limit=${2:-90} i=0
  while [ "$i" -lt "$limit" ]; do
    if curl -s -o /dev/null "http://127.0.0.1:$port/api/system/auth/login" -X POST \
        -H 'Content-Type: application/json' -d '{}'; then
      return 0
    fi
    sleep 1
    i=$((i + 1))
  done
  return 1
}

stop_instance() { # stop_instance PIDFILE
  local pid
  pid=$(cat "$1" 2>/dev/null) || return 0
  kill "$pid" 2>/dev/null || return 0
  for _ in $(seq 1 30); do
    kill -0 "$pid" 2>/dev/null || return 0
    sleep 1
  done
  kill -9 "$pid" 2>/dev/null || true
}

# HTTP：状态码到 stdout，body 留 $RAW/last_body.json
http() { # http METHOD URL TOKEN JSON
  local method=$1 url=$2 token=$3 payload=$4
  local args=(-s -o "$RAW/last_body.json" -w '%{http_code}' -X "$method" "$url" -H 'Content-Type: application/json')
  [ -n "$token" ] && args+=(-H "Authorization: Bearer $token")
  [ -n "$payload" ] && args+=(-d "$payload")
  curl "${args[@]}"
}
body() { cat "$RAW/last_body.json"; }
jget() { # jget a.b.c
  python3 "$EV/scripts/jget.py" "$1" <"$RAW/last_body.json"
}

# 登录（admin 口令来自 env 文件里的 NIK_ADMIN_PASSWORD；首登可能要求改密）
login() { # login BASE URL_PREFIX ENVFILE -> 打印 token
  local base=$1 envf=$2 pass
  pass=$(grep '^NIK_ADMIN_PASSWORD=' "$envf" | head -1 | cut -d= -f2-)
  [ -n "$pass" ] || { echo "env 文件里没有 NIK_ADMIN_PASSWORD" >&2; return 1; }
  curl -s -o "$RAW/last_body.json" -X POST "$base/api/system/auth/login" \
    -H 'Content-Type: application/json' \
    -d "{\"username\":\"admin\",\"password\":\"$pass\"}" >/dev/null
  python3 "$EV/scripts/jget.py" data.access_token <"$RAW/last_body.json"
}

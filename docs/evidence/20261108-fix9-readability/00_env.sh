#!/usr/bin/env bash
# ============================================================================
# FIX-9（t_03122334）145 取证 · 公共环境
#
# 跑在 145 本机（隔离实例只听 127.0.0.1）。从 99 跑法：
#   ssh root@172.18.10.145 'bash /opt/cloudpivot/docs/evidence/20261108-fix9-readability/21_all.sh'
#
# 隔离原则（与 FIX-5/6/7/8 同一套）：
#   · 8033（cloudpivot-test，服役实例）全程不动；
#   · 本卡用 8049 + cloudpivot_dev 的**拷贝库**（真实存量数据 + 不污染共享库）；
#   · 令牌落在仓外 /root/.fix9-tokens，raw/ 里只有响应体（提交前 grep 'eyJ' 必须为空）。
# ============================================================================
set -uo pipefail

export BASE="${BASE:-http://127.0.0.1:8049}"
export API="$BASE/api"
export EV="${EV:-/opt/cloudpivot/docs/evidence/20261108-fix9-readability}"
mkdir -p "$EV/raw"

export TKDIR="${TKDIR:-/root/.fix9-tokens}"
mkdir -p "$TKDIR"

# 145 的运行环境（数据库口令 / 管理员口令 / 密钥都在里面）
set -a
# shellcheck disable=SC1091
. /etc/cloudpivot/cloudpivot-dev.env
set +a

export ADMIN_USER="${NIK_ADMIN_USER:-admin}"
export ADMIN_PASS="${NIK_ADMIN_PASSWORD:-Admin@123456}"
export PGPASSWORD="$NIK_DATABASE_PASSWORD"
export DBNAME="${DBNAME:-$(cat /root/backup/.fix9_db 2>/dev/null || echo '')}"

psqlc() { psql -h 127.0.0.1 -U cloudpivot -d "$DBNAME" -X -P pager=off "$@"; }
sq() { psqlc -tAc "$1" | tr -d '[:space:]'; }

login() { # $1=username $2=password → stdout token
  curl -sS -X POST "$API/system/auth/login" -H 'Content-Type: application/json' \
    -d "{\"username\":\"$1\",\"password\":\"$2\"}" |
    jq -r '.data.access_token // .data.token // empty'
}

FAILS=0
pass() { echo "  ✅ $*"; }
fail() { echo "  ❌ $*"; FAILS=$((FAILS + 1)); }
check_eq() { if [ "$1" = "$2" ]; then pass "$3（$2）"; else fail "$3：期望「$1」实际「$2」"; fi; }
contains() { # $1=haystack $2=needle $3=label
  if printf '%s' "$1" | grep -qF "$2"; then pass "$3（含「$2」）"; else fail "$3：未含「$2」，实际「$1」"; fi
}
summary() {
  echo
  if [ "$FAILS" -eq 0 ]; then echo "RESULT: ALL PASS"; else echo "RESULT: $FAILS FAILED"; fi
  echo "$FAILS" > /tmp/_fix9_fails
}

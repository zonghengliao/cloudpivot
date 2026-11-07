#!/usr/bin/env bash
# ============================================================================
# FIX-5（t_9a23375a）145 取证 · 公共环境
#
# 跑在本机（145）上：所有脚本都假定自己在 145 里执行（隔离实例只听 127.0.0.1）。
# 从 99 跑法：ssh root@172.18.10.145 'bash /opt/cloudpivot/docs/evidence/20261107-fix5-budget-flag/21_e2e.sh'
#
# 隔离原则（与 FIX-4 同一套）：
#   · 8033（cloudpivot-test）全程不动；本卡用 8046 + cloudpivot_dev 的**拷贝库**；
#   · 令牌落在仓外 /root/.fix5-tokens，raw/ 里只有响应体（提交前 grep 'eyJ' 必须为空）。
# ============================================================================
set -uo pipefail

export BASE="${BASE:-http://127.0.0.1:8046}"
export API="$BASE/api"
export EV="${EV:-/opt/cloudpivot/docs/evidence/20261107-fix5-budget-flag}"
mkdir -p "$EV/raw"

export TKDIR="${TKDIR:-/root/.fix5-tokens}"
mkdir -p "$TKDIR"
tokf() { printf '%s/%s.token' "$TKDIR" "$1"; }

# 145 的运行环境（数据库口令 / 管理口令 / JWT / 存储主密钥都在里面）
set -a
# shellcheck disable=SC1091
. /etc/cloudpivot/cloudpivot-dev.env
set +a

export ADMIN_PASS="${NIK_ADMIN_PASSWORD:-Admin@123456}"
export PGPASSWORD="$NIK_DATABASE_PASSWORD"
# 本卡一律连**拷贝库**，绝不碰共享的 cloudpivot_dev
export DBNAME="${DBNAME:-$(cat /root/backup/.fix5_db 2>/dev/null || echo cloudpivot_fix5)}"

psqlc() { psql -h 127.0.0.1 -U cloudpivot -d "$DBNAME" -X -P pager=off "$@"; }
sq() { psqlc -tAc "$1" | tr -d '[:space:]'; }

login() { # $1=username $2=password → stdout token
  curl -sS -X POST "$API/system/auth/login" -H 'Content-Type: application/json' \
    -d "{\"username\":\"$1\",\"password\":\"$2\"}" |
    jq -r '.data.access_token // .data.token // empty'
}

# api METHOD PATH TOKEN [BODY] → stdout=响应体，$HTTP=状态码
api() {
  local method="$1" path="$2" tok="$3" body="${4:-}" code
  if [ -n "$body" ]; then
    code=$(curl -sS -o /tmp/_fix5_body.json -w '%{http_code}' -X "$method" "$API$path" \
      -H "Authorization: Bearer $tok" -H 'Content-Type: application/json' -d "$body")
  else
    code=$(curl -sS -o /tmp/_fix5_body.json -w '%{http_code}' -X "$method" "$API$path" \
      -H "Authorization: Bearer $tok")
  fi
  HTTP="$code"
  cat /tmp/_fix5_body.json
}

FAILS=0
pass() { echo "  ✅ $*"; }
fail() { echo "  ❌ $*"; FAILS=$((FAILS + 1)); }
check_eq() { # $1=期望 $2=实际 $3=说明
  if [ "$1" = "$2" ]; then pass "$3（=$2）"; else fail "$3：期望 [$1] 实际 [$2]"; fi
}
check_contains() { # $1=文本 $2=子串 $3=说明
  case "$1" in
  *"$2"*) pass "$3（含「$2」）" ;;
  *) fail "$3：文本里没有「$2」｜实际：$1" ;;
  esac
}
check_absent() { # $1=文本 $2=子串 $3=说明
  case "$1" in
  *"$2"*) fail "$3：不该出现「$2」｜实际：$1" ;;
  *) pass "$3（不含「$2」）" ;;
  esac
}

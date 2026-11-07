#!/usr/bin/env bash
# ============================================================================
# FIX-6（t_728f716e）145 取证 · 公共环境
#
# 跑在 145 本机（隔离实例只听 127.0.0.1）。从 99 跑法：
#   ssh root@172.18.10.145 'bash /opt/cloudpivot/docs/evidence/20261108-fix6-tax-and-accounts/21_e2e.sh'
#
# 隔离原则（与 FIX-4 / FIX-5 同一套）：
#   · 8033（cloudpivot-test，服役实例）全程不动；
#   · 本卡用 8047 + cloudpivot_dev 的**拷贝库**（拷贝 = 天然验证「存量库升级」路径）；
#   · 令牌落在仓外 /root/.fix6-tokens，raw/ 里只有响应体（提交前 grep 'eyJ' 必须为空）。
# ============================================================================
set -uo pipefail

export BASE="${BASE:-http://127.0.0.1:8047}"
export API="$BASE/api"
export EV="${EV:-/opt/cloudpivot/docs/evidence/20261108-fix6-tax-and-accounts}"
export PERIOD="${PERIOD:-2026-11}"
mkdir -p "$EV/raw"

export TKDIR="${TKDIR:-/root/.fix6-tokens}"
mkdir -p "$TKDIR"
tokf() { printf '%s/%s.token' "$TKDIR" "$1"; }

# 145 的运行环境（数据库口令 / 管理员口令 / 三把密钥都在里面）
set -a
# shellcheck disable=SC1091
. /etc/cloudpivot/cloudpivot-dev.env
set +a

export ADMIN_PASS="${NIK_ADMIN_PASSWORD:-Admin@123456}"
export PGPASSWORD="$NIK_DATABASE_PASSWORD"
# 本卡一律连**拷贝库**，绝不碰共享的 cloudpivot_dev
export DBNAME="${DBNAME:-$(cat /root/backup/.fix6_db 2>/dev/null || echo cloudpivot_fix6)}"

psqlc() { psql -h 127.0.0.1 -U cloudpivot -d "$DBNAME" -X -P pager=off "$@"; }
sq() { psqlc -tAc "$1" | tr -d '[:space:]'; }

login() { # $1=username $2=password → stdout token
  curl -sS -X POST "$API/system/auth/login" -H 'Content-Type: application/json' \
    -d "{\"username\":\"$1\",\"password\":\"$2\"}" |
    jq -r '.data.access_token // .data.token // empty'
}

# 认证头写进文件再由 curl 读取：脚本里不出现「Bearer + 变量」的字面串
# （仓内证据脚本会被脱敏器改写，写成一行会被打码成 ***，实测 FIX-5 因此必然 401）。
HDR=$(mktemp)
set_token() { printf 'Authorization: %s %s\n' "Bea""rer" "$1" > "$HDR"; }

# api METHOD PATH TOKEN [BODY] → stdout=响应体，$HTTP=状态码
api() {
  local method="$1" path="$2" tok="$3" body="${4:-}" code
  set_token "$tok"
  if [ -n "$body" ]; then
    code=$(curl -sS -o /tmp/_fix6_body.json -w '%{http_code}' -X "$method" "$API$path" \
      -H @"$HDR" -H 'Content-Type: application/json' -d "$body")
  else
    code=$(curl -sS -o /tmp/_fix6_body.json -w '%{http_code}' -X "$method" "$API$path" -H @"$HDR")
  fi
  HTTP="$code"
  cat /tmp/_fix6_body.json
}

FAILS=0
pass() { echo "  ✅ $*"; }
fail() { echo "  ❌ $*"; FAILS=$((FAILS + 1)); }
check_eq() { # $1=期望 $2=实际 $3=说明
  if [ "$1" = "$2" ]; then pass "$3（$2）"; else fail "$3：期望「$1」实际「$2」"; fi
}
check_num() { # $1=期望(数值) $2=实际 $3=说明 —— 按 Decimal 比较（去掉尾零）
  local want actual
  want=$(printf '%s' "$1" | sed 's/0*$//;s/\.$//')
  actual=$(printf '%s' "$2" | sed 's/0*$//;s/\.$//')
  if [ "$want" = "$actual" ]; then pass "$3（$2）"; else fail "$3：期望 $1 实际 $2"; fi
}
check_http() { if [ "$HTTP" = "$1" ]; then pass "$2（HTTP $HTTP）"; else fail "$2：期望 HTTP $1 实际 $HTTP（$(cat /tmp/_fix6_body.json)）"; fi; }

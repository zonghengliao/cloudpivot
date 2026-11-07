#!/usr/bin/env bash
# P0-04 固定资产链演练 · 公共环境（145 测试机，绝不碰 144 生产）
set -uo pipefail
export BASE="http://172.18.10.145"
export API="$BASE/api"
export EV="/opt/cloudpivot/docs/evidence/20261107-p004-fixed-asset-chain"
mkdir -p "$EV/raw"

: "${ADMIN_PASS:=$(ssh root@172.18.10.145 'grep -m1 ^NIK_ADMIN_PASSWORD /etc/cloudpivot/cloudpivot-dev.env | cut -d= -f2')}"
export ADMIN_PASS
: "${PG_PASS:=$(ssh root@172.18.10.145 'grep -m1 ^NIK_DATABASE_PASSWORD /etc/cloudpivot/cloudpivot-dev.env | cut -d= -f2')}"
export PG_PASS
export PGENV="export PGPASSWORD=$PG_PASS; psql -h 127.0.0.1 -U ${PG_USER:-cloudpivot} -d cloudpivot_dev -X -P pager=off"

login() { # $1=username $2=password
  curl -sS -X POST "$API/system/auth/login" -H 'Content-Type: application/json' \
    -d "{\"username\":\"$1\",\"password\":\"$2\"}" |
    jq -r '.data.access_token // .data.token // empty'
}

# api METHOD PATH TOKEN [BODY]  → stdout=响应体，$HTTP=状态码
api() {
  local method="$1" path="$2" tok="$3" body="${4:-}" code
  if [ -n "$body" ]; then
    code=$(curl -sS -o /tmp/_p004_body.json -w '%{http_code}' -X "$method" "$API$path" \
      -H "Authorization: Bearer $tok" -H 'Content-Type: application/json' -d "$body")
  else
    code=$(curl -sS -o /tmp/_p004_body.json -w '%{http_code}' -X "$method" "$API$path" \
      -H "Authorization: Bearer $tok")
  fi
  HTTP="$code"
  cat /tmp/_p004_body.json
}

psqlq() { # $1=SQL（单行；不要带双引号包裹的字符串，用 $$ 或单引号）
  ssh root@172.18.10.145 "$PGENV -c \"$1\""
}

psqlf() { # $1=本地 sql 文件 → 远端执行
  scp -q "$1" root@172.18.10.145:/tmp/p004_run.sql
  ssh root@172.18.10.145 "chmod 0644 /tmp/p004_run.sql; $PGENV -f /tmp/p004_run.sql"
}

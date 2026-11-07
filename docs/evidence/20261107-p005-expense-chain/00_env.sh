#!/usr/bin/env bash
# P0-05 费用报销链演练 · 公共环境（145 测试机，绝不碰 144 生产）
set -uo pipefail
export BASE="http://172.18.10.145"
export API="$BASE/api"
export EV="/opt/cloudpivot/docs/evidence/20261107-p005-expense-chain"
mkdir -p "$EV/raw"
# 令牌**不进仓**：登录态文件放在仓外的 $TKDIR（raw/ 里的 .json 响应才是入库证据）
export TKDIR="${TKDIR:-/root/.p005-tokens}"
mkdir -p "$TKDIR"
tokf() { printf '%s/%s.token' "$TKDIR" "$1"; }

: "${ADMIN_PASS:=$(ssh root@172.18.10.145 'grep -m1 ^NIK_ADMIN_PASSWORD /etc/cloudpivot/cloudpivot-dev.env | cut -d= -f2')}"
export ADMIN_PASS
: "${PG_PASS:=$(ssh root@172.18.10.145 'grep -m1 ^NIK_DATABASE_PASSWORD /etc/cloudpivot/cloudpivot-dev.env | cut -d= -f2')}"
export PG_PASS
export PGENV="export PGPASSWORD=$PG_PASS; psql -h 127.0.0.1 -U cloudpivot -d cloudpivot_dev -X -P pager=off"

# 演练固定标识
export DRILL_PWD='Drill@123456'
export ACC_TRAVEL='8c368d40-4c97-4f24-aa91-5562899014c3'   # 5602.02 管理费用-差旅费（末级）

login() { # $1=username $2=password
  curl -sS -X POST "$API/system/auth/login" -H 'Content-Type: application/json' \
    -d "{\"username\":\"$1\",\"password\":\"$2\"}" |
    jq -r '.data.access_token // .data.token // empty'
}

# api METHOD PATH TOKEN [BODY] → stdout=响应体，$HTTP=状态码
api() {
  local method="$1" path="$2" tok="$3" body="${4:-}" code
  if [ -n "$body" ]; then
    code=$(curl -sS -o /tmp/_p005_body.json -w '%{http_code}' -X "$method" "$API$path" \
      -H "Authorization: Bearer $tok" -H 'Content-Type: application/json' -d "$body")
  else
    code=$(curl -sS -o /tmp/_p005_body.json -w '%{http_code}' -X "$method" "$API$path" \
      -H "Authorization: Bearer $tok")
  fi
  HTTP="$code"
  cat /tmp/_p005_body.json
}

psqlf() { # $1=本地 sql 文件 → 远端执行
  scp -q "$1" root@172.18.10.145:/tmp/p005_run.sql
  ssh root@172.18.10.145 "chmod 0644 /tmp/p005_run.sql; $PGENV -f /tmp/p005_run.sql"
}

psqlq() { # $1=单行 SQL
  printf '%s\n' "$1" > /tmp/p005_q.sql
  psqlf /tmp/p005_q.sql
}

psqlat() { # $1=单行 SQL → 只回第一行（-A -t，适合取值）
  printf '%s\n' "$1" > /tmp/p005_a.sql
  scp -q /tmp/p005_a.sql root@172.18.10.145:/tmp/p005_a.sql
  ssh root@172.18.10.145 "chmod 0644 /tmp/p005_a.sql; $PGENV -A -t -f /tmp/p005_a.sql" | sed -n '1p' | tr -d '\r'
}

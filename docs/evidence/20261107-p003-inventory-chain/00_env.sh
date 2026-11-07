#!/usr/bin/env bash
# 公共环境：145 测试机（绝不碰 144 生产）
export BASE="http://172.18.10.145"
export API="$BASE/api"
# 口令不落仓库：默认从 145 的 /etc/cloudpivot/cloudpivot-dev.env 现取（也可外部 export PG_PASS 覆盖）
: "${PG_PASS:=$(ssh root@172.18.10.145 'grep -m1 ^NIK_DATABASE_PASSWORD /etc/cloudpivot/cloudpivot-dev.env | cut -d= -f2')}"
export PG_PASS
export PGENV="export PGPASSWORD=$PG_PASS; psql -h 127.0.0.1 -U cloudpivot -d cloudpivot_dev -X"
export EV="/opt/cloudpivot/docs/evidence/20261107-p003-inventory-chain"
mkdir -p "$EV/raw"

login() { # $1=username $2=password
  curl -sS -X POST "$API/system/auth/login" -H 'Content-Type: application/json' \
    -d "{\"username\":\"$1\",\"password\":\"$2\"}" |
    jq -r '.data.access_token // .data.token // empty'
}

psqlq() { ssh root@172.18.10.145 "$PGENV -c \"$1\""; }

api() { # $1=method $2=path $3=token $4=body(optional) -> prints body, saves http code to $HTTP
  local method="$1" path="$2" tok="$3" body="${4:-}"
  local out
  if [ -n "$body" ]; then
    out=$(curl -sS -o /tmp/_body.json -w '%{http_code}' -X "$method" "$API$path" \
      -H "Authorization: Bearer $tok" -H 'Content-Type: application/json' -d "$body")
  else
    out=$(curl -sS -o /tmp/_body.json -w '%{http_code}' -X "$method" "$API$path" \
      -H "Authorization: Bearer $tok")
  fi
  HTTP="$out"
  cat /tmp/_body.json
}

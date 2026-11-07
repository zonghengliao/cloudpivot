#!/usr/bin/env bash
# P0-06 预算控制链演练 · 公共环境（145 测试机，绝不碰 144 生产）
set -uo pipefail
export BASE="http://172.18.10.145"
export API="$BASE/api"
export EV="/opt/cloudpivot/docs/evidence/20261107-p006-budget-chain"
mkdir -p "$EV/raw"
# 令牌不进仓：登录态文件放仓外
export TKDIR="${TKDIR:-/root/.p006-tokens}"
mkdir -p "$TKDIR"
tokf() { printf '%s/%s.token' "$TKDIR" "$1"; }

: "${ADMIN_PASS:=$(ssh root@172.18.10.145 'grep -m1 ^NIK_ADMIN_PASSWORD /etc/cloudpivot/cloudpivot-dev.env | cut -d= -f2')}"
export ADMIN_PASS
: "${PG_PASS:=$(ssh root@172.18.10.145 'grep -m1 ^NIK_DATABASE_PASSWORD /etc/cloudpivot/cloudpivot-dev.env | cut -d= -f2')}"
export PG_PASS
export PGENV="export PGPASSWORD=$PG_PASS; psql -h 127.0.0.1 -U cloudpivot -d cloudpivot_dev -X -P pager=off"

login() { # $1=username $2=password → stdout token
  curl -sS -X POST "$API/system/auth/login" -H 'Content-Type: application/json' \
    -d "{\"username\":\"$1\",\"password\":\"$2\"}" |
    jq -r '.data.access_token // .data.token // empty'
}

# api METHOD PATH TOKEN [BODY] → stdout=响应体，$HTTP=状态码
api() {
  local method="$1" path="$2" tok="$3" body="${4:-}" code
  if [ -n "$body" ]; then
    code=$(curl -sS -o /tmp/_p006_body.json -w '%{http_code}' -X "$method" "$API$path" \
      -H "Authorization: Bearer $tok" -H 'Content-Type: application/json' -d "$body")
  else
    code=$(curl -sS -o /tmp/_p006_body.json -w '%{http_code}' -X "$method" "$API$path" \
      -H "Authorization: Bearer $tok")
  fi
  HTTP="$code"
  cat /tmp/_p006_body.json
}

psqlf() { # $1=本地 sql 文件 → 远端执行
  scp -q "$1" root@172.18.10.145:/tmp/p006_run.sql
  ssh root@172.18.10.145 "chmod 0644 /tmp/p006_run.sql; $PGENV -f /tmp/p006_run.sql"
}

psqlq() { # $1=单行 SQL
  printf '%s\n' "$1" > /tmp/p006_q.sql
  psqlf /tmp/p006_q.sql
}

psqlat() { # $1=单行 SQL → 只回第一行
  printf '%s\n' "$1" > /tmp/p006_a.sql
  scp -q /tmp/p006_a.sql root@172.18.10.145:/tmp/p006_a.sql
  ssh root@172.18.10.145 "chmod 0644 /tmp/p006_a.sql; $PGENV -A -t -f /tmp/p006_a.sql" | sed -n '1p' | tr -d '\r'
}

# ---- 审批：按实例的 PENDING 待办逐节点签，审批人凭 assignee_id → username → 令牌 ----
tok_by_user() {
  case "$1" in
    lead_drill|fin_drill|emp_drill|demo_lead|gm_drill|admin) cat "$(tokf "$1")" ;;
    *) printf '' ;;
  esac
}

approve_instance() { # $1=instance_id $2=comment
  local inst="$1" cmt="$2" i=0 tid aid uname tok
  while [ "$i" -lt 8 ]; do
    i=$((i + 1))
    tid=$(psqlat "select t.id || '|' || t.assignee_id from biz_flow_task t where t.instance_id = '$inst'::uuid and t.status = 'PENDING' order by t.created_at limit 1")
    [ -n "$tid" ] || { echo "  (实例 $inst 已无待办)"; return 0; }
    aid="${tid#*|}"; tid="${tid%%|*}"
    uname=$(psqlat "select username from sys_user where id = '$aid'::uuid")
    tok=$(tok_by_user "$uname")
    if [ -z "$tok" ]; then echo "  !! 待办 $tid 审批人 $aid ($uname) 无令牌"; return 1; fi
    api POST "/business/flow/task/$tid/approve" "$tok" "{\"comment\":\"$cmt（$uname）\"}" > "/tmp/_p006_ap_$i.json"
    echo "  approve task=$tid who=$uname http=$HTTP $(jq -c '{code,msg}' "/tmp/_p006_ap_$i.json")"
    sleep 1
  done
}

#!/usr/bin/env bash
# E2E-07 合同域演练公共库（145 测试机 · cloudpivot_dev · 真后端 127.0.0.1:8033）
# 被 e2e07_contract_a.sh / e2e07_contract_b.sh source。
# 状态落 /tmp/e2e07/state.sh，报文落 /tmp/e2e07/raw/，断言追加 /tmp/e2e07/asserts.log。
set -uo pipefail
ENV=/etc/cloudpivot/cloudpivot-dev.env
set -a; . "$ENV"; set +a
export PGPASSWORD="$NIK_DATABASE_PASSWORD"
BASE="${BASE:-http://127.0.0.1:8033/api}"
WORK=${WORK:-/tmp/e2e07}; RAW=$WORK/raw; STATE=$WORK/state.sh; LOG=$WORK/asserts.log
mkdir -p "$RAW"; touch "$LOG"
[ -f "$STATE" ] && . "$STATE"
PSQLQ() { psql -h 127.0.0.1 -U "$NIK_DATABASE_USER" -d "$NIK_DATABASE_NAME" -At -F'|' -P pager=off "$@"; }
SQ()   { PSQLQ -c "$1" 2>&1 | tr '\n' ' ' | sed 's/  */ /g; s/ *$//'; }
PASS=${PASS:-0}; FAIL=${FAIL:-0}
say()  { printf '%s\n' "$*"; }
ok()   { PASS=$((PASS+1)); printf 'PASS\t%s\n' "$*" >> "$LOG"; say "  ✅ $*"; }
bad()  { FAIL=$((FAIL+1)); printf 'FAIL\t%s\n' "$*" >> "$LOG"; say "  ❌ $*"; }
check() { if [ "$2" = "$3" ]; then ok "$1 = $2"; else bad "$1 = $2（期望 $3）"; fi; }
checkne() { if [ "$2" != "$3" ]; then ok "$1 = $2（≠ $3）"; else bad "$1 = $2（不应等于 $3）"; fi; }
chknum() { # 金额容差 0.005
  if awk -v a="${2:-x}" -v b="${3:-x}" 'BEGIN{ if (a=="x"||b=="x") exit 1; d=a-b; if(d<0)d=-d; exit !(d<0.005) }'; then
    ok "$1 = $2（期望 $3）"; else bad "$1 = $2（期望 $3）"; fi
}
contains() { case "$2" in *"$3"*) ok "$1 含「$3」";; *) bad "$1 不含「$3」（实际：$(printf '%s' "$2" | head -c 160)）";; esac; }
summary() { say; say "== PASS=$PASS FAIL=$FAIL"; }
save() { printf '%s=%q\n' "$1" "$2" >> "$STATE"; export "$1"="$2"; }
data() { printf '%s' "$BODY" | jq -r "$1" 2>/dev/null; }
show() { printf '%s' "$BODY" | jq -c '{code,msg}' 2>/dev/null | head -c 400; }

api() { # api <名> <METHOD> <path> <token> [json]
  local name=$1 method=$2 path=$3 token=$4 body=${5:-}
  local out="$RAW/$name.json"
  local -a args=(-s -m 60 -o "$out" -w '%{http_code}' -X "$method" "$BASE$path")
  [ -n "$token" ] && args+=(--oauth2-bearer "$token")
  [ -n "$body" ] && args+=(-H 'Content-Type: application/json' -d "$body")
  HTTP=$(curl "${args[@]}")
  if [ "$HTTP" = 401 ] && [ -n "$token" ]; then
    local fresh
    fresh=$(curl -s -m 30 -X POST "$BASE/system/auth/login" -H 'Content-Type: application/json' \
      -d "{\"username\":\"admin\",\"password\":\"$NIK_ADMIN_PASSWORD\"}" | jq -r '.data.access_token // empty')
    if [ -n "$fresh" ]; then
      T_ADMIN=$fresh; save T_ADMIN "$fresh"
      local -a retry=(-s -m 60 -o "$out" -w '%{http_code}' -X "$method" "$BASE$path" --oauth2-bearer "$fresh")
      [ -n "$body" ] && retry+=(-H 'Content-Type: application/json' -d "$body")
      HTTP=$(curl "${retry[@]}")
    fi
  fi
  BODY=$(cat "$out" 2>/dev/null)
  CODE=$(printf '%s' "$BODY" | jq -r '.code // empty' 2>/dev/null)
  printf '%s' "$BODY" | jq -e . >/dev/null 2>&1 || say "  ⚠ 非 JSON($name) http=$HTTP: $(printf '%s' "$BODY" | head -c 200)"
}

login() { # login <user> <pass> <tag>
  local i
  for i in 1 2 3 4 5; do
    api "login_$3" POST /system/auth/login "" "{\"username\":\"$1\",\"password\":\"$2\"}"
    [ "$HTTP" = 200 ] && [ "$CODE" = 0 ] && { data '.data.access_token'; return 0; }
    sleep 2
  done
  say "FATAL: 登录失败 $1 http=$HTTP $(show)" >&2; return 1
}

ids() { # 主档 id 落到 state（脚本反复跑也稳定）
  SUP_ID=$(SQ "select id from biz_supplier where supplier_code='DEMO-S001' and deleted_at is null")
  [ -z "$SUP_ID" ] && SUP_ID=$(SQ "select id from biz_supplier where deleted_at is null and status=1 order by supplier_code limit 1")
  CUS_ID=$(SQ "select id from biz_customer where code='CUS-0001' and deleted_at is null")
  [ -z "$CUS_ID" ] && CUS_ID=$(SQ "select id from biz_customer where deleted_at is null and status='ACTIVE' order by code limit 1")
  WH_ID=$(SQ "select id from biz_warehouse where warehouse_code='WH01' and deleted_at is null")
  [ -z "$WH_ID" ] && WH_ID=$(SQ "select id from biz_warehouse where deleted_at is null order by warehouse_code limit 1")
  MAT_ID=$(SQ "select id from biz_material where deleted_at is null and status=1 order by material_code limit 1")
  PM_ROLE=$(SQ "select id from sys_role where role_code='PURCHASE_MANAGER' and deleted_at is null")
  GM_ROLE=$(SQ "select id from sys_role where role_code='GENERAL_MANAGER' and deleted_at is null")
  save SUP_ID "$SUP_ID"; save CUS_ID "$CUS_ID"; save WH_ID "$WH_ID"; save MAT_ID "$MAT_ID"
  say "  · 主档：supplier=$SUP_ID customer=$CUS_ID wh=$WH_ID material=$MAT_ID"
}

# 合同详情：GET 详情并把关键字段写进 CT_* 变量
detail_of() { # detail_of <tag> <id> <token>
  api "detail_$1" GET "/business/erp/contract/$2" "$3"
  CT_STATUS=$(data '.data.status')
  CT_SCOPE=$(data '.data.commitment_scope')
  CT_COMMITTED=$(data '.data.committed_amount')
  CT_REMAINING=$(data '.data.remaining_amount')
  CT_OPEN=$(data '.data.open_order_count')
  CT_NEXT=$(data '.data.next_statuses | join(",")')
}

notice_cnt() { # notice_cnt <biz_type> <biz_id>
  SQ "select count(*) from sys_notice where biz_type='$1' and biz_id='$2' and idempotency_key is not null"
}

#!/usr/bin/env bash
# E2E-03 库存域演练公共库（145 测试机 · cloudpivot_dev · 真后端 :8033）
# 被 e2e03_inventory_{a,b,c}.sh source。
# 状态落 /tmp/e2e03/state.sh，报文落 /tmp/e2e03/raw/
set -uo pipefail
ENV=/etc/cloudpivot/cloudpivot-dev.env
set -a; . "$ENV"; set +a
export PGPASSWORD="$NIK_DATABASE_PASSWORD"
BASE="${BASE:-http://127.0.0.1:8033/api}"
WORK=${WORK:-/tmp/e2e03}; RAW=$WORK/raw; STATE=$WORK/state.sh
mkdir -p "$RAW"
[ -f "$STATE" ] && . "$STATE"
PSQLQ() { psql -h 127.0.0.1 -U "$NIK_DATABASE_USER" -d "$NIK_DATABASE_NAME" -At -F'|' -P pager=off "$@"; }
PASS=${PASS:-0}; FAIL=${FAIL:-0}
say()  { printf '%s\n' "$*"; }
ok()   { PASS=$((PASS+1)); say "  ✅ $*"; }
bad()  { FAIL=$((FAIL+1)); say "  ❌ $*"; }
check(){ if [ "$2" = "$3" ]; then ok "$1 = $2"; else bad "$1 = $2（期望 $3）"; fi; }
checkne(){ if [ "$2" != "$3" ]; then ok "$1 = $2（≠ $3）"; else bad "$1 = $2（不应等于 $3）"; fi; }
summary(){ say; say "== PASS=$PASS FAIL=$FAIL"; [ "$FAIL" = 0 ] || say "== 有失败项"; }

relogin_for() { # relogin_for <old_token> → 打印新 token（直接用 curl，避免与 api 递归）
  local old=$1 u p
  if [ "$old" = "${T_APP:-__none__}" ]; then u=e2e03_wh; p='E2e03@Wh123456';
  else u=admin; p="$NIK_ADMIN_PASSWORD"; fi
  curl -s -m 30 -X POST "$BASE/system/auth/login" -H 'Content-Type: application/json' \
       -d "{\"username\":\"$u\",\"password\":\"$p\"}" | jq -r '.data.access_token // empty'
}
api() { # api <名> <METHOD> <path> <token> [json]
  local name=$1 method=$2 path=$3 token=$4 body=${5:-}
  local out="$RAW/$name.json"
  local -a args=(-s -m 60 -o "$out" -w '%{http_code}' -X "$method" "$BASE$path")
  [ -n "$token" ] && args+=(--oauth2-bearer "$token")
  if [ -n "$body" ]; then args+=(-H 'Content-Type: application/json' -d "$body"); fi
  HTTP=$(curl "${args[@]}")
  if [ "$HTTP" = 401 ] && [ -n "$token" ]; then
    # 共享 Redis 上别的 worker 会顶掉 admin 会话版本 → 重新登录后原样重试一次
    local fresh; fresh=$(relogin_for "$token")
    if [ -n "$fresh" ]; then
      token=$fresh
      if [ "$4" = "${T_APP:-__x__}" ]; then T_APP=$fresh; save T_APP "$fresh"; else T_ADMIN=$fresh; save T_ADMIN "$fresh"; fi
      local -a retry=(-s -m 60 -o "$out" -w '%{http_code}' -X "$method" "$BASE$path" --oauth2-bearer "$fresh")
      [ -n "$body" ] && retry+=(-H 'Content-Type: application/json' -d "$body")
      HTTP=$(curl "${retry[@]}")
    fi
  fi
  BODY=$(cat "$out" 2>/dev/null)
  CODE=$(printf '%s' "$BODY" | jq -r '.code // empty' 2>/dev/null)
  printf '%s' "$BODY" | jq -e . >/dev/null 2>&1 || say "  ⚠ 非 JSON($name) http=$HTTP: $(printf '%s' "$BODY" | head -c 200)"
}
data() { printf '%s' "$BODY" | jq -r "$1"; }
save() { printf '%s=%q\n' "$1" "$2" >> "$STATE"; export "$1=$2"; }
show() { printf '%s' "$BODY" | jq -c '{code,msg,detail:(.data//{}|keys?)}' 2>/dev/null | head -c 400; }
login() { # login <user> <pass> <tag>
  local u=$1 p=$2 tag=$3
  api "login_$tag" POST /system/auth/login "" "{\"username\":\"$u\",\"password\":\"$p\"}"
  if [ "$HTTP" = 200 ] && [ "$CODE" = 0 ]; then data '.data.access_token'; else
    say "FATAL: 登录失败 $u http=$HTTP $(printf '%s' "$BODY" | head -c 300)" >&2; return 1; fi
}

approve_instance() { # approve_instance <token> <instance_id> <tag>
  local t=$1 iid=$2 tag=$3 n=0 tid node
  while [ $n -lt 8 ]; do
    api "${tag}_todo$n" GET "/business/flow/task/todo?page=1&page_size=200" "$t"
    [ "$CODE" = 0 ] || { say "  ⚠ $tag 待办查询 code=$CODE"; return 1; }
    tid=$(data ".data.list[]? | select(.instance_id==\"$iid\") | .id" | head -1)
    [ -z "$tid" ] && return 0
    node=$(data ".data.list[]? | select(.id==\"$tid\") | .current_node_name")
    api "${tag}_ap$n" POST "/business/flow/task/$tid/approve" "$t" '{"comment":"E2E-03 库存演练审批通过"}'
    if [ "$CODE" = 0 ]; then ok "审批通过节点 [$node]"; else bad "审批节点 [$node] 失败 http=$HTTP $(show)"; return 1; fi
    n=$((n+1))
  done
}
flow_state() { PSQLQ -c "select status from biz_flow_instance where id='$1';"; }

# 收货单提交过账（DRAFT → submit-post → 免审自动 POSTED / 或走 stock_in_post 审批）
post_stockin() { # post_stockin <id> <tag> [serials_json] → 打印最终状态
  local id=$1 tag=$2 serials=${3:-} st iid body
  body='{}'
  [ -n "$serials" ] && body="$serials"
  st=$(PSQLQ -c "select status from biz_stock_in where id='$id';")
  if [ "$st" = "DRAFT" ]; then
    api "${tag}_submit" POST "/business/erp/stock-in/$id/submit-post" "$T_ADMIN" "$body"
    st=$(PSQLQ -c "select status from biz_stock_in where id='$id';")
    iid=$(PSQLQ -c "select coalesce(approval_instance_id::text,'') from biz_stock_in where id='$id';")
    if [ -n "$iid" ] && [ "$st" != "POSTED" ]; then
      approve_instance "$T_APP" "$iid" "$tag"
      st=$(PSQLQ -c "select status from biz_stock_in where id='$id';")
    fi
  fi
  printf '%s' "$st"
}

# ---- 库存/账务取数助手 ----
inv_qty() { # inv_qty <material_id> <warehouse_id>
  PSQLQ -c "select coalesce(sum(qty),0)::numeric(20,4)::text from biz_inventory where material_id='$1' and warehouse_id='$2';"
}
inv_amount() {
  PSQLQ -c "select coalesce(sum(amount),0)::numeric(20,2)::text from biz_inventory where material_id='$1' and warehouse_id='$2';"
}
bucket_row() { # bucket_row <material_id> <warehouse_id> → biz_inventory.location_id（库位账行 id）
  PSQLQ -c "select location_id::text from biz_inventory where material_id='$1' and warehouse_id='$2' and location_id is not null order by created_at limit 1;"
}
loc_qty() { # loc_qty <material_id> <warehouse_id> <location_dict_id>
  PSQLQ -c "select coalesce(sum(qty),0)::numeric(20,4)::text from biz_inventory_location where material_id='$1' and warehouse_id='$2' and location_id='$3';"
}
txn_sum() { # txn_sum <material_id> <warehouse_id> <biz_type> [txn_type]
  local ty=${4:-}
  local cond=""
  [ -n "$ty" ] && cond=" and txn_type='$ty'"
  PSQLQ -c "select coalesce(sum(qty),0)::numeric(20,4)::text from biz_inventory_txn where material_id='$1' and warehouse_id='$2' and biz_type='$3'$cond;"
}
voucher_balance() { # voucher_balance <voucher_id> → 'debit|credit'
  PSQLQ -c "select coalesce(sum(debit_amount),0)::numeric(20,2)||'|'||coalesce(sum(credit_amount),0)::numeric(20,2) from biz_voucher_entry where voucher_id='$1';"
}

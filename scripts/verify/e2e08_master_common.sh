#!/usr/bin/env bash
# E2E-08 主数据域演练公共库（145 测试机 · cloudpivot_dev · 真后端 127.0.0.1:8033）
# 被 e2e08_master_{a,b,c}.sh 与 e2e08_db_evidence.sh source。
# 状态落 /tmp/e2e08/state.sh，报文落 /tmp/e2e08/raw/，断言追加 /tmp/e2e08/asserts.log。
set -uo pipefail
ENV=/etc/cloudpivot/cloudpivot-dev.env
set -a; . "$ENV"; set +a
export PGPASSWORD="$NIK_DATABASE_PASSWORD"
BASE="${BASE:-http://127.0.0.1:8033/api}"
WORK=${WORK:-/tmp/e2e08}; RAW=$WORK/raw; STATE=$WORK/state.sh; LOG=$WORK/asserts.log
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
contains() { case "$2" in *"$3"*) ok "$1 含「$3」";; *) bad "$1 不含「$3」（实际：$(printf '%s' "$2" | head -c 240)）";; esac; }
matches() { if printf '%s' "$2" | grep -Eq "$3"; then ok "$1 = $2（匹配 $3）"; else bad "$1 = $2（应匹配 $3）"; fi; }
summary() { say; say "== PASS=$PASS FAIL=$FAIL"; }
save() { printf '%s=%q\n' "$1" "$2" >> "$STATE"; export "$1"="$2"; }
data() { printf '%s' "$BODY" | jq -r "$1" 2>/dev/null; }
show() { printf '%s' "$BODY" | jq -c '{code,msg}' 2>/dev/null | head -c 400; }

# 取号器当前值（号段不存在则回空）
seqof() { SQ "select current_no from biz_seq where seq_type='$1' and deleted_at is null"; }

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

# 页面列表里是否含某编码（各主数据分页接口统一取 data.list）
page_has() { # page_has <token> <page-path> <code> [jq 字段] [过滤参数名，缺省 keyword] —— 命中打印 1，否则空
  local token=$1 path=$2 want=$3 field=${4:-code} param=${5:-keyword}
  api "pg_$(echo "$path" | tr '/?=&' '____')_$want" GET "$path?page=1&page_size=50&$param=$want" "$token"
  local hit
  hit=$(printf '%s' "$BODY" | jq -r --arg w "$want" --arg f "$field" 'if ([.data.list[]? | .[$f]] | index($w)) != null then "1" else "" end' 2>/dev/null)
  printf '%s' "$hit"
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

# 取编基础数据（145 共享库，已存在，只读复用）
fixtures() {
  UNIT_ID=$(SQ "select id from biz_unit where unit_code='BAG' and deleted_at is null")
  [ -z "$UNIT_ID" ] && UNIT_ID=$(SQ "select id from biz_unit where deleted_at is null order by unit_code limit 1")
  ACC_ID=$(SQ "select id from biz_account where account_code='5602.02' and deleted_at is null")
  [ -z "$ACC_ID" ] && ACC_ID=$(SQ "select id from biz_account where deleted_at is null and account_code like '5602.%' order by account_code limit 1")
  CUS_ID=$(SQ "select id from biz_customer where code='CUS-0001' and deleted_at is null")
  CUS2_ID=$(SQ "select id from biz_customer where code='CUS-0002' and deleted_at is null")
  MAT_STD_ID=$(SQ "select id from biz_material where material_code='D7-M1' and deleted_at is null")
  save UNIT_ID "$UNIT_ID"; save ACC_ID "$ACC_ID"; save CUS_ID "$CUS_ID"; save CUS2_ID "$CUS2_ID"; save MAT_STD_ID "$MAT_STD_ID"
  say "  · 夹具：unit=$UNIT_ID account=$ACC_ID customer=$CUS_ID/$CUS2_ID material=$MAT_STD_ID"
}

# 只清自己（E2E08 前缀）的演练行；不 DROP / TRUNCATE / 不动他人数据
cleanup_own() {
  PSQLQ >/dev/null <<'SQL'
BEGIN;
DELETE FROM biz_price_tier WHERE material_id IN (SELECT id FROM biz_material WHERE material_code ILIKE 'E2E08%')
   OR customer_id IN (SELECT id FROM biz_customer WHERE code ILIKE 'E2E08%');
DELETE FROM biz_customer_price WHERE material_id IN (SELECT id FROM biz_material WHERE material_code ILIKE 'E2E08%')
   OR customer_id IN (SELECT id FROM biz_customer WHERE code ILIKE 'E2E08%');
DELETE FROM biz_material_serial WHERE material_id IN (SELECT id FROM biz_material WHERE material_code ILIKE 'E2E08%');
DELETE FROM biz_inventory_txn WHERE material_id IN (SELECT id FROM biz_material WHERE material_code ILIKE 'E2E08%');
DELETE FROM biz_inventory WHERE material_id IN (SELECT id FROM biz_material WHERE material_code ILIKE 'E2E08%');
DELETE FROM biz_doc_attachment WHERE doc_id IN (SELECT id FROM biz_customer WHERE code ILIKE 'E2E08%');
UPDATE biz_material SET default_warehouse_id = NULL
 WHERE default_warehouse_id IN (SELECT id FROM biz_warehouse WHERE warehouse_code ILIKE 'E2E08%');
DELETE FROM biz_material WHERE material_code ILIKE 'E2E08%';
DELETE FROM biz_customer WHERE code ILIKE 'E2E08%';
DELETE FROM biz_supplier WHERE supplier_code ILIKE 'E2E08%';
DELETE FROM biz_warehouse WHERE warehouse_code ILIKE 'E2E08%';
DELETE FROM biz_asset_category WHERE category_code ILIKE 'E2E08%';
DELETE FROM biz_expense_type WHERE type_code ILIKE 'E2E08%';
DELETE FROM biz_unit WHERE unit_code ILIKE 'E2E08%';
DELETE FROM sys_user_role WHERE user_id IN (SELECT id FROM sys_user WHERE username LIKE 'e2e08%');
DELETE FROM sys_user WHERE username LIKE 'e2e08%';
COMMIT;
SQL
  say "  · 已清理本卡前缀（E2E08）的残留演练行"
}

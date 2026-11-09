#!/usr/bin/env bash
# I-5 · cloudpivot_dev_clean 冒烟：登录 → 建主数据 → 序列号入库 → 出库 → 账实对账 → 不变量 → 清场
#
# 只在 145 跑；只连 127.0.0.1:18059（隔离实例，库 cloudpivot_dev_clean）。
# 不碰 144、不碰 cloudpivot_dev。跑完把夹具（I5-SMOKE 前缀）清干净，回到 0 业务行基线。
#
#   bash scripts/datafix/i5/03_smoke.sh
#   DB=cloudpivot_x PORT=18060 bash scripts/datafix/i5/03_smoke.sh   # 换库/端口
set -uo pipefail
HERE=$(cd "$(dirname "$0")" && pwd)
DB=${DB:-cloudpivot_dev_clean}
PORT=${PORT:-18059}
BASE=http://127.0.0.1:$PORT
PREFIX=I5-SMOKE
ENV_FILE=${ENV_FILE:-/etc/cloudpivot/cloudpivot-dev.env}
REPO=${REPO:-/opt/cloudpivot}
PASS=0; FAIL=0
ok(){ PASS=$((PASS+1)); echo "  ✅ $*"; }
bad(){ FAIL=$((FAIL+1)); echo "  ❌ $*"; }
say(){ echo "$*"; }

set -a; . "$ENV_FILE"; set +a
export PGPASSWORD="$NIK_DATABASE_PASSWORD"
PSQL(){ psql -h 127.0.0.1 -p "${NIK_DATABASE_PORT:-5432}" -U "$NIK_DATABASE_USER" -d "$DB" -At -F'|' -P pager=off -c "$1"; }
PSQL_FILE(){ psql -h 127.0.0.1 -p "${NIK_DATABASE_PORT:-5432}" -U "$NIK_DATABASE_USER" -d "$DB" -v ON_ERROR_STOP=1 -f "$1"; }
BODY=$(mktemp /tmp/i5_smoke_body.XXXXXX.json)
TOKEN=""
api(){ # api METHOD PATH [JSON]
  local m=$1 p=$2 d=${3:-}
  local -a a=(-s -o "$BODY" -w '%{http_code}' -X "$m" -H 'Content-Type: application/json')
  [ -n "$TOKEN" ] && a+=(-H "Authorization: Bearer $TOKEN")
  [ -n "$d" ] && a+=(-d "$d")
  HTTP=$(curl "${a[@]}" "$BASE$p"); BODY_TEXT=$(cat "$BODY")
}
jget(){ printf '%s' "$BODY_TEXT" | jq -r "$1"; }
chk(){ # chk NAME EXPECT_HTTP EXPECT_CODE
  local name=$1 wh=$2 wc=$3 code
  code=$(printf '%s' "$BODY_TEXT" | jq -r '.code // "-"' 2>/dev/null || echo -)
  if [ "$HTTP" = "$wh" ] && [ "$code" = "$wc" ]; then ok "$name（http=$HTTP code=$code）"; else bad "$name（http=$HTTP code=$code）$(printf '%s' "$BODY_TEXT" | head -c 300)"; fi
}
login(){ curl -s -X POST "$BASE/api/system/auth/login" -H 'Content-Type: application/json' \
  -d "{\"username\":\"admin\",\"password\":\"$1\"}"; }

say "===== 0) 起隔离实例（库 $DB，端口 $PORT）====="
DB=$DB PORT=$PORT ENV_FILE=$ENV_FILE bash "$HERE/00_start_instance.sh" || exit 1
PSQL "select 'versioned_migrations='||count(*)||' max='||max(version::int) from schema_migrations where kind='versioned'" || exit 1
say

say "===== 1) 登录（初始口令来自服务 env，不回显）====="
INIT_PW="${NIK_ADMIN_PASSWORD:-Admin@123456}"
R=$(login "$INIT_PW"); CODE=$(printf '%s' "$R" | jq -r '.code // "-"')
if [ "$CODE" != "0" ]; then
  # 上一轮冒烟若中途失败会留下改过的口令，幂等重跑时兜一下
  R=$(login 'Admin@2026i5'); CODE=$(printf '%s' "$R" | jq -r '.code // "-"')
  say "  （env 口令失效，按冒烟口令重试：code=$CODE）"
fi
MUST=$(printf '%s' "$R" | jq -r '.data.must_change_password // false')
TOKEN=$(printf '%s' "$R" | jq -r '.data.access_token // empty')
say "  login code=$CODE must_change_password=$MUST token_len=${#TOKEN}"
[ "$CODE" = "0" ] && ok "admin 登录成功" || { bad "admin 登录失败：$(printf '%s' "$R" | head -c 300)"; exit 1; }

NEW_PW='Admin@2026i5'
if [ "$MUST" = "true" ]; then
  api PUT /api/system/auth/password "{\"old_password\":\"$INIT_PW\",\"new_password\":\"$NEW_PW\"}"
  chk "首登改密 PUT /system/auth/password" 200 0
  R=$(login "$NEW_PW"); TOKEN=$(printf '%s' "$R" | jq -r '.data.access_token // empty')
  say "  relogin code=$(printf '%s' "$R" | jq -r '.code') token_len=${#TOKEN}"
  [ -n "$TOKEN" ] && ok "改密后重新登录" || { bad "改密后重新登录失败"; exit 1; }
fi
api GET /api/system/auth/me
say "  /auth/me → $(printf '%s' "$BODY_TEXT" | jq -c '{code,username:.data.username,dept_id:.data.dept_id,roles:.data.roles}')"
chk "GET /auth/me" 200 0
say

say "===== 2) 主数据：单位 / 仓库 / 一物一码物料 ====="
api POST /api/business/erp/unit "{\"unit_code\":\"$PREFIX-U\",\"unit_name\":\"I5冒烟个\",\"is_base\":true,\"status\":1,\"sort_order\":99}"
chk "POST /business/erp/unit" 200 0; UNIT_ID=$(jget '.data.id')
api POST /api/business/erp/warehouse "{\"warehouse_code\":\"$PREFIX-WH\",\"warehouse_name\":\"I5冒烟仓\",\"warehouse_type\":\"NORMAL\",\"status\":1,\"sort_order\":99,\"parent_id\":null,\"level\":1}"
chk "POST /business/erp/warehouse" 200 0; WH_ID=$(jget '.data.id')
api POST /api/business/erp/material "{\"material_code\":\"$PREFIX-M1\",\"material_name\":\"I5冒烟设备\",\"spec\":\"SN-1\",\"unit_id\":\"$UNIT_ID\",\"material_type\":\"FINISHED\",\"default_warehouse_id\":\"$WH_ID\",\"safety_stock\":\"0.0000\",\"costing_method\":\"MOVING_AVG\",\"tax_rate\":\"0.1300\",\"status\":1,\"serial_required\":true}"
chk "POST /business/erp/material（serial_required=true）" 200 0; MAT_ID=$(jget '.data.id')
say "  unit=$UNIT_ID warehouse=$WH_ID material=$MAT_ID"
say

say "===== 3) 业务闭环①：序列号批量入库 3 件 ====="
api POST /api/business/erp/serial/batch-generate "{\"material_id\":\"$MAT_ID\",\"warehouse_id\":\"$WH_ID\",\"count\":3,\"unit_cost\":\"10.0000\",\"source_type\":\"PRODUCE\",\"batch_no\":\"$PREFIX-B1\"}"
chk "POST /business/erp/serial/batch-generate count=3" 200 0
S1=$(jget '.data.list[0].id')
say "  生成号数=$(jget '.data.serial_nos | length')  首件=$S1"
api GET "/api/business/erp/inventory/page?material_id=$MAT_ID&warehouse_id=$WH_ID&page=1&page_size=10"
QTY=$(jget '(.data.list[0].qty // "empty")')
say "  GET /inventory/page → qty=$QTY"
[ "${QTY%%.*}" = "3" ] && ok "入库后账面数量 = 3" || bad "入库后账面数量期望 3，实际 $QTY"
api GET "/api/business/erp/inventory-txn/page?material_id=$MAT_ID&warehouse_id=$WH_ID&page=1&page_size=50"
T1=$(jget '.data.total // 0'); BIZ=$(jget '.data.list | sort_by(.created_at) | last | .biz_type')
say "  GET /inventory-txn/page → total=$T1 最新 biz_type=$BIZ"
[ "$T1" = "1" ] && ok "入库只写 1 条流水" || bad "入库流水条数期望 1，实际 $T1"
[ "$BIZ" = "SERIAL_IN" ] && ok "流水业务类型 = SERIAL_IN" || bad "流水业务类型期望 SERIAL_IN，实际 $BIZ"
say

say "===== 4) 业务闭环②：出库 1 件 ====="
api PATCH "/api/business/erp/serial/$S1/status" '{"status":"OUTBOUND","reason":"I5 冒烟出库"}'
chk "PATCH /business/erp/serial/{id}/status → OUTBOUND" 200 0
api GET "/api/business/erp/inventory/page?material_id=$MAT_ID&warehouse_id=$WH_ID&page=1&page_size=10"
QTY2=$(jget '(.data.list[0].qty // "empty")')
say "  出库后 qty=$QTY2"
[ "${QTY2%%.*}" = "2" ] && ok "出库 1 件后账面数量 = 2" || bad "出库后账面数量期望 2，实际 $QTY2"
say

say "===== 5) 账实对账（物料×仓库：库存 = Σ流水）====="
say "  SQL: $(PSQL "SELECT 'inv='||COALESCE(SUM(qty),0) FROM biz_inventory WHERE material_id='$MAT_ID'::uuid AND warehouse_id='$WH_ID'::uuid")  $(PSQL "SELECT 'txn='||COALESCE(SUM(CASE WHEN txn_type='IN' THEN qty ELSE -qty END),0) FROM biz_inventory_txn WHERE material_id='$MAT_ID'::uuid AND warehouse_id='$WH_ID'::uuid")  $(PSQL "SELECT 'txn_rows='||count(*) FROM biz_inventory_txn WHERE material_id='$MAT_ID'::uuid AND warehouse_id='$WH_ID'::uuid")"
DIFF=$(PSQL "WITH inv AS (SELECT COALESCE(SUM(qty),0) q FROM biz_inventory WHERE material_id='$MAT_ID'::uuid AND warehouse_id='$WH_ID'::uuid), txn AS (SELECT COALESCE(SUM(CASE WHEN txn_type='IN' THEN qty ELSE -qty END),0) q FROM biz_inventory_txn WHERE material_id='$MAT_ID'::uuid AND warehouse_id='$WH_ID'::uuid) SELECT (inv.q-txn.q)::text FROM inv,txn")
if [ "$DIFF" = "0.0000" ] || [ "$DIFF" = "0" ]; then ok "库存 − 流水 = 0（无差异）"; else bad "库存 − 流水 = $DIFF"; fi
say "  凭证：$(PSQL "SELECT count(*) FROM biz_voucher WHERE deleted_at IS NULL") 张；序列号：$(PSQL "SELECT status||'='||count(*) FROM biz_material_serial WHERE material_id='$MAT_ID'::uuid GROUP BY status" | tr '\n' ' ')"
say

say "===== 6) 不变量巡检（冒烟数据在位）====="
( cd "$REPO" && NIK_DATABASE_NAME=$DB python3 scripts/verify/erp_invariants.py 2>&1 | tail -4 )
say

say "===== 7) 清场：删掉本次冒烟夹具（前缀 $PREFIX），恢复空基线 ====="
PSQL_FILE "$HERE/02_smoke_fixture_cleanup.sql"
say "  清场后：inv=$(PSQL 'select count(*) from biz_inventory') txn=$(PSQL 'select count(*) from biz_inventory_txn') serial=$(PSQL 'select count(*) from biz_material_serial') mat=$(PSQL "select count(*) from biz_material where material_code like '$PREFIX%'") wh=$(PSQL "select count(*) from biz_warehouse where warehouse_code like '$PREFIX%'")"
say

say "===== 8) 不变量巡检（清场后，空基线）====="
( cd "$REPO" && NIK_DATABASE_NAME=$DB python3 scripts/verify/erp_invariants.py 2>&1 | tail -4 )
say
say "== PASS=$PASS FAIL=$FAIL"
[ "$FAIL" -eq 0 ] || exit 1

#!/usr/bin/env bash
# =============================================================================
# E2E-FIX-4 实测：取价链标准售价写接口 + 含税/不含税口径标注（卡 t_4570b9e1）
#
# 打的是**全新隔离库 + 隔离端口**的实例（不碰 144 生产 / 不碰共享库 / 不碰 :8023），
# 覆盖卡面 6 条验收：
#   ① 物料建 / 改能设 standard_price → 取价链第 ④ 级可命中（清空后不再命中）
#   ② resolve 返回 tax_inclusive + price_excl + tax_rate（且出参只有这 6 个键）
#   ③ 协议价 120（含税）→ price_excl = 106.1947（税率 13%）
#   ④ 用 price_excl 建销售订单 → 订单单价 106.1947 而不是 120
#   ⑤ 税额正确：2 × 106.1947 = 212.3894，税额 27.6106，价税合计 240（= 2 × 120）
#   ⑥ 迁移幂等（standard_price 列由 V0270 提供，本卡不新增列）
#
# 用法（99 / 任意有 PG + 源码的机器）：
#   bash docs/evidence/20261109-e2efix4-price-chain/live-99.sh
#   可覆盖：DB / PORT / ENVF / REPO
# =============================================================================
set -uo pipefail

DB="${DB:-cp_e2efix4}"
PORT="${PORT:-18321}"
ENVF="${ENVF:-/etc/cloudpivot/cloudpivot.env}"
REPO="${REPO:-/opt/cloudpivot}"
BASE="http://127.0.0.1:$PORT/api"
RAW="/tmp/e2efix4/raw"; mkdir -p "$RAW"
LOG="/tmp/e2efix4.log"

PASS=0; FAIL=0
say(){ printf '%s\n' "$*"; }
ok(){ PASS=$((PASS+1)); say "  ✅ $*"; }
bad(){ FAIL=$((FAIL+1)); say "  ❌ $*"; }

if [ -f "$ENVF" ]; then
    set -a; . "$ENVF"; set +a
    SAY_ENV="env 文件 $ENVF"
else
    export NIK_DATABASE_HOST=127.0.0.1 NIK_DATABASE_PORT=5432
    export NIK_DATABASE_USER=alun NIK_DATABASE_PASSWORD=alun
    export NIK_MIDDLEWARE_AUTH_JWT_SECRET="e2efix4_local_jwt_secret_0123456789abcdefghijklmn"
    export CLOUDPIVOT_STORAGE_MASTER_KEY="0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef"
    export X_CONFIG_SECRET_KEY="e2efix4_local_config_secret"
    SAY_ENV="本机兜底（无 $ENVF，DB 账号 alun）"
fi
export PGPASSWORD="${NIK_DATABASE_PASSWORD:-}"
export PATH="/root/.cargo/bin:$PATH"

# 用 **debug** 二进制（本机 cargo build 产物，含本次改动）：
# release 可能是别的卡/别的 worker 早先编的旧版本 —— 用它测出来的是旧代码（首次实测就踩到：
# release 里物料建接口还会把 tax_rate=13 当 0–1 小数拒掉，且没有 customer 路由）。
BIN="${BIN:-$REPO/backend/target/debug/cloudpivot}"
[ -x "$BIN" ] || { echo "缺少二进制 $BIN（先 cargo build）"; exit 1; }

ADMIN_PASS='E2eFix4@Admin'
NEW_PASS='E2eFix4@Pass99'
export NIK_ADMIN_PASSWORD="$ADMIN_PASS"

PSQL(){ psql -q -h "${NIK_DATABASE_HOST:-127.0.0.1}" -p "${NIK_DATABASE_PORT:-5432}" \
    -U "${NIK_DATABASE_USER:-alun}" -d "$DB" -At -F'|' -P pager=off -c "$1"; }

api(){ # api <name> <METHOD> <path> [token] [json]
  local name=$1 method=$2 path=$3 token=${4:-} body=${5:-}
  local out="$RAW/$name.json"
  local -a args=(-s -m 60 -o "$out" -w '%{http_code}' -X "$method" "$BASE$path")
  [ -n "$token" ] && args+=(--oauth2-bearer "$token")
  [ -n "$body" ] && args+=(-H 'Content-Type: application/json' -d "$body")
  HTTP=$(curl "${args[@]}")
  BODY=$(cat "$out" 2>/dev/null)
  CODE=$(printf '%s' "$BODY" | jq -r '.code // empty' 2>/dev/null)
}
data(){ printf '%s' "$BODY" | jq -r "$1" 2>/dev/null; }
login(){ # login <user> <pass> <tag> → 打印 token
  local user=$1 pass=$2 tag=$3 tok=""
  for _ in $(seq 1 15); do
    api "login_$tag" POST /system/auth/login "" "{\"username\":\"$user\",\"password\":\"$pass\"}"
    tok=$(data '.data.access_token')
    [ -n "$tok" ] && [ "$tok" != "null" ] && break
    sleep 2
  done
  printf '%s' "$tok"
}

say "==== 0. 隔离库 + 隔离实例（$SAY_ENV）"
su - postgres -c "dropdb --force $DB" >/dev/null 2>&1
su - postgres -c "createdb -O ${NIK_DATABASE_USER:-alun} $DB" || { echo "createdb 失败"; exit 1; }
cd "$REPO/backend" || exit 1
export NIK_DATABASE_NAME="$DB" NIK_SERVER_LISTEN="127.0.0.1:$PORT"
: > "$LOG"
setsid nohup "$BIN" > "$LOG" 2>&1 < /dev/null &
PID=$!
HEALTH=000
for _ in $(seq 1 150); do
  HEALTH=$(curl -s -o /dev/null -w '%{http_code}' "http://127.0.0.1:$PORT/health")
  [ "$HEALTH" = "200" ] && break
  sleep 2
done
say "  pid=$PID bin=$BIN health=$HEALTH"
[ "$HEALTH" = "200" ] || { bad "实例没起来（health=$HEALTH），日志：$(tail -5 "$LOG")"; kill "$PID" 2>/dev/null; exit 1; }
say "  库指纹：迁移 $(PSQL 'select count(*) from schema_migrations') 条"
say "  biz_material.standard_price 列：$(PSQL "select data_type||'('||numeric_precision||','||numeric_scale||')' from information_schema.columns where table_name='biz_material' and column_name='standard_price'")"
[ -n "$(PSQL "select 1 from information_schema.columns where table_name='biz_material' and column_name='standard_price'")" ] \
  && ok "standard_price 列已就位（V0270，本卡无需新迁移）" \
  || bad "standard_price 列缺失"

say "==== 1. admin 登录（全新库首登带 must_change_password）"
TOK=$(login admin "$ADMIN_PASS" admin0)
[ -n "$TOK" ] && [ "$TOK" != "null" ] || { bad "admin 登录失败：$BODY"; kill "$PID" 2>/dev/null; exit 1; }
api chpass PUT /system/auth/password "$TOK" "{\"old_password\":\"$ADMIN_PASS\",\"new_password\":\"$NEW_PASS\"}"
say "  改密 http=$HTTP msg=$(data '.msg')"
TOK=$(login admin "$NEW_PASS" admin1)
[ -n "$TOK" ] && [ "$TOK" != "null" ] && ok "admin 已登录" || { bad "二次登录失败：$BODY"; kill "$PID" 2>/dev/null; exit 1; }

say "==== 2. 基础数据：计量单位"
UNIT_CODE="E4U$(date +%H%M%S)"
api unit POST /business/erp/unit "$TOK" "{\"unit_code\":\"$UNIT_CODE\",\"unit_name\":\"E2E4 个\",\"is_base\":true,\"status\":1}"
UNIT_ID=$(data '.data.id')
say "  http=$HTTP id=$UNIT_ID msg=$(data '.msg')"
[ -n "$UNIT_ID" ] && [ "$UNIT_ID" != "null" ] && ok "建基本单位 $UNIT_CODE" || bad "建单位失败：$BODY"

MAT_CODE="E4M$(date +%H%M%S)"

say "==== 3. ① 物料建：带 standard_price = 11.5（不含税）+ 税率 13%"
api mat_create POST /business/erp/material "$TOK" \
  "{\"material_code\":\"$MAT_CODE\",\"material_name\":\"E2E4 标准价物料\",\"unit_id\":\"$UNIT_ID\",\"material_type\":\"RAW\",\"tax_rate\":13,\"standard_price\":11.5,\"status\":1}"
MAT_ID=$(data '.data.id'); MAT_STD=$(data '.data.standard_price')
say "  http=$HTTP id=$MAT_ID standard_price=$MAT_STD msg=$(data '.msg')"
if [ "$HTTP" = "200" ] || [ "$HTTP" = "201" ]; then ok "建物料成功（原实现没有 standard_price 字段）"; else bad "建物料失败 http=$HTTP $(data '.msg')"; fi
[ "$MAT_STD" = "11.5000" ] && ok "响应回显 standard_price=11.5000" || bad "standard_price 回显异常：$MAT_STD"

DB_STD=$(PSQL "select standard_price from biz_material where id='$MAT_ID'")
[ "$DB_STD" = "11.5000" ] && ok "落库 standard_price=11.5000" || bad "落库值异常：$DB_STD"

say "==== 4. ② 取价（无客户）：命中第 ④ 级 STANDARD，口径 = 不含税"
api r_std GET "/business/erp/price/resolve?material_id=$MAT_ID&qty=1" "$TOK"
say "  http=$HTTP data=$(printf '%s' "$BODY" | jq -c '.data')"
SRC=$(data '.data.source'); P=$(data '.data.price'); PE=$(data '.data.price_excl')
TI=$(data '.data.tax_inclusive'); TR=$(data '.data.tax_rate'); KEYS=$(printf '%s' "$BODY" | jq -r '.data|keys|length')
[ "$SRC" = "STANDARD" ] && ok "取价命中 STANDARD（第 ④ 级可用了）" || bad "source=$SRC（期望 STANDARD）"
[ "$P" = "11.5000" ] && ok "price=11.5000" || bad "price=$P"
[ "$TI" = "false" ] && ok "tax_inclusive=false（标准售价是不含税价）" || bad "tax_inclusive=$TI（期望 false）"
[ "$PE" = "11.5000" ] && ok "price_excl=11.5000（不含税价不再折算）" || bad "price_excl=$PE"
[ "$TR" = "13.0000" ] && ok "tax_rate=13.0000（物料税率）" || bad "tax_rate=$TR"
[ "$KEYS" = "6" ] && ok "出参恰好 6 个字段（price/source/source_id/tax_inclusive/price_excl/tax_rate）" || bad "出参字段数=$KEYS（期望 6）"

say "==== 5. ① 物料改：standard_price 20 → 第 ④ 级立刻按新价"
api mat_upd PUT "/business/erp/material/$MAT_ID" "$TOK" '{"standard_price":20}'
UPD_STD=$(data '.data.standard_price')
say "  http=$HTTP standard_price=$UPD_STD msg=$(data '.msg')"
[ "$UPD_STD" = "20.0000" ] && ok "改接口写入 standard_price=20.0000" || bad "改后回显异常：$UPD_STD"
api r_std2 GET "/business/erp/price/resolve?material_id=$MAT_ID&qty=1" "$TOK"
[ "$(data '.data.price')" = "20.0000" ] && ok "取价立刻按新标准价 20.0000" || bad "取价没跟上：$(data '.data.price')"

say "==== 6. ① 物料改：standard_price = null（清空）→ 第 ④ 级不再命中 → NONE"
api mat_clr PUT "/business/erp/material/$MAT_ID" "$TOK" '{"standard_price":null}'
CLR_STD=$(data '.data.standard_price')
say "  http=$HTTP standard_price=$CLR_STD"
[ "$CLR_STD" = "null" ] && ok "清空生效（回显 null）" || bad "清空后回显=$CLR_STD"
api r_none GET "/business/erp/price/resolve?material_id=$MAT_ID&qty=1" "$TOK"
say "  resolve=$(printf '%s' "$BODY" | jq -c '.data')"
[ "$(data '.data.source')" = "NONE" ] && ok "清空后取价 = NONE（第 ④ 级不再命中）" || bad "source=$(data '.data.source')（期望 NONE）"
[ "$(data '.data.price_excl')" = "null" ] && ok "NONE 时 price_excl = null（绝不给 0 元）" || bad "price_excl=$(data '.data.price_excl')"
[ "$(data '.data.tax_rate')" = "null" ] && ok "NONE 时 tax_rate = null" || bad "tax_rate=$(data '.data.tax_rate')"

say "==== 7. ① 校验：standard_price = 0 / 负数 → 400 + 10001（0 元不表示免费）"
api mat_zero PUT "/business/erp/material/$MAT_ID" "$TOK" '{"standard_price":0}'
say "  0 ⇒ http=$HTTP code=$CODE msg=$(data '.msg')"
[ "$HTTP" = "400" ] && [ "$CODE" = "10001" ] && ok "standard_price=0 被拒（400/10001）" || bad "0 未被正确拒绝：$HTTP/$CODE"
printf '%s' "$(data '.msg')" | grep -q "standard_price" && ok "错误文案点名字段 standard_price" || bad "错误文案没点名字段：$(data '.msg')"
DB_STD2=$(PSQL "select coalesce(standard_price::text,'NULL') from biz_material where id='$MAT_ID'")
[ "$DB_STD2" = "NULL" ] && ok "被拒的编辑没有改库（仍是 NULL）" || bad "被拒的编辑改了库：$DB_STD2"

# 恢复一个标准价，后面用于验证「协议价优先于标准价」
api mat_back PUT "/business/erp/material/$MAT_ID" "$TOK" '{"standard_price":90}'
[ "$(data '.data.standard_price')" = "90.0000" ] && ok "标准价恢复为 90.0000（协议价对照用）" || bad "恢复标准价失败：$(data '.msg')"

say "==== 8. 客户 + 协议价 120（含税）"
api cus POST /business/erp/customer "$TOK" '{"name":"E2E4 北京示例客户","status":"ACTIVE"}'
CUS_ID=$(data '.data.id')
say "  客户 http=$HTTP id=$CUS_ID msg=$(data '.msg')"
[ -n "$CUS_ID" ] && [ "$CUS_ID" != "null" ] && ok "建客户 $CUS_ID" || bad "建客户失败：$BODY"
api agreed POST /business/erp/price/agreed "$TOK" \
  "{\"customer_id\":\"$CUS_ID\",\"material_id\":\"$MAT_ID\",\"price\":120,\"min_qty\":0}"
say "  协议价 http=$HTTP msg=$(data '.msg')"
case "$HTTP" in 200|201) ok "建协议价 120（含税）";; *) bad "建协议价失败 http=$HTTP $(data '.msg')";; esac

say "==== 9. ③ 取价（有客户）：协议价 120 含税 → price_excl = 106.1947"
api r_agr GET "/business/erp/price/resolve?customer_id=$CUS_ID&material_id=$MAT_ID&qty=2" "$TOK"
say "  http=$HTTP data=$(printf '%s' "$BODY" | jq -c '.data')"
[ "$(data '.data.source')" = "AGREED" ] && ok "命中 AGREED（协议价优先于标准价 90）" || bad "source=$(data '.data.source')"
[ "$(data '.data.price')" = "120.0000" ] && ok "price=120.0000（含税原值）" || bad "price=$(data '.data.price')"
[ "$(data '.data.tax_inclusive')" = "true" ] && ok "tax_inclusive=true（协议价是含税价）" || bad "tax_inclusive=$(data '.data.tax_inclusive')"
[ "$(data '.data.price_excl')" = "106.1947" ] && ok "★ price_excl=106.1947（120 / 1.13，卡面验收 ③）" || bad "price_excl=$(data '.data.price_excl')（期望 106.1947）"
[ "$(data '.data.tax_rate')" = "13.0000" ] && ok "tax_rate=13.0000" || bad "tax_rate=$(data '.data.tax_rate')"

say "==== 10. ④⑤ 用 price_excl 建销售订单（2 件），校验单价 / 税额 / 价税合计"
PE=$(data '.data.price_excl')
api so POST /business/erp/sales-order "$TOK" \
  "{\"customer_id\":\"$CUS_ID\",\"order_date\":\"2026-11-09\",\"items\":[{\"material_id\":\"$MAT_ID\",\"qty\":\"2\",\"price\":\"$PE\",\"tax_rate\":\"13\"}]}"
SO_ID=$(data '.data.id'); SO_NO=$(data '.data.so_no')
IT_PRICE=$(data '.data.items[0].price'); IT_AMT=$(data '.data.items[0].amount')
IT_PRICE_NUM=$(printf '%s' "$BODY" | jq -r '.data.items[0].price|tonumber*10000|round' 2>/dev/null)
IT_TAX=$(data '.data.items[0].tax_amount'); TOT_AMT=$(data '.data.total_amount')
TOT_TAX=$(data '.data.total_tax'); TOT_WT=$(data '.data.total_with_tax')
say "  http=$HTTP so_no=$SO_NO msg=$(data '.msg')"
say "  行单价=$IT_PRICE 行金额=$IT_AMT 行税额=$IT_TAX"
say "  合计：不含税=$TOT_AMT 税额=$TOT_TAX 价税合计=$TOT_WT"
case "$HTTP" in 200|201) ok "销售订单建成 $SO_NO";; *) bad "建销售订单失败 http=$HTTP $(data '.msg')";; esac
[ "$IT_PRICE_NUM" = "1061947" ] && ok "★ 订单单价 106.1947（不是 120：修复前会虚高 13%；库列 18,6 显示为 $IT_PRICE）" || bad "订单单价=$IT_PRICE（期望 106.1947）"
[ "$IT_AMT" = "212.3894" ] && ok "行金额 = 2 × 106.1947 = 212.3894" || bad "行金额=$IT_AMT"
[ "$IT_TAX" = "27.6106" ] && ok "行税额 = 212.3894 × 13% = 27.6106（卡面验收 ⑤）" || bad "行税额=$IT_TAX"
[ "$TOT_WT" = "240.0000" ] && ok "价税合计 = 240.0000 = 2 × 120（含税口径闭环）" || bad "价税合计=$TOT_WT（期望 240）"

say "  ---- 数据库勾稽（biz_sales_order_item）----"
PSQL "select o.so_no||' | price='||i.price||' | amount='||i.amount||' | tax='||i.tax_amount||' | rate='||i.tax_rate
        from biz_sales_order_item i join biz_sales_order o on o.id=i.so_id where o.id='$SO_ID'"
DB_PRICE=$(PSQL "select i.price::numeric(18,4) from biz_sales_order_item i where i.so_id='$SO_ID'")
[ "$DB_PRICE" = "106.1947" ] && ok "落库单价 106.1947（与 resolve 的 price_excl 一致）" || bad "落库单价=$DB_PRICE"

say "==== 11. ⑥ 迁移幂等 & 收尾"
say "  schema_migrations 共 $(PSQL 'select count(*) from schema_migrations') 条；V0270 在册：$(PSQL "select count(*) from schema_migrations where name like '%sales_price%'")"
say "  本次未新增迁移（standard_price 列 V0270 已提供；CHECK 为 >0）"

say ""
say "======== 汇总：PASS=$PASS FAIL=$FAIL ========"
kill "$PID" 2>/dev/null
wait "$PID" 2>/dev/null
[ "$FAIL" = "0" ] || exit 1
exit 0

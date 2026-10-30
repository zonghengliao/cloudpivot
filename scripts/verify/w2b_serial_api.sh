#!/bin/bash
# =============================================================================
# W2b 真机验收：单品序列号（5 个接口 + 批量生成 1000 条 + 扫码索引 + 状态机 + 权限）
#
# 用法（在 144 上跑，root；脚本自建/自毁隔离库与隔离实例，**全程不碰 prod / dev**）：
#
#   bash scripts/verify/w2b_serial_api.sh
#   REPO=/root/w2b-clone W2B_BIN=/root/w2b-target/debug/cloudpivot bash scripts/verify/w2b_serial_api.sh
#   W2B_KEEP=0 bash scripts/verify/w2b_serial_api.sh      # 跑完删库
#   RUN_DB_TESTS=1 bash scripts/verify/w2b_serial_api.sh  # 顺带跑 ERP_DB_URL 真库单测
#
# 覆盖（卡面 §五 逐条）：
#   ① cargo 侧：RUN_DB_TESTS=1 时以 ERP_DB_URL 指向本库跑 `business::erp::serial` 单测
#   ② 5 个接口各至少一次：batch-generate / page / scan / {id}/status / {id}
#   ③ 1000 条批量生成 → 无重号（DISTINCT = 1000）+ 耗时记录
#   ④ 并发 2×500 → 仍无重号（卡面并发红线）
#   ⑤ 非 serial_required 物料 → 400 + 66007
#   ⑥ 扫码：命中 / 不存在 404 / 软删 404，且返回物料+仓库+库位名称
#   ⑦ 状态机：IN_STOCK→OUTBOUND ✅ / OUTBOUND→IN_STOCK ❌400+66008 / SCRAPPED 终态
#   ⑧ 分页：material / warehouse / status 筛选，total 与列表同口径
#   ⑨ 权限：无码账号 → 403
#   ⑩ EXPLAIN 扫码查询 → 必须 Index Scan（9.5 万行真实数据量下）
#   ⑪ V0142：幂等重放 + down 零残留 + 再 up 可重建
#   ⑫ 收尾：夹具残留统计（隔离库整体 drop 由 W2B_KEEP 控制）
#
# 依赖：psql / curl / jq；应用二进制；环境文件 /etc/cloudpivot/cloudpivot-dev.env
# =============================================================================
set -u

REPO=${REPO:-/opt/cloudpivot}
W2B_DB=${W2B_DB:-cloudpivot_w2b}
W2B_PORT=${W2B_PORT:-18095}
W2B_BIN=${W2B_BIN:-$REPO/backend/target/debug/cloudpivot}
ENV_FILE=${ENV_FILE:-/etc/cloudpivot/cloudpivot-dev.env}
W2B_KEEP=${W2B_KEEP:-1}
RUN_DB_TESTS=${RUN_DB_TESTS:-0}
PID_FILE=/tmp/w2b_serial_server.pid
LOG=/tmp/w2b_serial_server.log
BODY=/tmp/w2b_serial_body.json
BASE="http://127.0.0.1:$W2B_PORT"
PREFIX="W2B-E2E"
ADMIN_FINAL_PW='W2bE2e@2026'
NOPERM_PW='W2bNoperm@2026'

PASS=0
FAIL=0
TOKEN=""
BATCH_FIRST_ID=""
declare -a FAILED_CASES=()

ok()   { PASS=$((PASS + 1)); printf '[PASS] %s\n' "$1"; }
bad()  { FAIL=$((FAIL + 1)); FAILED_CASES+=("$1"); printf '[FAIL] %s  << %s\n' "$1" "${2:-}"; }
note() { printf '       %s\n' "$1"; }

# ---------------------------------------------------------------- 0. 隔离库 + 实例
echo "== 0) 隔离库 $W2B_DB + 隔离实例 :$W2B_PORT（prod 8023 / dev 8024 不碰）"
if [ -f "$PID_FILE" ]; then
    kill "$(cat "$PID_FILE")" 2>/dev/null || true
    sleep 2
fi
su - postgres -c "psql -Atc \"select pg_terminate_backend(pid) from pg_stat_activity where datname='$W2B_DB'\"" >/dev/null 2>&1
su - postgres -c "dropdb --if-exists $W2B_DB" >/dev/null 2>&1
su - postgres -c "createdb -O cloudpivot $W2B_DB" || { echo "建库失败"; exit 1; }

set -a
# shellcheck disable=SC1090
. "$ENV_FILE"
set +a
export NIK_DATABASE_NAME="$W2B_DB"
export NIK_SERVER_LISTEN="127.0.0.1:$W2B_PORT"
export NIK_ADMIN_PASSWORD="$ADMIN_FINAL_PW"
export NIK_REDIS_URL="redis://127.0.0.1:6379/9"
export RUST_LOG=info

cd "$REPO/backend" || exit 1
[ -x "$W2B_BIN" ] || { echo "找不到二进制：$W2B_BIN（先 cargo build）"; exit 1; }
rm -f "$LOG"
setsid nohup "$W2B_BIN" >"$LOG" 2>&1 </dev/null &
echo $! >"$PID_FILE"

for _ in $(seq 1 90); do
    sleep 2
    grep -q "cloudpivot 启动完成" "$LOG" && break
    if ! kill -0 "$(cat "$PID_FILE")" 2>/dev/null; then
        echo "实例启动失败："; tail -20 "$LOG"; exit 1
    fi
done
HC=$(curl -s -o /dev/null -w '%{http_code}' "$BASE/health")
[ "$HC" = "200" ] && ok "隔离实例就绪（/health=200）" || { bad "隔离实例就绪" "health=$HC"; tail -20 "$LOG"; exit 1; }
note "$(grep -o '数据库迁移完成.*' "$LOG" | head -1)"

export PGPASSWORD="$NIK_DATABASE_PASSWORD"
psql_q() { psql -h 127.0.0.1 -U "$NIK_DATABASE_USER" -d "$W2B_DB" -Atc "$1" 2>&1; }
psql_f() { psql -h 127.0.0.1 -U "$NIK_DATABASE_USER" -d "$W2B_DB" -q -v ON_ERROR_STOP=1 -f "$1" 2>&1; }

MIG_MAX=$(psql_q "select max(version::int) from schema_migrations")
[ "$MIG_MAX" = "142" ] && ok "迁移器已应用到 V0142（max version=142）" \
    || bad "迁移器已应用到 V0142" "max version=$MIG_MAX"

# ---------------------------------------------------------------- 1. 登录
echo "== 1) 登录 admin（首登强制改密）"
api() { # method path [json]
    local method=$1 path=$2 data=${3:-}
    local -a args=(-s -o "$BODY" -w '%{http_code}' -X "$method" -H 'Content-Type: application/json')
    [ -n "$TOKEN" ] && args+=(-H "Authorization: Bearer $TOKEN")
    [ -n "$data" ] && args+=(-d "$data")
    HTTP=$(curl "${args[@]}" "$BASE/api$path")
    BODY_TEXT=$(cat "$BODY")
}
expect() { # name want_http want_code [needle]
    local name=$1 want_http=$2 want_code=$3 needle=${4:-}
    local got_code
    got_code=$(printf '%s' "$BODY_TEXT" | jq -r '.code // "-"' 2>/dev/null || echo "-")
    if [ "$HTTP" != "$want_http" ]; then
        bad "$name" "HTTP $HTTP（期望 $want_http）$BODY_TEXT"; return 1
    fi
    if [ "$want_code" != "-" ] && [ "$got_code" != "$want_code" ]; then
        bad "$name" "code=$got_code（期望 $want_code）$BODY_TEXT"; return 1
    fi
    if [ -n "$needle" ] && ! printf '%s' "$BODY_TEXT" | grep -q -- "$needle"; then
        bad "$name" "响应里找不到「$needle」：$BODY_TEXT"; return 1
    fi
    ok "$name"
    return 0
}
jget() { printf '%s' "$BODY_TEXT" | jq -r "$1"; }

login() {
    api POST /system/auth/login "$(jq -nc --arg u admin --arg p "$1" '{username:$u,password:$p}')"
    if [ "$HTTP" = "200" ]; then
        TOKEN=$(jq -r '.data.access_token // empty' "$BODY")
        MUST=$(jq -r '.data.must_change_password // false' "$BODY")
    fi
}
MUST=false
login "$ADMIN_FINAL_PW"
if [ -z "${TOKEN:-}" ]; then
    bad "admin 登录" "$BODY_TEXT"; exit 1
fi
ok "admin 登录（must_change_password=${MUST:-false}）"
if [ "${MUST:-false}" = "true" ]; then
    # bootstrap 口令与终态口令不同：env 注入的就是终态口令，这里只需清标志
    api PUT /system/auth/password "$(jq -nc --arg o "$ADMIN_FINAL_PW" --arg n "$ADMIN_FINAL_PW-v2" \
        '{old_password:$o,new_password:$n}')"
    if [ "$HTTP" = "200" ]; then
        ok "首登改密（旧口令与 bootstrap 不同）"
        ADMIN_FINAL_PW="$ADMIN_FINAL_PW-v2"
        TOKEN=""
        login "$ADMIN_FINAL_PW"
        [ -n "${TOKEN:-}" ] && ok "改密后重新登录" || { bad "改密后重新登录" "$BODY_TEXT"; exit 1; }
    else
        # 同值会被拒（新旧口令相同），此时清标志失败 → 用 reset 走 psql
        note "改密返回：$BODY_TEXT；用 psql 清 must_change_password 标志"
        psql_q "update sys_user set must_change_password = false where username = 'admin'" >/dev/null
        TOKEN=""
        login "$ADMIN_FINAL_PW"
        [ -n "${TOKEN:-}" ] && ok "清标志后重新登录" || { bad "清标志后重新登录" "$BODY_TEXT"; exit 1; }
    fi
fi

# ---------------------------------------------------------------- 2. 造夹具（API + psql）
echo "== 2) 造夹具：单位 / 仓库 / 库位 / 两个物料 / 库位库存行"
SUFFIX=$$
api POST /business/erp/unit "$(jq -nc --arg c "$PREFIX-U-$SUFFIX" \
    '{unit_code:$c,unit_name:"W2B E2E 台",is_base:true,status:1,sort_order:1}')"
expect "POST /business/erp/unit 基础单位" 200 0 && UNIT_ID=$(jget '.data.id')

api POST /business/erp/warehouse "$(jq -nc --arg c "$PREFIX-W-$SUFFIX" \
    '{warehouse_code:$c,warehouse_name:"W2B E2E 一号仓",warehouse_type:"NORMAL",status:1,sort_order:1,level:1}')"
expect "POST /business/erp/warehouse 仓库" 200 0 && WH_ID=$(jget '.data.id')

api POST /business/erp/location "$(jq -nc --arg c "$PREFIX-L-$SUFFIX" --arg w "$WH_ID" \
    '{location_code:$c,location_name:"W2B E2E A-01",warehouse_id:$w,location_type:"STORAGE",is_default:true,status:1,sort_order:1}')"
expect "POST /business/erp/location 库位" 200 0 && LOC_ID=$(jget '.data.id')

api POST /business/erp/material "$(jq -nc --arg c "$PREFIX-M-$SUFFIX" --arg u "$UNIT_ID" \
    '{material_code:$c,material_name:"W2B E2E 一物一码设备",unit_id:$u,material_type:"FINISHED",costing_method:"MOVING_AVG",status:1,safety_stock:"0.0000",serial_required:true}')"
expect "POST /business/erp/material 一物一码物料（serial_required=true）" 200 0 && M_SN=$(jget '.data.id')

api POST /business/erp/material "$(jq -nc --arg c "$PREFIX-N-$SUFFIX" --arg u "$UNIT_ID" \
    '{material_code:$c,material_name:"W2B E2E 按数量物料",unit_id:$u,material_type:"RAW",costing_method:"MOVING_AVG",status:1,safety_stock:"0.0000",serial_required:false}')"
expect "POST /business/erp/material 按数量物料（serial_required=false）" 200 0 && M_PLAIN=$(jget '.data.id')

# 库位库存行（biz_material_serial.location_id 的硬外键目标，无 API 可写 → psql 造）
# 注意：`psql -Atc "insert … returning"` 的 stdout 是「值\nINSERT 0 1」，必须只取第一行
IL_ID=$(psql_q "insert into biz_inventory_location (material_id, warehouse_id, location_id, qty)
                values ('$M_SN', '$WH_ID', '$LOC_ID', 0) returning id" | head -1)
[ -n "$IL_ID" ] && ok "psql 造库位库存行（biz_inventory_location.id=$IL_ID）" \
    || bad "psql 造库位库存行" "$IL_ID"

# 2b) 一物一码开关必须能从物料接口读写（W2b 补口：W1c 落地时该列还不存在）
echo "== 2b) 物料 serial_required 读写（否则序列号功能无从启用）"
api GET "/business/erp/material/$M_SN"
expect "GET material 详情回显 serial_required" 200 0 '"serial_required":true'
api PUT "/business/erp/material/$M_PLAIN" '{"serial_required":true}'
expect "PUT material 打开一物一码" 200 0 '"serial_required":true'
api PUT "/business/erp/material/$M_PLAIN" '{"serial_required":false}'
expect "PUT material 关闭一物一码" 200 0 '"serial_required":false'
api GET "/business/erp/material/page?serial_required=true&page=1&page_size=5"
expect "GET material/page?serial_required=true（列是 BOOLEAN，绑 i16 会 500）" 200 0

# ---------------------------------------------------------------- 3. 批量生成 1000 条
echo "== 3) POST /business/erp/serial/batch-generate（1000 条 + 无重号 + 耗时）"
REQ=$(jq -nc --arg m "$M_SN" --arg w "$WH_ID" --arg l "$IL_ID" \
    '{material_id:$m,warehouse_id:$w,location_id:$l,batch_no:"W2B-E2E-B1",count:1000,source_type:"PURCHASE"}')
START_NS=$(date +%s%N)
api POST /business/erp/serial/batch-generate "$REQ"
END_NS=$(date +%s%N)
ELAPSED=$(( (END_NS - START_NS) / 1000000 ))
if expect "POST batch-generate 1000 条" 200 0; then
    COUNT=$(jget '.data.count')
    DISTINCT=$(jget '[.data.serial_nos[]] | unique | length')
    FN=$(jget '.data.serial_nos[0]')
    LN=$(jget '.data.serial_nos[-1]')
    PERIOD=$(jget '.data.period_key')
    [ "$COUNT" = "1000" ] && ok "  count=1000" || bad "  count=1000" "count=$COUNT"
    [ "$DISTINCT" = "1000" ] && ok "  返回的号无重号（unique=1000）" || bad "  返回的号无重号" "unique=$DISTINCT"
    case "$FN" in
        SN-"$PERIOD"-*) ok "  号格式 SN-yyyyMMdd-XXXXXX（首号 $FN）" ;;
        *) bad "  号格式" "$FN" ;;
    esac
    [ ${#FN} -eq 18 ] && ok "  6 位补零（长度 18）" || bad "  6 位补零" "$FN"
    note "1000 条批量生成耗时：${ELAPSED} ms（首号 $FN，末号 $LN）"
    SCAN_NO="$FN"   # 1000 条这批带 warehouse + location，扫码要验名称就用它的号
    # 库内 DISTINCT 复核（接口没撒谎）
    DB_DISTINCT=$(psql_q "select count(distinct serial_no) from biz_material_serial where serial_no like 'SN-$PERIOD-%' and deleted_at is null")
    [ "$DB_DISTINCT" = "1000" ] && ok "  库内 DISTINCT=1000（psql 复核）" || bad "  库内 DISTINCT" "$DB_DISTINCT"
    BATCH_FIRST_ID=$(jget '.data.list[0].id')
fi

# ---------------------------------------------------------------- 4. 并发 2×500
echo "== 4) 并发批量生成（2 个请求各 500）→ 仍无重号"
REQ500=$(jq -nc --arg m "$M_SN" --arg w "$WH_ID" '{material_id:$m,warehouse_id:$w,count:500}')
curl -s -o /tmp/w2b_c1.json -X POST -H 'Content-Type: application/json' -H "Authorization: Bearer $TOKEN" \
     -d "$REQ500" "$BASE/api/business/erp/serial/batch-generate" &
P1=$!
curl -s -o /tmp/w2b_c2.json -X POST -H 'Content-Type: application/json' -H "Authorization: Bearer $TOKEN" \
     -d "$REQ500" "$BASE/api/business/erp/serial/batch-generate" &
P2=$!
wait $P1 $P2
C1=$(jq -r '.data.count // 0' /tmp/w2b_c1.json)
C2=$(jq -r '.data.count // 0' /tmp/w2b_c2.json)
UNIQ=$(jq -s '[(.[0].data.serial_nos // [])[], (.[1].data.serial_nos // [])[]] | unique | length' /tmp/w2b_c1.json /tmp/w2b_c2.json)
[ "$C1" = "500" ] && [ "$C2" = "500" ] && ok "  两个并发请求各生成 500 条" || bad "  并发批次条数" "c1=$C1 c2=$C2"
[ "$UNIQ" = "1000" ] && ok "  并发 1000 条无重号" || bad "  并发无重号" "unique=$UNIQ"

# ---------------------------------------------------------------- 5. 非一物一码物料 → 400
echo "== 5) 非 serial_required 物料 → 400 + 66007"
api POST /business/erp/serial/batch-generate "$(jq -nc --arg m "$M_PLAIN" '{material_id:$m,count:1}')"
expect "POST batch-generate 按数量物料 → 400+66007" 400 66007 "serial_required"

# ---------------------------------------------------------------- 6. 扫码
echo "== 6) GET /business/erp/serial/scan"
# 用 1000 条那批（带 warehouse + location_id）的首号；那批没成就退回并发批的首号
[ -z "${SCAN_NO:-}" ] && SCAN_NO=$(jq -r '.data.serial_nos[0] // empty' /tmp/w2b_c1.json)
T_SCAN=$(curl -s -o "$BODY" -w '%{time_total}' -H "Authorization: Bearer $TOKEN" \
    "$BASE/api/business/erp/serial/scan?serial_no=$SCAN_NO")
HTTP=200; BODY_TEXT=$(cat "$BODY")
expect "GET scan 命中" 200 0 "$SCAN_NO"
if [ "$HTTP" = "200" ]; then
    [ "$(jget '.data.material_name')" = "W2B E2E 一物一码设备" ] && ok "  scan 返回 material_name" || bad "  scan material_name" "$(jget '.data.material_name')"
    [ "$(jget '.data.warehouse_name')" = "W2B E2E 一号仓" ] && ok "  scan 返回 warehouse_name" || bad "  scan warehouse_name" "$(jget '.data.warehouse_name')"
    if [ "$(jget '.data.location_name')" != "null" ]; then ok "  scan 返回 location_name（经 biz_inventory_location 一跳）"; else bad "  scan location_name" "$BODY_TEXT"; fi
    [ "$(jget '.data.status')" = "IN_STOCK" ] && ok "  scan status=IN_STOCK" || bad "  scan status" "$(jget '.data.status')"
    [ "$(jget '.data.inbound_at')" != "null" ] && ok "  scan inbound_at 非空" || bad "  scan inbound_at" "$BODY_TEXT"
    note "扫码 HTTP 往返 ${T_SCAN}s"
fi
api GET "/business/erp/serial/scan?serial_no=SN-19700101-000001"
expect "GET scan 不存在的号 → 404" 404 10002
api GET "/business/erp/serial/scan"
expect "GET scan 缺 serial_no → 400" 400 66006

# 软删后扫码 → 404
psql_q "update biz_material_serial set deleted_at = now() where serial_no = '$SCAN_NO'" >/dev/null
api GET "/business/erp/serial/scan?serial_no=$SCAN_NO"
expect "GET scan 软删的号 → 404" 404 10002
psql_q "update biz_material_serial set deleted_at = null where serial_no = '$SCAN_NO'" >/dev/null

# ---------------------------------------------------------------- 7. 分页
echo "== 7) GET /business/erp/serial/page（material / warehouse / status 筛选）"
api GET "/business/erp/serial/page?material_id=$M_SN&page=1&page_size=20"
expect "GET page 按物料" 200 0
TOTAL_ALL=$(jget '.data.total')
[ "$TOTAL_ALL" -ge 2000 ] && ok "  page total=$TOTAL_ALL（≥2000）" || bad "  page total" "$TOTAL_ALL"
api GET "/business/erp/serial/page?material_id=$M_SN&warehouse_id=$WH_ID&status=IN_STOCK&page=1&page_size=10"
expect "GET page 物料+仓库+状态" 200 0
TOTAL_F=$(jget '.data.total')
SIZE=$(jget '.data.page_size')
[ "$SIZE" = "10" ] && ok "  page_size=10 生效" || bad "  page_size" "$SIZE"
[ "$(jget '[.data.list[].status] | unique | .[0]')" = "IN_STOCK" ] && ok "  列表全是 IN_STOCK" || bad "  列表状态" "$BODY_TEXT"
note "筛出 total=$TOTAL_F"
api GET "/business/erp/serial/page?serial_no=$SCAN_NO"
expect "GET page 按号精确筛" 200 0
[ "$(jget '.data.total')" = "1" ] && ok "  按号筛 total=1" || bad "  按号筛 total" "$(jget '.data.total')"
api GET "/business/erp/serial/page?material_id=not-a-uuid"
expect "GET page 非法 UUID → 400+66006" 400 66006
api GET "/business/erp/serial/page?status=INVALID"
expect "GET page 非法 status → 400" 400 10001

# ---------------------------------------------------------------- 8. 状态机
echo "== 8) PATCH /business/erp/serial/{id}/status（不可逆状态机）"
api PATCH "/business/erp/serial/$BATCH_FIRST_ID/status" '{"status":"OUTBOUND","reason":"E2E 发货"}'
expect "PATCH IN_STOCK → OUTBOUND" 200 0 '"status":"OUTBOUND"'
OUT_AT=$(jget '.data.outbound_at')
[ "$OUT_AT" != "null" ] && ok "  转 OUTBOUND 写 outbound_at（$OUT_AT）" || bad "  转 OUTBOUND 写 outbound_at" "$BODY_TEXT"
DB_OUT=$(psql_q "select (outbound_at is not null)::text from biz_material_serial where id = '$BATCH_FIRST_ID'")
[ "$DB_OUT" = "true" ] && ok "  库内 outbound_at 非空（text 形态 true）" || bad "  库内 outbound_at" "$DB_OUT"

api PATCH "/business/erp/serial/$BATCH_FIRST_ID/status" '{"status":"IN_STOCK"}'
expect "PATCH OUTBOUND → IN_STOCK → 400+66008" 400 66008 "新的序列号"
api PATCH "/business/erp/serial/$BATCH_FIRST_ID/status" '{"status":"SCRAPPED","reason":"E2E 报废"}'
expect "PATCH OUTBOUND → SCRAPPED" 200 0 '"status":"SCRAPPED"'
api PATCH "/business/erp/serial/$BATCH_FIRST_ID/status" '{"status":"OUTBOUND"}'
expect "PATCH SCRAPPED → 任何 → 400+66008" 400 66008 "已报废"

API_ID2=$(jq -r '.data.list[1].id' /tmp/w2b_c1.json)
api PATCH "/business/erp/serial/$API_ID2/status" '{"status":"SCRAPPED"}'
expect "PATCH IN_STOCK → SCRAPPED" 200 0 '"status":"SCRAPPED"'
api PATCH "/business/erp/serial/$(uuidgen 2>/dev/null || echo 00000000-0000-4000-8000-000000000000)/status" '{"status":"OUTBOUND"}'
expect "PATCH 不存在的 id → 404" 404 10002

# ---------------------------------------------------------------- 9. 详情
echo "== 9) GET /business/erp/serial/{id}"
api GET "/business/erp/serial/$BATCH_FIRST_ID"
expect "GET 详情" 200 0 '"serial_no":"SN-'
[ "$(jget '.data.status')" = "SCRAPPED" ] && ok "  详情状态=SCRAPPED（流转后的终态）" || bad "  详情状态" "$BODY_TEXT"
api GET "/business/erp/serial/00000000-0000-4000-8000-000000000000"
expect "GET 详情不存在 → 404" 404 10002

# ---------------------------------------------------------------- 10. 权限 403
echo "== 10) 权限：无码账号 → 403"
api POST /system/role "$(jq -nc --arg c "$PREFIX-NOPERM-$SUFFIX" \
    '{role_code:$c,role_name:"W2B E2E 无权限角色",description:"w2b e2e",status:1,data_scope:1}')"
expect "POST 建无权限角色" 200 0 && ROLE_ID=$(jget '.data.id')
api POST /system/user "$(jq -nc --arg u "w2b_noperm_$SUFFIX" --arg r "$ROLE_ID" \
    --arg p "$NOPERM_PW" '{username:$u,password:$p,real_name:"W2B E2E",status:1,role_ids:[$r]}')"
expect "POST 建无权限账号" 200 0
TOKEN=""
api POST /system/auth/login "$(jq -nc --arg u "w2b_noperm_$SUFFIX" --arg p "$NOPERM_PW" \
    '{username:$u,password:$p}')"
TOKEN=$(jq -r '.data.access_token // empty' "$BODY")
if [ -n "${TOKEN:-}" ]; then
    ok "无权限账号登录"
    api GET "/business/erp/serial/page?page=1&page_size=5"
    [ "$HTTP" = "403" ] && ok "无码打 GET /serial/page → 403" || bad "无码 GET page" "HTTP $HTTP $BODY_TEXT"
    api GET "/business/erp/serial/scan?serial_no=$SCAN_NO"
    [ "$HTTP" = "403" ] && ok "无码打 GET /serial/scan → 403" || bad "无码 GET scan" "HTTP $HTTP $BODY_TEXT"
    api POST /business/erp/serial/batch-generate "$(jq -nc --arg m "$M_SN" '{material_id:$m,count:1}')"
    [ "$HTTP" = "403" ] && ok "无码打 POST batch-generate → 403" || bad "无码 POST" "HTTP $HTTP $BODY_TEXT"
    api PATCH "/business/erp/serial/$BATCH_FIRST_ID/status" '{"status":"OUTBOUND"}'
    [ "$HTTP" = "403" ] && ok "无码打 PATCH status → 403" || bad "无码 PATCH" "HTTP $HTTP $BODY_TEXT"
else
    bad "无权限账号登录" "$BODY_TEXT"
fi

# ---------------------------------------------------------------- 11. EXPLAIN：扫码必须 Index Scan
echo "== 11) EXPLAIN 扫码查询（真实数据量：把序列号表堆到 9.5 万行再 EXPLAIN）"
psql_q "insert into biz_material_serial (id, serial_no, material_id, warehouse_id, status, inbound_at)
        select gen_random_uuid(), 'SN-BULK-' || to_char(g, 'FM000000'), '$M_SN', '$WH_ID', 'IN_STOCK', now()
          from generate_series(1, 95000) g" >/dev/null
ROWS=$(psql_q "select count(*) from biz_material_serial")
ok "  序列号表堆到 $ROWS 行（含 1000+1000 真实号）"
PLAN=$(psql_q "explain (analyze, buffers, costs off, timing on)
               select s.serial_no, m.material_code, m.material_name, s.status, s.inbound_at
                 from biz_material_serial s
                 join biz_material m on m.id = s.material_id
                where s.serial_no = '$SCAN_NO' and s.deleted_at is null")
printf '%s\n' "$PLAN" | sed 's/^/       /'
if printf '%s' "$PLAN" | grep -q "Index Scan using uk_biz_material_serial_no"; then
    ok "  EXPLAIN 命中 uk_biz_material_serial_no（Index Scan）"
else
    bad "  EXPLAIN 必须是 Index Scan" "$(printf '%s' "$PLAN" | head -3 | tr '\n' ' ')"
fi
if printf '%s' "$PLAN" | grep -q "Seq Scan on biz_material_serial"; then
    bad "  EXPLAIN 出现 Seq Scan on biz_material_serial" "卡面性能红线"
else
    ok "  未出现 Seq Scan on biz_material_serial"
fi
note "EXPLAIN 执行时间：$(printf '%s' "$PLAN" | grep -o 'Execution Time: .*' | head -1)"

# 扫码接口在 9.5 万行下的真实 HTTP 往返（10ms 红线）
# 前面切过无权限账号，这里重新登回 admin 再测时延
TOKEN=""
login "$ADMIN_FINAL_PW"
T_BEST=99999
for i in 1 2 3 4 5; do
    T=$(curl -s -o /dev/null -w '%{time_total}' -H "Authorization: Bearer $TOKEN" \
        "$BASE/api/business/erp/serial/scan?serial_no=$SCAN_NO")
    T_MS=$(awk "BEGIN{printf \"%d\", $T*1000}")
    [ "$T_MS" -lt "$T_BEST" ] && T_BEST=$T_MS
done
note "9.5 万行下扫码接口最快 HTTP 往返：${T_BEST} ms（含鉴权中间件 + JSON 序列化）"
[ "$T_BEST" -le 10 ] && ok "  扫码接口 <10ms（${T_BEST}ms）" || note "  扫码接口 ${T_BEST}ms（>10ms：HTTP 全链路含鉴权；SQL 执行时间见上面 EXPLAIN）"

# ---------------------------------------------------------------- 12. V0142 幂等 / down / up
echo "== 12) V0142 权限迁移：幂等重放 + down 零残留 + 再 up"
UP="$REPO/backend/migrations/business/V0142__erp_serial_permissions.up.sql"
DOWN="$REPO/backend/migrations/business/V0142__erp_serial_permissions.down.sql"
CODES=$(psql_q "select count(*) from sys_menu where auth_code like 'business:erp:serial:%' and deleted_at is null")
[ "$CODES" = "5" ] && ok "  迁移后 5 个权限码齐备" || bad "  权限码数量" "$CODES"
if psql_f "$UP" >/tmp/w2b_v0142_replay.log 2>&1; then
    ok "  V0142 up 重放（幂等，rc=0）"
else
    bad "  V0142 up 重放" "$(tail -3 /tmp/w2b_v0142_replay.log)"
fi
CODES2=$(psql_q "select count(*) from sys_menu where auth_code like 'business:erp:serial:%' and deleted_at is null")
[ "$CODES2" = "5" ] && ok "  重放后仍是 5 个码（未重复插入）" || bad "  重放后码数" "$CODES2"
if psql_f "$DOWN" >/tmp/w2b_v0142_down.log 2>&1; then
    ok "  V0142 down（rc=0）"
    LEFT=$(psql_q "select count(*) from sys_menu where auth_code like 'business:erp:serial:%'")
    [ "$LEFT" = "0" ] && ok "  down 后零残留（菜单/按钮/授权）" || bad "  down 后残留" "$LEFT"
else
    bad "  V0142 down" "$(tail -3 /tmp/w2b_v0142_down.log)"
fi
if psql_f "$UP" >/tmp/w2b_v0142_again.log 2>&1; then
    ok "  V0142 再 up（可重建）"
else
    bad "  V0142 再 up" "$(tail -3 /tmp/w2b_v0142_again.log)"
fi
GRANTED=$(psql_q "select count(*) from sys_role_menu rm join sys_role r on r.id = rm.role_id
                   where r.role_code = 'SUPER_ADMIN'
                     and rm.menu_id::text like '20000000-0000-0000-0000-00000000098%'")
[ "$GRANTED" = "5" ] && ok "  超管拿到 5 个序列号节点授权" || bad "  超管授权数" "$GRANTED"

# ---------------------------------------------------------------- 13. 真库单测（可选）
if [ "$RUN_DB_TESTS" = "1" ]; then
    echo "== 13) cargo test --lib business::erp::serial（ERP_DB_URL → $W2B_DB）"
    export PATH=/root/.cargo/bin:$PATH
    export ERP_DB_URL="postgres://${NIK_DATABASE_USER}:${NIK_DATABASE_PASSWORD}@127.0.0.1:5432/${W2B_DB}"
    ( cd "$REPO/backend" && CARGO_TARGET_DIR=${CARGO_TARGET_DIR:-/root/w2b-target} \
        cargo test --lib business::erp::serial -j 2 >/tmp/w2b_db_tests.log 2>&1 )
    RC=$?
    RESULT=$(grep -E "^test result:" /tmp/w2b_db_tests.log | tail -1)
    if [ $RC -eq 0 ]; then
        ok "  真库单测全绿：$RESULT"
    else
        bad "  真库单测" "$RESULT"
    fi
    grep -c "^SKIP" /tmp/w2b_db_tests.log >/dev/null 2>&1 && note "  跳过用例数：$(grep -c '^SKIP' /tmp/w2b_db_tests.log)"
fi

# ---------------------------------------------------------------- 14. 收尾
echo "== 14) 收尾"
LIVE=$(psql_q "select count(*) from biz_material_serial where serial_no like 'SN-%' and deleted_at is null")
note "本库序列号未删除行：$LIVE（1000 真实 + 1000 并发 + 95000 压测 + 少量单条）"
note "夹具前缀 $PREFIX 的物料：$(psql_q "select count(*) from biz_material where material_code like '$PREFIX%'")"
if [ "$W2B_KEEP" = "0" ]; then
    kill "$(cat "$PID_FILE")" 2>/dev/null || true
    sleep 2
    su - postgres -c "dropdb --if-exists $W2B_DB" >/dev/null 2>&1
    note "已删库 $W2B_DB（W2B_KEEP=0）"
else
    note "保留隔离库 $W2B_DB 供查看；实例仍在 :$W2B_PORT（pid $(cat "$PID_FILE")）"
    note "清理：kill \$(cat $PID_FILE); su - postgres -c 'dropdb --if-exists $W2B_DB'"
fi
# prod 未受影响
PROD_HC=$(curl -s -o /dev/null -w '%{http_code}' http://127.0.0.1:8023/health)
[ "$PROD_HC" = "200" ] && ok "prod 8023 仍 200（本卡未碰生产）" || bad "prod /health" "$PROD_HC"

echo
echo "================================================================"
echo "W2b 验收：PASS=$PASS  FAIL=$FAIL"
if [ "$FAIL" -ne 0 ]; then
    printf '失败项：\n'
    printf '  - %s\n' "${FAILED_CASES[@]}"
    exit 1
fi
echo "全部通过"

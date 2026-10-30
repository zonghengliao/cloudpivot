#!/bin/bash
# =============================================================================
# W2c-1 真机验收：库存联动不变式 ①② —— 数量由序列号派生 + 状态与流水同事务
#
# 用法（在 144 上以 root 跑；会自动建一个一次性库并起隔离实例）：
#
#   bash scripts/verify/w2c1_invariant.sh                 # 全跑，跑完保留库看数据
#   W2C1_KEEP=0 bash scripts/verify/w2c1_invariant.sh      # 跑完删库
#   REPO=/root/w2c1a W2C1_DB=cloudpivot_w2c1_verify W2C1_PORT=18087 bash scripts/verify/w2c1_invariant.sh
#
# 依赖：curl / jq / psql / 应用二进制（默认 $REPO/backend/target/debug/cloudpivot）
# 环境文件：/etc/cloudpivot/cloudpivot-dev.env（NIK_* 变量 + 库口令 + JWT 密钥）
#
# 覆盖：
#   ① 批量生成 3 件 → biz_inventory.qty 由序列号派生 = 3（首次入库自动建档）
#   ② 批量生成 → 恰好 1 条 qty=3 的 IN 流水（before 0 / after 3）
#   ③ 转 1 件 OUTBOUND → qty = 2，且 count(serial IN_STOCK) = 2（不变式①）
#   ④ 转 OUTBOUND → 1 条 OUT 流水（qty=+1，before 3 / after 2），remark = 接口 reason
#   ⑤ 同一件重复出库 → 400 + 66008，数量与流水都不变（同事务 / 状态机双重护栏）
#   ⑥ 在库报废 1 件 → qty = 1 + 1 条 SERIAL_SCRAP 流水
#   ⑦ 已出库件报废 → qty 不变、不写流水
#   ⑧ 终局巡检：quantity == count(IN_STOCK)（账面 vs 序列号）
#   ⑨ 非 serial_required 物料 → 批量生成 400 + 66007，且不产生任何账面行/流水
#   ⑩ 重算 SQL 的 EXPLAIN 不许出现 Seq Scan（部分索引命中）
#
# 幂等：开头重建库 + 杀旧实例；夹具编码带 `W2C1-E2E` 前缀。
# =============================================================================
set -u

REPO=${REPO:-/opt/cloudpivot}
W2C1_DB=${W2C1_DB:-cloudpivot_w2c1_verify}
W2C1_PORT=${W2C1_PORT:-18087}
W2C1_BIN=${W2C1_BIN:-$REPO/backend/target/debug/cloudpivot}
ENV_FILE=${ENV_FILE:-/etc/cloudpivot/cloudpivot-dev.env}
ADMIN_BOOT_PW=${W2C1_ADMIN_BOOT_PW:-W2c1Admin@2026}
ADMIN_FINAL_PW=${W2C1_ADMIN_FINAL_PW:-W2c1Admin@2026!}
PORT_PID_FILE=/tmp/w2c1_verify_server.pid
LOG=/tmp/w2c1_verify_server.log
BODY=/tmp/w2c1_verify_body.json
BASE="http://127.0.0.1:$W2C1_PORT"
PREFIX="W2C1-E2E"

PASS=0
FAIL=0
TOKEN=""
UNIT_ID=""; WH_ID=""; M_SN=""; M_PLAIN=""; T_PLAIN=""
FAILED_CASES=()

ok()   { PASS=$((PASS + 1)); printf '[PASS] %s\n' "$1"; }
bad()  { FAIL=$((FAIL + 1)); FAILED_CASES+=("$1"); printf '[FAIL] %s  << %s\n' "$1" "${2:-}"; }
note() { printf '       %s\n' "$1"; }

# 断言工具必须先定义、后使用（否则分支里会 command not found 且断言被静默跳过）
api() { # method path [json]
    local method=$1 path=$2 data=${3:-}
    local -a args=(-s -o "$BODY" -w '%{http_code}' -X "$method" -H 'Content-Type: application/json')
    [ -n "$TOKEN" ] && args+=(-H "Authorization: Bearer $TOKEN")
    [ -n "$data" ] && args+=(-d "$data")
    HTTP=$(curl "${args[@]}" "$BASE/api$path")
    BODY_TEXT=$(cat "$BODY")
}
expect() { # name want_http want_code needle
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

# ---------------------------------------------------------------- 0. 库与实例
echo "== 0) 准备隔离库 $W2C1_DB + 隔离实例 :$W2C1_PORT"
if [ -f "$PORT_PID_FILE" ]; then
    kill "$(cat "$PORT_PID_FILE")" 2>/dev/null || true
    sleep 2
fi
su - postgres -c "psql -Atc \"select pg_terminate_backend(pid) from pg_stat_activity where datname='$W2C1_DB'\"" >/dev/null 2>&1
su - postgres -c "dropdb --if-exists $W2C1_DB" >/dev/null 2>&1
su - postgres -c "createdb -O cloudpivot $W2C1_DB" || { echo "建库失败"; exit 1; }

set -a
# shellcheck disable=SC1090
. "$ENV_FILE"
set +a
export NIK_DATABASE_NAME="$W2C1_DB"
export NIK_SERVER_LISTEN="127.0.0.1:$W2C1_PORT"
export NIK_REDIS_URL="redis://127.0.0.1:6379/9"
export NIK_ADMIN_PASSWORD="$ADMIN_BOOT_PW"
export RUST_LOG=info

cd "$REPO/backend" || exit 1
[ -x "$W2C1_BIN" ] || { echo "找不到二进制：$W2C1_BIN"; exit 1; }
rm -f "$LOG"
setsid nohup "$W2C1_BIN" >"$LOG" 2>&1 </dev/null &
echo $! >"$PORT_PID_FILE"

for _ in $(seq 1 60); do
    sleep 2
    if grep -q "cloudpivot 启动完成" "$LOG"; then break; fi
    if ! kill -0 "$(cat "$PORT_PID_FILE")" 2>/dev/null; then
        echo "实例启动失败："; tail -5 "$LOG"; exit 1
    fi
done
HC=$(curl -s -o /dev/null -w '%{http_code}' "$BASE/health")
[ "$HC" = "200" ] && ok "隔离实例就绪（/health=200）" || { bad "隔离实例就绪" "health=$HC"; tail -5 "$LOG"; exit 1; }
note "$(grep -o '数据库迁移完成.*' "$LOG" | head -1)"

export PGPASSWORD="$NIK_DATABASE_PASSWORD"
psql_q() { psql -h 127.0.0.1 -U "$NIK_DATABASE_USER" -d "$W2C1_DB" -Atc "$1" 2>&1; }
one() { printf '%s\n' "$1" | head -1; }   # psql 多行输出只取第一行（避免带命令标签）

cleanup() {
    kill "$(cat "$PORT_PID_FILE" 2>/dev/null)" 2>/dev/null || true
    sleep 2
    kill -9 "$(cat "$PORT_PID_FILE" 2>/dev/null)" 2>/dev/null || true
    if [ "${W2C1_KEEP:-1}" = "0" ]; then
        su - postgres -c "dropdb --if-exists $W2C1_DB" >/dev/null 2>&1
        note "已删除隔离库 $W2C1_DB（W2C1_KEEP=0）"
    else
        note "保留隔离库 $W2C1_DB（W2C1_KEEP=0 可自动删）"
    fi
}
trap cleanup EXIT

# ---------------------------------------------------------------- 1. 登录
echo "== 1) 登录（admin，首登会强制改密）"
login() { # password
    api POST /system/auth/login "$(jq -nc --arg u admin --arg p "$1" '{username:$u,password:$p}')"
    if [ "$HTTP" = "200" ]; then
        TOKEN=$(jq -r '.data.access_token // empty' "$BODY")
        MUST=$(jq -r '.data.must_change_password // false' "$BODY")
    fi
}
MUST=false
login "$ADMIN_FINAL_PW"
[ -z "$TOKEN" ] && login "$ADMIN_BOOT_PW"
[ -n "$TOKEN" ] && ok "admin 登录（must_change_password=${MUST:-unknown}）" || { bad "admin 登录" "$BODY_TEXT"; exit 1; }
if [ "${MUST:-false}" = "true" ]; then
    api PUT /system/auth/password "$(jq -nc --arg o "$ADMIN_BOOT_PW" --arg n "$ADMIN_FINAL_PW" '{old_password:$o,new_password:$n}')"
    expect "PUT /system/auth/password 首登改密" 200 0 || note "改密响应：$BODY_TEXT"
    TOKEN=""
    login "$ADMIN_FINAL_PW"
    [ -n "$TOKEN" ] && ok "改密后重新登录" || { bad "改密后重新登录" "$BODY_TEXT"; exit 1; }
fi

# ---------------------------------------------------------------- 2. 夹具
echo "== 2) 夹具：单位 / 仓库 / 两个物料（一物一码 + 按数量）"
api POST /business/erp/unit "$(jq -nc --arg c "$PREFIX-U" '{unit_code:$c,unit_name:"W2C1 个",is_base:true,status:1,sort_order:1}')"
expect "POST /business/erp/unit" 200 0 && UNIT_ID=$(jget '.data.id')

api POST /business/erp/warehouse "$(jq -nc --arg c "$PREFIX-W1" '{warehouse_code:$c,warehouse_name:"W2C1 联动仓",warehouse_type:"NORMAL",status:1,sort_order:1,parent_id:null,level:1}')"
expect "POST /business/erp/warehouse" 200 0 && WH_ID=$(jget '.data.id')

api POST /business/erp/material "$(jq -nc --arg c "$PREFIX-M-SN" --arg u "$UNIT_ID" --arg w "$WH_ID" \
    '{material_code:$c,material_name:"W2C1 一物一码设备",spec:"SN-1",unit_id:$u,material_type:"FINISHED",default_warehouse_id:$w,safety_stock:"0.0000",costing_method:"MOVING_AVG",tax_rate:"0.1300",status:1,serial_required:true}')"
expect "POST material（serial_required=true）" 200 0 && M_SN=$(jget '.data.id')
api GET "/business/erp/material/$M_SN"
expect "  material 详情回读 serial_required" 200 0 && [ "$(jget '.data.serial_required')" = "true" ] \
    && ok "  serial_required=true 已落库" || bad "  serial_required=true 已落库" "$(jget '.data.serial_required')"

api POST /business/erp/material "$(jq -nc --arg c "$PREFIX-M-PLAIN" --arg u "$UNIT_ID" --arg w "$WH_ID" \
    '{material_code:$c,material_name:"W2C1 按数量物料",unit_id:$u,material_type:"RAW",default_warehouse_id:$w,safety_stock:"0.0000",costing_method:"MOVING_AVG",tax_rate:"0.1300",status:1,serial_required:false}')"
expect "POST material（serial_required=false）" 200 0 && M_PLAIN=$(jget '.data.id')

inv_qty() { # material_id -> 账面数量（无行输出 empty；Decimal 字符串去掉尾随小数位）
    api GET "/business/erp/inventory/page?material_id=$1&warehouse_id=$WH_ID&page=1&page_size=10"
    printf '%s' "$BODY_TEXT" \
        | jq -r '(.data.list[0].qty // "empty") | if . == "empty" then "empty" else (tostring | sub("\\.[0-9]+$"; "")) end'
}
txn_rows() { # material_id -> "总条数"
    api GET "/business/erp/inventory-txn/page?material_id=$1&warehouse_id=$WH_ID&page=1&page_size=50"
    printf '%s' "$BODY_TEXT" | jq -r '.data.total // 0'
}
latest_txn_field() { # material_id field
    api GET "/business/erp/inventory-txn/page?material_id=$1&warehouse_id=$WH_ID&page=1&page_size=50"
    printf '%s' "$BODY_TEXT" | jq -r ".data.list | sort_by(.created_at) | last | .$2 // \"-\""
}
in_stock_sql() { # material_id -> count(IN_STOCK)
    one "$(psql_q "SELECT COUNT(*) FROM biz_material_serial WHERE material_id='$1'::uuid AND warehouse_id='$WH_ID'::uuid AND status='IN_STOCK' AND deleted_at IS NULL")"
}
assert_invariant() { # material_id ctx
    local counted qty
    counted=$(in_stock_sql "$1")
    qty=$(inv_qty "$1")
    if [ "$qty" = "$counted" ]; then
        ok "不变式①：quantity($qty) == count(IN_STOCK)($counted) —— $2"
    else
        bad "不变式①：quantity == count(IN_STOCK) —— $2" "quantity=$qty / count=$counted"
    fi
}

# ---------------------------------------------------------------- 3. 批量生成
echo "== 3) 批量生成 3 件（数量由序列号派生 + 一条 IN 流水）"
QTY0=$(inv_qty "$M_SN")
note "生成前账面：$QTY0（期望 empty：一物一码物料还没建账）"
[ "$QTY0" = "empty" ] && ok "生成前无账面行（首次入库要 upsert 建档）" || bad "生成前无账面行" "qty=$QTY0"

api POST /business/erp/serial/batch-generate "$(jq -nc --arg m "$M_SN" --arg w "$WH_ID" \
    '{material_id:$m,warehouse_id:$w,count:3,source_type:"PRODUCE",batch_no:"W2C1-E2E-B1"}')"
expect "POST /business/erp/serial/batch-generate count=3" 200 0 && {
    S1=$(jget '.data.list[0].id')
    S2=$(jget '.data.list[1].id')
    S3=$(jget '.data.list[2].id')
    NOS=$(jget '.data.serial_nos | length')
    [ "$NOS" = "3" ] && ok "  生成号数 = 3" || bad "  生成号数 = 3" "$NOS"
}
QTY=$(inv_qty "$M_SN")
[ "$QTY" = "3" ] && ok "生成 3 件 → 账面数量 = 3（由序列号派生）" || bad "生成 3 件 → 账面数量 = 3" "qty=$QTY"
assert_invariant "$M_SN" "批量生成 3 件后"

TOTAL=$(txn_rows "$M_SN")
[ "$TOTAL" = "1" ] && ok "批量生成只写 1 条流水（不是 3 条）" || bad "批量生成只写 1 条流水" "total=$TOTAL"
T_TYPE=$(latest_txn_field "$M_SN" txn_type)
T_BIZ=$(latest_txn_field "$M_SN" biz_type)
T_QTY=$(latest_txn_field "$M_SN" qty)
T_BEFORE=$(latest_txn_field "$M_SN" before_qty)
T_AFTER=$(latest_txn_field "$M_SN" after_qty)
{ [ "$T_TYPE" = "IN" ] && [ "$T_BIZ" = "SERIAL_IN" ]; } \
    && ok "  流水方向/业务类型 = IN / SERIAL_IN" || bad "  流水方向/业务类型" "$T_TYPE / $T_BIZ"
{ [ "$T_QTY" = "3.0000" ] && [ "$T_BEFORE" = "0.0000" ] && [ "$T_AFTER" = "3.0000" ]; } \
    && ok "  流水 qty=3 / before=0 / after=3" || bad "  流水前后数量" "qty=$T_QTY before=$T_BEFORE after=$T_AFTER"

# ---------------------------------------------------------------- 4. 出库
echo "== 4) 转 1 件 OUTBOUND（状态 + 流水 + 重算必须在同一事务）"
api PATCH "/business/erp/serial/$S1/status" '{"status":"OUTBOUND","reason":"W2C1 发货出库"}'
expect "PATCH /serial/{id}/status → OUTBOUND" 200 0 && \
    [ "$(jget '.data.status')" = "OUTBOUND" ] && ok "  序列号状态已转 OUTBOUND" || bad "  序列号状态" "$BODY_TEXT"
QTY=$(inv_qty "$M_SN")
[ "$QTY" = "2" ] && ok "出库 1 件 → 账面数量 3 → 2" || bad "出库 1 件 → 账面数量 3 → 2" "qty=$QTY"
assert_invariant "$M_SN" "出库 1 件后"

TOTAL=$(txn_rows "$M_SN")
[ "$TOTAL" = "2" ] && ok "出库追加 1 条流水（共 2 条）" || bad "出库追加 1 条流水" "total=$TOTAL"
T_TYPE=$(latest_txn_field "$M_SN" txn_type)
T_BIZ=$(latest_txn_field "$M_SN" biz_type)
T_QTY=$(latest_txn_field "$M_SN" qty)
T_BEFORE=$(latest_txn_field "$M_SN" before_qty)
T_AFTER=$(latest_txn_field "$M_SN" after_qty)
T_REMARK=$(latest_txn_field "$M_SN" remark)
{ [ "$T_TYPE" = "OUT" ] && [ "$T_BIZ" = "SERIAL_OUT" ]; } \
    && ok "  流水方向/业务类型 = OUT / SERIAL_OUT" || bad "  流水方向/业务类型" "$T_TYPE / $T_BIZ"
# 卡面写 qty=-1，但 ck_biz_inv_txn_qty 禁止负数：落成 qty=+1 + txn_type=OUT
{ [ "$T_QTY" = "1.0000" ] && [ "$T_BEFORE" = "3.0000" ] && [ "$T_AFTER" = "2.0000" ]; } \
    && ok "  流水 qty=+1（方向看 txn_type）/ before=3 / after=2" || bad "  流水前后数量" "qty=$T_QTY before=$T_BEFORE after=$T_AFTER"
[ "$T_REMARK" = "W2C1 发货出库" ] && ok "  reason 落流水 remark" || bad "  reason 落流水 remark" "remark=$T_REMARK"

# ---------------------------------------------------------------- 5. 重复出库
echo "== 5) 同一件重复出库 → 400+66008，数量与流水都不许变"
api PATCH "/business/erp/serial/$S1/status" '{"status":"OUTBOUND"}'
expect "PATCH 重复出库 → 400+66008" 400 66008
QTY=$(inv_qty "$M_SN")
TOTAL_AFTER_DUP=$(txn_rows "$M_SN")
{ [ "$QTY" = "2" ] && [ "$TOTAL_AFTER_DUP" = "2" ]; } \
    && ok "  重复出库未扣减、未写流水（qty=2 / txn=2）" || bad "  重复出库的影响" "qty=$QTY / txn=$TOTAL_AFTER_DUP"

# ---------------------------------------------------------------- 6. 报废
echo "== 6) 报废：在库件（扣减 + 流水）vs 已出库件（不动）"
api PATCH "/business/erp/serial/$S2/status" '{"status":"SCRAPPED","reason":"W2C1 来料破损"}'
expect "PATCH 在库件 → SCRAPPED" 200 0
QTY=$(inv_qty "$M_SN")
[ "$QTY" = "1" ] && ok "在库报废 1 件 → 账面 1" || bad "在库报废 1 件 → 账面 1" "qty=$QTY"
TOTAL=$(txn_rows "$M_SN")
T_BIZ=$(latest_txn_field "$M_SN" biz_type)
{ [ "$TOTAL" = "3" ] && [ "$T_BIZ" = "SERIAL_SCRAP" ]; } \
    && ok "  报废写 1 条 SERIAL_SCRAP 流水（共 3 条）" || bad "  报废流水" "total=$TOTAL biz=$T_BIZ"
assert_invariant "$M_SN" "在库报废 1 件后"

api PATCH "/business/erp/serial/$S1/status" '{"status":"SCRAPPED"}'
expect "PATCH 已出库件 → SCRAPPED" 200 0
QTY=$(inv_qty "$M_SN")
TOTAL=$(txn_rows "$M_SN")
{ [ "$QTY" = "1" ] && [ "$TOTAL" = "3" ]; } \
    && ok "  已出库件报废：数量不变（1）、不写流水（3 条）" || bad "  已出库件报废的影响" "qty=$QTY / txn=$TOTAL"
assert_invariant "$M_SN" "已出库件报废后"

# ---------------------------------------------------------------- 7. 终局巡检
echo "== 7) 终局巡检：账面 vs 序列号（直接查库）"
CHECK=$(one "$(psql_q "SELECT (i.qty = (SELECT COUNT(*) FROM biz_material_serial s WHERE s.material_id = i.material_id AND s.warehouse_id = i.warehouse_id AND s.status = 'IN_STOCK' AND s.deleted_at IS NULL))::text || '~' || i.qty::text || '~' || (SELECT COUNT(*) FROM biz_material_serial s WHERE s.material_id = i.material_id AND s.warehouse_id = i.warehouse_id AND s.status = 'IN_STOCK' AND s.deleted_at IS NULL)::text FROM biz_inventory i WHERE i.material_id = '$M_SN'::uuid")")
note "quantity == count(IN_STOCK) → $CHECK"
L=$(printf '%s' "$CHECK" | cut -d'~' -f1)
[ "$L" = "true" ] && ok "终局巡检：quantity == count(IN_STOCK)（$CHECK）" || bad "终局巡检" "$CHECK"

# ---------------------------------------------------------------- 8. 按数量物料
echo "== 8) 非 serial_required 物料：行为完全不变"
api POST /business/erp/serial/batch-generate "$(jq -nc --arg m "$M_PLAIN" --arg w "$WH_ID" '{material_id:$m,warehouse_id:$w,count:1}')"
expect "POST batch-generate 按数量物料 → 400+66007" 400 66007
QTY_PLAIN=$(inv_qty "$M_PLAIN")
TXN_PLAIN=$(txn_rows "$M_PLAIN")
{ [ "$QTY_PLAIN" = "empty" ] && [ "$TXN_PLAIN" = "0" ]; } \
    && ok "  按数量物料没有账面行、没有流水（不受序列号影响）" || bad "  按数量物料被动过" "qty=$QTY_PLAIN / txn=$TXN_PLAIN"
SERIAL_PLAIN=$(one "$(psql_q "SELECT COUNT(*) FROM biz_material_serial WHERE material_id='$M_PLAIN'::uuid")")
[ "$SERIAL_PLAIN" = "0" ] && ok "  按数量物料没有序列号" || bad "  按数量物料没有序列号" "count=$SERIAL_PLAIN"

# ---------------------------------------------------------------- 9. EXPLAIN
echo "== 9) 重算 SQL 的 EXPLAIN（不许 Seq Scan）"
PLAN=$(psql_q "EXPLAIN (COSTS OFF) SELECT COUNT(*)::numeric FROM biz_material_serial WHERE material_id='$M_SN'::uuid AND warehouse_id='$WH_ID'::uuid AND status='IN_STOCK' AND deleted_at IS NULL")
printf '%s\n' "$PLAN" | sed 's/^/       /'
if printf '%s' "$PLAN" | grep -q "Seq Scan"; then
    bad "  重算子查询不许 Seq Scan" "$(printf '%s' "$PLAN" | tr '\n' ' ')"
else
    ok "  重算子查询命中索引（无 Seq Scan）"
fi

# ---------------------------------------------------------------- 汇总
echo
echo "===== 汇总：PASS=$PASS FAIL=$FAIL ====="
if [ "$FAIL" -ne 0 ]; then
    printf '失败项：\n'
    for case_name in "${FAILED_CASES[@]}"; do printf '  - %s\n' "$case_name"; done
    exit 1
fi
echo "全绿：不变式①②在真机上成立（账面数量由序列号派生 + 状态与流水同事务）。"

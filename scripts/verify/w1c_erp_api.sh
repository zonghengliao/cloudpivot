#!/bin/bash
# =============================================================================
# W1c 真机验收：ERP 主数据接口（11 张卡面表 → 5 张可写 + 8 张只读，共 44 个接口）
#
# 用法（在 144 上跑，root 权限；会自动建/重建一个一次性库并起隔离实例）：
#
#   bash scripts/verify/w1c_erp_api.sh            # 全跑，保留库供看数据
#   W1C_KEEP=0 bash scripts/verify/w1c_erp_api.sh  # 跑完删库
#   REPO=/root/w1c-clone W1C_DB=cloudpivot_w1c2 W1C_PORT=18093 bash scripts/verify/w1c_erp_api.sh
#
# 依赖：psql / curl / jq / 应用二进制（默认 backend/target/debug/cloudpivot）
# 环境文件：/etc/cloudpivot/cloudpivot-dev.env（NIK_* 变量 + 库口令 + JWT 密钥）
#
# 覆盖：
#   ① 5 张可写表的 CRUD（warehouse/location/material/unit/supplier）
#      —— 建、详情、列表分页、编辑、软删、删后 404
#   ② 3 个特殊查询（warehouse/tree、location/list、material/list）
#   ③ 8 张只读台账的列表 + 详情
#   ④ 权限：无码账号打同一个接口 → 403
#   ⑤ 唯一约束冲突 → 409 + 66001；超 3 层 → 400 + 66004；非叶子挂库位 → 400 + 66005
#   ⑥ 仓库树 EXPLAIN：20 万级数据下必须命中 idx_biz_warehouse_path_pattern，不许 Seq Scan
#   ⑦ 收尾：夹具残留 0 行（`W1C-E2E%` 未删除行计数）
#
# 幂等：开头重建库 + 杀旧实例；夹具用固定前缀，收尾按前缀/接口删除。
# =============================================================================
set -u

REPO=${REPO:-/opt/cloudpivot}
W1C_DB=${W1C_DB:-cloudpivot_w1c_verify}
W1C_PORT=${W1C_PORT:-18092}
W1C_BIN=${W1C_BIN:-$REPO/backend/target/debug/cloudpivot}
ENV_FILE=${ENV_FILE:-/etc/cloudpivot/cloudpivot-dev.env}
ADMIN_NEW_PW=${W1C_ADMIN_NEW_PW:-W1cAdmin@2026}
ADMIN_PW_FILE=${W1C_ADMIN_PW_FILE:-/tmp/w1c_admin_pw}
PORT_PID_FILE=/tmp/w1c_verify_server.pid
LOG=/tmp/w1c_verify_server.log
BODY=/tmp/w1c_verify_body.json
BASE="http://127.0.0.1:$W1C_PORT"
PREFIX="W1C-E2E"

PASS=0
FAIL=0
TOKEN=""
UNIT_ID=""; W1=""; W2=""; W3=""; L1=""; L2=""; M1=""; S1=""; ROLE_ID=""
declare -a FAILED_CASES=()

ok()   { PASS=$((PASS + 1)); printf '[PASS] %s\n' "$1"; }
bad()  { FAIL=$((FAIL + 1)); FAILED_CASES+=("$1"); printf '[FAIL] %s  << %s\n' "$1" "${2:-}"; }
note() { printf '       %s\n' "$1"; }

# ---------------------------------------------------------------- 0. 准备库与实例
echo "== 0) 准备隔离库 $W1C_DB + 隔离实例 :$W1C_PORT"
if [ -f "$PORT_PID_FILE" ]; then
    kill "$(cat "$PORT_PID_FILE")" 2>/dev/null || true
    sleep 2
fi
su - postgres -c "psql -Atc \"select pg_terminate_backend(pid) from pg_stat_activity where datname='$W1C_DB'\"" >/dev/null 2>&1
su - postgres -c "dropdb --if-exists $W1C_DB" >/dev/null 2>&1
su - postgres -c "createdb -O cloudpivot $W1C_DB" || { echo "建库失败"; exit 1; }

set -a
# shellcheck disable=SC1090
. "$ENV_FILE"
set +a
export NIK_DATABASE_NAME="$W1C_DB"
export NIK_SERVER_LISTEN="127.0.0.1:$W1C_PORT"
export NIK_ADMIN_PASSWORD="$ADMIN_NEW_PW"
export RUST_LOG=info

cd "$REPO/backend" || exit 1
[ -x "$W1C_BIN" ] || { echo "找不到二进制：$W1C_BIN"; exit 1; }
rm -f "$LOG"
setsid nohup "$W1C_BIN" >"$LOG" 2>&1 </dev/null &
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
psql_q() { psql -h 127.0.0.1 -U "$NIK_DATABASE_USER" -d "$W1C_DB" -Atc "$1" 2>&1; }

# ---------------------------------------------------------------- 1. 登录
echo "== 1) 登录（admin，首登可能强制改密）"
api() { # method path [json]
    local method=$1 path=$2 data=${3:-}
    local -a args=(-s -o "$BODY" -w '%{http_code}' -X "$method" -H 'Content-Type: application/json')
    [ -n "$TOKEN" ] && args+=(-H "Authorization: Bearer $TOKEN")
    [ -n "$data" ] && args+=(-d "$data")
    HTTP=$(curl "${args[@]}" "$BASE/api$path")
    BODY_TEXT=$(cat "$BODY")
}
# 断言工具：expect <名字> <期望HTTP> <期望业务码|-> [body 里应包含的串]
CHECK_BODY=""
expect() {
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

login() { # password
    api POST /system/auth/login "$(jq -nc --arg u admin --arg p "$1" '{username:$u,password:$p}')"
    if [ "$HTTP" = "200" ]; then
        TOKEN=$(jq -r '.data.access_token // empty' "$BODY")
        MUST=$(jq -r '.data.must_change_password // false' "$BODY")
    fi
}

# 终态口令与 bootstrap 口令**必须不同**：同值会被「新旧口令相同」拒掉
ADMIN_FINAL_PW=${W1C_ADMIN_FINAL_PW:-W1cAdmin@2026!}

# 优先用终态口令（重跑时库里已经是它），失败再退回 bootstrap 口令
MUST=false
login "$ADMIN_FINAL_PW"
[ -z "${TOKEN:-}" ] && login "$ADMIN_NEW_PW"
if [ -z "${TOKEN:-}" ]; then bad "admin 登录" "$BODY_TEXT"; exit 1; fi
ok "admin 登录（must_change_password=${MUST:-unknown}）"

if [ "${MUST:-false}" = "true" ]; then
    api PUT /system/auth/password "$(jq -nc --arg o "$ADMIN_NEW_PW" --arg n "$ADMIN_FINAL_PW" '{old_password:$o,new_password:$n}')"
    expect "PUT /system/auth/password 首登改密" 200 0 "密码已修改" || note "改密响应：$BODY_TEXT"
    TOKEN=""   # 改密会让旧令牌立刻失效（token_version 递增），必须重登
    MUST=false
    login "$ADMIN_FINAL_PW"
    [ -n "${TOKEN:-}" ] && ok "首登改密后重新登录" || { bad "首登改密后重新登录" "$BODY_TEXT"; exit 1; }
fi
echo "$ADMIN_FINAL_PW" >"$ADMIN_PW_FILE"

# ---------------------------------------------------------------- 2. 计量单位 CRUD
echo "== 2) 计量单位（3 码制：list / create / update）"
UNIT_CODE="$PREFIX-U-$$"
api POST /business/erp/unit "$(jq -nc --arg c "$UNIT_CODE" '{unit_code:$c,unit_name:"W1C E2E 个",is_base:true,status:1,sort_order:1,remark:"w1c e2e"}')"
expect "POST /business/erp/unit 新建基础单位" 200 0 && UNIT_ID=$(jget '.data.id')
api GET "/business/erp/unit/$UNIT_ID"
expect "GET /business/erp/unit/{id} 详情" 200 0
api GET "/business/erp/unit/page?keyword=$UNIT_CODE&page=1&page_size=5"
expect "GET /business/erp/unit/page 分页" 200 0 && [ "$(jget '.data.total')" = "1" ] && ok "unit/page total=1" || bad "unit/page total=1" "$(jget '.data.total')"
api PUT "/business/erp/unit/$UNIT_ID" '{"unit_name":"W1C E2E 个（改）","status":1}'
expect "PUT /business/erp/unit/{id} 编辑" 200 0 "个（改）"
api POST /business/erp/unit "$(jq -nc --arg c "$UNIT_CODE" '{unit_code:$c,unit_name:"dup",is_base:true}')"
expect "POST unit 重复编码 → 409+66001" 409 66001

# ---------------------------------------------------------------- 3. 仓库 + 树
echo "== 3) 仓库（含 3 层树、层级上限、删除守卫）"
WROOT_CODE="$PREFIX-W1-$$"
api POST /business/erp/warehouse "$(jq -nc --arg c "$WROOT_CODE" '{warehouse_code:$c,warehouse_name:"W1C E2E 总仓",warehouse_type:"NORMAL",status:1,sort_order:1,parent_id:null,level:1}')"
expect "POST /business/erp/warehouse 建第 1 层" 200 0 && {
    W1=$(jget '.data.id')
    WPATH=$(jget '.data.warehouse_path')
    [ "$WPATH" = "/$W1/" ] && ok "  物化路径 = /{id}/" || bad "  物化路径 = /{id}/" "$WPATH"
}
api POST /business/erp/warehouse "$(jq -nc --arg c "$PREFIX-W2-$$" --arg p "$W1" '{warehouse_code:$c,warehouse_name:"W1C E2E 分仓",warehouse_type:"RAW",status:1,sort_order:1,parent_id:$p,level:2}')"
expect "POST warehouse 建第 2 层" 200 0 && W2=$(jget '.data.id')
api POST /business/erp/warehouse "$(jq -nc --arg c "$PREFIX-W3-$$" --arg p "$W2" '{warehouse_code:$c,warehouse_name:"W1C E2E 库房",warehouse_type:"FINISHED",status:1,sort_order:1,parent_id:$p,level:3}')"
expect "POST warehouse 建第 3 层" 200 0 && W3=$(jget '.data.id')
api POST /business/erp/warehouse "$(jq -nc --arg c "$PREFIX-W4-$$" --arg p "$W3" '{warehouse_code:$c,warehouse_name:"W1C E2E 第4层",warehouse_type:"NORMAL",status:1,parent_id:$p,level:3}')"
expect "POST warehouse 第 4 层 → 400+66004" 400 66004 "最多 3 层"
api GET "/business/erp/warehouse/tree?root_id=$W1"
expect "GET /business/erp/warehouse/tree 子树" 200 0
if [ "$(jget '[.data[0].children[0].children | length] | first')" = "1" ]; then
    ok "  树结构：根 → 1 子 → 1 孙"
else
    bad "  树结构：根 → 1 子 → 1 孙" "$BODY_TEXT"
fi
api GET "/business/erp/warehouse/page?keyword=$PREFIX-W&page=1&page_size=10"
expect "GET /business/erp/warehouse/page 分页" 200 0 && {
    T=$(jget '.data.total'); S=$(jget '.data.page_size')
    { [ "$T" -ge 3 ] && [ "$S" = "10" ]; } && ok "  page total=$T page_size=$S" || bad "  page total/page_size" "total=$T size=$S"
}
api GET "/business/erp/warehouse/$W2"
expect "GET /business/erp/warehouse/{id} 详情" 200 0 && [ "$(jget '.data.is_leaf')" = "false" ] && ok "  非叶子仓库 is_leaf=false" || bad "  非叶子仓库 is_leaf=false" "$(jget '.data.is_leaf')"
api PUT "/business/erp/warehouse/$W1" '{"warehouse_name":"W1C E2E 总仓（改）","manager":"张三"}'
expect "PUT /business/erp/warehouse/{id} 编辑" 200 0 "总仓（改）"
api DELETE "/business/erp/warehouse/$W1"
expect "DELETE warehouse 有下级 → 409+66003" 409 66003

# ---------------------------------------------------------------- 4. 库位
echo "== 4) 库位（叶子校验 / 默认位互斥 / 按仓库列表）"
api POST /business/erp/location "$(jq -nc --arg c "$PREFIX-L1-$$" --arg w "$W1" '{location_code:$c,warehouse_id:$w,location_name:"W1C E2E A-01",location_type:"STORAGE",is_default:true,capacity:"100.0000",status:1,sort_order:1}')"
expect "POST location 挂非叶子仓库 → 400+66005" 400 66005
api POST /business/erp/location "$(jq -nc --arg c "$PREFIX-L1-$$" --arg w "$W3" '{location_code:$c,warehouse_id:$w,location_name:"W1C E2E A-01",location_type:"STORAGE",is_default:true,capacity:"100.0000",status:1,sort_order:1}')"
expect "POST /business/erp/location 主默认库位" 200 0 && L1=$(jget '.data.id')
api POST /business/erp/location "$(jq -nc --arg c "$PREFIX-L2-$$" --arg w "$W3" '{location_code:$c,warehouse_id:$w,location_name:"W1C E2E A-02",location_type:"PICKING",is_default:true,status:1,sort_order:2}')"
expect "POST /business/erp/location 第二个默认库位" 200 0 && L2=$(jget '.data.id')
api GET "/business/erp/location/$L1"
expect "GET /business/erp/location/{id} 详情" 200 0
if [ "$(jget '.data.is_default')" = "false" ]; then ok "  旧默认位已被清掉（默认位唯一）"; else bad "  旧默认位已被清掉" "$(jget '.data.is_default')"; fi
api GET "/business/erp/location/list?warehouse_id=$W3"
expect "GET /business/erp/location/list?warehouse_id=" 200 0 && {
    N=$(jget '.data | length')
    [ "$N" -ge 2 ] && ok "  按仓库过滤回 $N 个库位" || bad "  按仓库过滤" "length=$N"
}
api GET "/business/erp/location/page?warehouse_id=$W3&page=1&page_size=10"
expect "GET /business/erp/location/page 分页" 200 0
api PUT "/business/erp/location/$L2" '{"location_name":"W1C E2E A-02（改）","status":1}'
expect "PUT /business/erp/location/{id} 编辑" 200 0 "A-02（改）"
api DELETE "/business/erp/location/$L2"
expect "DELETE /business/erp/location/{id} 软删" 200 0
api GET "/business/erp/location/$L2"
expect "GET 已删库位 → 404" 404 10002

# ---------------------------------------------------------------- 5. 物料
echo "== 5) 物料（引用校验 / 关键词搜索 / 软删）"
api POST /business/erp/material "$(jq -nc --arg c "$PREFIX-M0-$$" '{material_code:$c,material_name:"W1C E2E 坏单位",unit_id:"00000000-0000-0000-0000-0000000000ff",material_type:"RAW",costing_method:"MOVING_AVG",status:1}')"
expect "POST material 单位不存在 → 400+66002" 400 66002
api POST /business/erp/material "$(jq -nc --arg c "$PREFIX-M1-$$" --arg u "$UNIT_ID" --arg w "$W3" '{material_code:$c,material_name:"W1C E2E 六角螺栓",spec:"M8x20",unit_id:$u,material_type:"RAW",default_warehouse_id:$w,safety_stock:"10.5000",costing_method:"MOVING_AVG",tax_rate:"0.1300",status:1}')"
expect "POST /business/erp/material 新建" 200 0 && M1=$(jget '.data.id')
api GET "/business/erp/material/list?keyword=$PREFIX-M1"
expect "GET /business/erp/material/list?keyword= 搜索" 200 0 && {
    N=$(jget '.data | length')
    [ "$N" -eq 1 ] && ok "  关键词命中 1 条" || bad "  关键词命中 1 条" "length=$N"
}
api GET "/business/erp/material/page?keyword=$PREFIX-M&page=1&page_size=5"
expect "GET /business/erp/material/page 分页" 200 0
api GET "/business/erp/material/$M1"
expect "GET /business/erp/material/{id} 详情" 200 0
api PUT "/business/erp/material/$M1" '{"material_name":"W1C E2E 六角螺栓（改）","spec":null}'
expect "PUT /business/erp/material/{id} 编辑（spec 显式 null 清空）" 200 0 "（改）"
api DELETE "/business/erp/material/$M1"
expect "DELETE /business/erp/material/{id} 软删" 200 0
api GET "/business/erp/material/$M1"
expect "GET 已删物料 → 404" 404 10002

# ---------------------------------------------------------------- 6. 供应商
echo "== 6) 供应商"
api POST /business/erp/supplier "$(jq -nc --arg c "$PREFIX-S1-$$" '{supplier_code:$c,supplier_name:"W1C E2E 供应商",short_name:"E2E",contact_person:"张三",contact_phone:"13800000000",credit_days:30,credit_limit:"12345.6789",status:1}')"
expect "POST /business/erp/supplier 新建" 200 0 && S1=$(jget '.data.id')
api GET "/business/erp/supplier/$S1"
expect "GET /business/erp/supplier/{id} 详情" 200 0
api GET "/business/erp/supplier/page?keyword=$PREFIX-S1&page=1&page_size=5"
expect "GET /business/erp/supplier/page 分页" 200 0
api PUT "/business/erp/supplier/$S1" '{"supplier_name":"W1C E2E 供应商（改）","credit_days":60,"status":1}'
expect "PUT /business/erp/supplier/{id} 编辑" 200 0 "（改）"
api DELETE "/business/erp/supplier/$S1"
expect "DELETE /business/erp/supplier/{id} 软删" 200 0
api GET "/business/erp/supplier/$S1"
expect "GET 已删供应商 → 404" 404 10002

# ---------------------------------------------------------------- 7. 只读台账 8 张
echo "== 7) 只读台账（8 张：列表 + 详情）"
for entity in account period seq purchase-order purchase-order-item inventory inventory-location inventory-txn; do
    api GET "/business/erp/$entity/page?page=1&page_size=5"
    expect "GET /business/erp/$entity/page" 200 0
done
MISSING_ID=$(cat /proc/sys/kernel/random/uuid)
for entity in account period seq purchase-order purchase-order-item inventory inventory-location inventory-txn; do
    api GET "/business/erp/$entity/$MISSING_ID"
    expect "GET /business/erp/$entity/{id} 不存在 → 404" 404 10002
done

# ---------------------------------------------------------------- 8. 权限：无码 → 403
echo "== 8) 权限（无码账号 → 403）"
api POST /system/role "$(jq -nc --arg c "$PREFIX-NOPERM" '{role_code:$c,role_name:"W1C E2E 无权限角色",description:"w1c e2e",status:1,data_scope:1}')"
expect "POST /system/role 建无权限角色" 200 0 && ROLE_ID=$(jget '.data.id')
api POST /system/user "$(jq -nc --arg c "${PREFIX,,}_noperm" --arg r "$ROLE_ID" '{username:$c,password:"W1cE2e@2026",real_name:"W1C E2E",status:1,role_ids:[$r]}')"
expect "POST /system/user 建无权限账号" 200 0
ADMIN_TOKEN=$TOKEN
TOKEN=""
login_by_user() {
    api POST /system/auth/login "$(jq -nc --arg u "${PREFIX,,}_noperm" --arg p "W1cE2e@2026" '{username:$u,password:$p}')"
    TOKEN=$(jq -r '.data.access_token // empty' "$BODY")
}
login_by_user
if [ -n "$TOKEN" ]; then
    api GET "/business/erp/warehouse/page?page=1&page_size=5"
    if [ "$HTTP" = "403" ]; then ok "无权限码打 GET /business/erp/warehouse/page → 403"; else bad "无权限码 → 403" "HTTP $HTTP $BODY_TEXT"; fi
    api POST /business/erp/unit "$(jq -nc '{unit_code:"W1C-E2E-NO",unit_name:"x",is_base:true}')"
    if [ "$HTTP" = "403" ]; then ok "无权限码打 POST /business/erp/unit → 403"; else bad "无权限码写 → 403" "HTTP $HTTP $BODY_TEXT"; fi
    note "403 响应体：$BODY_TEXT"
else
    bad "无权限账号登录" "$BODY_TEXT"
fi
TOKEN=$ADMIN_TOKEN

# ---------------------------------------------------------------- 9. EXPLAIN：仓库树必须走索引
echo "== 9) EXPLAIN：20 万级数据下仓库树查询必须命中 text_pattern_ops 索引"
WAREHOUSE_ROOT_PATH=$(psql_q "SELECT warehouse_path FROM biz_warehouse WHERE id = '$W1'::uuid")
# 200 × 1000 = 20 万行，全部挂在 E2E 总仓下面（路径真实、层级真实）
psql_q "INSERT INTO biz_warehouse (id, warehouse_code, warehouse_name, warehouse_type, status, sort_order, parent_id, level, warehouse_path, is_leaf)
        SELECT ('f1'||lpad(r::text,6,'0')||'-0000-4000-8000-'||lpad(i::text,12,'0'))::uuid,
               'W1C-BULK-'||r||'-'||i, 'W1C 压测仓 '||r||'-'||i, 'NORMAL', 1, 1, '$W1'::uuid, 2,
               '$WAREHOUSE_ROOT_PATH' || 'f1'||lpad(r::text,6,'0')||'-0000-4000-8000-'||lpad(i::text,12,'0') || '/', 1
          FROM generate_series(1, 200) AS r, generate_series(1, 1000) AS i" >/dev/null
psql_q "ANALYZE biz_warehouse" >/dev/null
BULK=$(psql_q "SELECT count(*) FROM biz_warehouse WHERE warehouse_code LIKE 'W1C-BULK-%'")
note "压测数据：$BULK 行（biz_warehouse 总计 $(psql_q 'SELECT count(*) FROM biz_warehouse') 行）"
# 前缀只命中 1000 行（r=1）—— 这正是「树形前缀查询」在生产里的形态
LEAF_PREFIX="${WAREHOUSE_ROOT_PATH}f1000001-0000-4000-8000-000000000001/"
LEAF_UPPER="${LEAF_PREFIX%/}0"
PLAN_FILE=/tmp/w1c_explain.txt
psql -h 127.0.0.1 -U "$NIK_DATABASE_USER" -d "$W1C_DB" -Atc "
  PREPARE w1c_tree(text, text) AS
    SELECT id, warehouse_code FROM biz_warehouse
     WHERE deleted_at IS NULL AND warehouse_path ~>=~ \$1 AND warehouse_path ~<~ \$2
     ORDER BY warehouse_path ASC, sort_order ASC;
  EXPLAIN (ANALYZE, BUFFERS) EXECUTE w1c_tree('$LEAF_PREFIX', '$LEAF_UPPER');
" >"$PLAN_FILE" 2>&1
sed 's/^/       /' "$PLAN_FILE" | head -12
if grep -q "idx_biz_warehouse_path_pattern" "$PLAN_FILE"; then
    ok "EXPLAIN 命中 idx_biz_warehouse_path_pattern"
else
    bad "EXPLAIN 命中 text_pattern_ops 索引" "见 $PLAN_FILE"
fi
if grep -q "Seq Scan on biz_warehouse" "$PLAN_FILE"; then
    bad "EXPLAIN 无全表扫" "计划里出现 Seq Scan on biz_warehouse"
else
    ok "EXPLAIN 无 Seq Scan on biz_warehouse"
fi
START=$(date +%s%N)
api GET "/business/erp/warehouse/tree?root_id=f1000001-0000-4000-8000-000000000001"
COST=$(( ($(date +%s%N) - START) / 1000000 ))
expect "20 万级数据下 GET /business/erp/warehouse/tree 仍 200" 200 0 \
    && note "root_id=单个压测仓，耗时 ${COST}ms，返回 $(jget '.data | length') 个节点（自身无下级）"

# ---------------------------------------------------------------- 10. 残留与收尾
echo "== 10) 收尾：夹具残留检查"
psql_q "DELETE FROM biz_warehouse WHERE warehouse_code LIKE 'W1C-BULK-%'" >/dev/null
# 5 张可写表都要清（第一版只清了 warehouse，unit/material/supplier/location 各剩 1–2 行 → 残留断言红）
psql_q "DELETE FROM biz_location WHERE location_code LIKE '$PREFIX-%'" >/dev/null
psql_q "DELETE FROM biz_material WHERE material_code LIKE '$PREFIX-%'" >/dev/null
psql_q "DELETE FROM biz_unit WHERE unit_code LIKE '$PREFIX-%'" >/dev/null
psql_q "DELETE FROM biz_supplier WHERE supplier_code LIKE '$PREFIX-%'" >/dev/null
psql_q "DELETE FROM biz_warehouse WHERE warehouse_code LIKE '$PREFIX-%'" >/dev/null
LEFT=$(psql_q "SELECT (SELECT count(*) FROM biz_unit WHERE unit_code LIKE '$PREFIX-%')
                    + (SELECT count(*) FROM biz_warehouse WHERE warehouse_code LIKE '$PREFIX-%')
                    + (SELECT count(*) FROM biz_material WHERE material_code LIKE '$PREFIX-%')
                    + (SELECT count(*) FROM biz_supplier WHERE supplier_code LIKE '$PREFIX-%')
                    + (SELECT count(*) FROM biz_location WHERE location_code LIKE '$PREFIX-%')")
if [ "$LEFT" = "0" ]; then ok "夹具残留 0 行（unit/warehouse/material/supplier/location）"; else bad "夹具残留" "剩 $LEFT 行"; fi
# V0141 落库核对：32 个权限码 + 34 个菜单节点（2 目录 / 5 主数据菜单 / 8 台账菜单 / 19 按钮）
CODES=$(psql_q "SELECT count(distinct auth_code) FROM sys_menu WHERE auth_code LIKE 'business:erp:%' AND deleted_at IS NULL")
NODES=$(psql_q "SELECT count(*) FROM sys_menu WHERE deleted_at IS NULL AND (id::text LIKE '20000000-0000-0000-0000-00000000094%' OR id::text LIKE '20000000-0000-0000-0000-00000000095%' OR id::text LIKE '20000000-0000-0000-0000-00000000096%' OR id::text LIKE '20000000-0000-0000-0000-00000000097%')")
[ "$CODES" = "32" ] && ok "V0141：32 个 ERP 权限码已落库" || bad "V0141 权限码落库" "实际 $CODES"
[ "$NODES" = "34" ] && ok "V0141：34 个菜单节点已落库（2 目录 + 13 菜单 + 19 按钮）" || bad "V0141 菜单节点落库" "实际 $NODES"
GRANTED=$(psql_q "SELECT count(*) FROM sys_role_menu rm JOIN sys_role r ON r.id = rm.role_id WHERE r.role_code = 'SUPER_ADMIN' AND rm.menu_id::text LIKE '20000000-0000-0000-0000-0000000009__'")
note "空库首次迁移时超管授权 = $GRANTED 个（bootstrap 尚未建出角色，与 V0045/V0128/V0133 同一口径）"

# 生产库（角色早已存在）里 V0141 的授权会真的落库；这里在 bootstrap 之后**重放一次迁移**
# 证明「幂等 + 授权补齐」两条都成立（重放不产生重复行，也不会覆盖运维改过的标题）。
psql -h 127.0.0.1 -U "$NIK_DATABASE_USER" -d "$W1C_DB" -q      -f "$REPO/backend/migrations/business/V0141__erp_permissions.up.sql" >/tmp/w1c_replay.log 2>&1
REPLAY_RC=$?
grep -E "NOTICE" /tmp/w1c_replay.log | sed 's/^/       /' | tail -3
[ "$REPLAY_RC" = "0" ] && ok "V0141 重放（幂等）：exit=0" || bad "V0141 重放（幂等）" "exit=$REPLAY_RC $(tail -3 /tmp/w1c_replay.log)"
GRANTED2=$(psql_q "SELECT count(*) FROM sys_role_menu rm JOIN sys_role r ON r.id = rm.role_id WHERE r.role_code = 'SUPER_ADMIN' AND rm.menu_id::text LIKE '20000000-0000-0000-0000-0000000009__'")
[ "$GRANTED2" = "34" ] && ok "超管已授权 34 个 ERP 节点（重放后补齐）" || bad "超管授权 34 节点" "实际 $GRANTED2"
DUP=$(psql_q "SELECT count(*) FROM (SELECT menu_id FROM sys_role_menu rm JOIN sys_role r ON r.id=rm.role_id WHERE r.role_code='SUPER_ADMIN' GROUP BY menu_id HAVING count(*) > 1) t")
[ "$DUP" = "0" ] && ok "角色-菜单授权无重复行（重放幂等）" || bad "授权无重复" "重复 $DUP 组"

if [ "${W1C_KEEP:-1}" = "0" ]; then
    kill "$(cat "$PORT_PID_FILE")" 2>/dev/null || true
    sleep 2
    su - postgres -c "dropdb --if-exists $W1C_DB" >/dev/null 2>&1
    note "已删库 $W1C_DB"
else
    note "保留库 $W1C_DB（实例 pid $(cat "$PORT_PID_FILE")，日志 $LOG）；W1C_KEEP=0 可跑完即删"
fi

echo
echo "=============================================="
echo " W1c 验收：PASS=$PASS  FAIL=$FAIL"
if [ "$FAIL" -gt 0 ]; then
    printf ' 失败项：\n'
    for item in "${FAILED_CASES[@]}"; do printf '   - %s\n' "$item"; done
    exit 1
fi
echo " 全部通过"

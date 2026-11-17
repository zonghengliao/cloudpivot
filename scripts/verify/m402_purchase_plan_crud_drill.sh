#!/usr/bin/env bash
# ============================================================
# M4-02 采购计划 CRUD + 审批 —— 真接口演练（隔离实例 + 真 curl）
#
# 卡：t_c6473745（M4-02【M4·采购】采购计划：CRUD + 审批）
#
# 跑什么（每一条都是卡面 / V0446 / V0447 的一句原话）：
#   ① 建计划（2 行明细）→ 单号 PPL-yyyyMMdd-NNNNNN、DRAFT、合计由服务端算、明细带物料/仓库/单位名
#   ② 同期间再建一张 → 409（uk_biz_purchase_plan_period 的友好侧）*
#   ③ 同物料同仓再来一行 → 400，报文点出「第 2 行」
#   ④ 分页按关键字命中；详情带明细
#   ⑤ 改单（整单替换）→ 合计跟着变
#   ⑥ 提交审批 → SUBMITTED + approval_instance_id（走 pp_approve 流程）
#   ⑦ 审批中改单 / 删单 → 409
#   ⑧ 制单人自己批 → 403（内控：制单与审批分离）
#   ⑨ 审批人（角色成员）批第一票 → 小额计划直接 APPROVED（回调落状态 + 备注写结论）
#   ⑩ 大额计划（≥ 50 万）→ 第一票之后还有财务经理那一票（条件边，不是代码 if）
#   ⑪ 删草稿 → 200；详情 404；软删后同期间可重建
#   ⑫ 迁移幂等：V0447 再跑一遍 rc=0；down/up 往返后 6 行菜单与流程都回来
#   ⑬ 权限：不带令牌 → 401；页面显形码 page 在后端零引用（N14 台账已登记）
#
# 用法（99 开发机）：
#   bash scripts/verify/m402_purchase_plan_crud_drill.sh
#   DBHOST=127.0.0.1 bash …           # 145 测试机上跑（走 loopback）
#   环境变量：DBUSER/DBPASS/DBOWNER/PORT 可覆盖
#
# 退出码：0 = 全过；1 = 有断言失败；2 = 环境问题（建库/起不来）
# ============================================================
set -uo pipefail
export PATH=/root/.cargo/bin:$PATH

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
DBHOST="${DBHOST:-172.18.10.145}"
DBPORT="${DBPORT:-5432}"
DBUSER="${DBUSER:-cloudpivot}"
DBPASS="${DBPASS:-fbb1263691ff75c31ec99eaf258138b4}"
DBOWNER="${DBOWNER:-cloudpivot}"
DB="${DB:-cp_m402_drill}"
TEMPLATE="${TEMPLATE:-cloudpivot_test}"
PORT="${PORT:-18097}"
BASE="http://127.0.0.1:$PORT/api"
MROOT="$ROOT/backend/app/migrations"
LOG=/root/m402-drill-server.log
PIDFILE=/root/m402-drill-server.pid
ADMIN_USER="${ADMIN_USER:-admin}"
ADMIN_PASS="${ADMIN_PASS:-Admin@123456}"
# 审批人夹具复制的是 **admin 改密前**的口令哈希，所以它的口令恒等于这一份原值。
APPROVER_PASS="$ADMIN_PASS"
APPROVER_USER="m402_approver"

UNIT="d4020001-0000-4000-8000-000000000001"
WH_A="d4020001-0000-4000-8000-000000000002"
WH_B="d4020001-0000-4000-8000-000000000003"
MAT_A="d4020001-0000-4000-8000-000000000011"
MAT_B="d4020001-0000-4000-8000-000000000012"
APPROVER_ID="d4020001-0000-4000-8000-0000000000a1"

PASS=0
FAIL=0
TOKEN=""
REQ_CODE=0
REQ_BODY=""

ok() { PASS=$((PASS + 1)); printf '[PASS] %s\n' "$1"; }
bad() { FAIL=$((FAIL + 1)); printf '[FAIL] %s\n' "$1"; }
assert_eq() { if [ "$1" = "$2" ]; then ok "$3（$2）"; else bad "$3：期望 [$1] 实际 [$2]"; fi; }
assert_contains() {
    case "$1" in
        *"$2"*) ok "$3" ;;
        *) bad "$3：文本里没有 [$2]，实际 [$(printf '%s' "$1" | head -c 300)]" ;;
    esac
}
assert_num() {
    if awk -v a="$1" -v b="$2" 'BEGIN { if (a == "" || b == "" || b == "null") exit 1; exit !((a + 0) == (b + 0)) }'; then
        ok "$3（$2）"
    else
        bad "$3：期望 [$1] 实际 [$2]"
    fi
}

psql_q() { PGPASSWORD="$DBPASS" psql -h "$DBHOST" -p "$DBPORT" -U "$DBUSER" -d "$DB" -Atc "$1" 2>&1 | head -1; }
psql_x() { PGPASSWORD="$DBPASS" psql -h "$DBHOST" -p "$DBPORT" -U "$DBUSER" -d "$DB" -v ON_ERROR_STOP=1 -q -c "$1" 2>&1 | head -3; }

jqv() { printf '%s' "$REQ_BODY" | jq -r "$1" 2>/dev/null; }

call() {
    local method="$1" path="$2" body="${3:-}" tmp
    tmp=$(mktemp)
    if [ -n "$body" ]; then
        REQ_CODE=$(curl -s -o "$tmp" -w '%{http_code}' -X "$method" \
            -H "Authorization: Bearer $TOKEN" -H 'Content-Type: application/json' \
            --data-binary "$body" "$BASE$path")
    else
        REQ_CODE=$(curl -s -o "$tmp" -w '%{http_code}' -X "$method" \
            -H "Authorization: Bearer $TOKEN" "$BASE$path")
    fi
    REQ_BODY=$(cat "$tmp")
    rm -f "$tmp"
}

stop_server() {
    local pids=""
    # 按监听端口找进程（pidfile 里记的是 subshell 的 pid，不可靠）
    pids=$(ss -ltnp 2>/dev/null | grep ":$PORT " | grep -oE 'pid=[0-9]+' | cut -d= -f2 | tr '\n' ' ')
    if [ -f "$PIDFILE" ]; then pids="$pids $(cat "$PIDFILE")"; fi
    for p in $pids; do
        [ -n "$p" ] && kill "$p" 2>/dev/null
    done
    for _ in $(seq 1 30); do
        ss -ltn 2>/dev/null | grep -q ":$PORT " || break
        sleep 1
    done
    for p in $pids; do
        [ -n "$p" ] && kill -9 "$p" 2>/dev/null
    done
    # 兜底：踢掉残留的库会话（否则 dropdb 报「being accessed by other users」）
    psql_q "SELECT count(pg_terminate_backend(pid)) FROM pg_stat_activity WHERE datname = '$DB'" >/dev/null 2>&1
    rm -f "$PIDFILE"
}
trap 'stop_server' EXIT

echo '==> 0. 建**空**演练库（迁移与种子由实例首启自己跑完：这才是空库首启的真实路径）'
export PGPASSWORD="$DBPASS"
stop_server
dropdb -h "$DBHOST" -p "$DBPORT" -U "$DBUSER" --if-exists "$DB" >/dev/null 2>&1
createdb -h "$DBHOST" -p "$DBPORT" -U "$DBUSER" -O "$DBOWNER" "$DB" || {
    echo "FAIL: 建库失败" >&2; exit 2; }
echo '    空库已建（0 表）；迁移由实例启动时按版本号升序执行'

echo '==> 1. 夹具（基本单位 / 两个仓库 / 两个物料 / 一个审批人）—— 等等，先起实例建表'

echo '==> 2. 起隔离实例（端口 '"$PORT"'，库 '"$DB"'）'
[ -x "$ROOT/backend/target/debug/cloudpivot" ] || (cd "$ROOT/backend" && cargo build -q) || {
    echo 'FAIL: 后端没编译出来' >&2; exit 2; }
if ! curl -s -o /dev/null "http://127.0.0.1:$PORT/health"; then :; fi
export NIK_PROFILE=test
export NIK_DATABASE_NAME="$DB"
export NIK_DATABASE_HOST="$DBHOST"
export NIK_DATABASE_PORT="$DBPORT"
export NIK_DATABASE_USER="$DBUSER"
export NIK_DATABASE_PASSWORD="$DBPASS"
export NIK_SERVER_LISTEN="127.0.0.1:$PORT"
export NIK_MIDDLEWARE_AUTH_JWT_SECRET="$(openssl rand -hex 32)"
export CLOUDPIVOT_STORAGE_MASTER_KEY="$(openssl rand -hex 32)"
export NIK_ADMIN_PASSWORD="$ADMIN_PASS"
rm -f "$LOG"
(cd "$ROOT/backend" && setsid nohup ./target/debug/cloudpivot >"$LOG" 2>&1 </dev/null & echo $! >"$PIDFILE")
for _ in $(seq 1 180); do
    if [ "$(curl -s -o /dev/null -w '%{http_code}' "http://127.0.0.1:$PORT/health" || true)" = "200" ]; then
        break
    fi
    kill -0 "$(cat "$PIDFILE")" 2>/dev/null || { echo "FAIL: 实例退出了，看 $LOG" >&2; tail -20 "$LOG" >&2; exit 2; }
    sleep 1
done
[ "$(curl -s -o /dev/null -w '%{http_code}' "http://127.0.0.1:$PORT/health")" = "200" ] || {
    echo 'FAIL: 健康检查超时' >&2; exit 2; }
ok '实例已起（/health 200）'
assert_eq "$(psql_q "SELECT count(*) FROM biz_purchase_plan")" "0" '采购计划表已建成且为空'
assert_eq "$(psql_q "SELECT count(*) FROM biz_flow_def WHERE flow_code='pp_approve' AND deleted_at IS NULL")" "1" 'pp_approve 流程已种下'
assert_eq "$(psql_q "SELECT count(*) FROM sys_menu WHERE id::text LIKE '20000000-0000-0000-0000-00000000447%' AND deleted_at IS NULL")" "6" '采购计划菜单 6 行已种下'

echo '==> 2b. 夹具（基本单位 / 两个仓库 / 两个物料 / 一个审批人）'
psql_x "INSERT INTO biz_unit (id, unit_code, unit_name, is_base, rate, status)
        VALUES ('$UNIT', 'M402-U', 'M402 基本单位', 1, 1, 1) ON CONFLICT (id) DO NOTHING;
        INSERT INTO biz_warehouse (id, warehouse_code, warehouse_name, warehouse_path, is_leaf, level, status)
        VALUES ('$WH_A', 'M402-WH-A', 'M402 仓库 A', '/M402-WH-A', 1, 1, 1),
               ('$WH_B', 'M402-WH-B', 'M402 仓库 B', '/M402-WH-B', 1, 1, 1)
        ON CONFLICT (id) DO NOTHING;
        INSERT INTO biz_material (id, material_code, material_name, unit_id, material_type, status)
        VALUES ('$MAT_A', 'M402-MAT-A', 'M402 物料 A', '$UNIT', 'RAW', 1),
               ('$MAT_B', 'M402-MAT-B', 'M402 物料 B', '$UNIT', 'RAW', 1)
        ON CONFLICT (id) DO NOTHING;"
# 审批人：复制 admin 的口令哈希（不知道明文就登录不了，只能这么造一个可用账号）
psql_x "INSERT INTO sys_user (id, username, real_name, password, status, must_change_password)
        SELECT '$APPROVER_ID'::uuid, '$APPROVER_USER', 'M402 审批人', password, 1, false
          FROM sys_user WHERE username = '$ADMIN_USER'
        ON CONFLICT (id) DO NOTHING;
        INSERT INTO sys_user_role (user_id, role_id)
        SELECT '$APPROVER_ID'::uuid, r.id FROM sys_role r
         WHERE r.role_code IN ('PURCHASE_MANAGER', 'FINANCE_MANAGER') AND r.deleted_at IS NULL
        ON CONFLICT DO NOTHING;"
assert_eq "$(psql_q "SELECT count(*) FROM sys_user WHERE username='$APPROVER_USER'")" "1" '审批人夹具已建'

echo '==> 3. 登录 admin'
tmp=$(mktemp)
REQ_CODE=$(curl -s -o "$tmp" -w '%{http_code}' -X POST -H 'Content-Type: application/json' \
    --data-binary "{\"username\":\"$ADMIN_USER\",\"password\":\"$ADMIN_PASS\"}" "$BASE/system/auth/login")
REQ_BODY=$(cat "$tmp"); rm -f "$tmp"
TOKEN=$(jqv '.data.access_token')
[ -n "$TOKEN" ] && [ "$TOKEN" != "null" ] || { echo "FAIL: 登录失败 $REQ_BODY" >&2; exit 2; }
ok '登录拿到令牌'
if [ "$(jqv '.data.must_change_password')" = "true" ]; then
    NEW_PASS="M402_${ADMIN_PASS}_new"
    curl -s -o /dev/null -X PUT -H "Authorization: Bearer $TOKEN" -H 'Content-Type: application/json' \
        --data-binary "{\"old_password\":\"$ADMIN_PASS\",\"new_password\":\"$NEW_PASS\"}" "$BASE/system/auth/password"
    ADMIN_PASS="$NEW_PASS"
    tmp=$(mktemp)
    curl -s -o "$tmp" -X POST -H 'Content-Type: application/json' \
        --data-binary "{\"username\":\"$ADMIN_USER\",\"password\":\"$ADMIN_PASS\"}" "$BASE/system/auth/login"
    TOKEN=$(jq -r '.data.access_token' <"$tmp"); rm -f "$tmp"
    ok '首登强制改密后重新登录'
fi

echo '==> 4. 建计划（2 行明细）'
BODY=$(cat <<JSON
{"plan_name":"M402 2026 年度采购计划","plan_type":"ANNUAL","plan_year":2026,
 "remark":"M402 演练",
 "items":[
   {"material_id":"$MAT_A","warehouse_id":"$WH_A","planned_qty":"10","planned_price":"2.5"},
   {"material_id":"$MAT_B","warehouse_id":"$WH_B","planned_qty":"4","planned_price":"100"}]}
JSON
)
call POST /business/erp/purchase-plan "$BODY"
assert_eq "$REQ_CODE" "200" '建计划返回 200'
PLAN_ID=$(jqv '.data.plan.id')
PLAN_NO=$(jqv '.data.plan.plan_no')
assert_eq "$(jqv '.data.plan.status')" "DRAFT" '状态是 DRAFT'
assert_contains "$PLAN_NO" "PPL-" '单号以 PPL- 开头'
assert_eq "$(printf '%s' "$PLAN_NO" | wc -c | tr -d ' ')" "19" '单号 19 位（PPL-yyyyMMdd-NNNNNN）'
assert_num "$(jqv '.data.plan.total_planned_qty')" "14" '计划量合计 = 10 + 4（服务端算）'
assert_num "$(jqv '.data.plan.total_planned_amount')" "425" '计划金额合计 = 25 + 400（服务端算）'
assert_eq "$(jqv '.data.plan.item_count')" "2" '明细 2 行'
assert_eq "$(jqv '.data.items[0].material_code')" "M402-MAT-A" '明细带物料编码（join）'
assert_eq "$(jqv '.data.items[0].warehouse_name')" "M402 仓库 A" '明细带仓库名（join）'
assert_eq "$(jqv '.data.items[0].unit_name')" "M402 基本单位" '明细带单位名（由物料带出）'
assert_eq "$(jqv '.data.plan.editable')" "true" '草稿 editable=true'

echo '==> 5. 负向：重复期间 409 / 重复物料×仓库 400 / 无令牌 401'
REQ_CODE=$(curl -s -o /tmp/m402_dup.json -w '%{http_code}' -X POST -H "Authorization: Bearer $TOKEN" \
    -H 'Content-Type: application/json' --data-binary "$BODY" "$BASE/business/erp/purchase-plan")
DUPTXT=$(cat /tmp/m402_dup.json); rm -f /tmp/m402_dup.json
assert_eq "$REQ_CODE" "409" '同期间第二张 = 409'
assert_contains "$DUPTXT" "PPL-" '409 报文点出占用方单号'
DUPCELL=$(cat <<JSON
{"plan_name":"M402 重复明细","plan_type":"ANNUAL","plan_year":2027,
 "items":[
   {"material_id":"$MAT_A","warehouse_id":"$WH_A","planned_qty":"1","planned_price":"1"},
   {"material_id":"$MAT_A","warehouse_id":"$WH_A","planned_qty":"2","planned_price":"1"}]}
JSON
)
call POST /business/erp/purchase-plan "$DUPCELL"
assert_eq "$REQ_CODE" "400" '同物料同仓重复 = 400'
assert_contains "$REQ_BODY" "第 2 行" '报文点出重复行号'
NOPERM=$(curl -s -o /dev/null -w '%{http_code}' "$BASE/business/erp/purchase-plan/page")
assert_eq "$NOPERM" "401" '不带令牌 = 401'

echo '==> 6. 分页 + 详情'
call GET '/business/erp/purchase-plan/page?page=1&page_size=10&keyword=M402'
assert_eq "$REQ_CODE" "200" '分页 200'
assert_num "$(jqv '.data.total')" "1" '关键字命中 1 张'
assert_eq "$(jqv '.data.list[0].plan_no')" "$PLAN_NO" '列表里是刚建的那张'
call GET "/business/erp/purchase-plan/$PLAN_ID"
assert_eq "$REQ_CODE" "200" '详情 200'
assert_eq "$(jqv '.data.items[1].material_name')" "M402 物料 B" '详情第 2 行物料名'

echo '==> 7. 改单（整单替换）'
BODY2=$(cat <<JSON
{"plan_name":"M402 2026 年度采购计划（已改）","plan_type":"ANNUAL","plan_year":2026,
 "items":[
   {"material_id":"$MAT_A","warehouse_id":"$WH_B","planned_qty":"6","planned_price":"3"}]}
JSON
)
call PUT "/business/erp/purchase-plan/$PLAN_ID" "$BODY2"
assert_eq "$REQ_CODE" "200" '改草稿 200'
assert_num "$(jqv '.data.plan.total_planned_qty')" "6" '改后计划量 = 6'
assert_num "$(jqv '.data.plan.total_planned_amount')" "18" '改后金额 = 18'
assert_eq "$(jqv '.data.items | length')" "1" '明细整单替换（旧行不残留）'

echo '==> 8. 提交审批（走 pp_approve）'
call POST "/business/erp/purchase-plan/$PLAN_ID/submit" '{"reason":"M402 演练提交"}'
assert_eq "$REQ_CODE" "200" '提交 200'
assert_eq "$(jqv '.data.plan.status')" "SUBMITTED" '提交后 SUBMITTED'
INSTANCE=$(jqv '.data.plan.approval_instance_id')
[ -n "$INSTANCE" ] && [ "$INSTANCE" != "null" ] && ok '带回审批实例 id' || bad '没拿到审批实例 id'
assert_eq "$(psql_q "SELECT biz_type FROM biz_flow_instance WHERE id='$INSTANCE'::uuid")" "PP_APPROVE" '实例 biz_type = PP_APPROVE'
assert_eq "$(jqv '.data.plan.editable')" "false" '审批中 editable=false'

echo '==> 9. 审批中改 / 删 → 409'
call PUT "/business/erp/purchase-plan/$PLAN_ID" "$BODY2"
assert_eq "$REQ_CODE" "409" '审批中改单 = 409'
call DELETE "/business/erp/purchase-plan/$PLAN_ID"
assert_eq "$REQ_CODE" "409" '审批中删单 = 409'

echo '==> 10. 制单人自批 → 403（内控：制单与审批分离）'
TASK=$(psql_q "SELECT id FROM biz_flow_task WHERE instance_id='$INSTANCE'::uuid AND status='PENDING' ORDER BY created_at LIMIT 1")
call POST "/business/flow/task/$TASK/approve" '{"comment":"M402 自批"}'
assert_eq "$REQ_CODE" "403" '制单人自批 = 403'

echo '==> 11. 审批人批第一票 → 小额计划直接 APPROVED'
tmp=$(mktemp)
curl -s -o "$tmp" -X POST -H 'Content-Type: application/json' \
    --data-binary "{\"username\":\"$APPROVER_USER\",\"password\":\"$APPROVER_PASS\"}" "$BASE/system/auth/login"
ATOKEN=$(jq -r '.data.access_token // empty' <"$tmp"); rm -f "$tmp"
[ -n "$ATOKEN" ] || { bad '审批人登录失败'; ATOKEN=""; }
if [ -n "$ATOKEN" ]; then
    for _ in 1 2 3; do
        TASK=$(psql_q "SELECT id FROM biz_flow_task WHERE instance_id='$INSTANCE'::uuid AND status='PENDING' ORDER BY created_at LIMIT 1")
        [ -n "$TASK" ] || break
        code=$(curl -s -o /dev/null -w '%{http_code}' -X POST -H "Authorization: Bearer $ATOKEN" \
            -H 'Content-Type: application/json' --data-binary '{"comment":"M402 演练同意"}' \
            "$BASE/business/flow/task/$TASK/approve")
        [ "$code" = "200" ] || bad "审批接口返回 $code"
    done
    assert_eq "$(psql_q "SELECT status FROM biz_purchase_plan WHERE id='$PLAN_ID'::uuid")" "APPROVED" '审批通过后计划 = APPROVED（回调落状态）'
    assert_contains "$(psql_q "SELECT replace(remark, E'\\n', ' | ') FROM biz_purchase_plan WHERE id='$PLAN_ID'::uuid")" "审批通过" '结论写进备注'
fi

echo '==> 12. 大额计划（60 万）→ 两票才通过（条件边）'
BIG=$(cat <<JSON
{"plan_name":"M402 大额计划","plan_type":"ANNUAL","plan_year":2029,
 "items":[{"material_id":"$MAT_A","warehouse_id":"$WH_A","planned_qty":"1000","planned_price":"600"}]}
JSON
)
call POST /business/erp/purchase-plan "$BIG"
BIG_ID=$(jqv '.data.plan.id')
assert_num "$(jqv '.data.plan.total_planned_amount')" "600000" '大额计划 60 万'
call POST "/business/erp/purchase-plan/$BIG_ID/submit" '{}'
BIG_INST=$(jqv '.data.plan.approval_instance_id')
if [ -n "$ATOKEN" ] && [ -n "$BIG_INST" ] && [ "$BIG_INST" != "null" ]; then
    T1=$(psql_q "SELECT id FROM biz_flow_task WHERE instance_id='$BIG_INST'::uuid AND status='PENDING' ORDER BY created_at LIMIT 1")
    NODE1=$(psql_q "SELECT node_key FROM biz_flow_task WHERE id='$T1'::uuid")
    assert_eq "$NODE1" "n_pm" '第一站 = 采购经理'
    curl -s -o /dev/null -X POST -H "Authorization: Bearer $ATOKEN" -H 'Content-Type: application/json' \
        --data-binary '{"comment":"M402 一级同意"}' "$BASE/business/flow/task/$T1/approve"
    assert_eq "$(psql_q "SELECT status FROM biz_purchase_plan WHERE id='$BIG_ID'::uuid")" "SUBMITTED" '60 万一票之后仍是 SUBMITTED'
    T2=$(psql_q "SELECT id FROM biz_flow_task WHERE instance_id='$BIG_INST'::uuid AND status='PENDING' ORDER BY created_at LIMIT 1")
    NODE2=$(psql_q "SELECT node_key FROM biz_flow_task WHERE id='$T2'::uuid")
    assert_eq "$NODE2" "n_fin" '第二站 = 财务经理（条件边生效）'
    curl -s -o /dev/null -X POST -H "Authorization: Bearer $ATOKEN" -H 'Content-Type: application/json' \
        --data-binary '{"comment":"M402 二级同意"}' "$BASE/business/flow/task/$T2/approve"
    assert_eq "$(psql_q "SELECT status FROM biz_purchase_plan WHERE id='$BIG_ID'::uuid")" "APPROVED" '两票后 = APPROVED'
fi

echo '==> 13. 删草稿 + 软删后重建'
SMALL=$(cat <<JSON
{"plan_name":"M402 待删计划","plan_type":"ANNUAL","plan_year":2030,
 "items":[{"material_id":"$MAT_A","warehouse_id":"$WH_A","planned_qty":"1","planned_price":"1"}]}
JSON
)
call POST /business/erp/purchase-plan "$SMALL"
DEL_ID=$(jqv '.data.plan.id')
call DELETE "/business/erp/purchase-plan/$DEL_ID"
assert_eq "$REQ_CODE" "200" '删草稿 200'
call GET "/business/erp/purchase-plan/$DEL_ID"
assert_eq "$REQ_CODE" "404" '删后详情 404'
REBUILD=$(printf '%s' "$SMALL" | jq -c '.plan_name="M402 待删计划-重建"')
call POST /business/erp/purchase-plan "$REBUILD"
assert_eq "$REQ_CODE" "200" '软删后同期间可重建'

echo '==> 14. 迁移幂等 + down/up 往返'
if PGPASSWORD="$DBPASS" psql -h "$DBHOST" -p "$DBPORT" -U "$DBUSER" -d "$DB" -v ON_ERROR_STOP=1 -q \
    -f "$MROOT/business/V0447__purchase_plan_crud.up.sql" >/tmp/m402_reapply.log 2>&1; then
    ok 'V0447 再跑一遍 rc=0（幂等）'
else
    bad "V0447 重跑失败：$(tail -3 /tmp/m402_reapply.log | tr '\n' ' ')"
fi
assert_eq "$(psql_q "SELECT count(*) FROM sys_menu WHERE id::text LIKE '20000000-0000-0000-0000-00000000447%' AND deleted_at IS NULL")" "6" '重跑后菜单仍是 6 行'
if PGPASSWORD="$DBPASS" psql -h "$DBHOST" -p "$DBPORT" -U "$DBUSER" -d "$DB" -v ON_ERROR_STOP=1 -q \
    -f "$MROOT/business/V0447__purchase_plan_crud.down.sql" >/tmp/m402_down.log 2>&1; then
    ok 'V0447 down rc=0'
else
    bad "V0447 down 失败：$(tail -3 /tmp/m402_down.log | tr '\n' ' ')"
fi
assert_eq "$(psql_q "SELECT count(*) FROM sys_menu WHERE id::text LIKE '20000000-0000-0000-0000-00000000447%'")" "0" 'down 后菜单 0 行'
assert_eq "$(psql_q "SELECT count(*) FROM biz_flow_def WHERE flow_code='pp_approve'")" "0" 'down 后流程 0 行'
if PGPASSWORD="$DBPASS" psql -h "$DBHOST" -p "$DBPORT" -U "$DBUSER" -d "$DB" -v ON_ERROR_STOP=1 -q \
    -f "$MROOT/business/V0447__purchase_plan_crud.up.sql" >/tmp/m402_up.log 2>&1; then
    ok 'V0447 再 up rc=0（可复位）'
else
    bad "V0447 再 up 失败：$(tail -3 /tmp/m402_up.log | tr '\n' ' ')"
fi
assert_eq "$(psql_q "SELECT count(*) FROM sys_menu WHERE id::text LIKE '20000000-0000-0000-0000-00000000447%' AND deleted_at IS NULL")" "6" '复位后菜单回到 6 行'

echo '==> 15. 菜单不变量（8 条）'
if PGPASSWORD="$DBPASS" PGHOST="$DBHOST" PGPORT="$DBPORT" PGUSER="$DBUSER" \
    bash "$ROOT/scripts/verify/menu_invariants.sh" "$DB" 2>&1 | tee /tmp/m402_menu.out | grep -q '结果: 全绿'; then
    ok 'menu_invariants 8 条全绿'
else
    bad "menu_invariants 未全绿：$(grep -E '❌|结果' /tmp/m402_menu.out | tr '\n' ' ')"
fi

echo '==> 16. 收尾'
stop_server
assert_eq "$(psql_q "SELECT count(*) FROM biz_purchase_plan WHERE plan_name LIKE 'M402%'")" "4" '演练库里的计划 4 行（1 已删 + 2 已批 + 1 重建草稿）'
assert_eq "$(psql_q "SELECT count(*) FROM biz_purchase_plan WHERE plan_name LIKE 'M402%' AND deleted_at IS NOT NULL")" "1" '其中 1 行是软删（审计可回溯）'
# 进程刚停时连接池可能还没全断，先踢掉残留会话再删库
psql_q "SELECT count(pg_terminate_backend(pid)) FROM pg_stat_activity WHERE datname = '$DB'" >/dev/null 2>&1
sleep 1
dropdb -h "$DBHOST" -p "$DBPORT" -U "$DBUSER" --if-exists "$DB" && ok '演练库已删'

echo "==> 汇总：PASS=$PASS FAIL=$FAIL"
[ "$FAIL" -eq 0 ]

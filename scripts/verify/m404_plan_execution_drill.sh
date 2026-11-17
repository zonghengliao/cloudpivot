#!/usr/bin/env bash
# ============================================================
# M4-04 采购计划执行率跟踪 —— 真接口演练（隔离实例 + 真 curl）
#
# 卡：t_901d117b（M4-04【M4·采购】采购计划：执行率跟踪 —— 已下单 / 计划量）
#
# 跑什么（每一条都是卡面 / V0449 头注释里的一句原话）：
#   ① 空库首启 → V0449 的两个视图 / 菜单行 / 两行角色授权落位
#   ② 建计划（2 行 × 2 仓）→ 提交 → 审批人批 → APPROVED（真走 pp_approve）
#   ③ 执行率详情（未转申请）：计划量 14 / 已申请 0 / 已下单 0 / 率 0 / 两行 NOT_STARTED
#   ④ 执行率总览：关键字 + 年度筛选命中 1 张，数字与详情一致
#   ⑤ 转申请（一仓一单 2 张）→ 详情：已申请 14 / 已下单 0 / 率仍是 0（转申请≠下单）
#      下钻来源两条，`counted=false` 且原因「未转采购单」
#   ⑥ 申请审批 → 转采购单 → 详情：已下单 14 / 率 1.0 / DONE / 剩余 0 / 来源 counted=true 带单号
#   ⑦ 总览 `min_rate=100` 命中、「min_rate=200` 不命中（150% 才算超）
#   ⑧ 采购单作废 → 详情：已下单回落 0、已申请不变 14、原因「采购单已作废」（链断即不算）
#   ⑨ 参数闸：不带令牌 401 / 非法 uuid 400 / min_rate 越界 400
#   ⑩ 迁移幂等（up 重跑 rc=0）/ down 视图与菜单消失 / up 复位
#   ⑪ 死权限码闸（perm_menu_gate）+ 菜单不变量 8 条
#
# 用法（99 开发机）：
#   bash scripts/verify/m404_plan_execution_drill.sh
#   DBHOST=127.0.0.1 bash …            # 145 测试机上跑（走 loopback）
#   环境变量：DBUSER/DBPASS/DBOWNER/DBPORT/PORT 可覆盖
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
DB="${DB:-cp_m404_exec_drill}"
PORT="${PORT:-18100}"
BASE="http://127.0.0.1:$PORT/api"
MROOT="$ROOT/backend/app/migrations"
LOG=/root/m404-drill-server.log
PIDFILE=/root/m404-drill-server.pid
ADMIN_USER="${ADMIN_USER:-admin}"
ADMIN_PASS="${ADMIN_PASS:-Admin@123456}"
APPROVER_USER="m404_approver"
APPROVER_PASS="$ADMIN_PASS"

UNIT="d4040001-0000-4000-8000-000000000001"
WH_A="d4040001-0000-4000-8000-000000000002"
WH_B="d4040001-0000-4000-8000-000000000003"
MAT_A="d4040001-0000-4000-8000-000000000011"
MAT_B="d4040001-0000-4000-8000-000000000012"
DEPT="d4040001-0000-4000-8000-000000000021"
SUPPLIER="d4040001-0000-4000-8000-000000000031"
APPROVER_ID="d4040001-0000-4000-8000-0000000000a1"
MENU_ID="20000000-0000-0000-0000-000000004478"

PASS=0
FAIL=0
TOKEN=""
ATOKEN=""
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

call_as_approver() {
    local method="$1" path="$2" body="${3:-}" tmp
    tmp=$(mktemp)
    if [ -n "$body" ]; then
        REQ_CODE=$(curl -s -o "$tmp" -w '%{http_code}' -X "$method" \
            -H "Authorization: Bearer $ATOKEN" -H 'Content-Type: application/json' \
            --data-binary "$body" "$BASE$path")
    else
        REQ_CODE=$(curl -s -o "$tmp" -w '%{http_code}' -X "$method" \
            -H "Authorization: Bearer $ATOKEN" "$BASE$path")
    fi
    REQ_BODY=$(cat "$tmp")
    rm -f "$tmp"
}

stop_server() {
    local pids=""
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
    psql_q "SELECT count(pg_terminate_backend(pid)) FROM pg_stat_activity WHERE datname = '$DB'" >/dev/null 2>&1
    rm -f "$PIDFILE"
}
trap 'stop_server' EXIT

echo '==> 0. 建**空**演练库（迁移与种子由实例首启自己跑完：这才是空库首启的真实路径）'
export PGPASSWORD="$DBPASS"
stop_server
dropdb -h "$DBHOST" -p "$DBPORT" -U "$DBUSER" --if-exists "$DB" >/dev/null 2>&1
createdb -h "$DBHOST" -p "$DBPORT" -U "$DBUSER" -O "$DBOWNER" "$DB" || {
    echo 'FAIL: 建库失败' >&2; exit 2; }
echo '    空库已建（0 表）；迁移由实例启动时按版本号升序执行'

echo '==> 1. 起隔离实例（端口 '"$PORT"'，库 '"$DB"'）'
[ -x "$ROOT/backend/target/debug/cloudpivot" ] || (cd "$ROOT/backend" && cargo build -q) || {
    echo 'FAIL: 后端没编译出来' >&2; exit 2; }
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
    kill -0 "$(cat "$PIDFILE")" 2>/dev/null || { echo "FAIL: 实例退出了，看 $LOG" >&2; tail -30 "$LOG" >&2; exit 2; }
    sleep 1
done
[ "$(curl -s -o /dev/null -w '%{http_code}' "http://127.0.0.1:$PORT/health")" = "200" ] || {
    echo 'FAIL: 健康检查超时' >&2; tail -30 "$LOG" >&2; exit 2; }
ok '实例已起（/health 200）'

echo '==> 2. V0449 落位自检（两个视图 / 关键列 / 菜单 1 行 / 授权 2 行）'
assert_eq "$(psql_q "SELECT count(*) FROM information_schema.views WHERE table_schema='public' AND table_name IN ('v_purchase_plan_item_execution','v_purchase_plan_execution')")" "2" '两个执行率视图在位'
assert_eq "$(psql_q "SELECT count(*) FROM information_schema.columns WHERE table_name='v_purchase_plan_item_execution' AND column_name IN ('planned_qty','booked_qty','req_qty','ordered_qty','remaining_qty','exec_rate','exec_state')")" "7" '明细视图关键列齐全'
assert_eq "$(psql_q "SELECT count(*) FROM information_schema.columns WHERE table_name='v_purchase_plan_execution' AND column_name IN ('line_count','planned_qty','req_qty','ordered_qty','exec_rate','not_started_lines','partial_lines','done_lines','over_lines')")" "9" '单头视图关键列齐全'
assert_eq "$(psql_q "SELECT count(*) FROM sys_menu WHERE id='$MENU_ID'::uuid AND deleted_at IS NULL AND menu_type=3 AND auth_code='business:erp:purchase-plan:execution' AND parent_id='20000000-0000-0000-0000-000000004471'::uuid")" "1" '执行率按钮行 1 行且父级/码/类型对'
assert_eq "$(psql_q "SELECT count(*) FROM sys_role_menu WHERE menu_id='$MENU_ID'::uuid")" "2" '授给采购经理 + 财务经理（2 行）'
assert_eq "$(psql_q "SELECT count(*) FROM sys_menu WHERE id::text LIKE '20000000-0000-0000-0000-00000000447%' AND deleted_at IS NULL")" "8" '采购计划菜单 8 行（1 页面 + 7 按钮）'
assert_eq "$(psql_q "SELECT count(*) FROM biz_purchase_plan WHERE plan_name LIKE 'V0449%'")" "0" '迁移自检的探针行已清干净（0 残留）'

echo '==> 3. 夹具（基本单位 / 两个仓库 / 两个物料 / 一个部门 / 一个供应商 / 一个审批人）'
psql_x "INSERT INTO biz_unit (id, unit_code, unit_name, is_base, rate, status)
        VALUES ('$UNIT', 'M404-U', 'M404 基本单位', 1, 1, 1) ON CONFLICT (id) DO NOTHING;
        INSERT INTO biz_warehouse (id, warehouse_code, warehouse_name, warehouse_path, is_leaf, level, status)
        VALUES ('$WH_A', 'M404-WH-A', 'M404 仓库 A', '/M404-WH-A', 1, 1, 1),
               ('$WH_B', 'M404-WH-B', 'M404 仓库 B', '/M404-WH-B', 1, 1, 1)
        ON CONFLICT (id) DO NOTHING;
        INSERT INTO biz_material (id, material_code, material_name, unit_id, material_type, status)
        VALUES ('$MAT_A', 'M404-MAT-A', 'M404 物料 A', '$UNIT', 'RAW', 1),
               ('$MAT_B', 'M404-MAT-B', 'M404 物料 B', '$UNIT', 'RAW', 1)
        ON CONFLICT (id) DO NOTHING;
        INSERT INTO biz_supplier (id, supplier_code, supplier_name, status)
        VALUES ('$SUPPLIER', 'M404-SUP-A', 'M404 供应商 A', 1) ON CONFLICT (id) DO NOTHING;
        INSERT INTO sys_dept (id, dept_code, dept_name, status)
        VALUES ('$DEPT', 'M404-DEPT', 'M404 采购部', 1) ON CONFLICT (id) DO NOTHING;"
# 审批人：复制 admin 的口令哈希 + 给采购经理 / 财务经理角色（职责分离：不能自己批自己的单）
psql_x "INSERT INTO sys_user (id, username, real_name, password, status, must_change_password)
        SELECT '$APPROVER_ID'::uuid, '$APPROVER_USER', 'M404 审批人', password, 1, false
          FROM sys_user WHERE username = '$ADMIN_USER'
        ON CONFLICT (id) DO NOTHING;
        INSERT INTO sys_user_role (user_id, role_id)
        SELECT '$APPROVER_ID'::uuid, r.id FROM sys_role r
         WHERE r.role_code IN ('PURCHASE_MANAGER', 'FINANCE_MANAGER') AND r.deleted_at IS NULL
        ON CONFLICT DO NOTHING;"
assert_eq "$(psql_q "SELECT count(*) FROM sys_user WHERE username='$APPROVER_USER'")" "1" '审批人夹具已建'

echo '==> 4. 登录 admin / 审批人'
tmp=$(mktemp)
REQ_CODE=$(curl -s -o "$tmp" -w '%{http_code}' -X POST -H 'Content-Type: application/json' \
    --data-binary "{\"username\":\"$ADMIN_USER\",\"password\":\"$ADMIN_PASS\"}" "$BASE/system/auth/login")
REQ_BODY=$(cat "$tmp"); rm -f "$tmp"
TOKEN=$(jqv '.data.access_token')
[ -n "$TOKEN" ] && [ "$TOKEN" != "null" ] || { echo "FAIL: 登录失败 $REQ_BODY" >&2; exit 2; }
ok '登录拿到令牌'
if [ "$(jqv '.data.must_change_password')" = "true" ]; then
    NEW_PASS="M404_${ADMIN_PASS}_new"
    curl -s -o /dev/null -X PUT -H "Authorization: Bearer $TOKEN" -H 'Content-Type: application/json' \
        --data-binary "{\"old_password\":\"$ADMIN_PASS\",\"new_password\":\"$NEW_PASS\"}" "$BASE/system/auth/password"
    ADMIN_PASS="$NEW_PASS"
    tmp=$(mktemp)
    curl -s -o "$tmp" -X POST -H 'Content-Type: application/json' \
        --data-binary "{\"username\":\"$ADMIN_USER\",\"password\":\"$ADMIN_PASS\"}" "$BASE/system/auth/login"
    TOKEN=$(jq -r '.data.access_token' <"$tmp"); rm -f "$tmp"
    ok '首登强制改密后重新登录'
fi
APPROVER_PASS="Admin@123456"
tmp=$(mktemp)
curl -s -o "$tmp" -X POST -H 'Content-Type: application/json' \
    --data-binary "{\"username\":\"$APPROVER_USER\",\"password\":\"$APPROVER_PASS\"}" "$BASE/system/auth/login"
ATOKEN=$(jq -r '.data.access_token // empty' <"$tmp"); rm -f "$tmp"
if [ -n "$ATOKEN" ]; then ok '审批人登录成功'; else bad '审批人登录失败（后面走审批 / 转单的用例会降级）'; fi

echo '==> 5. 建计划（2 行 × 2 仓）+ 提交 + 审批到 APPROVED'
BODY=$(cat <<JSON
{"plan_name":"M404 执行率演练计划","plan_type":"ANNUAL","plan_year":2031,"dept_id":"$DEPT",
 "remark":"M404 演练",
 "items":[
   {"material_id":"$MAT_A","warehouse_id":"$WH_A","planned_qty":"10","planned_price":"2.5"},
   {"material_id":"$MAT_B","warehouse_id":"$WH_B","planned_qty":"4","planned_price":"100"}]}
JSON
)
call POST /business/erp/purchase-plan "$BODY"
assert_eq "$REQ_CODE" "200" '建计划 200'
PLAN_ID=$(jqv '.data.plan.id')
assert_eq "$(jqv '.data.plan.status')" "DRAFT" '计划是草稿'
call POST "/business/erp/purchase-plan/$PLAN_ID/submit" '{}'
assert_eq "$REQ_CODE" "200" '提交 200'
INSTANCE=$(jqv '.data.plan.approval_instance_id')
if [ -n "$ATOKEN" ]; then
    for _ in 1 2 3; do
        TASK=$(psql_q "SELECT id FROM biz_flow_task WHERE instance_id='$INSTANCE'::uuid AND status='PENDING' ORDER BY created_at LIMIT 1")
        [ -n "$TASK" ] || break
        call_as_approver POST "/business/flow/task/$TASK/approve" '{"comment":"M404 演练同意"}'
        [ "$REQ_CODE" = "200" ] || bad "审批接口返回 $REQ_CODE：$REQ_BODY"
    done
fi
assert_eq "$(psql_q "SELECT status FROM biz_purchase_plan WHERE id='$PLAN_ID'::uuid")" "APPROVED" '审批通过后计划 APPROVED'

echo '==> 6. 执行率详情（未转申请）：已申请 0 / 已下单 0 / 率 0 / 两行未开工'
call GET "/business/erp/purchase-plan/$PLAN_ID/execution"
assert_eq "$REQ_CODE" "200" '执行率详情 200'
assert_eq "$(jqv '.data.plan.line_count')" "2" '明细 2 行'
assert_num "$(jqv '.data.plan.planned_qty')" "14" '计划量 = 10 + 4'
assert_num "$(jqv '.data.plan.req_qty')" "0" '已申请量 0'
assert_num "$(jqv '.data.plan.ordered_qty')" "0" '已下单量 0'
assert_num "$(jqv '.data.plan.remaining_qty')" "14" '剩余可下单 = 14'
assert_num "$(jqv '.data.plan.convertible_qty')" "14" '还能转申请 = 14'
assert_num "$(jqv '.data.plan.exec_rate')" "0" '执行率 0'
assert_num "$(jqv '.data.plan.exec_rate_pct')" "0" '执行率百分比 0.00'
assert_eq "$(jqv '.data.plan.exec_state')" "NOT_STARTED" '整单状态未开工'
assert_eq "$(jqv '.data.plan.not_started_lines')" "2" '两行都未开工'
assert_eq "$(jqv '.data.items[0].sources | length')" "0" '未转申请：第 1 行没有来源单据'
assert_eq "$(jqv '.data.items[0].exec_state_label')" "未开工" '行状态中文「未开工」'

echo '==> 7. 执行率总览（关键字 / 年度 / 状态筛选）'
call GET "/business/erp/purchase-plan/execution/page?keyword=M404%20%E6%89%A7%E8%A1%8C%E7%8E%87&plan_year=2031"
assert_eq "$REQ_CODE" "200" '总览 200'
assert_eq "$(jqv '.data.total')" "1" '关键字 + 年度命中 1 张'
assert_eq "$(jqv '.data.list[0].plan_id')" "$PLAN_ID" '命中的就是刚才那张'
assert_num "$(jqv '.data.list[0].planned_qty')" "14" '总览计划量与详情一致'
assert_eq "$(jqv '.data.list[0].exec_state')" "NOT_STARTED" '总览整单状态未开工'
call GET "/business/erp/purchase-plan/execution/page?keyword=M404&plan_year=2099"
assert_eq "$(jqv '.data.total')" "0" '换年度就查不到（2059 无计划）'

echo '==> 8. 转申请（一仓一单）→ 已申请 14 / 已下单仍是 0 / 来源「未转采购单」'
call POST "/business/erp/purchase-plan/$PLAN_ID/to-requisition" '{"tax_rate":"13","remark":"M404 演练转申请"}'
assert_eq "$REQ_CODE" "200" '转申请 200'
assert_eq "$(jqv '.data.requisitions | length')" "2" '一仓一单：2 张申请单'
REQ1=$(jqv '.data.requisitions[0].requisition_id')
REQ2=$(jqv '.data.requisitions[1].requisition_id')
REQ1_NO=$(jqv '.data.requisitions[0].req_no')

call GET "/business/erp/purchase-plan/$PLAN_ID/execution"
assert_num "$(jqv '.data.plan.req_qty')" "14" '已申请量 = 14（按单据现算）'
assert_num "$(jqv '.data.plan.booked_qty')" "14" '回写列 executed_qty 也是 14（与单据链不分叉）'
assert_num "$(jqv '.data.plan.ordered_qty')" "0" '**转申请≠下单**：已下单仍是 0'
assert_num "$(jqv '.data.plan.exec_rate')" "0" '执行率仍 0'
assert_num "$(jqv '.data.plan.req_rate')" "1" '已申请率 100%'
assert_eq "$(jqv '.data.plan.exec_state')" "NOT_STARTED" '只转申请：仍是未开工'
assert_num "$(jqv '.data.plan.convertible_qty')" "0" '全转完：可转 0'
assert_eq "$(jqv '.data.items[0].sources | length')" "1" '第 1 行有 1 条来源'
assert_eq "$(jqv '.data.items[0].sources[0].counted')" "false" '没转采购单 → 不计入已下单'
assert_eq "$(jqv '.data.items[0].sources[0].reason')" "未转采购单" '来源原因「未转采购单」'
assert_eq "$(jqv '.data.items[0].sources[0].req_no')" "$REQ1_NO" '来源带申请单号'

echo '==> 9. 申请审批 → 转采购单 → 已下单 14 / 率 1.0 / DONE / 来源 counted=true'
if [ -n "$ATOKEN" ]; then
    for R in "$REQ1" "$REQ2"; do
        call POST "/business/erp/purchase-requisition/$R/submit" '{}'
        [ "$REQ_CODE" = "200" ] || bad "申请 $R 提交失败：$REQ_CODE $REQ_BODY"
        INST=$(psql_q "SELECT approval_instance_id FROM biz_purchase_requisition WHERE id='$R'::uuid")
        for _ in 1 2 3; do
            TASK=$(psql_q "SELECT id FROM biz_flow_task WHERE instance_id='$INST'::uuid AND status='PENDING' ORDER BY created_at LIMIT 1")
            [ -n "$TASK" ] || break
            call_as_approver POST "/business/flow/task/$TASK/approve" '{"comment":"M404 演练同意申请"}'
        done
        assert_eq "$(psql_q "SELECT status FROM biz_purchase_requisition WHERE id='$R'::uuid")" "APPROVED" "申请 $R 已审批通过"
    done
fi
call POST "/business/erp/purchase-requisition/$REQ1/to-po" \
    "{\"requisition_ids\":[\"$REQ1\",\"$REQ2\"],\"supplier_id\":\"$SUPPLIER\",\"warehouse_id\":\"$WH_A\"}"
assert_eq "$REQ_CODE" "200" '合并转采购单 200'
PO_NO=$(jqv '.data.header.po_no')
PO_ID=$(jqv '.data.header.po_id')
assert_contains "$PO_NO" "PO-" '采购单号形状 PO-…'

call GET "/business/erp/purchase-plan/$PLAN_ID/execution"
assert_num "$(jqv '.data.plan.ordered_qty')" "14" '已下单量 = 14'
assert_num "$(jqv '.data.plan.remaining_qty')" "0" '剩余可下单 0'
assert_num "$(jqv '.data.plan.exec_rate')" "1" '执行率 100%'
assert_num "$(jqv '.data.plan.exec_rate_pct')" "100" '执行率百分比 100.00'
assert_eq "$(jqv '.data.plan.exec_state')" "DONE" '整单已下满'
assert_eq "$(jqv '.data.plan.done_lines')" "2" '两行都下满'
assert_eq "$(jqv '.data.plan.over_lines')" "0" '没有超计划行'
assert_eq "$(jqv '.data.items[0].sources[0].counted')" "true" '已转单 → 计入已下单'
assert_eq "$(jqv '.data.items[0].sources[0].reason')" "已下单" '来源原因「已下单」'
assert_eq "$(jqv '.data.items[0].sources[0].po_no')" "$PO_NO" '来源带采购单号'
assert_eq "$(jqv '.data.items[0].over_plan')" "false" '恰好下满不算超计划'

echo '==> 10. 总览执行率下限筛选（min_rate=100 命中 / 200 不命中）'
call GET "/business/erp/purchase-plan/execution/page?keyword=M404&plan_year=2031&min_rate=100"
assert_eq "$REQ_CODE" "200" 'min_rate=100 查询 200'
assert_eq "$(jqv '.data.total')" "1" 'min_rate=100 命中已下满的计划'
call GET "/business/erp/purchase-plan/execution/page?keyword=M404&plan_year=2031&min_rate=200"
assert_eq "$(jqv '.data.total')" "0" 'min_rate=200 不命中 100% 的计划'
call GET "/business/erp/purchase-plan/execution/page?min_rate=9999"
assert_eq "$REQ_CODE" "400" 'min_rate 越界 = 400'

echo '==> 11. 采购单作废 → 已下单回落 0、已申请不变、原因「采购单已作废」'
call PATCH "/business/erp/purchase-order/$PO_ID/status" '{"status":"CANCELLED","reason":"M404 演练作废"}'
assert_eq "$REQ_CODE" "200" '作废采购单 200'
call GET "/business/erp/purchase-plan/$PLAN_ID/execution"
assert_num "$(jqv '.data.plan.ordered_qty')" "0" '作废后已下单回落 0'
assert_num "$(jqv '.data.plan.req_qty')" "14" '已申请不变（申请单还在）'
assert_num "$(jqv '.data.plan.remaining_qty')" "14" '剩余回到 14'
assert_eq "$(jqv '.data.plan.exec_state')" "NOT_STARTED" '整单回到未开工'
assert_eq "$(jqv '.data.items[0].sources[0].counted')" "false" '作废的采购单不计入'
assert_eq "$(jqv '.data.items[0].sources[0].reason')" "采购单已作废" '来源原因「采购单已作废」'
assert_eq "$(jqv '.data.items[0].sources[0].po_status')" "CANCELLED" '来源带采购单状态'

echo '==> 12. 参数闸 / 未授权'
NOPERM=$(curl -s -o /dev/null -w '%{http_code}' "$BASE/business/erp/purchase-plan/$PLAN_ID/execution")
assert_eq "$NOPERM" "401" '不带令牌 = 401'
call GET "/business/erp/purchase-plan/not-a-uuid/execution"
assert_eq "$REQ_CODE" "400" '非法 uuid = 400'
call GET "/business/erp/purchase-plan/00000000-0000-0000-0000-0000000000ff/execution"
assert_eq "$REQ_CODE" "404" '不存在的计划 = 404'
call GET "/business/erp/purchase-plan/execution/page?status=CLOSED"
assert_eq "$REQ_CODE" "400" '状态闭集外的值 = 400'

echo '==> 13. 库内落点（迁移往返之前查）'
assert_eq "$(psql_q "SELECT count(*) FROM v_purchase_plan_execution WHERE plan_id='$PLAN_ID'::uuid")" "1" '总览视图里有这张计划（1 行）'
assert_eq "$(psql_q "SELECT count(*) FROM v_purchase_plan_item_execution WHERE plan_id='$PLAN_ID'::uuid")" "2" '明细视图 2 行'
assert_num "$(psql_q "SELECT req_qty FROM v_purchase_plan_item_execution WHERE plan_id='$PLAN_ID'::uuid ORDER BY line_no LIMIT 1")" "10" '明细视图第 1 行已申请 = 10'
assert_eq "$(psql_q "SELECT exec_state FROM v_purchase_plan_execution WHERE plan_id='$PLAN_ID'::uuid")" "NOT_STARTED" '视图里整单状态 = 未开工（作废后）'

echo '==> 14. 迁移幂等 + down/up 往返'
if PGPASSWORD="$DBPASS" psql -h "$DBHOST" -p "$DBPORT" -U "$DBUSER" -d "$DB" -v ON_ERROR_STOP=1 -q \
    -f "$MROOT/business/V0449__purchase_plan_execution.up.sql" >/tmp/m404_reapply.log 2>&1; then
    ok 'V0449 再跑一遍 rc=0（幂等）'
else
    bad "V0449 重跑失败：$(tail -3 /tmp/m404_reapply.log | tr '\n' ' ')"
fi
assert_eq "$(psql_q "SELECT count(*) FROM sys_menu WHERE id='$MENU_ID'::uuid AND deleted_at IS NULL")" "1" '重跑后按钮仍 1 行'
if PGPASSWORD="$DBPASS" psql -h "$DBHOST" -p "$DBPORT" -U "$DBUSER" -d "$DB" -v ON_ERROR_STOP=1 -q \
    -f "$MROOT/business/V0449__purchase_plan_execution.down.sql" >/tmp/m404_down.log 2>&1; then
    ok 'V0449 down rc=0'
else
    bad "V0449 down 失败：$(tail -3 /tmp/m404_down.log | tr '\n' ' ')"
fi
assert_eq "$(psql_q "SELECT count(*) FROM information_schema.views WHERE table_schema='public' AND table_name IN ('v_purchase_plan_item_execution','v_purchase_plan_execution')")" "0" 'down 后两个视图没了'
assert_eq "$(psql_q "SELECT count(*) FROM sys_menu WHERE id='$MENU_ID'::uuid")" "0" 'down 后按钮没了'
assert_eq "$(psql_q "SELECT count(*) FROM sys_role_menu WHERE menu_id='$MENU_ID'::uuid")" "0" 'down 后授权没了'
assert_eq "$(psql_q "SELECT count(*) FROM biz_purchase_plan WHERE id='$PLAN_ID'::uuid")" "1" 'down 不删业务单（计划还在）'
if PGPASSWORD="$DBPASS" psql -h "$DBHOST" -p "$DBPORT" -U "$DBUSER" -d "$DB" -v ON_ERROR_STOP=1 -q \
    -f "$MROOT/business/V0449__purchase_plan_execution.up.sql" >/tmp/m404_up.log 2>&1; then
    ok 'V0449 再 up rc=0（可复位）'
else
    bad "V0449 再 up 失败：$(tail -3 /tmp/m404_up.log | tr '\n' ' ')"
fi
assert_eq "$(psql_q "SELECT count(*) FROM information_schema.views WHERE table_schema='public' AND table_name IN ('v_purchase_plan_item_execution','v_purchase_plan_execution')")" "2" '复位后两个视图回来'
assert_eq "$(psql_q "SELECT count(*) FROM sys_menu WHERE id='$MENU_ID'::uuid AND deleted_at IS NULL")" "1" '复位后按钮回来'
assert_eq "$(psql_q "SELECT count(*) FROM sys_role_menu WHERE menu_id='$MENU_ID'::uuid")" "2" '复位后授权回来'

echo '==> 15. 死权限码闸（perm_menu_gate）+ 菜单不变量（8 条）'
if NIK_DATABASE_HOST="$DBHOST" NIK_DATABASE_PORT="$DBPORT" NIK_DATABASE_USER="$DBUSER" \
   NIK_DATABASE_PASSWORD="$DBPASS" NIK_DATABASE_NAME="$DB" \
   python3 "$ROOT/scripts/verify/perm_menu_gate.py" 2>&1 | tee /tmp/m404_perm.out | grep -q 'missing_in_menu = 0'; then
    ok 'perm_menu_gate：missing_in_menu = 0（含新增的 execution 码）'
else
    bad "perm_menu_gate 判红：$(grep -E 'missing_in_menu|·' /tmp/m404_perm.out | head -5 | tr '\n' ' ')"
fi
if PGPASSWORD="$DBPASS" PGHOST="$DBHOST" PGPORT="$DBPORT" PGUSER="$DBUSER" \
    bash "$ROOT/scripts/verify/menu_invariants.sh" "$DB" 2>&1 | tee /tmp/m404_menu.out | grep -q '结果: 全绿'; then
    ok 'menu_invariants 8 条全绿'
else
    bad "menu_invariants 未全绿：$(grep -E '❌|结果' /tmp/m404_menu.out | tr '\n' ' ')"
fi

echo '==> 16. 收尾'
stop_server
psql_q "SELECT count(pg_terminate_backend(pid)) FROM pg_stat_activity WHERE datname = '$DB'" >/dev/null 2>&1
sleep 1
dropdb -h "$DBHOST" -p "$DBPORT" -U "$DBUSER" --if-exists "$DB" && ok '演练库已删'

echo "==> 汇总：PASS=$PASS FAIL=$FAIL"
[ "$FAIL" -eq 0 ]

#!/usr/bin/env bash
# ============================================================
# M4-06 采购计划**前端页面**演练（真库 + 真后端 + 真浏览器）
#
# 卡：t_62aa0328（M4-06【M4·采购】采购计划：前端页面 —— 录入 + 执行率）
#
# 为什么要有这个脚本
# ------------------
# 前端卡最怕「typecheck 过了、build 过了，但一点就是白的」。本脚本把
# 「页面能开 → 表单能建单 → 坏输入被拦 → 执行率数字对得上 → 下钻能看清来源」
# 这五件事放在**真栈**上跑一遍：临时库（145）+ profile=test 后端（18101）+
# vite dev（5190）+ Playwright/Chromium。
#
# 跑什么
# ------
#   0. 建空演练库（cp_m406_ui）→ 实例首启自己跑完 381 条迁移
#   1. 夹具：1 单位 / 2 仓库 / 2 物料 / 1 部门 / 1 供应商 / 1 审批人（职责分离）
#   2. 真接口造两张计划：
#        · 「M406 已下满计划」→ 提交 → 审批 → 转申请（一仓一单 2 张）
#          → 两张申请各自审批 → 合并转采购单 → 执行率 DONE / 100.00%
#        · 「M406 草稿计划」→ 停在 DRAFT（给录入 Tab 当列表行）
#   3. 起 vite dev（BACKEND_ORIGIN 指 18101），确认首页 200
#   4. 跑 `frontend/e2e/m406-purchase-plan.spec.ts`（5 条用例，真点真断言）
#   5. 汇总 PASS/FAIL 并落证据到 docs/evidence/
#
# 用法（99 开发机；145 上跑加 DBHOST=127.0.0.1）
#   bash scripts/verify/m406_purchase_plan_ui_drill.sh
#   SKIP_BROWSER=1 bash scripts/verify/m406_purchase_plan_ui_drill.sh   # 只造数据，不跑浏览器
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
DB="${DB:-cp_m406_ui}"
PORT="${PORT:-18101}"
FE_PORT="${FE_PORT:-5190}"
BASE="http://127.0.0.1:$PORT/api"
LOG=/root/m406-ui-server.log
PIDFILE=/root/m406-ui-server.pid
FE_LOG=/root/m406-ui-vite.log
FE_PIDFILE=/root/m406-ui-vite.pid
EVID="$(date +%Y-%m-%d)"
EVID_DIR="$ROOT/docs/evidence/$EVID-m406-purchase-plan-ui"
mkdir -p "$EVID_DIR"
ADMIN_USER="${ADMIN_USER:-admin}"
ADMIN_PASS="${ADMIN_PASS:-Admin@123456}"
APPROVER_USER=m406_approver

UNIT="d4060001-0000-4000-8000-000000000001"
WH_A="d4060001-0000-4000-8000-000000000002"
WH_B="d4060001-0000-4000-8000-000000000003"
MAT_A="d4060001-0000-4000-8000-000000000011"
MAT_B="d4060001-0000-4000-8000-000000000012"
DEPT="d4060001-0000-4000-8000-000000000021"
SUPPLIER="d4060001-0000-4000-8000-000000000031"
APPROVER_ID="d4060001-0000-4000-8000-0000000000a1"

PASS=0
FAIL=0
TOKEN=""
ATOKEN=""
REQ_CODE=0
REQ_BODY=""

ok() { PASS=$((PASS + 1)); printf '[PASS] %s\n' "$1"; }
bad() { FAIL=$((FAIL + 1)); printf '[FAIL] %s\n' "$1"; }
assert_eq() { if [ "$1" = "$2" ]; then ok "$3（$2）"; else bad "$3：期望 [$1] 实际 [$2]"; fi; }
assert_num() {
    if awk -v a="$1" -v b="$2" 'BEGIN { if (a == "" || b == "" || b == "null") exit 1; exit !((a + 0) == (b + 0)) }'; then
        ok "$3（$2）"
    else
        bad "$3：期望 [$1] 实际 [$2]"
    fi
}
assert_contains() {
    case "$1" in
        *"$2"*) ok "$3" ;;
        *) bad "$3：文本里没有 [$2]，实际 [$(printf '%s' "$1" | head -c 300)]" ;;
    esac
}

psql_q() { PGPASSWORD="$DBPASS" psql -h "$DBHOST" -p "$DBPORT" -U "$DBUSER" -d "$DB" -Atc "$1" 2>&1 | head -1; }
psql_x() { PGPASSWORD="$DBPASS" psql -h "$DBHOST" -p "$DBPORT" -U "$DBUSER" -d "$DB" -v ON_ERROR_STOP=1 -q -c "$1" 2>&1 | head -5; }

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

stop_all() {
    local pids="" p
    for port in "$PORT" "$FE_PORT"; do
        pids=$(ss -ltnp 2>/dev/null | grep ":$port " | grep -oE 'pid=[0-9]+' | cut -d= -f2 | tr '\n' ' ')
        for p in $pids; do [ -n "$p" ] && kill "$p" 2>/dev/null; done
    done
    for f in "$PIDFILE" "$FE_PIDFILE"; do
        if [ -f "$f" ]; then p=$(cat "$f"); [ -n "$p" ] && kill "$p" 2>/dev/null; fi
    done
    for _ in $(seq 1 20); do
        ss -ltn 2>/dev/null | grep -qE ":($PORT|$FE_PORT) " || break
        sleep 1
    done
    for port in "$PORT" "$FE_PORT"; do
        pids=$(ss -ltnp 2>/dev/null | grep ":$port " | grep -oE 'pid=[0-9]+' | cut -d= -f2 | tr '\n' ' ')
        for p in $pids; do [ -n "$p" ] && kill -9 "$p" 2>/dev/null; done
    done
    psql_q "SELECT count(pg_terminate_backend(pid)) FROM pg_stat_activity WHERE datname = '$DB'" >/dev/null 2>&1
    rm -f "$PIDFILE" "$FE_PIDFILE"
}
trap 'stop_all' EXIT

echo '==> 0. 建**空**演练库（迁移由实例首启按版本号升序执行：这才是空库首启的真实路径）'
export PGPASSWORD="$DBPASS"
stop_all
dropdb -h "$DBHOST" -p "$DBPORT" -U "$DBUSER" --if-exists "$DB" >/dev/null 2>&1
createdb -h "$DBHOST" -p "$DBPORT" -U "$DBUSER" -O "$DBOWNER" "$DB" || { echo 'FAIL: 建库失败' >&2; exit 2; }
echo "    空库 $DB 已建"

echo "==> 1. 起隔离后端（127.0.0.1:$PORT → $DBHOST/$DB）"
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
for _ in $(seq 1 240); do
    if [ "$(curl -s -o /dev/null -w '%{http_code}' "http://127.0.0.1:$PORT/health" || true)" = "200" ]; then break; fi
    kill -0 "$(cat "$PIDFILE")" 2>/dev/null || { echo "FAIL: 实例退出了，看 $LOG" >&2; tail -40 "$LOG" >&2; exit 2; }
    sleep 1
done
[ "$(curl -s -o /dev/null -w '%{http_code}' "http://127.0.0.1:$PORT/health")" = "200" ] || {
    echo 'FAIL: 健康检查超时' >&2; tail -40 "$LOG" >&2; exit 2; }
ok '后端实例已起（/health 200）'
assert_eq "$(psql_q "SELECT count(*) FROM sys_menu WHERE path='/business/erp/purchase-plan' AND deleted_at IS NULL")" "1" 'V0447 的菜单页行在位（路由要对得上它）'

echo '==> 2. 夹具（主数据 + 审批人；审批人 != 制单人，pp_approve 带职责分离内控）'
psql_x "UPDATE sys_user SET must_change_password = false WHERE username = '$ADMIN_USER';
        INSERT INTO biz_unit (id, unit_code, unit_name, is_base, rate, status)
        VALUES ('$UNIT', 'M406-U', 'M406 基本单位', 1, 1, 1) ON CONFLICT (id) DO NOTHING;
        INSERT INTO biz_warehouse (id, warehouse_code, warehouse_name, warehouse_path, is_leaf, level, status)
        VALUES ('$WH_A', 'M406-WH-A', 'M406 仓库 A', '/M406-WH-A', 1, 1, 1),
               ('$WH_B', 'M406-WH-B', 'M406 仓库 B', '/M406-WH-B', 1, 1, 1)
        ON CONFLICT (id) DO NOTHING;
        INSERT INTO biz_material (id, material_code, material_name, unit_id, material_type, status)
        VALUES ('$MAT_A', 'M406-MAT-A', 'M406 物料 A', '$UNIT', 'RAW', 1),
               ('$MAT_B', 'M406-MAT-B', 'M406 物料 B', '$UNIT', 'RAW', 1)
        ON CONFLICT (id) DO NOTHING;
        INSERT INTO biz_supplier (id, supplier_code, supplier_name, status)
        VALUES ('$SUPPLIER', 'M406-SUP-A', 'M406 供应商 A', 1) ON CONFLICT (id) DO NOTHING;
        INSERT INTO sys_dept (id, dept_code, dept_name, status)
        VALUES ('$DEPT', 'M406-DEPT', 'M406 采购部', 1) ON CONFLICT (id) DO NOTHING;"
psql_x "INSERT INTO sys_user (id, username, real_name, password, status, must_change_password)
        SELECT '$APPROVER_ID'::uuid, '$APPROVER_USER', 'M406 审批人', password, 1, false
          FROM sys_user WHERE username = '$ADMIN_USER'
        ON CONFLICT (id) DO NOTHING;
        INSERT INTO sys_user_role (user_id, role_id)
        SELECT '$APPROVER_ID'::uuid, r.id FROM sys_role r
         WHERE r.role_code IN ('PURCHASE_MANAGER', 'FINANCE_MANAGER') AND r.deleted_at IS NULL
        ON CONFLICT DO NOTHING;"
assert_eq "$(psql_q "SELECT count(*) FROM sys_user WHERE username='$APPROVER_USER'")" "1" '审批人夹具已建'

echo '==> 3. 登录 admin / 审批人'
login() {
    local user="$1" pass="$2" tmp
    tmp=$(mktemp)
    curl -s -o "$tmp" -X POST -H 'Content-Type: application/json' \
        --data-binary "{\"username\":\"$user\",\"password\":\"$pass\"}" "$BASE/system/auth/login"
    jq -r '.data.access_token // empty' <"$tmp"
    rm -f "$tmp"
}
TOKEN=$(login "$ADMIN_USER" "$ADMIN_PASS")
[ -n "$TOKEN" ] || { echo 'FAIL: admin 登录失败' >&2; exit 2; }
ok 'admin 登录拿到令牌'
ATOKEN=$(login "$APPROVER_USER" "$ADMIN_PASS")
[ -n "$ATOKEN" ] && ok '审批人登录拿到令牌' || bad '审批人登录失败（审批/转单会降级）'

echo '==> 4. 造「已下满」计划：建单 → 提交 → 审批 → 转申请 → 申请审批 → 合并转采购单'
BODY=$(cat <<JSON
{"plan_name":"M406 已下满计划","plan_type":"ANNUAL","plan_year":2032,"dept_id":"$DEPT",
 "remark":"M4-06 前端演练夹具",
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
assert_eq "$REQ_CODE" "200" '提交审批 200'
INSTANCE=$(jqv '.data.plan.approval_instance_id')
for _ in 1 2 3; do
    TASK=$(psql_q "SELECT id FROM biz_flow_task WHERE instance_id='$INSTANCE'::uuid AND status='PENDING' ORDER BY created_at LIMIT 1")
    [ -n "$TASK" ] || break
    call_as_approver POST "/business/flow/task/$TASK/approve" '{"comment":"M406 前端演练同意"}'
    [ "$REQ_CODE" = "200" ] || bad "审批接口返回 $REQ_CODE：$REQ_BODY"
done
assert_eq "$(psql_q "SELECT status FROM biz_purchase_plan WHERE id='$PLAN_ID'::uuid")" "APPROVED" '审批通过 → APPROVED（页面「转采购申请」按钮的准入条件）'

call POST "/business/erp/purchase-plan/$PLAN_ID/to-requisition" '{"tax_rate":"13","remark":"M4-06 前端演练转申请"}'
assert_eq "$REQ_CODE" "200" '转申请 200'
assert_eq "$(jqv '.data.requisitions | length')" "2" '一仓一单：2 张申请单'
REQ1=$(jqv '.data.requisitions[0].requisition_id')
REQ2=$(jqv '.data.requisitions[1].requisition_id')
for R in "$REQ1" "$REQ2"; do
    call POST "/business/erp/purchase-requisition/$R/submit" '{}'
    [ "$REQ_CODE" = "200" ] || bad "申请 $R 提交失败：$REQ_CODE"
    INST=$(psql_q "SELECT approval_instance_id FROM biz_purchase_requisition WHERE id='$R'::uuid")
    for _ in 1 2 3; do
        TASK=$(psql_q "SELECT id FROM biz_flow_task WHERE instance_id='$INST'::uuid AND status='PENDING' ORDER BY created_at LIMIT 1")
        [ -n "$TASK" ] || break
        call_as_approver POST "/business/flow/task/$TASK/approve" '{"comment":"M406 前端演练同意申请"}'
    done
done
call POST "/business/erp/purchase-requisition/$REQ1/to-po" \
    "{\"requisition_ids\":[\"$REQ1\",\"$REQ2\"],\"supplier_id\":\"$SUPPLIER\",\"warehouse_id\":\"$WH_A\"}"
assert_eq "$REQ_CODE" "200" '合并转采购单 200'
PO_NO=$(jqv '.data.header.po_no')
assert_contains "$PO_NO" "PO-" '采购单号形状 PO-…'

call GET "/business/erp/purchase-plan/$PLAN_ID/execution"
assert_num "$(jqv '.data.plan.planned_qty')" "14" '执行率：计划量 14'
assert_num "$(jqv '.data.plan.ordered_qty')" "14" '执行率：已下单 14'
assert_num "$(jqv '.data.plan.exec_rate_pct')" "100" '执行率：100.00'
assert_eq "$(jqv '.data.plan.exec_state')" "DONE" '执行率：已下满'
assert_eq "$(jqv '.data.items[0].sources[0].reason')" "已下单" '来源原因「已下单」'

echo '==> 5. 造一张草稿计划（录入 Tab 的列表行 / 编辑入口）'
BODY=$(cat <<JSON
{"plan_name":"M406 草稿计划","plan_type":"MONTHLY","plan_year":2032,"plan_month":3,"dept_id":"$DEPT",
 "items":[{"material_id":"$MAT_A","warehouse_id":"$WH_B","planned_qty":"6","planned_price":"9.5"}]}
JSON
)
call POST /business/erp/purchase-plan "$BODY"
assert_eq "$REQ_CODE" "200" '草稿计划已建'
DRAFT_ID=$(jqv '.data.plan.id')
assert_eq "$(jqv '.data.plan.plan_type')" "MONTHLY" '草稿是月度计划（页面要显示「月度计划 · 2032 年 3 月」）'

echo '==> 6. 起前端 dev（127.0.0.1:'"$FE_PORT"' → 代理到 '"$PORT"'）'
if [ "${SKIP_BROWSER:-0}" = "1" ]; then
    echo '    SKIP_BROWSER=1：跳过浏览器段'
else
    [ -d "$ROOT/frontend/node_modules" ] || { echo 'FAIL: frontend/node_modules 不存在' >&2; exit 2; }
    rm -f "$FE_LOG"
    (cd "$ROOT/frontend" && BACKEND_ORIGIN="http://127.0.0.1:$PORT" setsid nohup \
        ./node_modules/.bin/vite --port "$FE_PORT" --strictPort --host 127.0.0.1 >"$FE_LOG" 2>&1 </dev/null & echo $! >"$FE_PIDFILE")
    for _ in $(seq 1 120); do
        code=$(curl -s -o /dev/null -w '%{http_code}' "http://127.0.0.1:$FE_PORT/" || true)
        [ "$code" = "200" ] && break
        sleep 1
    done
    [ "$(curl -s -o /dev/null -w '%{http_code}' "http://127.0.0.1:$FE_PORT/")" = "200" ] || {
        echo 'FAIL: 前端 dev 起不来' >&2; tail -20 "$FE_LOG" >&2; exit 2; }
    ok "前端 dev 已起（127.0.0.1:$FE_PORT 200）"
    # 代理链路自检：前端 /api → 后端。**未带令牌必须是 401**（不是 502、也不是 200 的 SPA HTML），
    # 否则「页面能开但列表永远空」会被误当成业务问题。
    assert_eq "$(curl -s -o /dev/null -w '%{http_code}' "http://127.0.0.1:$FE_PORT/api/business/erp/purchase-plan/page")" "401" '/api 代理链路通（未带令牌 401）'

    echo '==> 7. 跑 Playwright（真 Chromium，5 条用例）'
    set +e
    ( cd "$ROOT/frontend" && \
      E2E_BASE_URL="http://127.0.0.1:$FE_PORT" \
      E2E_USERNAME="$ADMIN_USER" E2E_PASSWORD="$ADMIN_PASS" \
      M406_SHOT_DIR="$EVID_DIR" \
      ./node_modules/.bin/playwright test e2e/m406-purchase-plan.spec.ts --workers=1 --reporter=list ) \
      2>&1 | tee "$EVID_DIR/10_playwright.out"
    PW_RC=${PIPESTATUS[0]}
    set -e
    if [ "$PW_RC" = "0" ]; then
        ok 'Playwright 5 条用例全过（页面能开 / 真建单 / 重复格被拦 / 执行率 100%+下钻 / 转申请拦截）'
    else
        bad "Playwright 退出码 $PW_RC（看 $EVID_DIR/10_playwright.out 与 test-results/）"
    fi
fi

echo
echo "RESULT: PASS=$PASS FAIL=$FAIL"
[ "$FAIL" = "0" ] && echo 'M4-06 采购计划前端页面演练：全过' || echo 'M4-06 采购计划前端页面演练：有失败'
[ "$FAIL" = "0" ]

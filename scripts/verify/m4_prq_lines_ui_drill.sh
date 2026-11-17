#!/usr/bin/env bash
# ============================================================
# 采购申请「建单明细插槽 = colKey」演练（真库 + 真后端 + 真浏览器）
#
# 卡：t_8dcb79d5（修复 purchase-requisition 建单明细 t-table 插槽名与 colKey 不匹配）
#
# 为什么要有这个脚本
# ------------------
# 这次修的是「插槽名 ≠ colKey」——`vue-tsc` 与 `pnpm build` 对这种写法**全绿**，
# 只有真浏览器能看出来（明细行渲染成裸值：物料列空白、数量是文本、下拉/日期/删除全没有）。
# 所以验收必须落到：起空库 → 起隔离后端 → 起 vite → 真 Chromium 点开「新建申请」，
# 确认 t-select / t-input-number / t-date-picker 真的在 DOM 里、能点开、能填，
# 而且填的值真的进了 POST 报文与数据库。
#
# 跑什么
# ------
#   0. 建空演练库（cp_m4prq_lines）→ 实例首启自己跑完迁移
#   1. 夹具：1 部门 / 1 仓库 / 1 物料 / 1 供应商（SQL 直插，页面下拉要能取到）
#   2. 接口自检：菜单页行在位、admin 登录、列表接口 200
#   3. 起 vite dev（BACKEND_ORIGIN 指后端），/api 代理链路自检（未带令牌 = 401）
#   4. 源码契约单测（插槽名必须等于 colKey）
#   5. 真浏览器跑 `frontend/e2e/m4-prq-create-lines.spec.ts`（5 条用例）
#   6. 落库复核：浏览器建的那张单，明细 qty=3 / est_price=2.5 真在库里
#
# 用法（99 开发机；145 上跑加 DBHOST=127.0.0.1）
#   bash scripts/verify/m4_prq_lines_ui_drill.sh
#   SKIP_BROWSER=1 bash scripts/verify/m4_prq_lines_ui_drill.sh   # 只起栈，不跑浏览器
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
DB="${DB:-cp_m4prq_lines}"
PORT="${PORT:-18102}"
FE_PORT="${FE_PORT:-5191}"
BASE="http://127.0.0.1:$PORT/api"
LOG=/root/m4prq-lines-server.log
PIDFILE=/root/m4prq-lines-server.pid
FE_LOG=/root/m4prq-lines-vite.log
FE_PIDFILE=/root/m4prq-lines-vite.pid
EVID="$(date +%Y-%m-%d)"
EVID_DIR="$ROOT/docs/evidence/$EVID-t_8dcb79d5-prq-lines-slot"
mkdir -p "$EVID_DIR"
ADMIN_USER="${ADMIN_USER:-admin}"
ADMIN_PASS="${ADMIN_PASS:-Admin@123456}"

UNIT="d8dc0001-0000-4000-8000-000000000001"
WH="d8dc0001-0000-4000-8000-000000000002"
MAT="d8dc0001-0000-4000-8000-000000000011"
DEPT="d8dc0001-0000-4000-8000-000000000021"
SUPPLIER="d8dc0001-0000-4000-8000-000000000031"

PASS=0
FAIL=0
TOKEN=""
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
assert_eq "$(psql_q "SELECT count(*) FROM sys_menu WHERE path='/business/erp/purchase-requisition' AND deleted_at IS NULL")" "1" 'V0331 的菜单页行在位（路由要对得上它）'

echo '==> 2. 夹具（部门 / 仓库 / 物料 / 供应商；页面下拉要能取到这几样）'
psql_x "UPDATE sys_user SET must_change_password = false WHERE username = '$ADMIN_USER';
        INSERT INTO biz_unit (id, unit_code, unit_name, is_base, rate, status)
        VALUES ('$UNIT', 'M4PRQ-U', 'M4PRQ 基本单位', 1, 1, 1) ON CONFLICT (id) DO NOTHING;
        INSERT INTO biz_warehouse (id, warehouse_code, warehouse_name, warehouse_path, is_leaf, level, status)
        VALUES ('$WH', 'M4PRQ-WH-A', 'M4PRQ 仓库 A', '/M4PRQ-WH-A', 1, 1, 1) ON CONFLICT (id) DO NOTHING;
        INSERT INTO biz_material (id, material_code, material_name, unit_id, material_type, status)
        VALUES ('$MAT', 'M4PRQ-MAT-A', 'M4PRQ 物料 A', '$UNIT', 'RAW', 1) ON CONFLICT (id) DO NOTHING;
        INSERT INTO biz_supplier (id, supplier_code, supplier_name, status)
        VALUES ('$SUPPLIER', 'M4PRQ-SUP-A', 'M4PRQ 供应商 A', 1) ON CONFLICT (id) DO NOTHING;
        INSERT INTO sys_dept (id, dept_code, dept_name, status)
        VALUES ('$DEPT', 'M4PRQ-DEPT', 'M4PRQ 采购部', 1) ON CONFLICT (id) DO NOTHING;" >/dev/null
assert_eq "$(psql_q "SELECT count(*) FROM biz_material WHERE material_code='M4PRQ-MAT-A'")" "1" '物料夹具已建'
assert_eq "$(psql_q "SELECT count(*) FROM biz_supplier WHERE supplier_code='M4PRQ-SUP-A'")" "1" '供应商夹具已建'

echo '==> 3. 登录 admin + 接口自检（下拉要取的主数据接口）'
TOKEN=$(curl -s -X POST -H 'Content-Type: application/json' \
    --data-binary "{\"username\":\"$ADMIN_USER\",\"password\":\"$ADMIN_PASS\"}" \
    "$BASE/system/auth/login" | jq -r '.data.access_token // empty')
[ -n "$TOKEN" ] || { echo 'FAIL: admin 登录失败' >&2; exit 2; }
ok 'admin 登录拿到令牌'
call GET '/business/erp/purchase-requisition/page?page=1&page_size=20'
assert_eq "$REQ_CODE" "200" '申请单列表接口 200'
call GET '/business/erp/material/page?page=1&page_size=200&status=1'
assert_eq "$REQ_CODE" "200" '物料下拉接口 200'
assert_eq "$(jqv '.data.list | map(select(.material_code=="M4PRQ-MAT-A")) | length')" "1" '物料下拉里能取到夹具物料'
call GET '/business/erp/supplier/page?status=1&page=1&page_size=200'
assert_eq "$REQ_CODE" "200" '供应商下拉接口 200'
assert_eq "$(jqv '.data.list | map(select(.supplier_code=="M4PRQ-SUP-A")) | length')" "1" '供应商下拉里能取到夹具供应商'
call GET '/system/dept/tree'
assert_eq "$REQ_CODE" "200" '部门树接口 200'

echo '==> 4. 起前端 dev（127.0.0.1:'"$FE_PORT"' → 代理到 '"$PORT"')'
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
    # 代理链路自检：未带令牌必须 401（不是 502、也不是 200 的 SPA HTML）
    assert_eq "$(curl -s -o /dev/null -w '%{http_code}' "http://127.0.0.1:$FE_PORT/api/business/erp/purchase-requisition/page")" "401" '/api 代理链路通（未带令牌 401）'

    echo '==> 5. 源码契约单测（插槽名必须逐字等于 colKey）'
    set +e
    ( cd "$ROOT/frontend" && npx tsx --test tests/unit/erp-purchase-requisition-lines-slot.test.ts ) \
        2>&1 | tee "$EVID_DIR/10_unit_contract.out" | tail -8
    UNIT_RC=${PIPESTATUS[0]}
    set +e
    if [ "$UNIT_RC" = "0" ]; then ok '契约单测 3/3（插槽名 = colKey / 控件有 testid / 旧命名不再出现）'; else bad "契约单测退出码 $UNIT_RC"; fi

    echo '==> 6. 跑 Playwright（真 Chromium，5 条用例）'
    set +e
    ( cd "$ROOT/frontend" && \
      E2E_BASE_URL="http://127.0.0.1:$FE_PORT" \
      E2E_USERNAME="$ADMIN_USER" E2E_PASSWORD="$ADMIN_PASS" \
      M4PRQ_SHOT_DIR="$EVID_DIR" \
      ./node_modules/.bin/playwright test e2e/m4-prq-create-lines.spec.ts --workers=1 --reporter=list ) \
      2>&1 | tee "$EVID_DIR/20_playwright.out" | tail -20
    PW_RC=${PIPESTATUS[0]}
    set +e
    if [ "$PW_RC" = "0" ]; then
        ok 'Playwright 5 条用例全过（控件真的渲染/能点开能填/空提交被拦/报文带值/加删行）'
    else
        bad "Playwright 退出码 $PW_RC（看 $EVID_DIR/20_playwright.out 与 frontend/test-results/）"
    fi

    echo '==> 7. 落库复核：浏览器在明细行里填的 3 / 2.5 真的写进了数据库'
    ROW=$(psql_q "SELECT i.qty::text || '|' || i.est_price::text || '|' || i.tax_rate::text || '|' || (i.suggest_supplier_id IS NULL)::text
                    FROM biz_purchase_requisition_item i
                    JOIN biz_purchase_requisition r ON r.id = i.requisition_id
                   WHERE r.deleted_at IS NULL AND r.req_no LIKE 'PRQ-%'
                   ORDER BY i.created_at DESC LIMIT 1")
    echo "    最新明细行（qty|est_price|tax_rate|supplier为空）: $ROW"
    assert_num "$(printf '%s' "$ROW" | cut -d'|' -f1)" "3" '明细数量 3（表格里填的）'
    assert_num "$(printf '%s' "$ROW" | cut -d'|' -f2)" "2.5" '明细单价 2.5（表格里填的）'
fi

echo
echo "RESULT: PASS=$PASS FAIL=$FAIL"
[ "$FAIL" = "0" ] && echo 't_8dcb79d5 采购申请建单明细插槽演练：全过' || echo 't_8dcb79d5 采购申请建单明细插槽演练：有失败'
# 显式退出码（不要指望最后一条 `[` 的返回值：EXIT trap 里的 grep 未命中会把它改掉）
if [ "$FAIL" = "0" ]; then exit 0; else exit 1; fi

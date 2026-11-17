#!/usr/bin/env bash
# ============================================================
# M4-11 框架协议**前端页面**演练（真库 + 真后端 + 真浏览器）
#
# 卡：t_da2f0a9b（M4-11【M4·采购】框架协议：前端页面 —— 协议台账 + 执行量）
#
# 为什么要有这个脚本
# ------------------
# 这一页的全部数据都来自 V0452 的两个视图，而视图口径里有三处**特别容易在渲染层被打回 0**：
#   ① `max_qty / total_max_qty / cap_*` 为 NULL = 「不限量」，不是 0
#   ② `remaining_qty` 可为**负数** = 超上限，不许截断
#   ③ `exec_state` 五态（UNLIMITED/NOT_STARTED/PARTIAL/DONE/OVER）由后端给，不许用 pct 反推
# 所以脚本把「接口数对 → 页面显示对 → 下钻看得清 → 空物料范围不报 404」四段都跑在真栈上：
#   临时库（145）+ profile=test 后端（18102）+ vite dev（5191）+ Playwright/Chromium。
#
# 跑什么
# ------
#   0. 建**空**演练库 cp_m411_e2e → 实例首启自己跑完全部迁移（含 V0450~V0452）
#   1. 起隔离后端 → /health 200
#   2. 夹具：1 单位 / 1 仓库 / 2 物料 / 1 供应商 / 1 部门
#   3. 六份协议（**没有 CRUD 接口**，只能 SQL 造；这本身就是本卡的口径）：
#        · M411 超限协议     两行都设上限（100 / 50），SQL 下单 60+50+45 + 一张作废 30
#                            → 已用 155 / 封顶 150 → 剩余 **-5**（超限）、行 OVER、释放 1 张
#        · M411 部分执行协议 单头封顶 50、明细不限量；走**真接口**下 20、再下 5 并作废
#                            → 剩余 30、40.00%、PARTIAL、下钻里能看到那张作废单 counted=false
#        · M411 不限量协议   明细 max_qty NULL；真接口下 20 → 剩余/执行率都是 **null（不限）**
#        · M411 拟稿协议     DRAFT（日期区间故意设成「今天之内」，证明草稿不因日期而显示生效中）
#        · M411 过期协议     APPROVED 但 2020 年就过期 → validity_state=EXPIRED、days_left 为负
#        · M411 无物料协议   APPROVED 生效中、一行明细都没有 → 详情 **200 + items=[]**
#   4. 接口断言（数量、率、剩余正负、null、over_only / validity / status 三个筛选、404）
#   5. 起 vite dev（BACKEND_ORIGIN 指 18102）→ 代理链路自检（未带令牌 401）
#   6. 跑 `frontend/e2e/m411-framework-agreement.spec.ts`（真点真断言 + 截图）
#
# 用法（99 开发机；145 上跑加 DBHOST=127.0.0.1）
#   bash scripts/verify/m411_framework_agreement_ui_drill.sh
#   SKIP_BROWSER=1 bash scripts/verify/m411_framework_agreement_ui_drill.sh   # 只跑接口段
#
# 退出码：0 = 全过；1 = 有断言失败；2 = 环境问题（建库 / 起不来）
# ============================================================
set -uo pipefail
export PATH=/root/.cargo/bin:$PATH

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
DBHOST="${DBHOST:-172.18.10.145}"
DBPORT="${DBPORT:-5432}"
DBUSER="${DBUSER:-cloudpivot}"
DBPASS="${DBPASS:-fbb1263691ff75c31ec99eaf258138b4}"
DBOWNER="${DBOWNER:-cloudpivot}"
DB="${DB:-cp_m411_e2e}"
PORT="${PORT:-18102}"
FE_PORT="${FE_PORT:-5191}"
BASE="http://127.0.0.1:$PORT/api"
LOG=/root/m411-ui-server.log
PIDFILE=/root/m411-ui-server.pid
FE_LOG=/root/m411-ui-vite.log
FE_PIDFILE=/root/m411-ui-vite.pid
EVID="$(date +%Y-%m-%d)"
EVID_DIR="$ROOT/docs/evidence/$EVID-m411-framework-agreement-ui"
mkdir -p "$EVID_DIR"
ADMIN_USER="${ADMIN_USER:-admin}"
ADMIN_PASS="${ADMIN_PASS:-Admin@123456}"

# 演练库名必须像「演练库」（防手滑对着 prod / dev 跑）
case "$DB" in
    *drill* | *e2e* | *test*) ;;
    *) echo "FAIL: DB=$DB 不含 drill/e2e/test，拒绝在它上面造数" >&2; exit 2 ;;
esac

# ---------- 夹具 UUID（前缀 a411/b411/c411/d411，一眼看得出是演练数据） ----------
UNIT="a4110001-0000-4000-8000-000000000001"
WH="a4110001-0000-4000-8000-000000000002"
MAT_A="a4110001-0000-4000-8000-000000000011"
MAT_B="a4110001-0000-4000-8000-000000000012"
SUPPLIER="a4110001-0000-4000-8000-000000000031"
DEPT="a4110001-0000-4000-8000-000000000021"

AG_OVER="b4110001-0000-4000-8000-000000000001"
AG_PARTIAL="b4110001-0000-4000-8000-000000000002"
AG_UNLIM="b4110001-0000-4000-8000-000000000003"
AG_DRAFT="b4110001-0000-4000-8000-000000000004"
AG_EXPIRED="b4110001-0000-4000-8000-000000000005"
AG_EMPTY="b4110001-0000-4000-8000-000000000006"

IT_OVER_A="c4110001-0000-4000-8000-000000000001"
IT_OVER_B="c4110001-0000-4000-8000-000000000002"
IT_PARTIAL_A="c4110001-0000-4000-8000-000000000003"
IT_UNLIM_A="c4110001-0000-4000-8000-000000000004"
IT_DRAFT_A="c4110001-0000-4000-8000-000000000005"
IT_EXP_A="c4110001-0000-4000-8000-000000000006"

PO_OVER_A1="d4110001-0000-4000-8000-000000000001"
PO_OVER_A2="d4110001-0000-4000-8000-000000000002"
PO_OVER_B1="d4110001-0000-4000-8000-000000000003"
PO_OVER_C1="d4110001-0000-4000-8000-000000000004"

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
assert_null() { if [ "$1" = "null" ] || [ -z "$1" ]; then ok "$2（null = 不限，不是 0）"; else bad "$2：期望 null 实际 [$1]"; fi; }
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

stop_all() {
    local pids="" p port
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
    psql_q "SELECT count(pg_terminate_backend(pid)) FROM pg_stat_activity WHERE datname = '$DB'" >/dev/null 2>&1 || true
    rm -f "$PIDFILE" "$FE_PIDFILE"
    return 0
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
    echo 'FAIL: 后端没编译出来' >&2
    exit 2
}
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
    echo 'FAIL: 健康检查超时' >&2
    tail -40 "$LOG" >&2
    exit 2
}
ok '后端实例已起（/health 200）'
assert_eq "$(psql_q "SELECT count(*) FROM sys_menu WHERE path='/business/erp/framework-agreement' AND deleted_at IS NULL AND menu_type=2")" "1" \
    'V0452 的页面菜单行在位（路由要对得上它）'
assert_eq "$(psql_q "SELECT count(*) FROM sys_menu WHERE auth_code='business:erp:framework-agreement:execution' AND deleted_at IS NULL")" "1" \
    'V0452 的执行量按钮行在位'

echo '==> 2. 夹具（主数据）'
psql_x "UPDATE sys_user SET must_change_password = false WHERE username = '$ADMIN_USER';
        INSERT INTO biz_unit (id, unit_code, unit_name, is_base, rate, status)
        VALUES ('$UNIT', 'M411-U', 'M411 基本单位', 1, 1, 1) ON CONFLICT (id) DO NOTHING;
        INSERT INTO biz_warehouse (id, warehouse_code, warehouse_name, warehouse_path, is_leaf, level, status)
        VALUES ('$WH', 'M411-WH', 'M411 收货仓', '/M411-WH', 1, 1, 1) ON CONFLICT (id) DO NOTHING;
        INSERT INTO biz_material (id, material_code, material_name, unit_id, material_type, status)
        VALUES ('$MAT_A', 'M411-MAT-A', 'M411 钢板', '$UNIT', 'RAW', 1),
               ('$MAT_B', 'M411-MAT-B', 'M411 螺栓', '$UNIT', 'RAW', 1)
        ON CONFLICT (id) DO NOTHING;
        INSERT INTO biz_supplier (id, supplier_code, supplier_name, status)
        VALUES ('$SUPPLIER', 'M411-SUP', 'M411 供应商甲', 1) ON CONFLICT (id) DO NOTHING;
        INSERT INTO sys_dept (id, dept_code, dept_name, status)
        VALUES ('$DEPT', 'M411-DEPT', 'M411 采购部', 1) ON CONFLICT (id) DO NOTHING;" >/dev/null
assert_eq "$(psql_q "SELECT count(*) FROM biz_material WHERE material_code LIKE 'M411-MAT-%'")" "2" '物料夹具已建'
assert_eq "$(psql_q "SELECT count(*) FROM biz_supplier WHERE supplier_code='M411-SUP'")" "1" '供应商夹具已建'

echo '==> 3. 六份协议（**没有 CRUD 接口** —— 只能 SQL 造，这正是本卡口径）'
psql_x "
INSERT INTO biz_framework_agreement
    (id, agreement_no, agreement_name, supplier_id, supplier_name, dept_id, dept_name, owner_name,
     start_date, end_date, sign_date, price_mode, price_includes_tax, price_currency, price_tolerance,
     total_max_qty, total_max_amount, status)
VALUES
    ('$AG_OVER',    'FWA-20260101-411001', 'M411 超限协议',    '$SUPPLIER', 'M411 供应商甲（签约时）', '$DEPT', 'M411 采购部', '张采购',
     '2020-01-01', '2099-12-31', '2020-01-01', 'FIXED', false, 'CNY', 0.05, NULL, NULL, 'APPROVED'),
    ('$AG_PARTIAL', 'FWA-20260101-411002', 'M411 部分执行协议', '$SUPPLIER', 'M411 供应商甲（签约时）', '$DEPT', 'M411 采购部', '张采购',
     '2020-01-01', '2099-12-31', '2020-01-01', 'FIXED', false, 'CNY', 0,    50,   NULL, 'APPROVED'),
    ('$AG_UNLIM',   'FWA-20260101-411003', 'M411 不限量协议',  '$SUPPLIER', 'M411 供应商甲（签约时）', NULL,    NULL,          NULL,
     '2020-01-01', '2099-12-31', '2020-01-01', 'FIXED', false, 'CNY', 0,    NULL, NULL, 'APPROVED'),
    ('$AG_DRAFT',   'FWA-20260101-411004', 'M411 拟稿协议',    '$SUPPLIER', 'M411 供应商甲（签约时）', NULL,    NULL,          NULL,
     '2020-01-01', '2099-12-31', NULL,         'FIXED', false, 'CNY', 0,    NULL, NULL, 'DRAFT'),
    ('$AG_EXPIRED', 'FWA-20260101-411005', 'M411 过期协议',    '$SUPPLIER', 'M411 供应商甲（签约时）', NULL,    NULL,          NULL,
     '2020-01-01', '2020-12-31', '2020-01-01', 'FIXED', false, 'CNY', 0,    NULL, NULL, 'APPROVED'),
    ('$AG_EMPTY',   'FWA-20260101-411006', 'M411 无物料协议',  '$SUPPLIER', 'M411 供应商甲（签约时）', NULL,    NULL,          NULL,
     '2020-01-01', '2099-12-31', '2020-01-01', 'FIXED', false, 'CNY', 0,    NULL, NULL, 'APPROVED')
ON CONFLICT (id) DO NOTHING;

INSERT INTO biz_framework_agreement_item
    (id, agreement_id, line_no, material_id, unit_id, agreed_price, tax_rate, max_qty)
VALUES
    ('$IT_OVER_A',    '$AG_OVER',    1, '$MAT_A', '$UNIT', 10, 0.13, 100),
    ('$IT_OVER_B',    '$AG_OVER',    2, '$MAT_B', '$UNIT',  5, 0.13,  50),
    ('$IT_PARTIAL_A', '$AG_PARTIAL', 1, '$MAT_A', '$UNIT',  8, 0.13, NULL),
    ('$IT_UNLIM_A',   '$AG_UNLIM',   1, '$MAT_A', '$UNIT',  6, 0.13, NULL),
    ('$IT_DRAFT_A',   '$AG_DRAFT',   1, '$MAT_A', '$UNIT',  9, 0.13,  30),
    ('$IT_EXP_A',     '$AG_EXPIRED', 1, '$MAT_A', '$UNIT',  7, 0.13,  30)
ON CONFLICT (id) DO NOTHING;" >/dev/null
assert_eq "$(psql_q "SELECT count(*) FROM biz_framework_agreement WHERE agreement_no LIKE 'FWA-20260101-411%'")" "6" '六份协议已建（超限 / 部分 / 不限量 / 拟稿 / 过期 / 空范围）'
assert_eq "$(psql_q "SELECT count(*) FROM biz_framework_agreement_item WHERE agreement_id='$AG_OVER'::uuid")" "2" '超限协议两行明细'

echo '==> 4. 超限夹具走 SQL（M4-08 会在入口拦 60+50>100，所以「超限」只能是报表侧的数据修复场景）'
psql_x "
INSERT INTO biz_purchase_order
    (id, order_no, supplier_id, order_date, warehouse_id, status, currency_code,
     total_qty, total_amount, total_tax, total_with_tax, remark,
     framework_agreement_id, framework_agreement_no_snapshot)
VALUES
    ('$PO_OVER_A1', 'PO-M411-0001', '$SUPPLIER', '2026-11-10', '$WH', 'APPROVED', 'CNY',
     60, 600, 78, 678, 'M411 超限夹具 1', '$AG_OVER', 'FWA-20260101-411001'),
    ('$PO_OVER_A2', 'PO-M411-0002', '$SUPPLIER', '2026-11-12', '$WH', 'APPROVED', 'CNY',
     50, 500, 65, 565, 'M411 超限夹具 2', '$AG_OVER', 'FWA-20260101-411001'),
    ('$PO_OVER_B1', 'PO-M411-0003', '$SUPPLIER', '2026-11-15', '$WH', 'PARTIAL_RECEIVED', 'CNY',
     45, 225, 29.25, 254.25, 'M411 超限夹具 3', '$AG_OVER', 'FWA-20260101-411001'),
    ('$PO_OVER_C1', 'PO-M411-0004', '$SUPPLIER', '2026-11-16', '$WH', 'CANCELLED', 'CNY',
     30, 300, 39, 339, 'M411 已作废夹具', '$AG_OVER', 'FWA-20260101-411001')
ON CONFLICT (id) DO NOTHING;

INSERT INTO biz_purchase_order_item
    (id, order_id, line_no, material_id, unit_id, qty, price, tax_rate,
     amount, tax_amount, with_tax_amount, framework_agreement_item_id)
VALUES
    (gen_random_uuid(), '$PO_OVER_A1', 1, '$MAT_A', '$UNIT', 60, 10, 0.13, 600, 78,    678,    '$IT_OVER_A'),
    (gen_random_uuid(), '$PO_OVER_A2', 1, '$MAT_A', '$UNIT', 50, 10, 0.13, 500, 65,    565,    '$IT_OVER_A'),
    (gen_random_uuid(), '$PO_OVER_B1', 1, '$MAT_B', '$UNIT', 45,  5, 0.13, 225, 29.25, 254.25, '$IT_OVER_B'),
    (gen_random_uuid(), '$PO_OVER_C1', 1, '$MAT_A', '$UNIT', 30, 10, 0.13, 300, 39,    339,    '$IT_OVER_A');" >/dev/null
assert_eq "$(psql_q "SELECT count(*) FROM biz_purchase_order WHERE order_no LIKE 'PO-M411-%'")" "4" '采购单夹具已建（3 张计入 + 1 张作废）'

echo '==> 5. 登录 admin'
tmp=$(mktemp)
curl -s -o "$tmp" -X POST -H 'Content-Type: application/json' \
    --data-binary "{\"username\":\"$ADMIN_USER\",\"password\":\"$ADMIN_PASS\"}" "$BASE/system/auth/login"
TOKEN=$(jq -r '.data.access_token // empty' <"$tmp")
rm -f "$tmp"
[ -n "$TOKEN" ] || { echo 'FAIL: admin 登录失败' >&2; exit 2; }
ok 'admin 登录拿到令牌'

echo '==> 6. 真接口造「部分执行」与「不限量」（证明 M4-08 下单 → M4-09 视图 → 页面这条链是通的）'
mk_order() {
    local ag="$1" qty="$2" price="$3" remark="$4"
    call POST /business/erp/purchase-order "$(cat <<JSON
{"supplier_id":"$SUPPLIER","warehouse_id":"$WH","purchase_type":"SPOT",
 "framework_agreement_id":"$ag","order_date":"2026-11-18","price_mode":"EXCL","remark":"$remark",
 "items":[{"material_id":"$MAT_A","qty":"$qty","price_excl":"$price","tax_rate":"13"}]}
JSON
)"
}
mk_order "$AG_PARTIAL" 20 8 'M411 部分执行 20'
assert_eq "$REQ_CODE" "200" '部分执行协议：真接口下单 20 成功'
PO_P1=$(jqv '.data.header.id')
assert_eq "$(jqv '.data.framework_agreement.agreement_id')" "$AG_PARTIAL" '采购单落上了协议引用（M4-08 现查现显）'
mk_order "$AG_PARTIAL" 5 8 'M411 部分执行 5（稍后作废）'
assert_eq "$REQ_CODE" "200" '部分执行协议：再下 5 成功'
PO_P2=$(jqv '.data.header.id')
call PATCH "/business/erp/purchase-order/$PO_P2/status" '{"status":"CANCELLED","reason":"M411 演练：作废释放额度"}'
assert_eq "$REQ_CODE" "200" '第二张单作废成功（下钻要能看到它 counted=false）'

mk_order "$AG_UNLIM" 20 6 'M411 不限量 20'
assert_eq "$REQ_CODE" "200" '不限量协议：下单 20 成功（max_qty NULL 不拦）'

echo '==> 7. 接口断言（页面显示的唯一依据）'
call GET "/business/erp/framework-agreement/execution/page?page=1&page_size=50"
assert_eq "$REQ_CODE" "200" '执行量总览 200'
assert_eq "$(jqv '.data.total')" "6" '总览共 6 份协议（软删 / 不存在的不进报表）'

over_row() { jqv ".data.list[] | select(.agreement_no == \"$1\") | $2"; }
assert_eq "$(over_row 'FWA-20260101-411001' '.exec_state')" "OVER" '超限协议：单头状态 OVER'
assert_num "$(over_row 'FWA-20260101-411001' '.executed_qty')" "155" '超限协议：已用 60+50+45=155（作废的 30 不算）'
assert_num "$(over_row 'FWA-20260101-411001' '.cap_qty')" "150" '超限协议：封顶 = 明细合计 100+50'
assert_num "$(over_row 'FWA-20260101-411001' '.remaining_qty')" "-5" '超限协议：剩余 **-5**（负数必须显形）'
assert_num "$(over_row 'FWA-20260101-411001' '.exec_qty_rate_pct')" "103.33" '超限协议：执行率 103.33%'
assert_eq "$(over_row 'FWA-20260101-411001' '.over_items')" "1" '超限协议：1 行超限'
assert_eq "$(over_row 'FWA-20260101-411001' '.order_doc_count')" "3" '超限协议：计入 3 张单'
assert_eq "$(over_row 'FWA-20260101-411001' '.released_doc_count')" "1" '超限协议：释放 1 张（作废）'

assert_eq "$(over_row 'FWA-20260101-411002' '.exec_state')" "PARTIAL" '部分执行协议：PARTIAL'
assert_num "$(over_row 'FWA-20260101-411002' '.remaining_qty')" "30" '部分执行协议：剩余 50-20=30'
assert_num "$(over_row 'FWA-20260101-411002' '.exec_qty_rate_pct')" "40.00" '部分执行协议：40.00%'
assert_eq "$(over_row 'FWA-20260101-411002' '.released_doc_count')" "1" '部分执行协议：作废的 5 进释放计数'

assert_eq "$(over_row 'FWA-20260101-411003' '.exec_state')" "UNLIMITED" '不限量协议：UNLIMITED'
assert_null "$(over_row 'FWA-20260101-411003' '.remaining_qty')" '不限量协议：剩余是 null'
assert_null "$(over_row 'FWA-20260101-411003' '.exec_qty_rate_pct')" '不限量协议：执行率是 null'
assert_num "$(over_row 'FWA-20260101-411003' '.executed_qty')" "20" '不限量协议：已用 20（不限量也照算用量）'

assert_eq "$(over_row 'FWA-20260101-411004' '.validity_state')" "DRAFT" '拟稿协议：有效期状态 DRAFT（日期区间对也不生效）'
assert_eq "$(over_row 'FWA-20260101-411005' '.validity_state')" "EXPIRED" '过期协议：EXPIRED'
assert_eq "$(over_row 'FWA-20260101-411001' '.validity_state')" "ACTIVE" '超限协议：ACTIVE'
assert_eq "$(over_row 'FWA-20260101-411001' '.is_effective')" "true" '超限协议：今天生效'

call GET "/business/erp/framework-agreement/execution/page?page=1&page_size=50&over_only=true"
assert_eq "$(jqv '.data.total')" "1" 'over_only=true 只回 1 份（有行超上限的那份）'
assert_eq "$(jqv '.data.list[0].agreement_no')" "FWA-20260101-411001" 'over_only 命中的是超限协议'

call GET "/business/erp/framework-agreement/execution/page?page=1&page_size=50&validity_state=EXPIRED"
assert_eq "$(jqv '.data.total')" "1" 'validity_state=EXPIRED 只回 1 份'
call GET "/business/erp/framework-agreement/execution/page?page=1&page_size=50&status=DRAFT"
assert_eq "$(jqv '.data.total')" "1" 'status=DRAFT 只回 1 份'
call GET "/business/erp/framework-agreement/execution/page?page=1&page_size=50&keyword=M411"
assert_eq "$(jqv '.data.total')" "6" '关键字 M411 命中 6 份（协议名里都有）'
call GET "/business/erp/framework-agreement/execution/page?page=1&page_size=50&keyword=411003"
assert_eq "$(jqv '.data.total')" "1" '关键字按协议号也能命中（411003 → 不限量协议）'
call GET "/business/erp/framework-agreement/execution/page?page=1&page_size=50&keyword=%E8%B6%85%E9%99%90"
assert_eq "$(jqv '.data.total')" "1" '关键字「超限」命中 1 份'

call GET "/business/erp/framework-agreement/$AG_OVER/execution"
assert_eq "$REQ_CODE" "200" '超限协议详情 200'
assert_eq "$(jqv '.data.items | length')" "2" '超限协议：2 行明细'
assert_eq "$(jqv "[.data.items[] | select(.line_no == 1)] | .[0].exec_state")" "OVER" '第 1 行 OVER'
assert_num "$(jqv "[.data.items[] | select(.line_no == 1)] | .[0].remaining_qty")" "-10" '第 1 行剩余 -10（100-110）'
assert_eq "$(jqv '.data.sources | length')" "4" '下钻 4 行（含作废那一行）'
assert_eq "$(jqv "[.data.sources[] | select(.counted == false)] | length")" "1" '下钻里恰有 1 行不计入'
assert_eq "$(jqv "[.data.sources[] | select(.counted == false)] | .[0].verdict")" "采购单已作废" '不计入的原因是「采购单已作废」'
assert_eq "$(jqv '.data.sources[0].order_no')" "PO-M411-0001" '下钻按行号 / 下单日 / 单号升序'

call GET "/business/erp/framework-agreement/$AG_EMPTY/execution"
assert_eq "$REQ_CODE" "200" '空物料协议：**200**（不是 404；V0453 之前这里是 500 除零）'
# ⚠ 先判码再解析：错误报文里 `.data` 是 null，`null | length` 也是 0，
#    不加这层会把「500 + 空 data」误当成「200 + items=[]」全绿（本卡演练真的踩过）。
if [ "$REQ_CODE" = "200" ] && [ "$(printf '%s' "$REQ_BODY" | jq -r '.data | type' 2>/dev/null)" = "object" ]; then
    assert_eq "$(jqv '.data.items | length')" "0" '空物料协议：items 是空数组'
    assert_eq "$(jqv '.data.sources | length')" "0" '空物料协议：sources 也是空'
    assert_null "$(jqv '.data.agreement.cap_qty')" '空物料协议：没有封顶（null，不是 0）'
    assert_null "$(jqv '.data.agreement.exec_qty_rate')" '空物料协议：执行率 null（不是 0，也不许除零）'
    assert_eq "$(jqv '.data.agreement.exec_state')" "UNLIMITED" '空物料协议：状态 UNLIMITED（不是「已用满」）'
    assert_eq "$(jqv '.data.agreement.item_count')" "0" '空物料协议：明细行数 0（页面靠它显示「无物料范围」）'
else
    bad "空物料协议详情解析失败（码 $REQ_CODE，报文 $(printf '%s' "$REQ_BODY" | head -c 200)）"
fi

call GET "/business/erp/framework-agreement/00000000-0000-4000-8000-000000000000/execution"
assert_eq "$REQ_CODE" "404" '不存在的协议：404（与空物料范围区分开）'

echo '==> 8. 起前端 dev（127.0.0.1:'"$FE_PORT"' → 代理到 '"$PORT"'）'
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
        echo 'FAIL: 前端 dev 起不来' >&2
        tail -20 "$FE_LOG" >&2
        exit 2
    }
    ok "前端 dev 已起（127.0.0.1:$FE_PORT 200）"
    # 代理链路自检：未带令牌必须 401（不是 502、也不是 200 的 SPA HTML）
    assert_eq "$(curl -s -o /dev/null -w '%{http_code}' "http://127.0.0.1:$FE_PORT/api/business/erp/framework-agreement/execution/page")" "401" \
        '/api 代理链路通（未带令牌 401）'

    echo '==> 9. 跑 Playwright（真 Chromium）'
    set +e
    ( cd "$ROOT/frontend" && \
      E2E_BASE_URL="http://127.0.0.1:$FE_PORT" \
      E2E_USERNAME="$ADMIN_USER" E2E_PASSWORD="$ADMIN_PASS" \
      M411_SHOT_DIR="$EVID_DIR" \
      ./node_modules/.bin/playwright test e2e/m411-framework-agreement.spec.ts --workers=1 --reporter=list ) \
        2>&1 | tee "$EVID_DIR/10_playwright.out"
    PW_RC=${PIPESTATUS[0]}
    # ⚠ 这里**必须**保持 `set +e`：恢复 `set -e` 会让 EXIT trap（stop_all 里 `psql_q` 之类
    #    可能非 0）把整个脚本的退出码改成 1 —— 断言全过却报失败，比不跑还误导。
    set +e
    if [ "$PW_RC" = "0" ]; then
        ok 'Playwright 用例全过（两个 Tab / 不限量 / 超限负数 / 只看超限 / 抽屉下钻 / 空物料范围）'
    else
        bad "Playwright 退出码 $PW_RC（看 $EVID_DIR/10_playwright.out 与 test-results/）"
    fi
fi

echo
echo "RESULT: PASS=$PASS FAIL=$FAIL"
if [ "$FAIL" = "0" ]; then
    echo 'M4-11 框架协议前端页面演练：全过'
    exit 0
fi
echo 'M4-11 框架协议前端页面演练：有失败'
exit 1

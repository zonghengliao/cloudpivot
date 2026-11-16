#!/usr/bin/env bash
# ============================================================================
# m303_quotation_convert_e2e.sh —— M3-03【M3·销售】报价单 → 销售订单转换（带行）E2E
#
# 卡：t_de446ead（报价单：→ 销售订单转换（带行），一键转单、保留来源）
# 上游：M3-01（V0432 建表）/ M3-02（V0433 5 个权限码 + 状态机）
#
# 验的是「一条链路」，全部打**真接口 + 真库 + 真权限**：
#
# ```text
#   空库首启（服务自己跑完 434 条迁移 + bootstrap，V0434 种转单权限码）
#        ↓
#   夹具客户 + 物料 → 建报价单（2 行明细）→ 发出 SENT → 客户接受 ACCEPTED
#        ↓
#   转单 POST /quotation/{id}/convert（不带 body）→ 200
#        ├─ 新销售订单 SO-yyyyMMdd-…，DRAFT，金额与报价单同一分钱
#        ├─ 明细 2 行逐行搬（物料 / 数量 / 单价 / 税率 / 金额）
#        ├─ 报价单回填 converted_so_id + converted_at（来源保留）
#        ├─ biz_status_log 一行 ACCEPTED→ACCEPTED, action=CONVERT, biz_ref_no=SO-…
#        └─ 订单备注带「由报价单 QT-… 转换」
#        ↓
#   再点一次 → 幂等：同一张订单，already_converted=true，订单总数仍是 1
#        ↓
#   守卫：DRAFT→409 / SENT→409（提示先标已接受）/ 不存在→404 / 非法 uuid→400
#        ↓
#   转单后的报价单：作废 409（会让订单失去来源）/ 删除 409
#        ↓
#   带参转单：交期早于下单日 400 / 非法销售员 400 / 备注拼在来源前面
# ```
#
# 用法：
#   bash scripts/verify/m303_quotation_convert_e2e.sh              # 99 开发机 → PG 145
#   PG_HOST=127.0.0.1 bash scripts/verify/m303_quotation_convert_e2e.sh   # 在 145 上跑
#   KEEP_DB=1 …     留库排查（打印库名）
#   SKIP_BUILD=1 …  复用已有 target/debug/cloudpivot（不重编）
#   M303_PORT=18231 … 换端口（默认 18231）
#
# 约定（AGENTS.md / 卡面）：
#   · 库建在 PG_HOST（99 上默认 145，145 上默认 127.0.0.1）；
#   · 库名含 e2e，脚本自己 DROP；
#   · **空库首启**：不用模板库 —— 服务自己跑完全部迁移 + bootstrap，admin 口令即
#     NIK_ADMIN_PASSWORD（首登强制改密，脚本里走真接口改）。
# ============================================================================
set -uo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
VERIFY="$ROOT/scripts/verify"
BIN="$ROOT/backend/target/debug/cloudpivot"

PG_HOST="${PG_HOST:-172.18.10.145}"
PG_PORT="${PG_PORT:-5432}"
PG_USER="${PG_USER:-cloudpivot}"
export PGPASSWORD="${PGPASSWORD:-fbb1263691ff75c31ec99eaf258138b4}"
DB="cp_m303_convert_e2e_$$"
PORT="${M303_PORT:-18231}"
BASE="http://127.0.0.1:$PORT"
ADMIN_PASSWORD="${M303_ADMIN_OLD_PASSWORD:-Admin@123456}"
KEEP_DB="${KEEP_DB:-0}"
SKIP_BUILD="${SKIP_BUILD:-0}"

TMP="$(mktemp -d "${TMPDIR:-/tmp}/m303.XXXXXX")"
LOG="$TMP/instance.log"
PASS=0
FAIL=0
INSTANCE_PID=""

ok()   { PASS=$((PASS+1)); printf '  \033[32mPASS\033[0m %s\n' "$1"; }
bad()  { FAIL=$((FAIL+1)); printf '  \033[31mFAIL\033[0m %s\n' "$1"; }
info() { printf '  ---- %s\n' "$1"; }
assert_eq() { # 期望 实际 说明
    if [ "$1" = "$2" ]; then ok "$3（=$2）"; else bad "$3（期望 [$1]，实际 [$2]）"; fi
}

psql_q()     { psql -h "$PG_HOST" -p "$PG_PORT" -U "$PG_USER" -d "$DB" -tAc "$1" 2>&1; }
psql_admin() { psql -h "$PG_HOST" -p "$PG_PORT" -U "$PG_USER" -d postgres -tAc "$1" 2>&1; }

cleanup() {
    if [ -n "$INSTANCE_PID" ]; then
        kill "$INSTANCE_PID" >/dev/null 2>&1 || true
        for _ in 1 2 3 4 5 6 7 8 9 10; do
            kill -0 "$INSTANCE_PID" >/dev/null 2>&1 || break
            sleep 1
        done
        kill -9 "$INSTANCE_PID" >/dev/null 2>&1 || true
    fi
    if [ "$KEEP_DB" = "1" ]; then
        printf '\n  （KEEP_DB=1：演练库保留 → %s；实例日志 %s）\n' "$DB" "$LOG"
        return
    fi
    psql_admin "DROP DATABASE IF EXISTS $DB" >/dev/null 2>&1 || dropdb -h "$PG_HOST" -p "$PG_PORT" -U "$PG_USER" --if-exists "$DB" >/dev/null 2>&1 || true
    rm -rf "$TMP"
}
trap cleanup EXIT

echo "== M3-03 报价单 → 销售订单转换（带行）E2E =="
echo "   PG=${PG_HOST}:${PG_PORT}  演练库=${DB}  服务=${BASE}"
echo

# ---------- 0) 前置：库名合规 / 端口空闲 / 二进制在 ----------
case "$DB" in
    *drill*|*test*|*e2e*) ok "库名合规（含 e2e）" ;;
    *) bad "库名不含 drill/test/e2e，拒绝执行"; exit 1 ;;
esac
if (exec 3<>"/dev/tcp/127.0.0.1/$PORT") 2>/dev/null; then
    bad "端口 $PORT 已被占用（换 M303_PORT 或先停掉占用进程）"; exit 1
else
    ok "端口 $PORT 空闲"
fi
if [ "$SKIP_BUILD" != "1" ]; then
    echo "  ---- cargo build（debug，确保二进制与当前源码一致）"
    if ( cd "$ROOT/backend" && cargo build >"$TMP/build.log" 2>&1 ); then
        ok "cargo build 成功"
    else
        bad "cargo build 失败（tail: $(tail -3 "$TMP/build.log" | tr '\n' ' ')）"; exit 1
    fi
fi
[ -x "$BIN" ] && ok "二进制在位（$BIN）" || { bad "二进制不存在：$BIN"; exit 1; }

# ---------- 1) 空库 + 起隔离实例（服务自己应用全部迁移 + bootstrap） ----------
echo
echo "== 1) 空库首启（迁移 + bootstrap + V0434 种转单权限码）"
dropdb -h "$PG_HOST" -p "$PG_PORT" -U "$PG_USER" --if-exists "$DB" >/dev/null 2>&1 || true
if psql_admin "CREATE DATABASE $DB" >/dev/null 2>&1; then
    ok "建空库 $DB"
else
    bad "建库失败（$(psql_admin "CREATE DATABASE $DB")）"; exit 1
fi

JWT_SECRET="$(openssl rand -hex 32)"
STORAGE_KEY="$(openssl rand -hex 32)"
CONFIG_KEY="$(openssl rand -hex 32)"

(
    cd "$ROOT/backend" || exit 1
    NIK_PROFILE=test \
    NIK_DATABASE_HOST="$PG_HOST" NIK_DATABASE_PORT="$PG_PORT" \
    NIK_DATABASE_USER="$PG_USER" NIK_DATABASE_NAME="$DB" NIK_DATABASE_PASSWORD="$PGPASSWORD" \
    NIK_SERVER_LISTEN="127.0.0.1:$PORT" \
    NIK_MIDDLEWARE_AUTH_JWT_SECRET="$JWT_SECRET" \
    CLOUDPIVOT_STORAGE_MASTER_KEY="$STORAGE_KEY" \
    X_CONFIG_SECRET_KEY="$CONFIG_KEY" \
    NIK_ADMIN_PASSWORD="$ADMIN_PASSWORD" \
    exec "$BIN"
) >"$LOG" 2>&1 &
INSTANCE_PID=$!

READY=0
for _ in $(seq 1 120); do
    sleep 3
    if ! kill -0 "$INSTANCE_PID" >/dev/null 2>&1; then
        bad "服务启动即退出（tail: $(tail -8 "$LOG" | tr '\n' ' ')）"; exit 1
    fi
    if curl -s -m 3 "$BASE/health" | grep -q '"status":"ok"'; then READY=1; break; fi
done
if [ "$READY" = "1" ]; then
    ok "服务就绪（$BASE/health，pid $INSTANCE_PID）"
else
    bad "服务 360 秒未就绪（tail: $(tail -8 "$LOG" | tr '\n' ' ')）"; exit 1
fi
MIGRATIONS="$(psql_q "SELECT count(*) FROM schema_migrations WHERE success")"
MAX_VERSION="$(psql_q "SELECT max(version::int) FROM schema_migrations WHERE success AND version ~ '^[0-9]+$'")"
if [ "${MIGRATIONS:-0}" -ge 360 ]; then
    ok "迁移全部应用（schema_migrations.success = $MIGRATIONS，最大版本 = $MAX_VERSION）"
else
    bad "迁移数异常（success = ${MIGRATIONS:-?}，应 ≥ 360）"
fi
assert_eq "435" "$MAX_VERSION" "最大已应用版本 = 435（V0435 报价单审批流程 + 列；M3-04 之后又加了一号）"
# ---------- 2') 把 quotation_approve 下线（M3-04 起「发出」要过审批；本脚本不验审批） ----------
# 本脚本验的是「状态机 / 转单」。V0435 起 `DRAFT → SENT` 是审批通过后的自动动作，
# 所以这里把流程下线，让「发出」走另一条**真实**出口：
# `POST /submit-approval` + `skip_approval`（`approval_gate::authorize_missing_flow`），
# 理由写进状态日志。与「人工 PATCH」共用同一条 `change_status_with`（三道守卫照跑）。
psql_q "UPDATE biz_flow_def SET status = 'DRAFT' WHERE flow_code = 'quotation_approve'" >/dev/null
assert_eq "DRAFT" "$(psql_q "SELECT status FROM biz_flow_def WHERE flow_code='quotation_approve'")" \
    "quotation_approve 已下线（本脚本走「没配流程 + 显式跳过」出口）"

# ---------- 2) V0434 落地形态（不依赖 HTTP 回显，直接查库） ----------
echo
echo "== 2) V0434 转单权限码 / 菜单落地形态（SQL 直查）"
assert_eq "1" "$(psql_q "SELECT count(*) FROM schema_migrations WHERE version = '434' AND success")" \
    "schema_migrations 登记 V0434（version=434，成功）"
assert_eq "6" "$(psql_q "SELECT count(*) FROM sys_menu WHERE deleted_at IS NULL AND auth_code LIKE 'business:erp:quotation:%'")" \
    "sys_menu 里 6 个 quotation 权限码齐全（V0433 五个 + V0434 转单）"
assert_eq "1" "$(psql_q "SELECT count(*) FROM sys_menu WHERE deleted_at IS NULL
    AND auth_code = 'business:erp:quotation:convert'")" \
    "转单码 business:erp:quotation:convert 有唯一承载行"
assert_eq "5" "$(psql_q "SELECT count(*) FROM sys_menu WHERE deleted_at IS NULL AND menu_type=3
    AND parent_id='20000000-0000-0000-0000-000000004331'::uuid")" \
    "5 个按钮挂在报价单页下（4 个 V0433 + 转单）"
assert_eq "50" "$(psql_q "SELECT sort_order FROM sys_menu
    WHERE id = '20000000-0000-0000-0000-000000004336'::uuid")" \
    "转单按钮 sort_order = 50（同父唯一）"
assert_eq "0" "$(psql_q "SELECT count(*) FROM sys_role_menu
    WHERE menu_id = '20000000-0000-0000-0000-000000004336'::uuid")" \
    "迁移不静默扩权（转单码没有自动授给任何角色）"

# ---------- 3) 真接口断言（转单主链 + 幂等 + 守卫 + 带参） ----------
echo
echo "== 3) 真接口断言（转单带行 / 回链 / 幂等 / 守卫 / 带参）"
if M303_API_BASE="$BASE/api" M303_ADMIN_OLD_PASSWORD="$ADMIN_PASSWORD" \
   M303_ADMIN_PASSWORD="${M303_ADMIN_PASSWORD:-M303@Convert1}" \
   python3 "$VERIFY/m303_quotation_convert_e2e.py" >"$TMP/api.log" 2>&1; then
    API_PASS="$(grep -c '^  PASS ' "$TMP/api.log")"
    ok "真接口断言全绿（$API_PASS 条）"
else
    bad "真接口断言有失败（见下方逐条输出）"
    echo "     ---- api.log 尾部 ----"
    tail -40 "$TMP/api.log" | sed 's/^/     /'
fi
grep -E '^  (PASS|FAIL) ' "$TMP/api.log" | sed 's/^/     /'
tail -3 "$TMP/api.log" | grep -E '====' | sed 's/^/     /'

# ---------- 4) 转单落库形态（来源回链 / 轨迹 / 金额不走样） ----------
echo
echo "== 4) 转单落库形态（biz_quotation 回链 + biz_status_log + biz_sales_order）"
# 脚本一共成功转单 3 次（主链 1 次 + 带参 1 次 + 参数失败后重试 1 次）；
# 重复点击（幂等）与失败守卫都不许加订单，所以订单数必须恰好 3。
assert_eq "3" "$(psql_q "SELECT count(*) FROM biz_quotation WHERE deleted_at IS NULL AND converted_so_id IS NOT NULL")" \
    "3 张报价单回填了 converted_so_id"
assert_eq "0" "$(psql_q "SELECT count(*) FROM biz_quotation
    WHERE converted_so_id IS NULL AND converted_at IS NOT NULL")" \
    "converted_so_id / converted_at 成对（没有只填一半的）"
assert_eq "3" "$(psql_q "SELECT count(*) FROM biz_sales_order WHERE deleted_at IS NULL")" \
    "只建出 3 张销售订单（重复点转单没有多建）"
assert_eq "3" "$(psql_q "SELECT count(*) FROM biz_status_log
    WHERE entity_type='QUOTATION' AND action='CONVERT' AND from_status = to_status AND to_status='ACCEPTED'")" \
    "3 行 CONVERT 轨迹（ACCEPTED→ACCEPTED，状态没变但留了痕）"
assert_eq "0" "$(psql_q "SELECT count(*) FROM biz_status_log
    WHERE entity_type='QUOTATION' AND action='CONVERT' AND coalesce(biz_ref_no,'') = ''")" \
    "CONVERT 轨迹都指向订单号（biz_ref_no 不空）"
assert_eq "3" "$(psql_q "SELECT count(*) FROM biz_sales_order o
    JOIN biz_quotation q ON q.converted_so_id = o.id
   WHERE o.deleted_at IS NULL
     AND o.total_amount = q.total_amount AND o.total_tax = q.total_tax
     AND o.total_with_tax = q.total_with_tax")" \
    "3 张订单金额与报价单逐项相等（价税不差一分钱）"
assert_eq "0" "$(psql_q "SELECT count(*) FROM biz_sales_order o
    JOIN biz_quotation q ON q.converted_so_id = o.id
   WHERE o.deleted_at IS NULL
     AND (SELECT count(*) FROM biz_sales_order_item i WHERE i.so_id = o.id)
       <> (SELECT count(*) FROM biz_quotation_line l WHERE l.quotation_id = q.id)")" \
    "订单行数 = 报价行数（带行，不丢行）"
assert_eq "0" "$(psql_q "SELECT count(*) FROM biz_sales_order
    WHERE deleted_at IS NULL AND status <> 'DRAFT'")" \
    "转出来的订单都还是 DRAFT（转单不动订单流程）"
info "转单订单：$(psql_q "SELECT string_agg(so_no||' ← '||q.quote_no, ' / ')
    FROM biz_sales_order o JOIN biz_quotation q ON q.converted_so_id = o.id")"

# ---------- 5) 菜单结构不变量（8 条断言） ----------
echo
echo "== 5) 菜单结构不变量（menu_invariants.sh / 8 条）"
if bash "$VERIFY/menu_invariants.sh" "$DB" >"$TMP/menu.log" 2>&1; then
    ok "menu_invariants 8 条全绿"
else
    bad "menu_invariants 报红：$(grep -E '✗|FAIL|违规' "$TMP/menu.log" | head -5 | tr '\n' ' ')"
fi
grep -E '✔|⏭|✗' "$TMP/menu.log" | sed 's/^/     /' | head -12

echo
echo "==== M3-03 演练结果：PASS=$PASS FAIL=$FAIL ===="
[ "$FAIL" -eq 0 ] || exit 1

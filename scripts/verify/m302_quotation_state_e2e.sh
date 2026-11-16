#!/usr/bin/env bash
# ============================================================================
# m302_quotation_state_e2e.sh —— M3-02【M3·销售】报价单 CRUD + 状态机 E2E
#
# 卡：t_0750ad31（报价单：CRUD + 状态机 草稿→已发→已接受/已拒绝，状态流转有校验）
# 上游：M3-01（t_6a61726c，V0432 建表 biz_quotation + biz_quotation_line + biz_seq(QT)）
# 下游：M3-03（转订单）/ M3-04（审批流）/ M3-05（单测）/ M3-06（前端列表详情）
#
# 本脚本验的是「一条链路」，全部打**真接口 + 真库 + 真权限**：
#
# ```text
#   空库首启（服务自己跑完 433 条迁移 + bootstrap，V0433 种 5 个权限码）
#        ↓
#   建客户 / 建物料 → 建报价单（2 行明细，金额服务端算）
#        ↓  DRAFT
#   改单（明细整表替换）→ 发出 SENT（之后改单 409 / 删除 409）
#        ↓
#   客户接受 ACCEPTED（只能再作废）→ 已接受转已拒绝 = 非法迁移 409
#        ↓
#   另一张：SENT → 拒绝（无 reason 400 / 有 reason 200）→ 终态再流转 409
#        ↓
#   草稿作废（不要原因）→ 软删 → 详情 404；已拒绝单删除 409
#        ↓
#   过期护栏：过期单发出 409（有效期已过）→ 改期后可发出
# ```
#
# 用法：
#   bash scripts/verify/m302_quotation_state_e2e.sh              # 99 开发机 → PG 145
#   PG_HOST=127.0.0.1 bash scripts/verify/m302_quotation_state_e2e.sh   # 在 145 上跑
#   KEEP_DB=1 …     留库排查（打印库名）
#   SKIP_BUILD=1 …  复用已有 target/debug/cloudpivot（不重编）
#   M302_PORT=18230 … 换端口（默认 18230）
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
DB="cp_m302_quote_e2e_$$"
PORT="${M302_PORT:-18230}"
BASE="http://127.0.0.1:$PORT"
ADMIN_PASSWORD="${M302_ADMIN_OLD_PASSWORD:-Admin@123456}"
KEEP_DB="${KEEP_DB:-0}"
SKIP_BUILD="${SKIP_BUILD:-0}"

TMP="$(mktemp -d "${TMPDIR:-/tmp}/m302.XXXXXX")"
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
    psql_admin "DROP DATABASE IF EXISTS $DB" >/dev/null 2>&1 || true
    rm -rf "$TMP"
}
trap cleanup EXIT

echo "== M3-02 报价单 CRUD + 状态机 E2E =="
echo "   PG=${PG_HOST}:${PG_PORT}  演练库=${DB}  服务=${BASE}"
echo

# ---------- 0) 前置：库名合规 / 端口空闲 / 二进制在 ----------
case "$DB" in
    *drill*|*test*|*e2e*) ok "库名合规（含 e2e）" ;;
    *) bad "库名不含 drill/test/e2e，拒绝执行"; exit 1 ;;
esac
if (exec 3<>"/dev/tcp/127.0.0.1/$PORT") 2>/dev/null; then
    bad "端口 $PORT 已被占用（换 M302_PORT 或先停掉占用进程）"; exit 1
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
echo "== 1) 空库首启（迁移 433 条 + bootstrap + V0433 种码）"
psql_admin "DROP DATABASE IF EXISTS $DB" >/dev/null 2>&1
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
assert_eq "433" "$MAX_VERSION" "最大已应用版本 = 433（本卡 V0433 是最后一号）"

# ---------- 2) V0433 落地形态（不依赖 HTTP 回显，直接查库） ----------
echo
echo "== 2) V0433 权限码 / 菜单落地形态（SQL 直查）"
assert_eq "1" "$(psql_q "SELECT count(*) FROM schema_migrations WHERE version = '433' AND success")" \
    "schema_migrations 登记 V0433（version=433，成功）"
assert_eq "5" "$(psql_q "SELECT count(*) FROM sys_menu WHERE deleted_at IS NULL AND auth_code LIKE 'business:erp:quotation:%'")" \
    "sys_menu 里 5 个 quotation 权限码齐全"
assert_eq "4" "$(psql_q "SELECT count(*) FROM sys_menu WHERE deleted_at IS NULL AND menu_type=3
    AND parent_id='20000000-0000-0000-0000-000000004331'::uuid")" \
    "4 个按钮挂在报价单页下"
assert_eq "报价单" "$(psql_q "SELECT title FROM sys_menu WHERE id='20000000-0000-0000-0000-000000004331'::uuid")" \
    "页面 title 是短菜单名（约定 12：侧栏显示名）"
assert_eq "f" "$(psql_q "SELECT is_visible FROM sys_menu WHERE id='20000000-0000-0000-0000-000000004331'::uuid")" \
    "页面 is_visible=FALSE（前端页由 M3-06 落地后开）"
assert_eq "" "$(psql_q "SELECT coalesce(path,'') FROM sys_menu WHERE id='20000000-0000-0000-0000-000000004331'::uuid")" \
    "页面 path 留空（n08 门禁：前端路由未落地前不许填）"
assert_eq "1" "$(psql_q "SELECT count(*) FROM biz_seq WHERE seq_type='QT' AND deleted_at IS NULL")" \
    "QT 取号规则在位（V0432 种子）"

# ---------- 3) 真接口断言（CRUD + 状态机全链路） ----------
echo
echo "== 3) 真接口断言（建单 / 分页 / 改单 / 发出 / 接受 / 拒绝 / 作废 / 删除 / 过期护栏）"
if M302_API_BASE="$BASE/api" M302_ADMIN_OLD_PASSWORD="$ADMIN_PASSWORD" \
   M302_ADMIN_PASSWORD="${M302_ADMIN_PASSWORD:-M302@Quote1}" \
   python3 "$VERIFY/m302_quotation_state_e2e.py" >"$TMP/api.log" 2>&1; then
    API_PASS="$(grep -c '^  PASS ' "$TMP/api.log")"
    ok "真接口断言全绿（$API_PASS 条）"
else
    bad "真接口断言有失败（见下方逐条输出）"
    echo "     ---- api.log 尾部 ----"
    tail -40 "$TMP/api.log" | sed 's/^/     /'
fi
grep -E '^  (PASS|FAIL) ' "$TMP/api.log" | sed 's/^/     /'
tail -3 "$TMP/api.log" | grep -E '====' | sed 's/^/     /'

# ---------- 4) 状态日志落库形态（轨迹必须真写进 biz_status_log） ----------
echo
echo "== 4) 状态机轨迹落库（biz_status_log）"
# 本脚本一共建了 4 张单：① 主链（→ ACCEPTED → CANCELLED）、② 拒绝链、③ 草稿作废、④ 过期→改期→发出。
# 所以建单日志（from_status IS NULL）必须恰好 4 行，DRAFT → SENT 恰好 3 行（③ 没发出过）。
assert_eq "4" "$(psql_q "SELECT count(*) FROM biz_status_log
    WHERE entity_type='QUOTATION' AND from_status IS NULL AND to_status='DRAFT'")" \
    "4 张单都有建单日志（from_status NULL → DRAFT）"
assert_eq "3" "$(psql_q "SELECT count(*) FROM biz_status_log
    WHERE entity_type='QUOTATION' AND from_status='DRAFT' AND to_status='SENT'")" \
    "DRAFT → SENT 有 3 条轨迹（草稿作废那张没发出）"
assert_eq "1" "$(psql_q "SELECT count(*) FROM biz_status_log
    WHERE entity_type='QUOTATION' AND from_status='DRAFT' AND to_status='CANCELLED'")" \
    "DRAFT → CANCELLED 有轨迹（草稿作废不要原因）"
assert_eq "1" "$(psql_q "SELECT count(*) FROM biz_status_log
    WHERE entity_type='QUOTATION' AND from_status='SENT' AND to_status='ACCEPTED'")" \
    "SENT → ACCEPTED 有轨迹"
assert_eq "1" "$(psql_q "SELECT count(*) FROM biz_status_log
    WHERE entity_type='QUOTATION' AND from_status='SENT' AND to_status='REJECTED'
      AND coalesce(reason,'')='客户嫌贵'")" \
    "SENT → REJECTED 带原因落轨迹"
assert_eq "1" "$(psql_q "SELECT count(*) FROM biz_status_log
    WHERE entity_type='QUOTATION' AND from_status='ACCEPTED' AND to_status='CANCELLED'
      AND coalesce(reason,'')='客户反悔，另开新单'")" \
    "ACCEPTED → CANCELLED 带原因落轨迹"
# 非法迁移一律不写日志：ACCEPTED→REJECTED / REJECTED→SENT 都应 0 行
assert_eq "0" "$(psql_q "SELECT count(*) FROM biz_status_log
    WHERE entity_type='QUOTATION' AND from_status='ACCEPTED' AND to_status IN ('REJECTED','SENT')")" \
    "非法迁移没有写下任何轨迹"
assert_eq "0" "$(psql_q "SELECT count(*) FROM biz_status_log
    WHERE entity_type='QUOTATION' AND from_status='REJECTED'")" \
    "已拒绝（终态）之后没有出边轨迹"
# 报价单表的实际状态分布（软删的那张不算）
info "未删报价单状态分布：$(psql_q "SELECT string_agg(status||'='||cnt, ' / ')
    FROM (SELECT status, count(*) AS cnt FROM biz_quotation WHERE deleted_at IS NULL
           GROUP BY status ORDER BY status) t")"

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
echo "==== M3-02 演练结果：PASS=$PASS FAIL=$FAIL ===="
[ "$FAIL" -eq 0 ] || exit 1

#!/usr/bin/env bash
# ============================================================================
# m218_warehouse_gap_e2e.sh —— M2-18【M2·仓库】仓库缺口收口 E2E（在途 + 库龄 + 安全库存）
#
# 卡：t_659b64de（M2-18 收口：在途 + 库龄 + 安全库存 E2E）
# 上游：M2-01..07（在途账 / 发货过账 / 签收确认 / 查询 / 对账 / 页面）
#       M2-08..12（库龄计算 / 分段报表 / 呆滞料 / 单测 / 页面）
#       M2-13..17（安全库存字段 / 水位检查 / 补货建议 / 单测 / 设置页）
#
# 本卡做的是**一条链路**：把三个子系统放在同一格库存上跑一遍，看它们是否互相咬合。
#
# ```text
#   入库四批（业务日期跨 4 个库龄档）
#        ↓  biz_inventory_txn 的 IN 流水
#   库龄分层 / 分段报表 / 呆滞标记
#        ↓  biz_inventory.qty（结存）
#   发货过账 → 在途 60（账还没扣）
#        ↓  在途 + 未清采购订单
#   补货建议：预计可用 140 → 建议补 160
#        ↓  结存 vs 最低水位
#   水位检查：低于水位 → 建待办（负责人 = 仓管主管）
#        ↓  签收确认（阶段 2 夹具复刻落库形态，过账内核由 m203 演练覆盖）
#   结存 100→70、在途 60→30、库龄四档→40/30/0/0、呆滞清零、预计可用**不变**
#        ↓  改水位（PUT 最低 50）
#   水位检查自动收口（closed=1）+ 补货建议转 OK
# ```
#
# 全部断言打**真接口**（真库 + 真服务 + 真权限），脚本自己建库、自己起服务、自己删库。
#
# 用法：
#   bash scripts/verify/m218_warehouse_gap_e2e.sh                 # 99 开发机 → PG 145
#   PG_HOST=127.0.0.1 bash scripts/verify/m218_warehouse_gap_e2e.sh   # 在 145 上跑
#   KEEP_DB=1 …     留库排查（打印库名）
#   SKIP_BUILD=1 …  复用已有 target/debug/cloudpivot（不重编）
#   M218_PORT=18219 … 换端口（默认 18218）
#
# 约定（AGENTS.md / 卡面）：
#   · 库建在 PG_HOST（99 上默认 145，145 上默认 127.0.0.1）；
#   · 库名含 e2e，脚本自己 DROP；
#   · **空库首启**：不用模板库 —— 服务自己跑完全部迁移（363+ 条）+ bootstrap，
#     admin 口令即 NIK_ADMIN_PASSWORD（首登强制改密，脚本里走真接口改）。
#     用模板库的话 admin 口令是别人演练留下的哈希，脚本没法登录，也带一堆残留数据。
# ============================================================================
set -uo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
VERIFY="$ROOT/scripts/verify"
APP_DIR="$ROOT/backend/app"
BIN="$ROOT/backend/target/debug/cloudpivot"

PG_HOST="${PG_HOST:-172.18.10.145}"
PG_PORT="${PG_PORT:-5432}"
PG_USER="${PG_USER:-cloudpivot}"
export PGPASSWORD="${PGPASSWORD:-fbb1263691ff75c31ec99eaf258138b4}"
DB="cp_m218_gap_e2e_$$"
PORT="${M218_PORT:-18218}"
BASE="http://127.0.0.1:$PORT"
ADMIN_PASSWORD="${M218_ADMIN_OLD_PASSWORD:-Admin@123456}"
KEEP_DB="${KEEP_DB:-0}"
SKIP_BUILD="${SKIP_BUILD:-0}"

TMP="$(mktemp -d "${TMPDIR:-/tmp}/m218.XXXXXX")"
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

psql_q()      { psql -h "$PG_HOST" -p "$PG_PORT" -U "$PG_USER" -d "$DB" -tAc "$1" 2>&1; }
psql_admin()  { psql -h "$PG_HOST" -p "$PG_PORT" -U "$PG_USER" -d postgres -tAc "$1" 2>&1; }
psql_file()   { psql -h "$PG_HOST" -p "$PG_PORT" -U "$PG_USER" -d "$DB" -v ON_ERROR_STOP=1 -q -f "$1" 2>&1; }

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

echo "== M2-18 仓库缺口收口 E2E（在途 + 库龄 + 安全库存）=="
echo "   PG=${PG_HOST}:${PG_PORT}  演练库=${DB}  服务=${BASE}"
echo

# ---------- 0) 前置：库名合规 / 端口空闲 / 二进制在 ----------
case "$DB" in
    *drill*|*test*|*e2e*) ok "库名合规（含 e2e）" ;;
    *) bad "库名不含 drill/test/e2e，拒绝执行"; exit 1 ;;
esac
if (exec 3<>"/dev/tcp/127.0.0.1/$PORT") 2>/dev/null; then
    bad "端口 $PORT 已被占用（换 M218_PORT 或先停掉占用进程）"; exit 1
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
echo "== 1) 空库首启（不用模板库：admin 口令可控、零残留）"
psql_admin "DROP DATABASE IF EXISTS $DB" >/dev/null 2>&1
if psql_admin "CREATE DATABASE $DB" >/dev/null 2>&1; then
    ok "建空库 $DB"
else
    bad "建库失败（$(psql_admin "CREATE DATABASE $DB")）"; exit 1
fi
assert_eq "0" "$(psql_q "SELECT count(*) FROM information_schema.tables WHERE table_schema='public'")" \
    "建库后 0 张表（确实空库）"

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
for _ in $(seq 1 90); do
    sleep 3
    if ! kill -0 "$INSTANCE_PID" >/dev/null 2>&1; then
        bad "服务启动即退出（tail: $(tail -5 "$LOG" | tr '\n' ' ')）"; exit 1
    fi
    if curl -s -m 3 "$BASE/health" | grep -q '"status":"ok"'; then READY=1; break; fi
done
if [ "$READY" = "1" ]; then
    ok "服务就绪（$BASE/health，pid $INSTANCE_PID）"
else
    bad "服务 270 秒未就绪（tail: $(tail -5 "$LOG" | tr '\n' ' ')）"; exit 1
fi
MIGRATIONS="$(psql_q "SELECT count(*) FROM schema_migrations WHERE success")"
if [ "${MIGRATIONS:-0}" -ge 350 ]; then
    ok "迁移全部应用（schema_migrations.success = $MIGRATIONS）"
else
    bad "迁移数异常（success = ${MIGRATIONS:-?}，应 ≥ 350）"
fi
assert_eq "1" "$(psql_q "SELECT count(*) FROM sys_user WHERE username='admin'")" \
    "bootstrap 建出 admin"

# ---------- 2) 阶段 A：库存现状（入库四批 / 在途 60 / 未清订单 100 / 水位 200-300） ----------
echo
echo "== 2) 阶段 A 夹具 + 真接口断言（库龄 / 呆滞 / 在途 / 对账 / 补货 / 水位检查）"
if psql_file "$VERIFY/m218_warehouse_gap_fixture.sql" >"$TMP/fixture1.log" 2>&1; then
    ok "阶段 1 夹具就位（$(sed -n 's/^.*NOTICE:  *//p' "$TMP/fixture1.log" | head -1)）"
    grep "NOTICE:.*M218" "$TMP/fixture1.log" | sed 's/^/       /'
else
    bad "阶段 1 夹具失败：$(tail -3 "$TMP/fixture1.log" | tr '\n' ' ')"; exit 1
fi
if M218_API_BASE="$BASE/api" M218_ADMIN_OLD_PASSWORD="$ADMIN_PASSWORD" \
   python3 "$VERIFY/m218_warehouse_gap_e2e.py" >"$TMP/phaseA.log" 2>&1; then
    A_PASS="$(grep -c '^  PASS ' "$TMP/phaseA.log")"
    ok "阶段 A 真接口断言全绿（$A_PASS 条）"
else
    bad "阶段 A 有失败（见下方逐条输出）"
fi
grep -E '^  (PASS|FAIL) ' "$TMP/phaseA.log" | sed 's/^/     /'

# ---------- 3) 待办落库形态（不依赖 HTTP 回显） ----------
echo
echo "== 3) 水位待办落库形态（SQL 直查）"
assert_eq "1" "$(psql_q "SELECT count(*) FROM biz_task WHERE doc_type='INVENTORY'
   AND doc_id='a2180000-0000-4000-8000-000000000601'")" \
    "该格只有 1 条水位待办（三层幂等的库层：部分唯一索引）"
assert_eq "库存低于最低水位：M218-A1 收口演练物料 @收口演练仓" \
    "$(psql_q "SELECT title FROM biz_task WHERE doc_type='INVENTORY'
       AND doc_id='a2180000-0000-4000-8000-000000000601'")" \
    "待办标题（物料编码 + 名称 + 仓库）"
assert_eq "TODO|P2" "$(psql_q "SELECT status||'|'||priority FROM biz_task
   WHERE doc_type='INVENTORY' AND doc_id='a2180000-0000-4000-8000-000000000601'")" \
    "待办状态 TODO / 优先级 P2（结存 100 > 0，不是断料）"
assert_eq "1" "$(psql_q "SELECT count(*) FROM biz_task t
   JOIN sys_user u ON u.id = t.owner_id
   JOIN sys_user_role ur ON ur.user_id = u.id
   JOIN sys_role r ON r.id = ur.role_id
  WHERE t.doc_type='INVENTORY' AND r.role_code='WAREHOUSE_MANAGER' AND r.deleted_at IS NULL")" \
    "负责人确实是仓管主管（WAREHOUSE_MANAGER）"
assert_eq "1" "$(psql_q "SELECT count(*) FROM biz_task
   WHERE doc_type='INVENTORY' AND description LIKE '%最低水位%' AND description LIKE '%200%'")" \
    "待办描述带最低水位来源（可复核为什么是 200）"

# ---------- 4) 阶段 B：签收确认后的落库形态 → 四处联动 + 守恒 ----------
echo
echo "== 4) 阶段 B 夹具（签收确认落库形态）+ 真接口断言（联动 / 守恒 / 收口）"
if psql_file "$VERIFY/m218_warehouse_gap_fixture_phase2.sql" >"$TMP/fixture2.log" 2>&1; then
    ok "阶段 2 夹具就位（$(sed -n 's/^.*NOTICE:  *//p' "$TMP/fixture2.log" | head -1)）"
    grep "NOTICE:.*M218" "$TMP/fixture2.log" | sed 's/^/       /'
else
    bad "阶段 2 夹具失败：$(tail -3 "$TMP/fixture2.log" | tr '\n' ' ')"; exit 1
fi
if M218_API_BASE="$BASE/api" M218_ADMIN_OLD_PASSWORD="$ADMIN_PASSWORD" \
   python3 "$VERIFY/m218_warehouse_gap_e2e.py" --phase b >"$TMP/phaseB.log" 2>&1; then
    B_PASS="$(grep -c '^  PASS ' "$TMP/phaseB.log")"
    ok "阶段 B 真接口断言全绿（$B_PASS 条）"
else
    bad "阶段 B 有失败（详见下文）"
fi
grep -E '^  (PASS|FAIL) ' "$TMP/phaseB.log" | sed 's/^/     /'

# ---------- 5) 收口后的库层形态 ----------
echo
echo "== 5) 收口后：待办被自动关闭、水位已改、结存与库龄一致"
assert_eq "CANCELLED" "$(psql_q "SELECT status FROM biz_task
   WHERE doc_type='INVENTORY' AND doc_id='a2180000-0000-4000-8000-000000000601'")" \
    "待办已被自动收口（CANCELLED）"
assert_eq "1" "$(psql_q "SELECT count(*) FROM biz_task
   WHERE doc_type='INVENTORY' AND cancel_reason LIKE 'AUTO_RECOVERED%'")" \
    "收口原因前缀 AUTO_RECOVERED（人手动取消的不享受重提醒抑制，系统收口的享受）"
assert_eq "50.0000" "$(psql_q "SELECT min_qty FROM biz_material_stock_policy
   WHERE material_id='a2180000-0000-4000-8000-000000000101' AND deleted_at IS NULL")" \
    "水位已改成 50（PUT /stock-policy upsert，没有新增第二行）"
assert_eq "1" "$(psql_q "SELECT count(*) FROM biz_material_stock_policy
   WHERE material_id='a2180000-0000-4000-8000-000000000101' AND deleted_at IS NULL")" \
    "水位行仍只有 1 行"
assert_eq "70.0000|70.0000" "$(psql_q "
   WITH inv AS (SELECT qty FROM biz_inventory WHERE id='a2180000-0000-4000-8000-000000000601'),
        layers AS (SELECT coalesce(sum(qty_remaining),0) AS q FROM
                     biz_inventory_aging_layers(CURRENT_DATE,
                        'a2180000-0000-4000-8000-000000000101',
                        'a2180000-0000-4000-8000-000000000010'))
   SELECT (SELECT qty FROM inv)::numeric(18,4)||'|'||(SELECT q FROM layers)::numeric(18,4)")" \
    "Σ库龄层剩余 == 账面结存 == 70（库龄与结存同源）"
assert_eq "30.0000" "$(psql_q "SELECT coalesce(sum(qty_in_transit),0) FROM biz_inventory_in_transit
   WHERE material_id='a2180000-0000-4000-8000-000000000101' AND status='IN_TRANSIT'")" \
    "在途剩 30（只算 IN_TRANSIT；已签收那 30 已清零）"
assert_eq "0" "$(psql_q "SELECT count(*) FROM biz_inventory_in_transit
   WHERE (status='IN_TRANSIT' AND received_qty >= shipped_qty)
      OR (status='RECEIVED'  AND received_qty <> shipped_qty)")" \
    "在途行状态 × 数量咬合（IN_TRANSIT 有余量 / RECEIVED 已签满）"

# ---------- 6) 汇总 ----------
echo
echo "== 汇总：PASS=$PASS FAIL=$FAIL（阶段 A $(grep -c '^  PASS ' "$TMP/phaseA.log" 2>/dev/null || echo 0) 条真接口断言 + 阶段 B $(grep -c '^  PASS ' "$TMP/phaseB.log" 2>/dev/null || echo 0) 条 + 本节 SQL 断言）=="
if [ "$FAIL" -ne 0 ]; then
    echo "FAILED"
    [ "$KEEP_DB" = "1" ] || echo "（实例日志在 $LOG，已随库一起清理；用 KEEP_DB=1 重跑可保留）"
    exit 1
fi
echo "ALL PASS"

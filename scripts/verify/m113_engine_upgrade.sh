#!/usr/bin/env bash
# =============================================================================
# m113_engine_upgrade.sh —— V0414（M1-13 现金流底稿表 biz_cashflow_worksheet）
#                            真引擎实测：克隆库升级 + 指纹登记 + 重启幂等
#
# 卡：t_31c91cd5。**不是** psql 手动执行 SQL，而是用编译出的后端二进制真启动，
# 走引擎自己的迁移器（文件扫描 / 方言拆分器 / schema_migrations 指纹 / 幂等 / 顺序）。
#
# 为什么单卡演练（m113_cashflow_worksheet_drill.sh）不够：
#   psql -f 只是「把整份文件丢给 PG」，**没有**经过引擎的 `split_statements`
#   与指纹登记。而生产上线走的正是引擎这条路 —— 拆坏了就整份迁移回滚、服务起不来。
#
# 三段：
#   A) 克隆库升级：模板库 cloudpivot_test → cp_m113_upgrade → 真启动 → 应用 V0414
#      → `V0414 自检通过` 的 NOTICE 出现在引擎日志 → schema_migrations 登记 414/true
#   B) 结构落地：升级后 14 列 / 7 条 CHECK / 4 条索引（psql 直查 information_schema）
#   C) 重启幂等：再启动一次 → 「应用 0 个迁移」（引擎级幂等，不是文件级）
#
# 用法：bash scripts/verify/m113_engine_upgrade.sh [PG_HOST]
#   默认 PG_HOST=172.18.10.145（99 上跑）；145 上跑传 127.0.0.1。
# 端口固定 18090（隔离段 18000-18999），库名含 drill/upgrade，脚本自己 DROP。
# =============================================================================
set -uo pipefail

PG_HOST="${1:-172.18.10.145}"
export PGPASSWORD="${PGPASSWORD:-fbb1263691ff75c31ec99eaf258138b4}"
PG_USER=cloudpivot
TEMPLATE=cloudpivot_test
DB_UP=cp_m113_upgrade
PORT="${M113_PORT:-18090}"

REPO_ROOT=$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)
BACKEND_DIR="$REPO_ROOT/backend"
BIN="$BACKEND_DIR/target/debug/cloudpivot"
RUN_DIR="${M113_RUN_DIR:-/root/.hermes/profiles/backend-developer/cache/scratch/m113-run}"
mkdir -p "$RUN_DIR"

PASS=0
FAIL=0
ok()  { PASS=$((PASS+1)); echo "  PASS  $*"; }
bad() { FAIL=$((FAIL+1)); echo "  FAIL  $*"; }

psql_admin() { psql -h "$PG_HOST" -U "$PG_USER" -d postgres -tAc "$1"; }
q_db()       { psql -h "$PG_HOST" -U "$PG_USER" -d "$DB_UP" -tAc "$1"; }

[[ -x "$BIN" ]] || { echo "FATAL: 缺 $BIN（cd backend && cargo build --bin cloudpivot）"; exit 2; }

JWT_FILE="$RUN_DIR/jwt_secret"
MASTER_FILE="$RUN_DIR/storage_master_key"
[[ -s "$JWT_FILE" ]]    || openssl rand -hex 32 > "$JWT_FILE"
[[ -s "$MASTER_FILE" ]] || openssl rand -hex 32 > "$MASTER_FILE"

start_backend() { # start_backend <logfile>
    local log="$1"
    (
        cd "$BACKEND_DIR" || exit 1
        export NIK_PROFILE=test
        export NIK_DATABASE_HOST="$PG_HOST" NIK_DATABASE_PORT=5432
        export NIK_DATABASE_NAME="$DB_UP" NIK_DATABASE_USER="$PG_USER"
        export NIK_DATABASE_PASSWORD="$PGPASSWORD" NIK_DATABASE_SSL_MODE=prefer
        export NIK_SERVER_LISTEN="127.0.0.1:$PORT"
        export NIK_MIDDLEWARE_AUTH_JWT_SECRET="$(cat "$JWT_FILE")"
        export CLOUDPIVOT_STORAGE_MASTER_KEY="$(cat "$MASTER_FILE")"
        export X_CONFIG_SECRET_KEY="m113-drill-config-secret"
        export NIK_ADMIN_PASSWORD='M113@Drill#2026'
        exec "$BIN"
    ) >"$log" 2>&1 &
    echo $! > "$RUN_DIR/backend.pid"
    local i
    for i in $(seq 1 120); do
        if curl -fsS -m 2 "http://127.0.0.1:$PORT/health" >/dev/null 2>&1; then return 0; fi
        sleep 1
    done
    tail -30 "$log"
    return 1
}

stop_backend() {
    [[ -f "$RUN_DIR/backend.pid" ]] || return 0
    local pid; pid="$(cat "$RUN_DIR/backend.pid")"
    kill "$pid" 2>/dev/null
    for _ in $(seq 1 30); do kill -0 "$pid" 2>/dev/null || break; sleep 1; done
    kill -9 "$pid" 2>/dev/null
    rm -f "$RUN_DIR/backend.pid"
    for _ in $(seq 1 20); do curl -fsS -m 1 "http://127.0.0.1:$PORT/health" >/dev/null 2>&1 || break; sleep 1; done
}

cleanup() {
    stop_backend
    psql_admin "DROP DATABASE IF EXISTS $DB_UP" >/dev/null 2>&1
}
trap cleanup EXIT

echo "== M1-13 / V0414 真引擎实测（现金流底稿表）=="
echo "   PG=${PG_HOST}:5432 模板=${TEMPLATE} 升级库=${DB_UP} 端口=${PORT}"
echo

# ============================================================================
echo "== A) 克隆库升级（真引擎：模板库 → $DB_UP）=="
psql_admin "DROP DATABASE IF EXISTS $DB_UP" >/dev/null 2>&1
if ! psql_admin "CREATE DATABASE $DB_UP TEMPLATE $TEMPLATE" >/dev/null 2>&1; then
    echo "  模板库被占用，5 秒后重试一次"
    sleep 5
    psql_admin "CREATE DATABASE $DB_UP TEMPLATE $TEMPLATE" >/dev/null || { echo "FATAL: 克隆模板库失败"; exit 2; }
fi
BEFORE_MAX="$(q_db "select coalesce(max(version::int),0) from schema_migrations where kind='versioned'")"
echo "  升级前：versioned 最大 version=$BEFORE_MAX（V0413 之后都是本卡要验的）"
[[ "$(q_db "select count(*) from information_schema.tables where table_schema='public' and table_name='biz_cashflow_worksheet'")" == "0" ]] \
    && ok "克隆库升级前不存在 biz_cashflow_worksheet" || bad "克隆库里已经有 biz_cashflow_worksheet"

start_backend "$RUN_DIR/upgrade.log" || { echo "FATAL: 升级实例没起来"; exit 2; }
ok "克隆库真启动成功（/health 200）"

APPLIED="$(grep -oE '数据库迁移完成: 应用 [0-9]+ 个迁移' "$RUN_DIR/upgrade.log" | head -1)"
echo "  $APPLIED"
APPLIED_N="$(echo "${APPLIED:-}" | grep -oE '[0-9]+' | head -1)"
[[ "${APPLIED_N:-0}" -ge 1 ]] && ok "引擎应用了 $APPLIED_N 个迁移（含 V0414）" || bad "一个迁移都没应用 —— V0414 没被引擎识别"
grep -q "V0414 自检通过" "$RUN_DIR/upgrade.log" \
    && ok "V0414 自检块在真引擎里通过（NOTICE 已打印 ⇒ 方言拆分器把 DO 块切对了）" \
    || bad "没看到 V0414 自检 NOTICE（可能是 DO 块被拆坏 / 自检失败）"

ROW="$(q_db "select version||'/'||success||'/'||source from schema_migrations where name='cashflow_worksheet'")"
echo "  schema_migrations：$ROW"
case "$ROW" in
    414/true/*V0414__cashflow_worksheet.up.sql) ok "schema_migrations 登记 414/true/源文件（指纹已登记）" ;;
    *) bad "schema_migrations 登记不对：[$ROW]" ;;
esac

# ============================================================================
echo
echo "== B) 结构落地 =="
COLN="$(q_db "select count(*) from information_schema.columns where table_schema='public' and table_name='biz_cashflow_worksheet'")"
[[ "$COLN" == "14" ]] && ok "14 列齐" || bad "列数 = $COLN"
AMT="$(q_db "select numeric_precision||','||numeric_scale||'/'||is_nullable from information_schema.columns where table_name='biz_cashflow_worksheet' and column_name='amount'")"
[[ "$AMT" == "18,4/NO" ]] && ok "amount = NUMERIC(18,4) NOT NULL" || bad "amount 形状异常：[$AMT]"
CK="$(q_db "select count(*) from pg_constraint where conrelid='public.biz_cashflow_worksheet'::regclass and contype='c' and conname like 'ck_biz_cashflow_ws_%'")"
[[ "$CK" == "7" ]] && ok "7 条 CHECK 在位" || bad "CHECK 只有 $CK 条"
UK="$(q_db "select count(*) from pg_indexes where schemaname='public' and tablename='biz_cashflow_worksheet' and indexname like 'uk_biz_cashflow_ws_%' and indexdef like '%WHERE%'")"
[[ "$UK" == "2" ]] && ok "两条部分唯一索引在位" || bad "部分唯一索引只有 $UK 条"
IDX="$(q_db "select count(*) from pg_indexes where schemaname='public' and tablename='biz_cashflow_worksheet' and indexname like 'idx_biz_cashflow_ws_%'")"
[[ "$IDX" == "2" ]] && ok "2 条普通索引在位（列表/跨期取数）" || bad "普通索引只有 $IDX 条"
IDX_ALL="$(q_db "select count(*) from pg_indexes where schemaname='public' and tablename='biz_cashflow_worksheet'")"
[[ "$IDX_ALL" == "5" ]] && ok "索引总数 5 = 2 唯一 + 2 普通 + 1 主键" || bad "索引总数 = $IDX_ALL（应为 5）"
# 迁移自检的探针行必须清干净（真库里不许攒垃圾）
PROBE="$(q_db "select count(*) from biz_cashflow_worksheet")"
[[ "$PROBE" == "0" ]] && ok "升级后表内 0 行（自检探针已自清场）" || bad "升级后表内有 $PROBE 行（探针没清干净）"

# ============================================================================
echo
echo "== C) 重启幂等 =="
stop_backend
start_backend "$RUN_DIR/upgrade-restart.log" || { echo "FATAL: 重启失败"; exit 2; }
APPLIED2="$(grep -oE '数据库迁移完成: 应用 [0-9]+ 个迁移' "$RUN_DIR/upgrade-restart.log" | head -1)"
case "$APPLIED2" in
    *"应用 0 个迁移"*) ok "重启：应用 0 个迁移（引擎级幂等，指纹一致）" ;;
    *) bad "重启又应用了迁移：[$APPLIED2]" ;;
esac
[[ "$(q_db "select count(*) from information_schema.columns where table_schema='public' and table_name='biz_cashflow_worksheet'")" == "14" ]] \
    && ok "重启后结构不变（14 列）" || bad "重启后结构变了"

echo
echo "== 汇总：PASS=$PASS FAIL=$FAIL =="
[ "$FAIL" -eq 0 ] || exit 1

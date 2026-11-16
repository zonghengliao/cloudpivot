#!/usr/bin/env bash
# =============================================================================
# m124_finance_chain_e2e.sh —— M1-24 财务缺口收口：多币种 + 底稿 + 账龄 E2E
#
# 卡：t_cf5ab203（M1 财务收口；上游 M1-01..M1-23）
#
# 这一层负责「真环境」，business 断言在 m124_finance_chain_e2e.py：
#   1. 克隆模板库（模板库停版 V0404）→ 用**真二进制**启动，让迁移引擎把 V0405..V0418 全应用
#      （多币种 9 条 + 底稿 4 条 + 账龄页面 1 条，一条不漏）
#   2. 断言引擎真的登记了 405..418 每一条 / 新表建出来了 / 新菜单行在位
#   3. 重启一次 → 0 条新迁移、菜单不重复（引擎幂等）
#   4. 跑 python 断言引擎：A 多币种 → B 现金流底稿 → C 账龄 → D 收口重放
#   5. 收场：停实例 + DROP 库（无论成败）
#
# 用法：
#   bash scripts/verify/m124_finance_chain_e2e.sh                 # 99 上跑，连 145:5432
#   bash scripts/verify/m124_finance_chain_e2e.sh 127.0.0.1       # 145 上跑，走 loopback
#
# 库名/端口：cp_m124_e2e / 18099（隔离段 18000-18999），脚本自己 DROP。
# =============================================================================
set -uo pipefail

PG_HOST="${1:-172.18.10.145}"
export PGPASSWORD="${PGPASSWORD:-fbb1263691ff75c31ec99eaf258138b4}"
PG_USER="${PG_USER:-cloudpivot}"
TEMPLATE="${M124_TEMPLATE:-cloudpivot_test}"
DB="${M124_DB:-cp_m124_e2e}"
PORT="${M124_HTTP_PORT:-18099}"
BASE="http://127.0.0.1:$PORT"
ADMIN_PASSWORD="${M124_ADMIN_PASSWORD:-M124@Drill#2026}"

REPO_ROOT=$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)
BACKEND_DIR="$REPO_ROOT/backend"
BIN="$BACKEND_DIR/target/debug/cloudpivot"
RUN_DIR="${M124_RUN_DIR:-/root/.hermes/profiles/backend-developer/cache/scratch/m124-run}"
mkdir -p "$RUN_DIR"

PASS=0
FAIL=0
ok()  { PASS=$((PASS+1)); echo "  PASS  $*"; }
bad() { FAIL=$((FAIL+1)); echo "  FAIL  $*"; }

psql_admin() { psql -h "$PG_HOST" -U "$PG_USER" -d postgres -tAc "$1"; }
q_db()       { psql -h "$PG_HOST" -U "$PG_USER" -d "$DB" -tAc "$1"; }

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
        export NIK_DATABASE_NAME="$DB" NIK_DATABASE_USER="$PG_USER"
        export NIK_DATABASE_PASSWORD="$PGPASSWORD" NIK_DATABASE_SSL_MODE=prefer
        export NIK_SERVER_LISTEN="127.0.0.1:$PORT"
        export NIK_MIDDLEWARE_AUTH_JWT_SECRET="$(cat "$JWT_FILE")"
        export CLOUDPIVOT_STORAGE_MASTER_KEY="$(cat "$MASTER_FILE")"
        export X_CONFIG_SECRET_KEY="m124-e2e-config-secret"
        export NIK_ADMIN_PASSWORD="$ADMIN_PASSWORD"
        exec "$BIN"
    ) >"$log" 2>&1 &
    echo $! > "$RUN_DIR/backend.pid"
    for _ in $(seq 1 180); do
        curl -fsS -m 2 "$BASE/health" >/dev/null 2>&1 && return 0
        sleep 1
    done
    tail -40 "$log"
    return 1
}

stop_backend() {
    [[ -f "$RUN_DIR/backend.pid" ]] || return 0
    local pid; pid="$(cat "$RUN_DIR/backend.pid")"
    kill "$pid" 2>/dev/null
    for _ in $(seq 1 30); do kill -0 "$pid" 2>/dev/null || break; sleep 1; done
    kill -9 "$pid" 2>/dev/null
    rm -f "$RUN_DIR/backend.pid"
    for _ in $(seq 1 20); do curl -fsS -m 1 "$BASE/health" >/dev/null 2>&1 || break; sleep 1; done
}

cleanup() {
    # 收场时 `set -e` 还开着：任何一步失败（kill 已退出的进程 / 库还被连着）
    # 都会让 EXIT trap 直接以非零退出，把全绿的结论翻成 exit 1。这里显式关掉。
    set +e
    stop_backend
    psql_admin "DROP DATABASE IF EXISTS $DB" >/dev/null 2>&1
    true
}
trap cleanup EXIT

echo "== M1-24 财务缺口收口 E2E · 真环境准备 =="
echo "   PG=${PG_HOST}:5432  模板=${TEMPLATE}  库=${DB}  端口=${PORT}"

# ============================================================================
echo
echo "== 0) 克隆模板库（模板停版 V0404，剩下的交给真引擎）=="
psql_admin "DROP DATABASE IF EXISTS $DB" >/dev/null 2>&1
psql_admin "CREATE DATABASE $DB TEMPLATE $TEMPLATE" >/dev/null 2>&1 || {
    echo "  模板库被占用，5 秒后重试"; sleep 5
    psql_admin "CREATE DATABASE $DB TEMPLATE $TEMPLATE" >/dev/null || { echo "FATAL: 克隆失败"; exit 2; }
}
MIG_BEFORE="$(q_db "select count(*) from schema_migrations where kind='versioned' and success;")"
[[ -n "$MIG_BEFORE" ]] && ok "克隆完成（应用前 versioned = $MIG_BEFORE）" || { echo "FATAL: 克隆库不可用"; exit 2; }
[[ "$(q_db "select count(*) from information_schema.tables where table_schema='public' and table_name='biz_cashflow_worksheet';")" == "0" ]] \
    && ok "模板库里还没有底稿表（M1-13 的迁移确实没应用过）" || bad "模板库已有底稿表，前置假设不成立"

# 模板里的 admin 是历史快照，腾出用户名让启动钩子重种一个已知口令的超管
q_db "update sys_user set username='admin_pre_m124' where username='admin'" >/dev/null
ok "腾出 admin 用户名（原行改名 admin_pre_m124）"

# ============================================================================
echo
echo "== 1) 真启动（引擎应用 V0405..V0418）+ 迁移断言 =="
start_backend "$RUN_DIR/backend-boot1.log" || { echo "FATAL: 后端没起来"; exit 2; }
ok "后端真启动成功（$BASE/health 200）"

MIG_AFTER="$(q_db "select count(*) from schema_migrations where kind='versioned' and success;")"
[[ "$MIG_AFTER" -gt "$MIG_BEFORE" ]] && ok "引擎应用了新迁移（$MIG_BEFORE → $MIG_AFTER）" \
    || bad "引擎没应用任何新迁移（仍是 $MIG_AFTER）"
HEAD="$(q_db "select max(version::int) from schema_migrations where kind='versioned' and success;")"
[[ "$HEAD" == "418" ]] && ok "迁移 head = V0418" || bad "迁移 head = V$HEAD（期望 418）"
MISSING="$(q_db "select string_agg(v::text, ',' order by v) from generate_series(405,418) v
                  where not exists (select 1 from schema_migrations
                                     where kind='versioned' and success and version::int = v);")"
[[ -z "$MISSING" ]] && ok "V0405..V0418 十四条逐条登记为已应用（多币种 9 + 底稿 4 + 账龄页 1）" \
    || bad "缺迁移：$MISSING"
for t in biz_cashflow_worksheet biz_cashflow_adjust_item; do
    [[ "$(q_db "select count(*) from information_schema.tables where table_schema='public' and table_name='$t';")" == "1" ]] \
        && ok "新表 $t 已建" || bad "新表 $t 没建出来"
done
[[ "$(q_db "select count(*) from information_schema.columns where table_name='biz_account' and column_name='is_foreign_currency';")" == "1" ]] \
    && ok "biz_account.is_foreign_currency 已加（V0408）" || bad "科目外币标记列缺失"
[[ "$(q_db "select count(*) from information_schema.columns where table_name='biz_voucher_entry' and column_name in ('fc_debit_amount','fc_credit_amount');")" == "2" ]] \
    && ok "biz_voucher_entry 原币两列已加（V0409）" || bad "分录原币列缺失"
[[ "$(q_db "select count(*) from information_schema.columns where table_name='biz_exchange_rate' and column_name='effective_to';")" == "1" ]] \
    && ok "biz_exchange_rate.effective_to 已加（V0407）" || bad "汇率区间列缺失"
for code in 'business:erp:report:fx-balance' 'business:erp:report:fx-ledger' 'business:erp:period:fx-reval' \
            'business:erp:cashflow-worksheet:save' 'business:erp:cashflow-statement:list' \
            'business:erp:cashflow-statement:export' 'business:erp:cashflow-adjust:manage' \
            'business:erp:receivable:aging' 'business:erp:report:receivable-aging' \
            'business:erp:report:payable-aging'; do
    n="$(q_db "select count(*) from sys_menu where auth_code='$code' and deleted_at is null;")"
    [[ "$n" -ge 1 ]] && ok "权限节点在位：$code" || bad "权限节点缺失：$code"
done

echo
echo "== 1b) 重启一次 → 0 条新迁移（引擎幂等）=="
stop_backend
start_backend "$RUN_DIR/backend-boot2.log" || { echo "FATAL: 重启失败"; exit 2; }
MIG_RESTART="$(q_db "select count(*) from schema_migrations where kind='versioned' and success;")"
[[ "$MIG_RESTART" == "$MIG_AFTER" ]] && ok "重启后迁移数不变（$MIG_RESTART，不重放）" \
    || bad "重启后迁移数变了：$MIG_AFTER → $MIG_RESTART"
[[ "$(q_db "select count(*) from sys_menu where auth_code='business:erp:report:receivable-aging';")" == "1" ]] \
    && ok "重启后菜单行不重复（仍 1 行）" || bad "菜单行被重放成多行"

# 演练库：清掉首登强制改密标记（只为本卡接口链路；真实环境不许这么干）
q_db "update sys_user set must_change_password = false where username='admin'" >/dev/null
[[ "$(q_db "select must_change_password from sys_user where username='admin'")" == "f" ]] \
    && ok "演练库：清掉 admin 的 must_change_password" || bad "清 must_change_password 失败"

# ============================================================================
echo
echo "== 2) 业务链路断言（多币种 → 底稿 → 账龄 → 收口重放）=="
set +e
M124_BASE="$BASE/api" \
M124_PGHOST="$PG_HOST" M124_PGDATABASE="$DB" M124_PGUSER="$PG_USER" M124_PGPASSWORD="$PGPASSWORD" \
M124_ADMIN=admin M124_PASSWORD="$ADMIN_PASSWORD" \
    python3 "$REPO_ROOT/scripts/verify/m124_finance_chain_e2e.py" 2>&1 | tee "$RUN_DIR/chain.out"
CHAIN_RC=${PIPESTATUS[0]}
set -e
PASSED=$(grep -c '^\[PASS\]' "$RUN_DIR/chain.out" || true)
FAILED=$(grep -c '^\[FAIL\]' "$RUN_DIR/chain.out" || true)
if [[ "$CHAIN_RC" == "0" && "$FAILED" == "0" ]]; then
    ok "业务链路全绿（$PASSED 条断言）"
else
    bad "业务链路有红：$FAILED 条（exit=$CHAIN_RC）"
    grep '^\[FAIL\]' "$RUN_DIR/chain.out" || true
fi

echo
echo "== 汇总：PASS=$PASS FAIL=$FAIL（业务断言 $PASSED 条）=="
echo "   证据目录：$RUN_DIR"
if [ "$FAIL" -ne 0 ] || [ "$CHAIN_RC" != "0" ]; then
    exit 1
fi
exit 0

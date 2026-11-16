#!/usr/bin/env bash
# =============================================================================
# m301_engine_upgrade.sh —— V0432（M3-01 销售报价单 biz_quotation + biz_quotation_line）
#                            真引擎实测：克隆库升级 + 指纹登记 + 重启幂等
#
# 卡：t_6a61726c。**不是** psql 手动执行 SQL，而是用编译出的后端二进制真启动，
# 走引擎自己的迁移器（文件扫描 / 方言拆分器 / schema_migrations 指纹 / 幂等 / 顺序）。
#
# 为什么单卡演练（m301_sales_quotation_drill.sh）不够：
#   psql -f 只是「把整份文件丢给 PG」，**没有**经过引擎的 `split_statements`
#   与指纹登记。而生产上线走的正是引擎这条路 —— 拆坏了就整份迁移回滚、服务起不来。
#
# 三段：
#   A) 克隆库升级：模板库 cloudpivot_test → cp_m301_upgrade → 真启动 → 应用 V0432
#      → `V0432 自检通过` 的 NOTICE 出现在引擎日志 → schema_migrations 登记 432/true
#   B) 结构落地：升级后 17+12 列 / 10 条 CHECK / 1 条 CASCADE 外键 / 7 条索引 / QT 取号行
#   C) 重启幂等：再启动一次 → 「应用 0 个迁移」（引擎级幂等，不是文件级）
#
# 用法：bash scripts/verify/m301_engine_upgrade.sh [PG_HOST]
#   默认 PG_HOST=172.18.10.145（99 上跑）；145 上跑传 127.0.0.1。
# 端口默认 18093（隔离段 18000-18999），库名含 upgrade，脚本自己 DROP。
# =============================================================================
set -uo pipefail

PG_HOST="${1:-172.18.10.145}"
export PGPASSWORD="${PGPASSWORD:-fbb1263691ff75c31ec99eaf258138b4}"
PG_USER=cloudpivot
TEMPLATE=cloudpivot_test
DB_UP=cp_m301_upgrade
PORT="${M301_PORT:-18093}"

REPO_ROOT=$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)
BACKEND_DIR="$REPO_ROOT/backend"
BIN="$BACKEND_DIR/target/debug/cloudpivot"
RUN_DIR="${M301_RUN_DIR:-/root/.hermes/profiles/backend-developer/cache/scratch/m301-run}"
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
        export X_CONFIG_SECRET_KEY="m301-drill-config-secret"
        export NIK_ADMIN_PASSWORD='M301@Drill#2026'
        exec "$BIN"
    ) >"$log" 2>&1 &
    echo $! > "$RUN_DIR/backend.pid"
    local i
    for i in $(seq 1 150); do
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

echo "== M3-01 / V0432 真引擎实测（销售报价单 biz_quotation + biz_quotation_line）=="
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
echo "  升级前：versioned 最大 version=$BEFORE_MAX"
[[ "$(q_db "select count(*) from information_schema.tables where table_schema='public' and table_name in ('biz_quotation','biz_quotation_line')")" == "0" ]] \
    && ok "克隆库升级前不存在报价单两张表" || bad "克隆库里已经有 biz_quotation / biz_quotation_line"

start_backend "$RUN_DIR/upgrade.log" || { echo "FATAL: 升级实例没起来"; exit 2; }
ok "克隆库真启动成功（/health 200）"

APPLIED="$(grep -oE '数据库迁移完成: 应用 [0-9]+ 个迁移' "$RUN_DIR/upgrade.log" | head -1)"
echo "  $APPLIED"
APPLIED_N="$(echo "${APPLIED:-}" | grep -oE '[0-9]+' | head -1)"
[[ "${APPLIED_N:-0}" -ge 1 ]] && ok "引擎应用了 $APPLIED_N 个迁移（含 V0432）" || bad "一个迁移都没应用 —— V0432 没被引擎识别"
grep -q "V0432 自检通过" "$RUN_DIR/upgrade.log" \
    && ok "V0432 自检块在真引擎里通过（NOTICE 已打印 ⇒ 方言拆分器把 DO 块切对了）" \
    || bad "没看到 V0432 自检 NOTICE（可能是 DO 块被拆坏 / 自检失败）"

ROW="$(q_db "select version||'/'||success||'/'||source from schema_migrations where name='sales_quotation'")"
echo "  schema_migrations：$ROW"
case "$ROW" in
    432/true/*V0432__sales_quotation.up.sql) ok "schema_migrations 登记 432/true/源文件（指纹已登记）" ;;
    *) bad "schema_migrations 登记不对：[$ROW]" ;;
esac

# ============================================================================
echo
echo "== B) 结构落地 =="
HDR="$(q_db "select count(*) from information_schema.columns where table_schema='public' and table_name='biz_quotation'")"
[[ "$HDR" == "17" ]] && ok "头表 17 列齐" || bad "头表列数 = $HDR"
LIN="$(q_db "select count(*) from information_schema.columns where table_schema='public' and table_name='biz_quotation_line'")"
[[ "$LIN" == "12" ]] && ok "行表 12 列齐" || bad "行表列数 = $LIN"
VU="$(q_db "select data_type||'/'||is_nullable from information_schema.columns
   where table_name='biz_quotation' and column_name='valid_until'")"
[[ "$VU" == "date/NO" ]] && ok "valid_until = DATE NOT NULL（有效期必填）" || bad "valid_until 形状异常：[$VU]"
CK="$(q_db "select count(*) from pg_constraint where conrelid='public.biz_quotation'::regclass and contype='c' and conname like 'ck_biz_quotation_%'")"
[[ "$CK" == "5" ]] && ok "头表 5 条 CHECK 在位" || bad "头表 CHECK 只有 $CK 条"
CKL="$(q_db "select count(*) from pg_constraint where conrelid='public.biz_quotation_line'::regclass and contype='c' and conname like 'ck_biz_quotation_line_%'")"
[[ "$CKL" == "5" ]] && ok "行表 5 条 CHECK 在位" || bad "行表 CHECK 只有 $CKL 条"
FK="$(q_db "select pg_get_constraintdef(oid) from pg_constraint where conname='fk_biz_quotation_line_quotation'")"
case "$FK" in
    *"REFERENCES biz_quotation(id) ON DELETE CASCADE"*) ok "明细外键 = ON DELETE CASCADE" ;;
    *) bad "外键定义异常：[$FK]" ;;
esac
IDX_ALL="$(q_db "select count(*) from pg_indexes where schemaname='public'
   and tablename in ('biz_quotation','biz_quotation_line')
   and (indexname like 'uk_biz_quotation%' or indexname like 'idx_biz_quotation%')")"
[[ "$IDX_ALL" == "7" ]] && ok "7 条设计索引在位（头 5 + 行 2）" || bad "设计索引 = $IDX_ALL（应为 7）"
SEQ="$(q_db "select count(*) from biz_seq where seq_type='QT' and status=1 and deleted_at is null")"
[[ "$SEQ" == "1" ]] && ok "QT 取号行 = 1 行（启用）" || bad "QT 取号行 = $SEQ（应为 1）"
PROBE="$(q_db "select (select count(*) from biz_quotation)||'/'||(select count(*) from biz_quotation_line)")"
[[ "$PROBE" == "0/0" ]] && ok "升级后表内 0 行（自检探针已自清场）" || bad "升级后表内有 $PROBE 行（探针没清干净）"

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
[[ "$(q_db "select count(*) from information_schema.columns where table_name='biz_quotation'")" == "17" ]] \
    && ok "重启后结构不变（17 列）" || bad "重启后结构变了"

echo
echo "== 汇总：PASS=$PASS FAIL=$FAIL =="
[ "$FAIL" -eq 0 ] || exit 1

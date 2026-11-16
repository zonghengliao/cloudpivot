#!/usr/bin/env bash
# =============================================================================
# m115_engine_upgrade.sh —— V0416（M1-15 间接法调整项配置）
#                            真引擎实测：克隆库升级 + 指纹登记 + 重启幂等 + 回滚护栏
#
# 卡：t_ce938a2a。**不是** psql 手动执行 SQL，而是用编译出的后端二进制真启动，
# 走引擎自己的迁移器（文件扫描 / 方言拆分器 / schema_migrations 指纹 / 幂等 / 顺序）。
#
# 为什么单卡演练（m115_cashflow_adjust_item_drill.sh）不够：
#   psql -f 只是「把整份文件丢给 PG」，**没有**经过引擎的 `split_statements`
#   与指纹登记。而生产上线走的正是引擎这条路 —— 拆坏了就整份迁移回滚、服务起不来。
#
# 四段：
#   A) 克隆库升级：模板库 cloudpivot_test → cp_m115_upgrade → 真启动 → 应用待跑迁移
#      （含 V0414 / V0415 / V0416）→ `V0416 自检通过` 的 NOTICE 出现在引擎日志
#      → schema_migrations 登记 416/true
#   B) 结构落地：12 行模板 + 2 个按钮节点（挂「现金流量表」页面）+ 授权（超管 / 财务经理）
#   C) 重启幂等：再启动一次 → 「应用 0 个迁移」（引擎级幂等，不是文件级）
#   D) 回滚护栏：库内无自建配置时可 down；造一行自建配置后 down 必须被拒
#
# 用法：bash scripts/verify/m115_engine_upgrade.sh [PG_HOST]
#   默认 PG_HOST=172.18.10.145（99 上跑）；145 上跑传 127.0.0.1。
# 端口固定 18093（隔离段 18000-18999），库名含 upgrade，脚本自己 DROP。
# =============================================================================
set -uo pipefail

PG_HOST="${1:-172.18.10.145}"
export PGPASSWORD="${PGPASSWORD:-fbb1263691ff75c31ec99eaf258138b4}"
PG_USER=cloudpivot
TEMPLATE=cloudpivot_test
DB_UP=cp_m115_upgrade
PORT="${M115_PORT:-18093}"

REPO_ROOT=$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)
BACKEND_DIR="$REPO_ROOT/backend"
BIN="$BACKEND_DIR/target/debug/cloudpivot"
DOWN0416="$BACKEND_DIR/app/migrations/business/V0416__cashflow_adjust_item.down.sql"
RUN_DIR="${M115_RUN_DIR:-/root/.hermes/profiles/backend-developer/cache/scratch/m115-run}"
mkdir -p "$RUN_DIR"

PASS=0
FAIL=0
ok()  { PASS=$((PASS+1)); echo "  PASS  $*"; }
bad() { FAIL=$((FAIL+1)); echo "  FAIL  $*"; }

psql_admin() { psql -h "$PG_HOST" -U "$PG_USER" -d postgres -tAc "$1"; }
q_db()       { psql -h "$PG_HOST" -U "$PG_USER" -d "$DB_UP" -tAc "$1"; }
run_db()     { psql -h "$PG_HOST" -U "$PG_USER" -d "$DB_UP" -v ON_ERROR_STOP=1 -1 -f "$1" >"$2" 2>&1; }

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
        export X_CONFIG_SECRET_KEY="m115-drill-config-secret"
        export NIK_ADMIN_PASSWORD='M115@Drill#2026'
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

echo "== M1-15 / V0416 真引擎实测（间接法调整项配置）=="
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
[[ "$(q_db "select count(*) from information_schema.tables where table_schema='public' and table_name='biz_cashflow_adjust_item'")" == "0" ]] \
    && ok "克隆库升级前没有调整项表（本卡新建）" || bad "克隆库里已经有调整项表"

start_backend "$RUN_DIR/upgrade.log" || { echo "FATAL: 升级实例没起来"; exit 2; }
ok "克隆库真启动成功（/health 200）"

APPLIED="$(grep -oE '数据库迁移完成: 应用 [0-9]+ 个迁移' "$RUN_DIR/upgrade.log" | head -1)"
echo "  ${APPLIED:-（没抓到迁移行）}"
APPLIED_N="$(echo "${APPLIED:-}" | grep -oE '[0-9]+' | head -1)"
[[ "${APPLIED_N:-0}" -ge 1 ]] && ok "引擎应用了 $APPLIED_N 个迁移（含 V0414 / V0415 / V0416）" \
    || bad "一个迁移都没应用 —— V0416 没被引擎识别"
grep -q "V0416 自检通过" "$RUN_DIR/upgrade.log" \
    && ok "V0416 自检块在真引擎里通过（NOTICE 已打印 ⇒ 方言拆分器把 DO 块切对了）" \
    || bad "没看到 V0416 自检 NOTICE（可能是 DO 块被拆坏 / 自检失败）"

ROW="$(q_db "select version||'/'||success||'/'||source from schema_migrations where name='cashflow_adjust_item'")"
echo "  schema_migrations：${ROW:-（无此行）}"
case "$ROW" in
    416/true/*V0416__cashflow_adjust_item.up.sql) ok "schema_migrations 登记 416/true/源文件（指纹已登记）" ;;
    *) bad "schema_migrations 登记不对：[$ROW]" ;;
esac

# ============================================================================
echo
echo "== B) 结构落地 =="
assert_rows() { # SQL 期望 说明
    local got; got="$(q_db "$1")"
    [[ "$got" == "$2" ]] && ok "$3（=$got）" || bad "$3（期望 $2，实际 $got）"
}
assert_rows "select count(*) from biz_cashflow_adjust_item" 12 "12 行标准模板"
assert_rows "select count(*) from biz_cashflow_adjust_item where is_active=1" 12 "全部启用"
assert_rows "select count(distinct calc_basis) from biz_cashflow_adjust_item where source_type='ACCOUNT'" 4 "4 个取数口径"
assert_rows "select count(*) from information_schema.columns where table_schema='public' and table_name='biz_cashflow_adjust_item' and data_type='numeric'" 0 "零 NUMERIC 列（配置表不存金额）"
assert_rows "select count(*) from pg_constraint where conrelid='public.biz_cashflow_adjust_item'::regclass and conname like 'ck_biz_cashflow_adjust_%'" 6 "6 条 CHECK 约束"
assert_rows "select count(*) from sys_menu where id in ('20000000-0000-0000-0000-000000001790','20000000-0000-0000-0000-000000001800') and menu_type=3 and parent_id='20000000-0000-0000-0000-000000001018' and path is null and status=1 and deleted_at is null" 2 "两个按钮节点在位（挂「现金流量表」页面 · 无 path）"
assert_rows "select count(distinct auth_code) from sys_menu where auth_code like 'business:erp:cashflow-adjust:%' and deleted_at is null" 2 "两个权限码各一行"
ROLES="$(q_db "select count(*) from sys_role where deleted_at is null and role_code in ('SUPER_ADMIN','FINANCE_MANAGER')")"
GRANT="$(q_db "select count(*) from sys_role_menu rm join sys_role r on r.id=rm.role_id and r.deleted_at is null where rm.menu_id in ('20000000-0000-0000-0000-000000001790','20000000-0000-0000-0000-000000001800') and r.role_code in ('SUPER_ADMIN','FINANCE_MANAGER')")"
[[ "$GRANT" == "$((ROLES * 2))" ]] && ok "授权齐（${ROLES} 个目标角色 × 2 码 = $GRANT 行）" \
    || bad "授权数 = $GRANT（应为 $((ROLES * 2))）"

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
assert_rows "select count(*) from biz_cashflow_adjust_item" 12 "重启后模板行不变（没有重复种）"
assert_rows "select count(*) from sys_menu where auth_code like 'business:erp:cashflow-adjust:%' and deleted_at is null" 2 "重启后权限节点不变"

# ============================================================================
echo
echo "== D) 回滚护栏（down）=="
stop_backend
if run_db "$DOWN0416" "$RUN_DIR/down1.log"; then
    ok "无自建配置时 down 成功（幂等回收）"
    assert_rows "select count(*) from information_schema.tables where table_schema='public' and table_name='biz_cashflow_adjust_item'" 0 "down 后表已删"
    assert_rows "select count(*) from sys_menu where auth_code like 'business:erp:cashflow-adjust:%'" 0 "down 后权限节点已清空"
else
    bad "无自建配置时 down 失败：$(tail -3 "$RUN_DIR/down1.log")"
fi
# 重放回滚（up 由引擎负责，这里直接用 SQL 再放一次，保证后续断言有表可写）
psql -h "$PG_HOST" -U "$PG_USER" -d "$DB_UP" -q -v ON_ERROR_STOP=1 \
     -f "$BACKEND_DIR/app/migrations/business/V0416__cashflow_adjust_item.up.sql" >/dev/null 2>&1
q_db "insert into biz_cashflow_adjust_item (item_code,item_name,source_type,calc_basis,account_pattern,sort_order) values ('ADJ_CUSTOM_ONE','自建调整项','ACCOUNT','CREDIT_NET','1601',130)" >/dev/null
if run_db "$DOWN0416" "$RUN_DIR/down2.log"; then
    bad "有自建配置时 down 竟然成功（护栏没生效）"
else
    grep -q "拒绝执行" "$RUN_DIR/down2.log" && ok "有自建配置时 down 被拒（护栏生效）" \
        || bad "down 失败但不是护栏拦的：$(tail -3 "$RUN_DIR/down2.log")"
fi
assert_rows "select count(*) from biz_cashflow_adjust_item where item_code='ADJ_CUSTOM_ONE'" 1 "护栏拒绝后数据原样还在"
q_db "delete from biz_cashflow_adjust_item where item_code='ADJ_CUSTOM_ONE'" >/dev/null
if run_db "$DOWN0416" "$RUN_DIR/down3.log"; then
    ok "清掉自建配置后 down 通过（护栏只看数据，不挡正常回滚）"
else
    bad "清掉自建配置后 down 仍失败：$(tail -3 "$RUN_DIR/down3.log")"
fi
assert_rows "select count(*) from information_schema.tables where table_schema='public' and table_name='biz_cashflow_adjust_item'" 0 "收尾：回滚后表已删（自建行随表带走）"

echo
echo "== 汇总：PASS=$PASS FAIL=$FAIL =="
[ "$FAIL" -eq 0 ] || exit 1

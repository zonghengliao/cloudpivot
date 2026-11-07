#!/usr/bin/env bash
# =============================================================================
# setup_new_env.sh —— 在**全新机器**上从零建库：纯迁移重建 + 结构与生产对齐校验
#                     （卡 MENU-03「以后部署到新机器也不乱」）
#
# 用法：
#   bash scripts/setup_new_env.sh <新库名>
#
#   例：bash scripts/setup_new_env.sh cloudpivot_fresh
#   （跑完库**保留**，作为以后的结构对比基线；不需要时 dropdb <库名>）
#
# 它做什么（6 步，任一步失败即退出并说明原因）：
#   1. 建空库（owner 可指定；本机账号无 CREATEDB 权限时自动借 postgres 账号建）
#   2. **起一次真实后端实例**，让迁移引擎按 `schema_migrations` 把 backend/migrations
#      下全部版本化迁移跑完 + 完成 bootstrap（这是「复用现有的迁移执行逻辑」的正确做法：
#      手写 psql 逐条 apply 不会写 schema_migrations，也跑不到引擎的 seed/bootstrap 那一档）
#   3. 迁移完整性自检：schema_migrations 按 kind 对账（versioned ↔ V*.up.sql、
#      seed ↔ S__*.sql，条数必须全等且无失败）—— 少一条就说明有迁移被跳过
#   4. 跑 scripts/verify/menu_invariants.sh <新库> —— **必须 5 条全绿**
#   5. 输出菜单树摘要（顶级菜单清单 + 总数）
#   6. 结构比对（只比 path / parent_path / menu_type；name / title / sort / icon 允许不同）：
#      ① 新库 vs **生产结构快照**（老大关心的"新机器跟生产一样"）
#      ② 新库 vs **迁移链基线** menu_structure_baseline.txt（--strict，无规则；冻结的 canonical 结构，
#         不依赖生产库，生产没赶上最新迁移时也不会抖）
#      两者都走 menu_struct_compare.sh，比对前套用 baseline/menu_struct_equiv.tsv 里
#      **写明了理由**的等价表示（如 purchase ↔ purchase-order 别名），并同时打印原始差异
#      与归一后差异 —— 不靠静默忽略把红灯变绿。
#      结论：`✅ 新环境菜单结构与生产一致` 或列出差异明细
#
# 为什么禁止 `pg_dump | psql` 从别的机器复制库：
#   那会把别人的手工改库（临时调层级、手建的测试菜单、改过的 path）一起搬过来，
#   各环境结构从此分叉 —— 145 菜单乱就是这么来的。迁移链才是**唯一**结构来源。
#
# 环境变量（都有默认值，按需覆盖）：
#   REPO=<仓根>                 脚本不在仓内时指定（默认按脚本位置推导，再退回 /opt/cloudpivot）
#   PGHOST/PGPORT/PGUSER/PGPASSWORD   连接参数（未设时依次尝试 /etc/cloudpivot/*.env 的 NIK_DATABASE_*）
#   DB_OWNER=<角色>             新库 owner（默认 = PGUSER）
#   RECREATE=1                  库已存在时先删后建（默认拒绝，防止误删在用的库）
#   BIN=<可执行>                后端二进制（默认 $REPO/backend/target/debug/cloudpivot）
#   PORT=<端口>                 迁移实例监听端口（默认 18097，只在本机回环）
#   PROD_STRUCT_FILE=<文件>     生产结构快照（默认 $REPO/scripts/verify/baseline/prod_menu_structure.txt，
#                               找不到再试脚本同级 prod_menu_structure.txt —— 单文件分发时可只用这一份）
#   PROD_DB=<库名>              给了就用「连库现取」的生产结构，代替快照文件
#   BASE_STRUCT_FILE=<文件>     迁移链基线（默认 $REPO/scripts/verify/baseline/menu_structure_baseline.txt）
#   MENU_STRUCT_EQUIV=<文件>    等价表示登记表（默认 $REPO/scripts/verify/baseline/menu_struct_equiv.tsv；
#                               置 /dev/null 即关闭归一，此时等价表示会被算成差异）
#   ENV_FILE=<文件>             显式指定要 source 的环境变量文件（取库口令用）
#
# 退出码：0 = 全绿（不变量 + 结构比对都通过）｜1 = 有失败项 ｜2 = 环境/参数问题
# =============================================================================
set -uo pipefail
export PATH=/root/.cargo/bin:/usr/local/bin:$PATH

usage() {
    sed -n '3,10p' "${BASH_SOURCE[0]}" | sed 's/^# \{0,1\}//'
}

DB="${1:-}"
[ -n "$DB" ] || { usage >&2; exit 2; }
shift || true
[ "$#" -eq 0 ] || { echo "⛔ 未知参数：$*" >&2; usage >&2; exit 2; }
# 库名白名单：只允许小写字母/数字/下划线，防止 CREATE DATABASE 拼接注入
case "$DB" in
    *[!a-z0-9_]*|'') echo "⛔ 非法库名：$DB（只允许小写字母/数字/下划线）" >&2; exit 2 ;;
esac

# ── 0) 定位仓根与辅助脚本 ────────────────────────────────────────────────────
SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
if [ -d "$SCRIPT_DIR/../backend/migrations" ]; then
    ROOT="$(cd "$SCRIPT_DIR/.." && pwd)"
else
    ROOT="${REPO:-/opt/cloudpivot}"
fi
[ -d "$ROOT/backend/migrations" ] || {
    echo "⛔ 迁移目录不存在：$ROOT/backend/migrations（脚本不在仓内时请传 REPO=<仓根>）" >&2; exit 2; }

# 辅助脚本解析：优先「脚本同级」（不污染目标仓），再退回仓内 scripts/verify
helper() {
    local n=$1 c
    for c in "$SCRIPT_DIR/$n" "$ROOT/scripts/verify/$n" "$ROOT/scripts/$n"; do
        [ -f "$c" ] && { printf '%s' "$c"; return 0; }
    done
    return 1
}
INVARIANTS=$(helper menu_invariants.sh) || {
    echo "⛔ 找不到 menu_invariants.sh（应随本脚本一起分发，或存在于 $ROOT/scripts/verify/）" >&2; exit 2; }
DUMP=$(helper menu_structure_dump.sh) || {
    echo "⛔ 找不到 menu_structure_dump.sh（应随本脚本一起分发，或存在于 $ROOT/scripts/verify/）" >&2; exit 2; }
COMPARE=$(helper menu_struct_compare.sh) || {
    echo "⛔ 找不到 menu_struct_compare.sh（应随本脚本一起分发，或存在于 $ROOT/scripts/verify/）" >&2; exit 2; }

command -v psql >/dev/null 2>&1 || { echo "⛔ 没有 psql 客户端（装 postgresql-client）" >&2; exit 2; }

# ── 连接参数：显式环境变量 > 环境变量文件 > psql 默认 ─────────────────────────
if [ -z "${PGPASSWORD:-}" ]; then
    for f in "${ENV_FILE:-}" /etc/cloudpivot/cloudpivot.env \
             /etc/cloudpivot/cloudpivot-dev.env /etc/cloudpivot/cloudpivot-test.env; do
        [ -n "$f" ] && [ -f "$f" ] && { set -a; . "$f" 2>/dev/null; set +a; break; }
    done
fi
PGHOST="${PGHOST:-${NIK_DATABASE_HOST:-127.0.0.1}}"
PGPORT="${PGPORT:-${NIK_DATABASE_PORT:-5432}}"
PGUSER="${PGUSER:-${NIK_DATABASE_USER:-cloudpivot}}"
PGPASSWORD="${PGPASSWORD:-${NIK_DATABASE_PASSWORD:-}}"
DB_OWNER="${DB_OWNER:-$PGUSER}"
export PGPASSWORD PGHOST PGPORT PGUSER
PROFILE_NAME="${NIK_PROFILE:-test}"
PORT="${PORT:-18097}"
BIN="${BIN:-$ROOT/backend/target/debug/cloudpivot}"
LOG="${LOG:-/tmp/setup_new_env_${DB}.log}"

psql_q() { psql -h "$PGHOST" -p "$PGPORT" -U "$PGUSER" -d "$1" -tA -q -v ON_ERROR_STOP=1 -c "$2" 2>&1; }

echo "═══ 新环境初始化：库 $DB ═══"
echo "仓根: $ROOT"
echo "连接: $PGUSER@$PGHOST:$PGPORT（owner=$DB_OWNER，profile=$PROFILE_NAME，二进制=$BIN）"

# ── 1) 建空库 ────────────────────────────────────────────────────────────────
EXISTS=$(psql -h "$PGHOST" -p "$PGPORT" -U "$PGUSER" -d postgres -tA -q -v ON_ERROR_STOP=1 \
            -c "select 1 from pg_database where datname='$DB'" 2>&1)
EX_RC=$?
if [ "$EX_RC" -ne 0 ]; then
    echo "⛔ 连不上 PG（$PGUSER@$PGHOST:$PGPORT）：$EXISTS" >&2
    exit 2
fi
if [ "$EXISTS" = "1" ]; then
    if [ "${RECREATE:-0}" = "1" ]; then
        su - postgres -c "dropdb --force $DB" >/dev/null 2>&1 \
            || psql_q postgres "drop database \"$DB\"" >/dev/null \
            || { echo "⛔ 删库失败：$DB（RECREATE=1 需要 dropdb 权限）" >&2; exit 2; }
        echo "==> 已删除旧库：$DB（RECREATE=1）"
    else
        echo "⛔ 库已存在：$DB。要重建请加 RECREATE=1（会**删库**，先确认不是在用库）" >&2
        exit 2
    fi
fi
if ! psql_q postgres "create database \"$DB\" owner \"$DB_OWNER\"" >/dev/null; then
    su - postgres -c "createdb -O $DB_OWNER $DB" >/dev/null 2>&1 \
        || { echo "⛔ createdb 失败：$DB（当前账号 $PGUSER 无 CREATEDB 权限，且借 postgres 账号也失败）" >&2; exit 2; }
fi
echo "==> 空库已建：$DB（owner=$DB_OWNER）"

# ── 2) 起一次实例：迁移 + bootstrap ──────────────────────────────────────────
[ -x "$BIN" ] || { echo "⛔ 二进制不存在：$BIN（先 cd backend && cargo build，或传 BIN=…）" >&2; exit 2; }

# 布尔值取自环境变量文件时可能是 "true"/"1" 等形式，统一判空即可
export NIK_PROFILE="$PROFILE_NAME"
export NIK_DATABASE_HOST="$PGHOST"
export NIK_DATABASE_PORT="$PGPORT"
export NIK_DATABASE_NAME="$DB"          # 关键：覆盖环境变量文件里指向旧库的库名
export NIK_DATABASE_USER="$PGUSER"
export NIK_DATABASE_PASSWORD="$PGPASSWORD"
export NIK_SERVER_LISTEN="127.0.0.1:$PORT"
export NIK_ADMIN_PASSWORD="${NIK_ADMIN_PASSWORD:-Admin@123456}"
# 启动守卫要求的三把密钥：已有就复用（同一环境一致），没有就现生成（仅本次实例使用）
export NIK_MIDDLEWARE_AUTH_JWT_SECRET="${NIK_MIDDLEWARE_AUTH_JWT_SECRET:-$(openssl rand -hex 32)}"
export CLOUDPIVOT_STORAGE_MASTER_KEY="${CLOUDPIVOT_STORAGE_MASTER_KEY:-$(openssl rand -hex 32)}"
export X_CONFIG_SECRET_KEY="${X_CONFIG_SECRET_KEY:-$(openssl rand -hex 24)}"

: > "$LOG"
( cd "$ROOT/backend" && exec "$BIN" >>"$LOG" 2>&1 </dev/null ) &
PID=$!
echo "==> 迁移实例已启动 pid=$PID 端口=$PORT 日志=$LOG"

migrated=0
for _ in $(seq 1 180); do
    grep -q "数据库迁移完成" "$LOG" 2>/dev/null && { migrated=1; break; }
    grep -qE "数据库迁移失败|迁移失败" "$LOG" 2>/dev/null && break
    kill -0 "$PID" 2>/dev/null || break
    sleep 1
done
# bootstrap（角色 / 超管 / 字典预热）在迁移之后异步落库：等菜单行数稳定
if [ "$migrated" = "1" ]; then
    prev=-1; stable=0
    for _ in $(seq 1 30); do
        cur=$(psql_q "$DB" "select count(*) from sys_menu" 2>/dev/null)
        if [ "$cur" = "$prev" ]; then stable=$((stable + 1)); [ "$stable" -ge 2 ] && break
        else stable=0; fi
        prev="$cur"; sleep 1
    done
fi
kill "$PID" 2>/dev/null; wait "$PID" 2>/dev/null
echo "==> 实例已停止（迁移日志：$(grep -c "数据库迁移完成" "$LOG" 2>/dev/null || echo 0) 条完成标记）"

if [ "$migrated" != "1" ]; then
    echo "⛔ 迁移没有完成，启动日志尾部：" >&2
    tail -25 "$LOG" | sed 's/^/    /' >&2
    exit 1
fi

# ── 3) 迁移完整性自检：按 kind 对账 ─────────────────────────────────────────
# 引擎把两类迁移都记进 schema_migrations（kind 区分）：
#   versioned ↔ backend/migrations/**/V*__*.up.sql   表结构 / 数据修正
#   seed      ↔ backend/migrations/seed/S__*.sql     初始数据（菜单基线就在这里）
# 少一条就说明有迁移被跳过或文件被改动，新环境结构必然与生产分叉。
FILES_V=$(find "$ROOT/backend/migrations" -name 'V*__*.up.sql' | wc -l)
FILES_S=$(find "$ROOT/backend/migrations" -path '*/seed/*' -name 'S__*.sql' | wc -l)
ROW=$(psql_q "$DB" "select count(*) filter (where kind='versioned') || '|' ||
                           count(*) filter (where kind='seed') || '|' ||
                           count(*) filter (where not success) || '|' ||
                           coalesce(max(version::int) filter (where kind='versioned'), 0)
                      from schema_migrations")
APPLIED_V=${ROW%%|*}; ROW=${ROW#*|}
APPLIED_S=${ROW%%|*}; ROW=${ROW#*|}
FAILED=${ROW%%|*};    MAXVER=${ROW#*|}
echo "==> schema_migrations: versioned $APPLIED_V/$FILES_V 条 ｜ seed $APPLIED_S/$FILES_S 条 ｜ 失败 $FAILED 条 ｜ 最大版本 V$MAXVER"
if [ "$FAILED" != "0" ]; then
    echo "⛔ 有 $FAILED 条迁移执行失败（见日志 $LOG）" >&2
    exit 1
fi
if [ "$APPLIED_V" != "$FILES_V" ] || [ "$APPLIED_S" != "$FILES_S" ]; then
    echo "⛔ 迁移条数不一致：versioned 库内 $APPLIED_V / 仓库 $FILES_V，seed 库内 $APPLIED_S / 仓库 $FILES_S" >&2
    echo "   （有迁移被跳过、被改动，或仓库新增了迁移但二进制还是旧的 —— 先重新 build）" >&2
    exit 1
fi

# ── 4) 菜单结构不变量（5 条，必须全绿）────────────────────────────────────────
echo
echo "── 菜单结构不变量 ──────────────────────────────────────────────"
bash "$INVARIANTS" "$DB"
INV_RC=$?
if [ "$INV_RC" -ne 0 ]; then
    echo "⛔ 菜单结构不变量未全绿（rc=$INV_RC）：新环境的结构本身就坏了，先修迁移，不要放行上线" >&2
    exit 1
fi

# ── 5) 菜单树摘要 ───────────────────────────────────────────────────────────
trap 'rm -f "$LOG.pid"' EXIT
TOTAL=$(psql_q "$DB" "select count(*) from sys_menu where deleted_at is null")
TOP_ROWS=$(psql_q "$DB" "select menu_type || '|' || coalesce(path,'~NULL~') || '|' || coalesce(name,'')
                          from sys_menu where deleted_at is null and parent_id is null
                         order by sort_order, path")
TOP_N=$(printf '%s\n' "$TOP_ROWS" | grep -c . || true)

echo
echo "── 菜单树摘要 ──────────────────────────────────────────────────"
echo "菜单总数（未删）: $TOTAL"
echo "顶级菜单: $TOP_N 个"
printf '%s\n' "$TOP_ROWS" | awk -F'|' 'NF{printf "  [%s] %-28s %s\n", $1, $2, $3}'

# ── 6) 结构比对：① 与生产快照比（老大的原始诉求）② 与迁移链基线比（未来新机器准绳）──
# 口径：只比 path / parent_path / menu_type（见 menu_structure_dump.sh 头注释）。
# 两个比对都走 menu_struct_compare.sh，它会在比对前套用 baseline/menu_struct_equiv.tsv
# 里**已备案、写明了理由**的等价表示（例如 purchase ↔ purchase-order 别名），
# 并同时打印「原始差异」与「归一后差异」——不靠静默忽略把红灯变绿。
echo
echo "── 比对①：新库 vs 生产结构 ────────────────────────────────────"
if [ -n "${PROD_DB:-}" ]; then
    PROD_SRC="$PROD_DB"
    PROD_DESC="连库现取：$PROD_DB"
else
    if [ -z "${PROD_STRUCT_FILE:-}" ]; then
        for c in "$ROOT/scripts/verify/baseline/prod_menu_structure.txt" \
                 "$SCRIPT_DIR/prod_menu_structure.txt"; do
            [ -f "$c" ] && { PROD_STRUCT_FILE="$c"; break; }
        done
    fi
    [ -n "${PROD_STRUCT_FILE:-}" ] && [ -f "$PROD_STRUCT_FILE" ] || {
        echo "⛔ 找不到生产结构快照：${PROD_STRUCT_FILE:-<未找到>}" >&2
        echo "   在 144 生产机导出一份：bash menu_structure_dump.sh cloudpivot prod_menu_structure.txt" >&2
        echo "   或用 PROD_DB=<可连的生产库名> 现取（生产库只应在生产机上本机连）" >&2
        exit 2
    }
    PROD_SRC="$PROD_STRUCT_FILE"
    PROD_DESC="快照文件：$PROD_STRUCT_FILE"
fi
bash "$COMPARE" "$DB" "$PROD_SRC"
PROD_RC=$?

# 迁移链基线：一份「纯迁移重建」的 canonical 结构快照，随仓库提交。
# 它和下面的比对①互补——①证明"新机器跟生产一致"，②证明"新机器跟已冻结的基线一致"，
# 后者不依赖生产库、也不会因为生产没部署最新迁移而抖动。
BASE_FILE="${BASE_STRUCT_FILE:-}"
if [ -z "$BASE_FILE" ]; then
    for c in "$ROOT/scripts/verify/baseline/menu_structure_baseline.txt" \
             "$SCRIPT_DIR/menu_structure_baseline.txt"; do
        [ -f "$c" ] && { BASE_FILE="$c"; break; }
    done
fi
BASE_RC=0
if [ -n "$BASE_FILE" ] && [ -f "$BASE_FILE" ]; then
    echo
    echo "── 比对②：新库 vs 迁移链基线（canonical，未来新机器准绳）────────"
    bash "$COMPARE" "$DB" "$BASE_FILE" --strict
    BASE_RC=$?
else
    echo
    echo "⚠ 未找到迁移链基线（scripts/verify/baseline/menu_structure_baseline.txt），跳过比对②"
    echo "   生成方式：bash scripts/verify/menu_structure_dump.sh <本库> \\"
    echo "             scripts/verify/baseline/menu_structure_baseline.txt"
fi

echo
if [ "$PROD_RC" -eq 0 ] && [ "$BASE_RC" -eq 0 ]; then
    echo "✅ 新环境菜单结构与生产一致"
    echo "   比对① 与生产（$PROD_DESC）归一后 0 差异（已备案等价规则见 scripts/verify/baseline/menu_struct_equiv.tsv）"
    echo "   比对② 与迁移链基线（--strict）0 差异"
    rc=0
else
    echo "❌ 新环境菜单结构与生产**不一致**（比对① rc=$PROD_RC ｜ 比对② rc=$BASE_RC）"
    echo "   差异只有两种来源，处理方式都不是拷库："
    echo "   ① 有人手工改过库 → 补一条幂等迁移把修正固化（参考 V0319/V0320 的写法）"
    echo "   ② 迁移链漏了/改了结构 → 补迁移，或确认后重新生成基线（commit 里写清为什么）"
    echo "   禁止 pg_dump | psql 跨机复制库：会把别人的手工改动一起搬过来，环境就此分叉。"
    rc=1
fi

echo
echo "库 $DB 保留（供以后对比基线）；不需要时：dropdb $DB"
exit "$rc"

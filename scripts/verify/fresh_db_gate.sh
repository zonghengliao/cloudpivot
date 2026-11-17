#!/usr/bin/env bash
# ======================================================================
# fresh_db_gate.sh —— 「空库首启」行为闸（卡 t_a0099024，补 t_08e1816a 的静态闸）
#
# ============================ 为什么需要它 ============================
# t_08e1816a 的 P0 真因是「版本化迁移 V0442 把 seed 行当硬前置条件」，而内核执行
# 次序固定为 baseline(0) → versioned(1) → repeatable(2) → seed(3) ⇒ **任何空库首启
# 必然启动失败**（make_full_suite_db.sh 造不出库、L3 干净检出闸与新客户部署全被挡）。
#
# 那一轮补的是**静态闸** `scripts/lint/seed_dependency_gate.py`（L0）—— 它抓「写法」：
# DO 块里 `SELECT count(*) INTO v … WHERE id=…` 紧跟 `IF v<>1 THEN RAISE EXCEPTION`。
#
# 静态闸抓不到**语义上**的同类问题：
#   · 迁移用 `JOIN` 动态解析了一个「seed 才建」的父节点 —— 语法完美，空库首启拿不到；
#   · 迁移依赖某个基线/种子表结构、依赖某条 repeatable 的结果、依赖某角色/字典行；
#   · 迁移之间互相依赖的次序（前面某条在空库上失败 → 后面的全不执行）。
# 这类问题只有一个判据可靠：**真的在一个空库上跑一次全量迁移 + bootstrap**。
# 本脚本就是那道闸 —— 把 `scripts/verify/make_full_suite_db.sh`（原先只是被动工具，
# 没有任何一层闸会跑它）挂进 `bash scripts/check.sh`。
#
# ============================== 判据 ==============================
# 六条，全硬（缺一条就是「空库首启起不来」）：
#   ① 迁移跑满：`schema_migrations` 的 versioned 行数 == 磁盘上 `V####__*.up.sql` 的条数
#      （旧版 V0442 实测停在 361 / 磁盘 363 ⇒ 立刻红）
#   ② 版本对齐：库内 `max(version)` == 磁盘最大版本号
#   ③ 0 条失败迁移：`select count(*) from schema_migrations where success = false` = 0
#   ④ bootstrap 跑完：`sys_user.username='admin'` 且绑定 `SUPER_ADMIN` 恰好 1 行
#      （与 make_full_suite_db.sh 同一条判据，直接复用）
#   ⑤ 日志无 `数据库迁移失败` / `FATAL`
#   ⑥ 实例起得来：**迁移完的库再启动一次**，/health 返回 200，且跑满 N 秒仍存活
#      （这一步顺带守住「已应用迁移的指纹漂移 / divergent」这类重启才炸的错）
#
# 失败时打**真因**：日志里 `V#### …` / `FATAL` / `query failed` / `[P0000n]` 的行
# 逐条打出来。t_08e1816a 已把驱动消息 + SQLSTATE 带进 `Display`
# （backend/vendor/nik-db-core/src/error.rs::DriverMessage），所以这里能看到
# `V0442: 祖先目录「系统管理」… [P0001]` 这类可执行的信息，而不是 `db error`。
#
# ============================ 四种模式 ============================
#   auto（默认）  前置条件缺一个就 **跳过（exit 0）** 并说明原因：
#                   · 缺 psql/createdb/dropdb
#                   · 连不上库（没有 ERP_DB_URL / NIK_DATABASE_* / 默认库口令）
#                   · 没有可执行二进制（backend/target/debug/cloudpivot）
#                   · 没有建库权限（createdb 被拒）
#                 —— 与 config_category_gate.py / assignee_valid.py 同风格：CI 与本机
#                    没有库是正常状态，不该把整个 check.sh 打红。
#   block         CI / 发版 / 演练强制真跑：缺前置条件 → **红**；缺二进制 → 先
#                 `cargo build --bin cloudpivot` 再跑。
#   warn          真跑，但红了不阻断（存量过渡期）。
#   off           跳过（逃生门）。
#
# ============================== 安全 ==============================
#   · 临时库名唯一：`cloudpivot_freshgate_<pid>_<YYYYmmddHHMMSS>`，**绝不碰既有库**；
#   · 跑完/失败/被 Ctrl-C：trap 里 `dropdb --force` + 杀掉自己起的实例；
#   · 收尾断言：自己的库在 `pg_database` 里必须已经不存在。
#
# ============================== 用法 ==============================
#   bash scripts/verify/fresh_db_gate.sh                # auto（check.sh --fresh-db 就这么调）
#   FRESH_DB_GATE=block bash scripts/check.sh --fresh-db  # 强制真跑
#   ERP_DB_URL='postgres://user:pw@host:5432/cloudpivot_test' \
#       bash scripts/verify/fresh_db_gate.sh            # 显式给库（只取 host/port/user/pw，不碰该库）
#   FRESH_DB_GATE=off bash scripts/check.sh --full       # 逃生门
#
# 环境变量：
#   FRESH_DB_GATE        auto（默认）/ block / warn / off
#   ERP_DB_URL           连接信息来源 ①（只解析 host/port/user/password，**不会写这个库**）
#   NIK_DATABASE_HOST/PORT/USER/PASSWORD  来源 ②
#   PGHOST/PGPORT/PGUSER/PGPASSWORD       来源 ③（libpq 惯例，优先级次于 ② 的显式值）
#   BIN                  被测二进制（默认 backend/target/debug/cloudpivot）
#   OWNER                新建库的属主（默认由既有库 sys_menu 表属主反推，再退回库账号）
#   FRESH_DB_TIMEOUT     建库+迁移+bootstrap 的总超时，默认 900s
#   FRESH_DB_READY_SECONDS  「跑满 N 秒仍健康」的 N，默认 5s
#   FRESH_DB_DEMO_SEED=1  起实例时打开演示组织种子（默认 **0**：纯空库首启 = 新客户部署形态）
#   FRESH_DB_KEEP_DB=1    出问题要复盘时保留临时库（默认 0：一定清场）
#
# 退出码：0 = 通过 / 跳过    1 = 不过（红）    2 = 用法错误
# ======================================================================
set -uo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
SELF="${BASH_SOURCE[0]}"

MODE="${1:-${FRESH_DB_GATE:-auto}}"
case "$MODE" in
    auto|block|warn|off) ;;
    -h|--help)
        sed -n '2,90p' "$SELF" | sed -E 's/^# ?//'
        exit 0
        ;;
    *)
        echo "未知模式：$MODE（支持 auto / block / warn / off）" >&2
        exit 2
        ;;
esac

if [ "$MODE" = "off" ]; then
    echo "==> 空库首启行为闸（FRESH_DB_GATE=off → 跳过）"
    exit 0
fi

# ----------------------------------------------------------------------
# 连接信息：三种来源，只用于「连上去 + 建临时库」，绝不读写既有业务库
# ----------------------------------------------------------------------
DB_HOST="" DB_PORT="" DB_USER="" DB_PASS=""   # 全部留空：只从 ERP_DB_URL / NIK_DATABASE_* / PG* / 默认值取，
                                              # 不吃环境里可能残留的 DB_*（别的脚本的中间变量）
DB_PASS_FALLBACK="fbb1263691ff75c31ec99eaf258138b4"   # 与 make_full_suite_db.sh 的 DB_PASS 默认值同一口径（145 演练库）

if [ -n "${ERP_DB_URL:-}" ]; then
    # 用 python 解析（密码里可能有 @ / : 等），shlex.quote 保证 eval 安全
    if ! command -v python3 >/dev/null 2>&1; then
        echo "  ✗ 有 ERP_DB_URL 但本机没有 python3（解析连接串需要它）" >&2
        exit 1
    fi
    eval "$(python3 - "$ERP_DB_URL" <<'PY'
import shlex
import sys
import urllib.parse as u

p = u.urlparse(sys.argv[1].strip())
for key, val in (
    ("DB_HOST", p.hostname),
    ("DB_PORT", p.port),
    ("DB_USER", p.username),
    ("DB_PASS", p.password),
):
    val = "" if val is None else str(val)
    print(f"{key}={shlex.quote(val)}")
PY
)"
    SRC_DESC="ERP_DB_URL"
else
    SRC_DESC="NIK_DATABASE_* / PG* / 默认值"
fi

DB_HOST="${DB_HOST:-${NIK_DATABASE_HOST:-${PGHOST:-}}}"
DB_PORT="${DB_PORT:-${NIK_DATABASE_PORT:-${PGPORT:-5432}}}"
DB_USER="${DB_USER:-${NIK_DATABASE_USER:-${PGUSER:-cloudpivot}}}"
DB_PASS="${DB_PASS:-${NIK_DATABASE_PASSWORD:-${PGPASSWORD:-$DB_PASS_FALLBACK}}}"

# 没给 host 时按「本机是不是 145」决定：99 走 145，145 走 loopback
# （与 make_full_suite_db.sh 同一条判据；不看连通性，避免把 99 上的 127.0.0.1:5432 转发当成 PG）
if [ -z "$DB_HOST" ]; then
    if hostname -I 2>/dev/null | tr ' ' '\n' | grep -qx '172.18.10.145'; then
        DB_HOST=127.0.0.1
    else
        DB_HOST=172.18.10.145
    fi
fi

CONN_BASE=( -h "$DB_HOST" -p "$DB_PORT" -U "$DB_USER" )
psql_q() { # <dbname> <sql> → 单值（-A -t）；失败返回非 0
    PGPASSWORD="$DB_PASS" psql -X -q -A -t -v ON_ERROR_STOP=1 \
        "${CONN_BASE[@]}" -d "$1" -c "$2" 2>/dev/null
}

echo "==> 空库首启行为闸（fresh_db_gate.sh，模式 $MODE）"
echo "    库主机：$DB_HOST:$DB_PORT（账号 $DB_USER，来源 $SRC_DESC）"

# ----------------------------------------------------------------------
# 「跳过 / 红」统一出口
# ----------------------------------------------------------------------
skip_reason() { # <原因>：auto/warn → 跳过（0）；block → 红（1）
    if [ "$MODE" = "block" ]; then
        echo "  ✗ FRESH_DB_GATE=block 要求真跑，但$1" >&2
        echo "    修法：见脚本头注释「四种模式」；本机演练可直接 bash scripts/verify/make_full_suite_db.sh" >&2
        return 1
    fi
    echo "  ℹ 跳过：$1"
    echo "    （本层不是 push 前必跑；CI / 发版 / 演练用 FRESH_DB_GATE=block 强制真跑）"
    return 0
}

# ----------------------------------------------------------------------
# 清场：无论怎么退出，都不许留下临时库和实例
# ----------------------------------------------------------------------
DB=""
DB_CREATED=0
BOOT_PID=""
PIDFILE=""
MKLOG=""
PROBE_LOG=""
KEEP_DB="${FRESH_DB_KEEP_DB:-0}"
cleanup() {
    local rc=$?
    if [ -n "$BOOT_PID" ] && kill -0 "$BOOT_PID" 2>/dev/null; then
        kill -9 "$BOOT_PID" 2>/dev/null
    fi
    if [ -n "$PIDFILE" ] && [ -f "$PIDFILE" ]; then
        kill -9 "$(cat "$PIDFILE")" 2>/dev/null
        rm -f "$PIDFILE"
    fi
    if [ -n "$DB" ] && [ "$DB_CREATED" = "1" ]; then
        if [ "$KEEP_DB" = "1" ]; then
            echo "  ℹ FRESH_DB_KEEP_DB=1：保留临时库 $DB（复盘完请手动 drop：dropdb -h $DB_HOST -U $DB_USER --force $DB）"
        else
            PGPASSWORD="$DB_PASS" dropdb "${CONN_BASE[@]}" --if-exists --force "$DB" >/dev/null 2>&1
            local left
            left=$(psql_q postgres "select count(*) from pg_database where datname = '$DB'" || echo "?")
            if [ "$left" = "0" ]; then
                echo "    清场：临时库 $DB 已删除"
            else
                echo "    ⚠ 清场失败：$DB 仍在 pg_database 里（count=$left），请手动 dropdb --force" >&2
            fi
        fi
    fi
    return "$rc"
}
trap cleanup EXIT INT TERM

# ----------------------------------------------------------------------
# 判据小工具
# ----------------------------------------------------------------------
# 日志里带 ANSI 色码（tracing 默认上色，重定向到文件也照写）⇒ 先去掉再判。
# LOGLVL = 「一条日志行的级别前缀」，判据只认这种行：日志同时有整份迁移 SQL 的 DEBUG
# 转储，迁移文件**注释正文**里就写着 `FATAL` / `数据库迁移失败`（实测 V0359 的注释），
# 裸 grep 会把它算成失败 —— 这是本闸第一版实测踩到的假红。
strip_ansi() { sed -E 's/\x1b\[[0-9;]*[a-zA-Z]//g'; }
LOGLVL='^[0-9]{4}-[0-9]{2}-[0-9]{2}T[0-9:.]+Z +'
# 失败信号：级别是 ERROR/FATAL 的行，或无时间戳的 `FATAL: …` / `ERROR: …`，
# 或级别行里点名「数据库迁移失败 / 服务启动失败」。
LOG_FAIL_RE="${LOGLVL}(ERROR|FATAL)\b|^(FATAL|ERROR)[:：]|${LOGLVL}[A-Z]+ .*(数据库迁移失败|服务启动失败)"

CRIT_FAIL=0
crit() { # <标签> <期望> <实际>
    local label="$1" want="$2" got="$3"
    if [ "$want" = "$got" ]; then
        printf '    %-42s %s ✓\n' "$label" "$got"
        return 0
    fi
    printf '    %-42s %s ✗（期望 %s）\n' "$label" "${got:-<空>}" "$want"
    CRIT_FAIL=$((CRIT_FAIL + 1))
    return 1
}

diagnose_log() { # <日志> [标题]
    local log="$1" title="${2:-失败真因}"
    echo "  ── $title ──────────────────────────────────────────"
    if [ ! -s "$log" ]; then
        echo "    （日志为空：$log）"
        return 0
    fi
    # 迁移护栏 / 驱动错误 / 启动失败：这些行就是可执行的真因。
    # ⚠ 必须先去 ANSI 色码（tracing 写文件也上色，不去色就 grep 不到日志级别），
    #    并且只认「失败信号」—— 日志里有整份迁移 SQL 的 DEBUG 转储，迁移文件**正文**里
    #    就写着 `FATAL` / `数据库迁移失败` / `RAISE EXCEPTION 'V0406: …'`（实测），
    #    裸 grep `FATAL` 或裸 grep `V[0-9]{4}:` 都会被这些正文淹没（本闸第一版实测踩到）。
    local hit
    hit=$(strip_ansi < "$log" \
        | grep -v ' DEBUG ' \
        | grep -vE '^[[:space:]]*--' \
        | grep -nE "$LOG_FAIL_RE|\[P[0-9A-Z]{5}\]|was modified after it was applied|divergent" \
        | head -20)
    if [ -n "$hit" ]; then
        printf '%s\n' "$hit" | sed 's/^/    /'
    else
        echo "    （没有 ERROR/FATAL 级别的行）"
    fi
    echo "    ── 日志尾部（$log）"
    strip_ansi < "$log" | grep -v ' DEBUG ' | tail -10 | sed 's/^/    /'
}

# ----------------------------------------------------------------------
# 主流程
# ----------------------------------------------------------------------
main() {
    local miss=""
    for c in psql createdb dropdb curl openssl; do
        command -v "$c" >/dev/null 2>&1 || miss="$miss $c"
    done
    if [ -n "$miss" ]; then
        skip_reason "缺命令行工具$miss（本机没装 PG 客户端 / curl / openssl）" || return 1
        return 0
    fi

    if ! psql_q postgres "select 1" >/dev/null; then
        skip_reason "连不上库 $DB_HOST:$DB_PORT（没有可用连接信息 / 库没起）—— 先导出 ERP_DB_URL 或 NIK_DATABASE_*" || return 1
        return 0
    fi

    # ---- 被测二进制 ---------------------------------------------------
    BIN="${BIN:-$ROOT/backend/target/debug/cloudpivot}"
    if [ ! -x "$BIN" ]; then
        if [ "$MODE" = "block" ]; then
            echo "  … 二进制不存在（$BIN），block 模式先编译：cargo build --bin cloudpivot"
            if ! ( cd "$ROOT/backend" && cargo build --bin cloudpivot ); then
                echo "  ✗ 编译失败，无法真跑空库首启闸" >&2
                return 1
            fi
        else
            skip_reason "没有可执行二进制 $BIN（先 cd backend && cargo build --bin cloudpivot，或用 BIN=… 指定）" || return 1
            return 0
        fi
    fi
    echo "    二进制：$BIN"

    # 二进制比源码旧 → 迁移内容是按 config 的 `[database.migration].path` **运行时**读盘的
    # （所以换迁移文件不用重编），但 bootstrap 是 Rust 代码，旧二进制上跑等于测旧代码。
    # auto/warn：如实告警后继续（别把「还没编译」变成「不能跑」）；block：先重编。
    local newest_src
    newest_src=$(find "$ROOT/backend/app/src" "$ROOT"/backend/crates/*/src -name '*.rs' \
        -newer "$BIN" 2>/dev/null | head -1)
    if [ -n "$newest_src" ]; then
        if [ "$MODE" = "block" ]; then
            echo "  … 二进制比源码旧（例：$newest_src），block 模式先重编"
            if ! ( cd "$ROOT/backend" && cargo build --bin cloudpivot ); then
                echo "  ✗ 编译失败，无法真跑空库首启闸" >&2
                return 1
            fi
        else
            echo "    ℹ 二进制比源码旧（例：$newest_src）：迁移按 config 运行时读盘，内容是最新的；"
            echo "      但 bootstrap 是旧代码。要覆盖代码侧改动请先 cd backend && cargo build --bin cloudpivot"
        fi
    fi

    # ---- 库属主（与 make_full_suite_db.sh 同一推导，显式传下去避免它再推一次） ----
    OWNER="${OWNER:-}"
    if [ -z "$OWNER" ]; then
        local probe
        for probe in cloudpivot_test cloudpivot_dev cloudpivot; do
            OWNER=$(psql_q "$probe" "select tableowner from pg_tables where tablename='sys_menu'" || true)
            [ -n "$OWNER" ] && break
        done
    fi
    OWNER="${OWNER:-$DB_USER}"

    # ---- 临时库：唯一名 + 建库权限探测（建不出来 = 没有建库权限） ----
    DB="cloudpivot_freshgate_$$_$(date +%Y%m%d%H%M%S)"
    local cdb_err
    if ! cdb_err=$(PGPASSWORD="$DB_PASS" createdb "${CONN_BASE[@]}" -O "$OWNER" "$DB" 2>&1); then
        skip_reason "没有建库权限（createdb $DB 被拒：${cdb_err:-无输出}）" || return 1
        return 0
    fi
    DB_CREATED=1
    echo "    临时库：$DB（owner=$OWNER，跑完 dropdb --force）"

    # ---- 磁盘上的版本化迁移（判据 ①② 的期望值） ----------------------
    local disk_files disk_count disk_max
    disk_files=$(ls "$ROOT"/backend/app/migrations/*/V[0-9][0-9][0-9][0-9]__*.up.sql 2>/dev/null)
    disk_count=$(printf '%s\n' "$disk_files" | grep -c . || true)
    disk_max=$(printf '%s\n' "$disk_files" | sed 's#.*/V##' | cut -c1-4 | sort -n | tail -1)
    # 防假绿基数断言（orphan_button_gate 同款）：扫不到迁移 = 目录指错了，不许静默变绿
    if [ "${disk_count:-0}" -lt 100 ] || [ -z "$disk_max" ]; then
        echo "  ✗ 扫描基数异常：磁盘上只找到 ${disk_count:-0} 个版本化迁移（应有 >100），" >&2
        echo "    迁移目录指错了？本闸没有真扫到东西就不许判绿。" >&2
        return 1
    fi
    # 文件名是 4 位补零（V0445），库里 `version::int` 是 445 ⇒ 比较必须按整数
    disk_max=$((10#$disk_max))

    # ---- 起一次实例：迁移 + bootstrap（复用 make_full_suite_db.sh） ----
    local t0=$SECONDS
    local port pid_port pg_remote=0
    # 99 上的 127.0.0.1:5432 是到 145 的隧道（卡面禁止当本机 PG 用）：只有「本机真有 PG 集群」
    # 才让 make_full_suite_db.sh 走 `su - postgres`。判据是 **unix socket 存在**——
    # 99 上 postgres 系统用户还在（uid=114）但集群已 purge、socket 目录都不存在，
    # 只看 `id postgres` 会误判成本机（实测：改成 -h/-U 远端路径才对）。
    case "$DB_HOST" in
        127.0.0.1|localhost)
            if ! id postgres >/dev/null 2>&1 || ! ls /var/run/postgresql/.s.PGSQL.* >/dev/null 2>&1; then
                pg_remote=1
            fi
            ;;
    esac
    port=$(python3 - <<'PY' 2>/dev/null || echo 18242
import socket
s = socket.socket()
s.bind(("127.0.0.1", 0))
print(s.getsockname()[1])
s.close()
PY
)
    MKLOG=$(mktemp "${TMPDIR:-/tmp}/fresh_db_gate.XXXXXX.log")
    PIDFILE=$(mktemp "${TMPDIR:-/tmp}/fresh_db_gate.XXXXXX.pid")
    rm -f "$PIDFILE"
    echo "==> 起实例（空库：migrations + bootstrap；演示种子 $([ "${FRESH_DB_DEMO_SEED:-0}" = "1" ] && echo 开 || echo 关)）"
    local mk_rc=0
    # 与 make_full_suite_db.sh 共用同一套密钥：两次启动必须用同一把（否则「迁移完再启动」不比）
    local jwt storage_key
    jwt="$(openssl rand -hex 32)"
    storage_key="$(openssl rand -hex 32)"
    env \
        REPO="$ROOT" ERP_DB="$DB" BIN="$BIN" PORT="$port" \
        LOG="$MKLOG" PIDFILE="$PIDFILE" SELFTEST=0 \
        NO_DEMO_SEED="$([ "${FRESH_DB_DEMO_SEED:-0}" = "1" ] && echo "" || echo 1)" \
        PGHOST="$DB_HOST" PGPORT="$DB_PORT" DB_USER="$DB_USER" DB_PASS="$DB_PASS" OWNER="$OWNER" \
        PG_REMOTE="$pg_remote" \
        NIK_MIDDLEWARE_AUTH_JWT_SECRET="$jwt" CLOUDPIVOT_STORAGE_MASTER_KEY="$storage_key" \
        timeout --signal=TERM --kill-after=30 "${FRESH_DB_TIMEOUT:-900}" \
        bash "$ROOT/scripts/verify/make_full_suite_db.sh" > "$MKLOG.out" 2>&1 || mk_rc=$?
    cat "$MKLOG.out" 2>/dev/null | grep -E '^(==>|!!)' | tail -12 | sed 's/^/    /'
    local mk_elapsed=$((SECONDS - t0))
    echo "    首次启动（迁移 + bootstrap）耗时 ${mk_elapsed}s（rc=$mk_rc）"

    if [ "$mk_rc" -ne 0 ]; then
        diagnose_log "$MKLOG" "空库首启失败真因"
        echo "    （make_full_suite_db.sh 自身输出见 $MKLOG.out）"
        return 1
    fi

    # ---- 判据 ①②③④⑤ ------------------------------------------------
    echo "==> 判据"
    local db_ver_n db_ver_max fail_n admin_n
    db_ver_n=$(psql_q "$DB" "select count(*) from schema_migrations where kind = 'versioned'" || echo "?")
    db_ver_max=$(psql_q "$DB" "select max(version::int) from schema_migrations where kind = 'versioned'" || echo "?")
    fail_n=$(psql_q "$DB" "select count(*) from schema_migrations where success = false" || echo "?")
    admin_n=$(psql_q "$DB" "select count(*) from sys_user u
                              join sys_user_role ur on ur.user_id = u.id
                              join sys_role r on r.id = ur.role_id
                             where u.username = 'admin' and r.role_code = 'SUPER_ADMIN'" || echo "?")

    crit "① 迁移跑满（versioned 行数 = 磁盘条数）" "$disk_count" "$db_ver_n"
    crit "② 版本对齐（max(version) = 磁盘最大）"    "$disk_max"   "$db_ver_max"
    crit "③ 失败迁移行数"                          "0"           "$fail_n"
    crit "④ bootstrap：admin 绑定 SUPER_ADMIN"     "1"           "$admin_n"

    local fatal_hit
    fatal_hit=$(strip_ansi < "$MKLOG" | grep -cE "$LOG_FAIL_RE" || true)
    crit "⑤ 日志里「数据库迁移失败 / FATAL」行数"   "0"           "${fatal_hit:-0}"
    if [ "${fatal_hit:-0}" != "0" ]; then
        diagnose_log "$MKLOG" "日志里的 FATAL 行"
    fi

    # ---- 判据 ⑥ 实例起得来（迁移完的库再启动，跑满 N 秒仍健康） --------
    local ready_s="${FRESH_DB_READY_SECONDS:-5}"
    local port2
    port2=$(python3 - <<'PY' 2>/dev/null || echo 18243
import socket
s = socket.socket()
s.bind(("127.0.0.1", 0))
print(s.getsockname()[1])
s.close()
PY
)
    PROBE_LOG=$(mktemp "${TMPDIR:-/tmp}/fresh_db_gate_boot.XXXXXX.log")
    echo "==> 判据 ⑥：迁移完的库再启动一次（/health + 跑满 ${ready_s}s）"
    (
        cd "$ROOT/backend" || exit 1
        NIK_PROFILE=test \
        NIK_DATABASE_NAME="$DB" NIK_DATABASE_HOST="$DB_HOST" \
        NIK_DATABASE_PORT="$DB_PORT" NIK_DATABASE_USER="$DB_USER" \
        NIK_DATABASE_PASSWORD="$DB_PASS" \
        NIK_SERVER_LISTEN="127.0.0.1:$port2" \
        NIK_MIDDLEWARE_AUTH_JWT_SECRET="$jwt" \
        CLOUDPIVOT_STORAGE_MASTER_KEY="$storage_key" \
        NIK_ADMIN_PASSWORD="${NIK_ADMIN_PASSWORD:-Admin@123456}" \
        NIK_SEED_DEMO_USERS="$([ "${FRESH_DB_DEMO_SEED:-0}" = "1" ] && echo true || echo false)" \
        exec "$BIN"
    ) > "$PROBE_LOG" 2>&1 < /dev/null &
    BOOT_PID=$!

    local health=0 i
    for i in $(seq 1 90); do
        if curl -fsS -m 2 "http://127.0.0.1:$port2/health" >/dev/null 2>&1; then
            health=1
            break
        fi
        kill -0 "$BOOT_PID" 2>/dev/null || break
        sleep 1
    done
    sleep "$ready_s"
    local alive=0 health2=0
    kill -0 "$BOOT_PID" 2>/dev/null && alive=1
    if [ "$health" = "1" ] && curl -fsS -m 2 "http://127.0.0.1:$port2/health" >/dev/null 2>&1; then
        health2=1
    fi
    if [ "$health2" = "1" ] && [ "$alive" = "1" ]; then
        printf '    %-42s %s ✓\n' "⑥ /health 200 且跑满 ${ready_s}s 仍存活" "PID $BOOT_PID"
    else
        printf '    %-42s %s ✗（/health=%s 存活=%s）\n' "⑥ /health 200 且跑满 ${ready_s}s 仍存活" "失败" "$health" "$alive"
        CRIT_FAIL=$((CRIT_FAIL + 1))
        diagnose_log "$PROBE_LOG" "二次启动日志"
    fi
    # 二次启动的日志也不许有 FATAL / 迁移失败（守卫「已应用迁移指纹漂移」这类只在重启炸的错）
    local fatal2
    fatal2=$(strip_ansi < "$PROBE_LOG" | grep -cE "${LOG_FAIL_RE}|${LOGLVL}[A-Z]+ .*(was modified after it was applied|divergent)" || true)
    crit "⑥b 二次启动日志无 FATAL / 迁移失败"      "0"           "${fatal2:-0}"
    if [ "${fatal2:-0}" != "0" ]; then
        diagnose_log "$PROBE_LOG" "二次启动失败真因"
    fi
    # 优雅停：先 TERM 让插件/调度器正常收工（日志干净、不产生 `Killed` 噪声），再兜底 KILL
    if [ -n "$BOOT_PID" ]; then
        kill -TERM "$BOOT_PID" 2>/dev/null
        local w
        for w in 1 2 3 4 5 6; do
            kill -0 "$BOOT_PID" 2>/dev/null || break
            sleep 1
        done
        disown "$BOOT_PID" 2>/dev/null || true
        kill -9 "$BOOT_PID" 2>/dev/null || true
        BOOT_PID=""
    fi

    local elapsed=$((SECONDS - t0))
    echo "==> 耗时：${elapsed}s（首次启动 ${mk_elapsed}s + 判据 ⑥ ${ready_s}s+）；迁移 ${db_ver_n}/${disk_count}，max version ${db_ver_max}"

    if [ "$CRIT_FAIL" -gt 0 ]; then
        if [ "$MODE" = "warn" ]; then
            echo "  ⚠ 空库首启闸有 $CRIT_FAIL 条判据未通过（FRESH_DB_GATE=warn → 不阻断）" >&2
            return 0
        fi
        echo "  ✗ 空库首启闸未通过：$CRIT_FAIL 条判据红 —— 即「新库 / 新客户部署 / CI 干净检出」起不来。" >&2
        echo "    完整首启日志：$MKLOG" >&2
        echo "    完整二次启动日志：$PROBE_LOG" >&2
        return 1
    fi

    echo "  ✓ 空库首启通过：全量迁移 + bootstrap 跑完、0 条失败迁移、实例起得来（${elapsed}s）"
    echo "    首启日志：$MKLOG"
    return 0
}

main
exit $?

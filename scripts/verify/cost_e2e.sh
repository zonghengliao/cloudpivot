#!/usr/bin/env bash
# ============================================================
# COST-B4-5 · 成本核算端到端演练（ARCH-9 §7.4 第 4 批门槛）
#
#   calc → 审 → 结算 → 对平 → 红冲 → 重算 → 再结算
#
# **一条命令**（自己建演练库、起隔离实例、跑完、清场）：
#
#   bash scripts/verify/cost_e2e.sh
#
# 常用开关：
#   --keep            跑完保留演练库与实例（排查用；库名会打印）
#   --db NAME         演练库名（默认 cost_e2e_drill；必须含 drill/e2e/test）
#   --port N          演练实例端口（默认 18140）
#   --period yyyy-MM  演练期间（默认当月，北京时间）
#   --out DIR         证据目录（默认 docs/evidence/<日期>-cost-b4-5-e2e）
#   --no-build        跳过 cargo build（用现有二进制）
#   --inject KIND     造错模式：bypass4101（旁路记 4101）/ drop_alloc（删一行分摊结果）
#                     → 验证结算被指定断言拦下、报文点名、修好后同一张单能重试
#   --up / --down     只准备（建库 + 起实例）/ 只清场（杀实例 + 删库）
#
# 退出码：0 = 全链路断言全过；非 0 = 有断言失败（日志里 `[FAIL] STEP n` 指到具体步与
# 期望/实际），或环境准备失败（会直接打印原因）。
#
# ---------------------------------------------------------------------------
# 为什么自己起实例
# ---------------------------------------------------------------------------
# 结算会**真动库存与凭证**（§7.3：`biz_cost_period` 一 SETTLED 就写账），所以演练必须打在
# 一次性演练库上 —— 脚本只认「库名含 drill/e2e/test」的库，跑完默认删掉，不留痕。
#
# ---------------------------------------------------------------------------
# calc 这一步为什么走 example 探针
# ---------------------------------------------------------------------------
# 设计 §5.2 #27 的 `POST /business/erp/cost-period/calc`（六段编排）**尚未落地**（属编排卡），
# 六段目前只有库内函数。探针 `backend/examples/cost_calc_probe.rs` 按各模块文档约定的顺序
# 调用**同一批函数**，跑的是真代码而不是手写归集行。接口卡上线后，把探针这一行换成
# `curl -XPOST /api/business/erp/cost-period/calc` 即可，其余步骤不用改。
# ============================================================
set -uo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
cd "$ROOT" || exit 1

DB_NAME="${COST_E2E_DB:-cost_e2e_drill}"
DB_HOST="${NIK_DATABASE_HOST:-127.0.0.1}"
DB_PORT="${NIK_DATABASE_PORT:-5432}"
DB_USER="${NIK_DATABASE_USER:-alun}"
DB_PASS="${NIK_DATABASE_PASSWORD:-alun}"
PORT="${COST_E2E_PORT:-18140}"
PERIOD="${COST_E2E_PERIOD:-$(TZ=Asia/Shanghai date +%Y-%m)}"
OUT="${COST_E2E_OUT:-docs/evidence/$(TZ=Asia/Shanghai date +%F)-cost-b4-5-e2e}"
KEEP=0
BUILD=1
MODE="run"
INJECT=""

while [ $# -gt 0 ]; do
    case "$1" in
        --keep) KEEP=1 ;;
        --db) DB_NAME="$2"; shift ;;
        --db=*) DB_NAME="${1#*=}" ;;
        --port) PORT="$2"; shift ;;
        --port=*) PORT="${1#*=}" ;;
        --period) PERIOD="$2"; shift ;;
        --period=*) PERIOD="${1#*=}" ;;
        --out) OUT="$2"; shift ;;
        --out=*) OUT="${1#*=}" ;;
        --no-build) BUILD=0 ;;
        --inject) INJECT="$2"; shift ;;
        --inject=*) INJECT="${1#*=}" ;;
        --up) MODE="up" ;;
        --down) MODE="down" ;;
        -h|--help) sed -n '2,45p' "$0"; exit 0 ;;
        *) echo "未知参数：$1（--help 看用法）" >&2; exit 2 ;;
    esac
    shift
done

PGENV=(env PGPASSWORD="$DB_PASS")
PIDFILE="/tmp/${DB_NAME}.pid"
LOGFILE="/tmp/${DB_NAME}.log"

say() { printf '\n\033[1m== %s\033[0m\n' "$*"; }

# ---------- 0 安全闸：只许演练库 ----------
case "$DB_NAME" in
    *drill*|*e2e*|*test*) ;;
    *) echo "拒绝执行：库名 $DB_NAME 不像演练库（须含 drill/e2e/test）" >&2; exit 2 ;;
esac
case "$DB_NAME" in
    cloudpivot|cloudpivot_prod) echo "拒绝执行：这是生产库名" >&2; exit 2 ;;
esac

kill_instance() {
    # ① 杀 pidfile 里的实例 ② 再兜一层：占着演练端口的 cloudpivot 一律清掉
    if [ -f "$PIDFILE" ]; then
        local pid; pid="$(cat "$PIDFILE")"
        if kill -0 "$pid" 2>/dev/null; then
            kill "$pid" 2>/dev/null
            for _ in $(seq 1 20); do kill -0 "$pid" 2>/dev/null || break; sleep 0.5; done
            kill -9 "$pid" 2>/dev/null
            echo "  已停演练实例 pid=$pid"
        fi
        mv -f "$PIDFILE" "$PIDFILE.done" 2>/dev/null || true
    fi
    local port_pid
    port_pid="$(ss -ltnp 2>/dev/null | grep -F ":$PORT " | sed 's/.*pid=\([0-9]*\).*/\1/' | head -1)"
    if [ -n "$port_pid" ]; then
        kill "$port_pid" 2>/dev/null
        for _ in $(seq 1 20); do kill -0 "$port_pid" 2>/dev/null || break; sleep 0.5; done
        kill -9 "$port_pid" 2>/dev/null
        echo "  已停占用 :$PORT 的实例 pid=$port_pid"
    fi
    for _ in $(seq 1 10); do
        ss -ltn 2>/dev/null | grep -qF ":$PORT " || break
        sleep 0.5
    done
    if ss -ltn 2>/dev/null | grep -qF ":$PORT "; then
        echo "  ⚠️ :$PORT 仍被占用（找不到 pid？用 lsof -i:$PORT 查）" >&2
    fi
}

drop_db() {
    # 库属主是 alun（无 CREATEDB 权限时也删得掉自己的库；保险起见走 postgres 超级用户）
    local sql="drop database if exists \"$DB_NAME\" with (force)"
    if command -v su >/dev/null && [ "$(id -u)" = "0" ]; then
        su - postgres -c "psql -q -v ON_ERROR_STOP=1 -c \"$sql\"" >/dev/null 2>&1 \
            || PGPASSWORD=alun psql -h "$DB_HOST" -p "$DB_PORT" -U postgres -d postgres -q -c "$sql" >/dev/null 2>&1
    else
        dropdb --if-exists "$DB_NAME" >/dev/null 2>&1
    fi
    echo "  已删演练库 $DB_NAME"
}

if [ "$MODE" = "down" ]; then
    say "清场"
    kill_instance
    if [ "$KEEP" = "0" ]; then drop_db; else echo "  （--keep：库保留）"; fi
    exit 0
fi

mkdir -p "$OUT"

# ---------- 1 建库（一次性） ----------
create_db() {
    local sql="drop database if exists \"$DB_NAME\" with (force)"
    if command -v su >/dev/null && [ "$(id -u)" = "0" ]; then
        su - postgres -c "psql -q -v ON_ERROR_STOP=1 -c \"$sql\"" >/dev/null 2>&1
        su - postgres -c "createdb -O $DB_USER \"$DB_NAME\"" || return 1
    else
        dropdb --if-exists "$DB_NAME" >/dev/null 2>&1
        createdb -O "$DB_USER" "$DB_NAME" || return 1
    fi
}

say "准备演练库与实例（库 $DB_NAME @ $DB_HOST:$DB_PORT，实例 :$PORT）"
# 端口必须干净：残留实例会让 health 探针**误判就绪**（旧实例答 /health、新实例还在迁移），
# 然后演练对着一个还没建表的库跑 —— 一律先清干净再起。
if ss -ltn 2>/dev/null | grep -qF ":$PORT "; then
    echo "  :$PORT 已被占用，先清场"
    kill_instance
fi
if [ -f "$PIDFILE" ] && ! kill -0 "$(cat "$PIDFILE")" 2>/dev/null; then
    mv -f "$PIDFILE" "$PIDFILE.stale" 2>/dev/null || true
fi
if [ ! -f "$PIDFILE" ]; then
    create_db || { echo "建库失败（需要本机 PG 与建库权限）" >&2; exit 3; }
    echo "  已建空库 $DB_NAME（迁移由实例启动时自动跑）"
else
    echo "  复用已在跑的实例（$PIDFILE）"
fi

# ---------- 2 起实例 ----------
# 三把密钥：优先用环境（/etc/cloudpivot/*.env 或调用方 export），没有就现场生成一次性密钥。
# 这是**一次性演练库**，密钥只在它生命周期内有效，不落盘、不写日志。
: "${NIK_MIDDLEWARE_AUTH_JWT_SECRET:=$(head -c 48 /dev/urandom | base64 | tr -d '\n')}"
: "${CLOUDPIVOT_STORAGE_MASTER_KEY:=$(head -c 32 /dev/urandom | od -An -tx1 | tr -d ' \n')}"
: "${X_CONFIG_SECRET_KEY:=$(head -c 24 /dev/urandom | base64 | tr -d '\n')}"
export NIK_MIDDLEWARE_AUTH_JWT_SECRET CLOUDPIVOT_STORAGE_MASTER_KEY X_CONFIG_SECRET_KEY
export NIK_PROFILE=test NIK_ENV=test
export NIK_SERVER_LISTEN="127.0.0.1:$PORT"
export NIK_DATABASE_HOST="$DB_HOST" NIK_DATABASE_USER="$DB_USER"
export NIK_DATABASE_PASSWORD="$DB_PASS" NIK_DATABASE_NAME="$DB_NAME"

if [ ! -f "$PIDFILE" ]; then
    if [ "$BUILD" = "1" ]; then
        say "构建（bin + calc 探针）"
        cargo build --quiet --manifest-path backend/Cargo.toml --bin cloudpivot \
            && cargo build --quiet --manifest-path backend/Cargo.toml --example cost_calc_probe \
            || { echo "cargo build 失败" >&2; exit 4; }
    fi
    say "启动隔离实例（首次要在空库上跑完全部迁移，约 30–60s）"
    # 必须从 backend/ 起：配置目录 `config/` 是**相对工作目录**解析的，从仓库根起会读不到
    # `config/config-test.toml`（→ `database.enabled` 落到默认 false → 实例起来但没连库）。
    ( cd "$ROOT/backend" && nohup ./target/debug/cloudpivot > "$LOGFILE" 2>&1 & echo $! > "$PIDFILE" )
    ready=0
    for _ in $(seq 1 180); do
        if curl -sf -m 2 "http://127.0.0.1:$PORT/health" >/dev/null 2>&1 \
           && [ "$(PGPASSWORD="$DB_PASS" psql -h "$DB_HOST" -p "$DB_PORT" -U "$DB_USER" \
                    -d "$DB_NAME" -tAc 'select count(*) from sys_dept' 2>/dev/null)" != "" ]; then
            ready=1; break
        fi
        if ! kill -0 "$(cat "$PIDFILE")" 2>/dev/null; then break; fi
        sleep 1
    done
    if [ "$ready" != "1" ]; then
        echo "实例没起来（日志 $LOGFILE 末尾）：" >&2
        tail -20 "$LOGFILE" >&2
        kill_instance
        exit 5
    fi
    echo "  实例就绪：http://127.0.0.1:$PORT/health"
    # pidfile 里改写**真正在监听的那个 pid**：`nohup` 起的包装进程会比服务进程早一号，
    # 记错的话清场时杀不掉（端口兜底能救，但 pidfile 该是准的）。
    LANE_PID="$(ss -ltnp 2>/dev/null | grep -F ":$PORT " | sed 's/.*pid=\([0-9]*\).*/\1/' | head -1)"
    if [ -n "$LANE_PID" ]; then echo "$LANE_PID" > "$PIDFILE"; fi
fi

if [ "$MODE" = "up" ]; then
    echo; echo "只准备（--up）：库 $DB_NAME，实例 :$PORT，日志 $LOGFILE"
    echo "跑完整演练：bash scripts/verify/cost_e2e.sh --db $DB_NAME --port $PORT"
    exit 0
fi

# ---------- 3 跑演练 ----------
say "跑演练（期间 $PERIOD，证据 $OUT）"
STAMP="$(TZ=Asia/Shanghai date +%Y%m%d-%H%M%S)"
set -o pipefail
COST_E2E_BASE="http://127.0.0.1:$PORT" \
COST_E2E_PERIOD="$PERIOD" \
COST_E2E_INJECT="$INJECT" \
    python3 scripts/verify/cost/cost_e2e.py 2>&1 | tee "$OUT/drill-$STAMP.out"
RC=${PIPESTATUS[0]}

# ---------- 4 清场 ----------
say "清场"
kill_instance
if [ "$KEEP" = "1" ]; then
    echo "  --keep：库 $DB_NAME 与日志 $LOGFILE 保留"
else
    drop_db
fi

echo
if [ "$RC" = "0" ]; then
    echo "✅ 演练全绿（日志：$OUT/drill-$STAMP.out）"
else
    echo "🚨 演练失败：exit $RC —— 日志里搜 '[FAIL] STEP' 看是哪一步、期望 vs 实际"
    echo "   复跑单步：先 --up 起实例，再直接跑 python 脚本（见日志首屏）"
fi
exit "$RC"

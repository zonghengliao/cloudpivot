#!/usr/bin/env bash
# cailu 开发环境一键启动（dev 实例，与 prod 完全隔离）
#
#   dev 后端   : 127.0.0.1:8024  cargo-watch（改 src/**.rs、config/*.toml 自动重编译重启）
#   dev 前端   : 0.0.0.0:5173    Vite（HMR），/api /health 代理到 8024
#   code-server: 0.0.0.0:8443    浏览器 IDE，打开 /opt/cailu
#
# 用法：
#   bash /opt/cailu/scripts/dev-up.sh                # 全部启动（幂等，已启动的跳过）
#   WATCH=0 bash /opt/cailu/scripts/dev-up.sh        # 后端不开热重载（纯 cargo run）
#   ONLY=backend bash /opt/cailu/scripts/dev-up.sh   # 只起某一部分：backend|frontend|ide
#
# 停止：bash /opt/cailu/scripts/dev-down.sh
# 状态：bash /opt/cailu/scripts/dev-status.sh
#
# ❗ prod 实例（127.0.0.1:8023 + nginx:80，systemd cailu.service）不受本脚本影响。
set -euo pipefail

ROOT=/opt/cailu
ENV_FILE=/etc/cailu/cailu-dev.env
LOG_DIR=/var/log/cailu-dev
PID_DIR=/var/run/cailu-dev
export PATH=/root/.cargo/bin:/usr/local/sbin:/usr/local/bin:/usr/sbin:/usr/bin:/sbin:/bin

[ -f "$ENV_FILE" ] || { echo "缺少 $ENV_FILE" >&2; exit 1; }
set -a; . "$ENV_FILE"; set +a
set +u

ONLY="${ONLY:-}"
WATCH="${WATCH:-1}"
VITE_PORT="${VITE_PORT:-5173}"
CODE_SERVER_PORT="${CODE_SERVER_PORT:-8443}"
mkdir -p "$LOG_DIR" "$PID_DIR"

say()  { printf '\033[1;36m==>\033[0m %s\n' "$*"; }
ok()   { printf '\033[1;32m  ✔\033[0m %s\n' "$*"; }
warn() { printf '\033[1;33m  !\033[0m %s\n' "$*"; }
die()  { printf '\033[1;31m  ✘ %s\033[0m\n' "$*" >&2; exit 1; }

pid_of() { # pid_of <name>  → 存活则打印 pid
  local f="$PID_DIR/$1.pid" p
  [ -f "$f" ] || return 1
  p="$(cat "$f" 2>/dev/null || true)"
  [ -n "${p:-}" ] || return 1
  kill -0 "$p" 2>/dev/null && { echo "$p"; return 0; }
  rm -f "$f"; return 1
}

port_busy() { ss -ltn "sport = :$1" 2>/dev/null | tail -n +2 | grep -q .; }

wait_http() { # wait_http <url> <超时秒> <名称>
  local url="$1" timeout="${2:-180}" name="${3:-$1}" i code=""
  for ((i=0; i<timeout; i++)); do
    code="$(curl -s -o /dev/null -m 2 -w '%{http_code}' "$url" 2>/dev/null || true)"
    case "$code" in 200|302) return 0;; esac
    sleep 1
  done
  warn "$name 在 ${timeout}s 内未就绪（$url，最后返回 ${code:-无响应}），排查日志：$LOG_DIR/"
  return 1
}

want() { [ -z "$ONLY" ] || [ "$ONLY" = "$1" ]; }

# ------------------------------------------------------------------ 0. 依赖
say "[0/4] 检查依赖服务（PG/Redis，与 prod 共用）"
for svc in postgresql redis-server; do
  systemctl is-active --quiet "$svc" || { warn "$svc 未运行，尝试启动"; systemctl start "$svc"; }
  systemctl is-active --quiet "$svc" && ok "$svc active" || die "$svc 起不来"
done

# dev 库不存在则建（owner=cailu，UTF8）；表结构由后端 migrations auto_migrate 自动建
export PGPASSWORD="$ALUN_DATABASE_PASSWORD"
if ! psql -h "$ALUN_DATABASE_HOST" -p "$ALUN_DATABASE_PORT" -U "$ALUN_DATABASE_USER" -d postgres -tAc \
      "select 1 from pg_database where datname='$ALUN_DATABASE_NAME'" | grep -q 1; then
  say "创建开发库 $ALUN_DATABASE_NAME"
  su - postgres -c "psql -q -c \"CREATE DATABASE $ALUN_DATABASE_NAME OWNER $ALUN_DATABASE_USER ENCODING 'UTF8' TEMPLATE template0\""
fi
ok "开发库 $ALUN_DATABASE_NAME 就绪"

# ------------------------------------------------------------------ 1. 后端
if want backend; then
  say "[1/4] dev 后端 → $ALUN_SERVER_LISTEN（profile=$ALUN_PROFILE, db=$ALUN_DATABASE_NAME）"
  if pid="$(pid_of backend)"; then
    ok "已在运行（pid $pid），跳过"
  elif port_busy 8024; then
    die "端口 8024 已被占用但不是本脚本启动的进程：$(ss -ltnp 'sport = :8024' | tail -n +2)"
  else
    [ -d "$ROOT/backend/target/debug" ] || warn "首次 debug 构建较慢（约 1-2 分钟），看日志：$LOG_DIR/backend.log"
    pushd "$ROOT/backend" >/dev/null
    if [ "$WATCH" = "1" ] && command -v cargo-watch >/dev/null 2>&1; then
      nohup cargo-watch -w src -w config -w migrations -x run >>"$LOG_DIR/backend.log" 2>&1 </dev/null &
      echo $! >"$PID_DIR/backend.pid"
      ok "cargo-watch 已启动（pid $(cat "$PID_DIR/backend.pid")），改 src/**/*.rs 自动重编译重启"
    else
      [ "$WATCH" = "1" ] && warn "未找到 cargo-watch，退回 cargo run（无热重载）"
      nohup cargo run >>"$LOG_DIR/backend.log" 2>&1 </dev/null &
      echo $! >"$PID_DIR/backend.pid"
      ok "cargo run 已启动（pid $(cat "$PID_DIR/backend.pid")）"
    fi
    popd >/dev/null
    wait_http "http://127.0.0.1:8024/health" 300 "dev 后端" || true
    ok "health: $(curl -s -m 3 http://127.0.0.1:8024/health || echo '未响应')"
  fi
fi

# ------------------------------------------------------------------ 2. 前端
if want frontend; then
  say "[2/4] dev 前端 → 0.0.0.0:$VITE_PORT（Vite HMR，代理 → $BACKEND_ORIGIN）"
  if pid="$(pid_of frontend)"; then
    ok "已在运行（pid $pid），跳过"
  elif port_busy "$VITE_PORT"; then
    die "端口 $VITE_PORT 已被占用：$(ss -ltnp "sport = :$VITE_PORT" | tail -n +2)"
  else
    pushd "$ROOT/frontend" >/dev/null
    BACKEND_ORIGIN="$BACKEND_ORIGIN" nohup pnpm dev --port "$VITE_PORT" --strictPort \
      >>"$LOG_DIR/frontend.log" 2>&1 </dev/null &
    echo $! >"$PID_DIR/frontend.pid"
    popd >/dev/null
    ok "vite 已启动（pid $(cat "$PID_DIR/frontend.pid")）"
    wait_http "http://127.0.0.1:$VITE_PORT/" 120 "dev 前端" || true
  fi
fi

# ------------------------------------------------------------------ 3. code-server
if want ide; then
  say "[3/4] code-server → 0.0.0.0:$CODE_SERVER_PORT（浏览器 IDE）"
  CS_CFG=/root/.config/code-server/config.yaml
  mkdir -p "$(dirname "$CS_CFG")"
  cat >"$CS_CFG" <<EOF
bind-addr: 0.0.0.0:$CODE_SERVER_PORT
auth: password
password: $CODE_SERVER_PASSWORD
cert: false
EOF
  chmod 600 "$CS_CFG"
  if pid="$(pid_of code-server)"; then
    ok "已在运行（pid $pid），跳过"
  elif port_busy "$CODE_SERVER_PORT"; then
    die "端口 $CODE_SERVER_PORT 已被占用"
  else
    nohup code-server --disable-telemetry --disable-update-check \
      --bind-addr "0.0.0.0:$CODE_SERVER_PORT" /opt/cailu \
      >>"$LOG_DIR/code-server.log" 2>&1 </dev/null &
    echo $! >"$PID_DIR/code-server.pid"
    ok "code-server 已启动（pid $(cat "$PID_DIR/code-server.pid")）"
    wait_http "http://127.0.0.1:$CODE_SERVER_PORT/login" 120 "code-server" || true
  fi
fi

# ------------------------------------------------------------------ 4. 汇总
say "[4/4] 健康检查汇总"
IP="$(hostname -I | awk '{print $1}')"
code() { curl -s -o /dev/null -m 3 -w '%{http_code}' "$1" 2>/dev/null || echo "-"; }
printf '  %-18s %-32s %s\n' 组件 地址 HTTP
printf '  %-18s %-32s %s\n' ---------------- -------------------------------- ----
printf '  %-18s %-32s %s\n' "dev 后端"    "http://127.0.0.1:8024/health"  "$(code http://127.0.0.1:8024/health)"
printf '  %-18s %-32s %s\n' "dev 前端"    "http://$IP:$VITE_PORT/"        "$(code http://127.0.0.1:$VITE_PORT/)"
printf '  %-18s %-32s %s\n' "code-server" "http://$IP:$CODE_SERVER_PORT/" "$(code http://127.0.0.1:$CODE_SERVER_PORT/login)"
printf '  %-18s %-32s %s\n' "prod 后端(不动)" "http://127.0.0.1:8023/health" "$(code http://127.0.0.1:8023/health)"
printf '  %-18s %-32s %s\n' "prod 前端(不动)" "http://$IP/"               "$(code http://127.0.0.1/)"
echo
echo "  日志: $LOG_DIR/{backend,frontend,code-server}.log"
echo "  PID : $PID_DIR/*.pid     状态: bash $ROOT/scripts/dev-status.sh"
echo "  停止: bash $ROOT/scripts/dev-down.sh"

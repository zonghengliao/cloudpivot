#!/usr/bin/env bash
# cailu 开发环境一键停止（只动 dev 的三件东西，prod 的 8023/80 绝不触碰）
#
# 用法： bash /opt/cailu/scripts/dev-down.sh            # 全停
#        ONLY=backend bash /opt/cailu/scripts/dev-down.sh
set -uo pipefail

ROOT=/opt/cailu
ENV_FILE=/etc/cailu/cailu-dev.env
PID_DIR=/var/run/cailu-dev
BACKEND_PORT=8024
FRONTEND_PORT=5173
IDE_PORT=8443

[ -f "$ENV_FILE" ] && { set -a; . "$ENV_FILE"; set +a; }
set +u
FRONTEND_PORT="${VITE_PORT:-5173}"
IDE_PORT="${CODE_SERVER_PORT:-8443}"
ONLY="${ONLY:-}"

say() { printf '\033[1;36m==>\033[0m %s\n' "$*"; }
ok()  { printf '\033[1;32m  ✔\033[0m %s\n' "$*"; }
warn(){ printf '\033[1;33m  !\033[0m %s\n' "$*"; }

want() { [ -z "$ONLY" ] || [ "$ONLY" = "$1" ]; }

kill_tree() { # kill_tree <pid> [信号] —— 先杀子孙再杀自己，cargo-watch→cargo→cailu 一并收
  local p="$1" sig="${2:-TERM}" k
  for k in $(pgrep -P "$p" 2>/dev/null || true); do kill_tree "$k" "$sig"; done
  kill -"$sig" "$p" 2>/dev/null || true
}

stop_one() { # stop_one <name> <port> <中文名>
  local name="$1" port="$2" label="$3" pid
  local pf="$PID_DIR/$name.pid"
  pid="$(cat "$pf" 2>/dev/null || true)"
  if [ -n "${pid:-}" ] && kill -0 "$pid" 2>/dev/null; then
    kill_tree "$pid" TERM
    for _ in $(seq 1 15); do kill -0 "$pid" 2>/dev/null || break; sleep 0.4; done
    if kill -0 "$pid" 2>/dev/null; then
      warn "$label 未响应 TERM，发 KILL"
      kill_tree "$pid" KILL
    fi
    ok "$label 已停止（pid $pid）"
  else
    ok "$label 未在运行（无 pid 文件）"
  fi
  rm -f "$pf"
  # 兜底：端口仍被占（历史残留、pid 文件丢失）→ 按端口清理；端口白名单校验，绝不误杀 prod
  if ss -ltn "sport = :$port" 2>/dev/null | tail -n +2 | grep -q .; then
    case "$port" in 8023|80) warn "拒绝清理 prod 端口 $port"; return 0;; esac
    warn "端口 $port 仍被占用，按端口清理残留进程"
    fuser -k -TERM "$port/tcp" 2>/dev/null || true
    sleep 1
    fuser -k -KILL "$port/tcp" 2>/dev/null || true
  fi
}

say "停止 cailu 开发环境"
want backend  && stop_one backend  "$BACKEND_PORT"  "dev 后端(8024)"
want frontend && stop_one frontend "$FRONTEND_PORT" "dev 前端(5173)"
want ide      && stop_one code-server "$IDE_PORT"   "code-server(8443)"

echo
printf '  %-24s %s\n' "prod 后端 127.0.0.1:8023" "$(curl -s -o /dev/null -m 3 -w '%{http_code}' http://127.0.0.1:8023/health || echo -)  (应保持 200)"
printf '  %-24s %s\n' "prod 前端 :80"            "$(curl -s -o /dev/null -m 3 -w '%{http_code}' http://127.0.0.1/ || echo -)  (应保持 200)"

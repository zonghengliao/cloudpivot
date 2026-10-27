#!/usr/bin/env bash
# cailu 开发环境状态 + 日志速查
#
# 用法：
#   bash /opt/cailu/scripts/dev-status.sh          # 状态总览
#   bash /opt/cailu/scripts/dev-status.sh logs     # 跟随后端日志（cargo-watch 输出）
#   bash /opt/cailu/scripts/dev-status.sh logs fe  # 跟前端日志；logs ide = code-server
set -uo pipefail

ROOT=/opt/cailu
LOG_DIR=/var/log/cailu-dev
PID_DIR=/var/run/cailu-dev
BACKEND_PORT=8024
FRONTEND_PORT="${VITE_PORT:-5173}"
IDE_PORT="${CODE_SERVER_PORT:-8443}"
[ -f /etc/cailu/cailu-dev.env ] && { set -a; . /etc/cailu/cailu-dev.env; set +a; }
set +u
FRONTEND_PORT="${VITE_PORT:-5173}"
IDE_PORT="${CODE_SERVER_PORT:-8443}"

IP="$(hostname -I | awk '{print $1}')"
code() { curl -s -o /dev/null -m 3 -w '%{http_code}' "$1" 2>/dev/null || echo "-"; }
pidof_file() { local f="$PID_DIR/$1.pid"; [ -f "$f" ] && cat "$f" || echo "-"; }

if [ "${1:-status}" = "logs" ]; then
  case "${2:-be}" in
    fe|frontend|front) f="$LOG_DIR/frontend.log" ;;
    ide|cs|code)       f="$LOG_DIR/code-server.log" ;;
    *)                 f="$LOG_DIR/backend.log" ;;
  esac
  echo "==> tail -f $f （Ctrl+C 退出）"
  exec tail -f "$f"
fi

echo "cailu 开发环境状态  ($(date '+%F %T'))"
echo
printf '  %-16s %-10s %-8s %-9s %s\n' 组件 PID 端口 HTTP 地址
printf '  %-16s %-10s %-8s %-9s %s\n' ---- --- ---- ---- ----
printf '  %-16s %-10s %-8s %-9s %s\n' "dev 后端"    "$(pidof_file backend)"     "$BACKEND_PORT"  "$(code http://127.0.0.1:$BACKEND_PORT/health)" "http://127.0.0.1:$BACKEND_PORT/health"
printf '  %-16s %-10s %-8s %-9s %s\n' "dev 前端"    "$(pidof_file frontend)"    "$FRONTEND_PORT" "$(code http://127.0.0.1:$FRONTEND_PORT/)"      "http://$IP:$FRONTEND_PORT/"
printf '  %-16s %-10s %-8s %-9s %s\n' "code-server" "$(pidof_file code-server)" "$IDE_PORT"      "$(code http://127.0.0.1:$IDE_PORT/login)"      "http://$IP:$IDE_PORT/"
printf '  %-16s %-10s %-8s %-9s %s\n' "prod 后端"   "$(pgrep -f 'target/release/cailu' | head -1)" "8023" "$(code http://127.0.0.1:8023/health)"       "http://127.0.0.1:8023/health"
printf '  %-16s %-10s %-8s %-9s %s\n' "prod 前端"   "$(pgrep -f 'nginx: master' | head -1)"        "80"   "$(code http://127.0.0.1/)"              "http://$IP/"
echo
echo "  profile(dev)=$(grep -oP '(?<=^ALUN_PROFILE=).*' /etc/cailu/cailu-dev.env)  DB=$(grep -oP '(?<=^ALUN_DATABASE_NAME=).*' /etc/cailu/cailu-dev.env)"
export PGPASSWORD="$(grep -oP '(?<=^ALUN_DATABASE_PASSWORD=).*' /etc/cailu/cailu-dev.env)"
DB=$(grep -oP '(?<=^ALUN_DATABASE_NAME=).*' /etc/cailu/cailu-dev.env)
echo "  dev 库表数: $(psql -h 127.0.0.1 -U cailu -d "$DB" -tAc "select count(*) from information_schema.tables where table_schema='public' and table_type='BASE TABLE'" 2>/dev/null || echo '连不上')"
echo
echo "  日志目录: $LOG_DIR"
echo "  看后端热重载日志: bash $ROOT/scripts/dev-status.sh logs be"
echo "  停止: bash $ROOT/scripts/dev-down.sh"

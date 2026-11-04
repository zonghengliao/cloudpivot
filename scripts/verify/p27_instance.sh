#!/usr/bin/env bash
# =============================================================================
# P2-7 账务巡检落库 · 隔离实例（空库起，迁移自动跑）
# =============================================================================
#
# 为什么用隔离库：巡检执行器会起子进程扫库，写真库既污染数据又让证据不可复现。
# 本脚本起一个**空库 + 隔离端口**的实例，迁移自动建表，然后用 P2-7 的接口跑两轮。
#
# 用法：
#   bash scripts/verify/p27_instance.sh up       # 起（后台，日志在 $RUN_DIR/backend.log）
#   bash scripts/verify/p27_instance.sh status   # 看健康 + 进程
#   bash scripts/verify/p27_instance.sh down     # 停
#
# 可用环境变量覆盖：P27_DB / P27_PORT / P27_OWNER / P27_PASSWORD / P27_RUN_DIR
set -uo pipefail

REPO_ROOT=$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)
BACKEND_DIR="$REPO_ROOT/backend"

P27_DB=${P27_DB:-cloudpivot_p27_inv}
P27_PORT=${P27_PORT:-18095}
P27_OWNER=${P27_OWNER:-alun}
P27_PASSWORD=${P27_PASSWORD:-alun}
P27_PG_HOST=${P27_PG_HOST:-127.0.0.1}
P27_PG_PORT=${P27_PG_PORT:-5432}
RUN_DIR=${P27_RUN_DIR:-/tmp/p27-inv}
BACKEND_BIN=${P27_BACKEND_BIN:-$BACKEND_DIR/target/debug/cloudpivot}

mkdir -p "$RUN_DIR"

say() { printf '%s\n' "$*"; }
die() { printf 'FATAL: %s\n' "$*" >&2; exit 1; }

check_ports() {
  [[ "$P27_PORT" =~ ^[0-9]+$ ]] || die "端口不是数字：$P27_PORT"
  (( P27_PORT >= 18000 && P27_PORT <= 18999 )) || die "端口 $P27_PORT 不在隔离段 18000-18999（prod 是 8023）"
  ss -ltn 2>/dev/null | grep -q ":$P27_PORT " && die "端口 $P27_PORT 已被占用"
  say "✅ 端口判据：$P27_PORT 在隔离段且空闲"
}

prepare_db() {
  local exists
  exists=$(su - postgres -c "psql -At -c \"select 1 from pg_database where datname='$P27_DB'\"" 2>/dev/null | tr -d '[:space:]')
  if [[ "$exists" == "1" ]]; then
    say "… 复用已有库 $P27_DB（要重来先 drop）"
  else
    su - postgres -c "createdb -O $P27_OWNER $P27_DB" >/dev/null || die "createdb $P27_DB 失败"
    say "✅ 新库 $P27_DB（owner=$P27_OWNER，空库，迁移会自动跑）"
  fi
}

start_backend() {
  local jwt_file=$RUN_DIR/jwt_secret master_file=$RUN_DIR/storage_master_key
  [[ -s "$jwt_file" ]] || openssl rand -hex 32 > "$jwt_file"
  [[ -s "$master_file" ]] || openssl rand -hex 32 > "$master_file"
  [[ -x "$BACKEND_BIN" ]] || die "找不到二进制 $BACKEND_BIN（cd backend && cargo build）"

  (
    cd "$BACKEND_DIR" || exit 1
    export NIK_PROFILE=test
    export NIK_DATABASE_HOST="$P27_PG_HOST" NIK_DATABASE_PORT="$P27_PG_PORT"
    export NIK_DATABASE_NAME="$P27_DB" NIK_DATABASE_USER="$P27_OWNER"
    export NIK_DATABASE_PASSWORD="$P27_PASSWORD" NIK_DATABASE_SSL_MODE=prefer
    export NIK_SERVER_LISTEN="127.0.0.1:$P27_PORT"
    export NIK_MIDDLEWARE_AUTH_JWT_SECRET="$(cat "$jwt_file")"
    export CLOUDPIVOT_STORAGE_MASTER_KEY="$(cat "$master_file")"
    # 管理员初始口令：仅本隔离库使用
    export NIK_ADMIN_PASSWORD="P27Admin@123456"
    exec "$BACKEND_BIN"
  ) >"$RUN_DIR/backend.log" 2>&1 &
  echo $! > "$RUN_DIR/backend.pid"

  local i rc=1
  for i in $(seq 1 90); do
    if curl -fsS -m 2 "http://127.0.0.1:$P27_PORT/health" >/dev/null 2>&1; then rc=0; break; fi
    sleep 1
  done
  [[ $rc -eq 0 ]] || { tail -20 "$RUN_DIR/backend.log" >&2; die "后端 90s 内没起来（日志 $RUN_DIR/backend.log）"; }
  say "✅ 后端就绪：http://127.0.0.1:$P27_PORT（库 $P27_DB）"
}

case "${1:-up}" in
  up)
    check_ports
    prepare_db
    start_backend
    ;;
  status)
    curl -fsS -m 2 "http://127.0.0.1:$P27_PORT/health" && echo || echo "未运行"
    ;;
  down)
    if [[ -f "$RUN_DIR/backend.pid" ]]; then
      kill "$(cat "$RUN_DIR/backend.pid")" 2>/dev/null && say "已停 pid $(cat "$RUN_DIR/backend.pid")" || say "进程不存在"
      rm -f "$RUN_DIR/backend.pid"
    else
      say "没有 pid 文件"
    fi
    ;;
  *)
    die "用法：$0 {up|status|down}"
    ;;
esac

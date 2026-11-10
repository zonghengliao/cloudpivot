#!/bin/bash
# 云枢测试运行器（145 测试机标准姿势）
#
# 用法：
#   ./scripts/ops/run_tests.sh backend            # 后端全部单测
#   ./scripts/ops/run_tests.sh backend idle       # 后端按关键字过滤
#   ./scripts/ops/run_tests.sh frontend           # 前端全部单测
#   ./scripts/ops/run_tests.sh all                # 后端 + 前端
#   ./scripts/ops/run_tests.sh clean              # 清理测试产物
#
# 为什么需要这个脚本（2026-11-10 教训）：
#   145 只有 7.2GB 内存，rustc 单进程峰值 ~6.9GB。
#   直接用 `cargo test`（多 job + 带 debuginfo）会被 OOM killer 杀掉，
#   而且 target/debug 会膨胀到 70G+。
#   本脚本固定：单 job + 关闭 debuginfo + 定期清理。

set -uo pipefail

ROOT=$(cd "$(dirname "$0")/../.." && pwd)
BACKEND="$ROOT/backend"
FRONTEND="$ROOT/frontend"

export PATH="/root/.cargo/bin:/root/.local/share/pnpm:/usr/local/bin:$PATH"

# 内存与磁盘保护参数
export CARGO_BUILD_JOBS=1
export CARGO_PROFILE_DEV_DEBUG=0
export CARGO_PROFILE_TEST_DEBUG=0

# target/debug 超过这个大小（MB）就跑完清理
DEBUG_MAX_MB=${DEBUG_MAX_MB:-20000}

log() { printf '\n\033[1m▶ %s\033[0m\n' "$*"; }
ok()  { printf '  \033[32m✓\033[0m %s\n' "$*"; }
bad() { printf '  \033[31m✗\033[0m %s\n' "$*"; }

show_env() {
  log "环境"
  echo "  代码: $ROOT ($(cd "$ROOT" && git rev-parse --short HEAD 2>/dev/null || echo '?'))"
  free -h | awk '/Mem:/{print "  内存: 总 " $2 " / 可用 " $7}'
  free -h | awk '/Swap:/{print "  Swap: 总 " $2 " / 可用 " $4}'
  df -h / | awk 'NR==2{print "  磁盘: 已用 " $3 " / 可用 " $4 " (" $5 ")"}'
}

run_backend() {
  local filter="${1:-}"
  log "后端单测${filter:+（过滤: $filter）}"
  cd "$BACKEND" || return 1

  local start; start=$(date +%s)
  if [ -n "$filter" ]; then
    cargo test --lib "$filter" 2>&1 | grep -vE '^\s+(Compiling|Downloading)' | tail -25
  else
    cargo test --lib 2>&1 | grep -vE '^\s+(Compiling|Downloading)' | tail -25
  fi
  local rc=${PIPESTATUS[0]}
  local end; end=$(date +%s)

  if [ "$rc" -eq 0 ]; then
    ok "后端测试通过（$((end - start)) 秒）"
  else
    bad "后端测试失败（退出码 $rc，$((end - start)) 秒）"
  fi

  # 产物清理
  local mb
  mb=$(du -sm "$BACKEND/target/debug" 2>/dev/null | cut -f1 || echo 0)
  [ -n "$mb" ] && [ "$mb" -gt "$DEBUG_MAX_MB" ] && {
    echo "  target/debug=${mb}MB > ${DEBUG_MAX_MB}MB，清理中…"
    rm -rf "$BACKEND/target/debug"
    ok "已清理 target/debug"
  } || echo "  target/debug=${mb:-0}MB（未超阈值 ${DEBUG_MAX_MB}MB）"

  return "$rc"
}

run_frontend() {
  log "前端单测"
  cd "$FRONTEND" || return 1

  local start; start=$(date +%s)
  pnpm test:unit 2>&1 | tail -12
  local rc=${PIPESTATUS[0]}
  local end; end=$(date +%s)

  if [ "$rc" -eq 0 ]; then
    ok "前端测试通过（$((end - start)) 秒）"
  else
    bad "前端测试失败（退出码 $rc，$((end - start)) 秒）"
  fi
  return "$rc"
}

case "${1:-all}" in
  backend)
    show_env
    run_backend "${2:-}"
    ;;
  frontend)
    show_env
    run_frontend
    ;;
  all)
    show_env
    run_backend
    rc1=$?
    run_frontend
    rc2=$?
    log "汇总"
    [ "$rc1" -eq 0 ] && ok "后端" || bad "后端"
    [ "$rc2" -eq 0 ] && ok "前端" || bad "前端"
    [ "$rc1" -eq 0 ] && [ "$rc2" -eq 0 ] || exit 1
    ;;
  clean)
    log "清理测试产物"
    du -sh "$BACKEND/target/debug" 2>/dev/null
    rm -rf "$BACKEND/target/debug"
    ok "已删除 target/debug"
    df -h / | awk 'NR==2{print "  磁盘可用: " $4}'
    ;;
  *)
    echo "用法: $0 {backend [过滤] | frontend | all | clean}"
    exit 2
    ;;
esac

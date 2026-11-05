#!/bin/bash
# ═══════════════════════════════════════════════════════════════════════════
# 安全清理遗留 cloudpivot 进程
#
# 依据规程: docs/运维规程-进程清理安全.md
# 事故来源: 2026-11-05 19:47 误杀 9 个 PostgreSQL 连接进程
#
# 核心原则（三道闸，全过才杀）:
#   闸1 白名单保护 —— 生产 MainPID 绝对跳过
#   闸2 黑名单排除 —— postgres/node/esbuild/code-server 一律跳过
#   闸3 归属确认   —— 必须是 debug 构建 / 临时 target / /var/tmp
#
# 用法:
#   ./safe_kill_stale_cloudpivot.sh --dry-run    # 只看判定，不动手
#   ./safe_kill_stale_cloudpivot.sh              # 执行清理
#   ./safe_kill_stale_cloudpivot.sh --check-only # 只做生产健康检查
# ═══════════════════════════════════════════════════════════════════════════
set -uo pipefail

MODE="${1:-apply}"
DRY=0
[ "$MODE" = "--dry-run" ] && DRY=1

PROD_PID=$(systemctl show cloudpivot -p MainPID --value 2>/dev/null || echo "")
PROD_EXE=$(readlink -f /proc/"$PROD_PID"/exe 2>/dev/null || echo "")
PROD_PORT=8023

echo "════════════════════════════════════════════════════════════"
echo "  云枢遗留进程安全清理   $(date '+%F %T')"
echo "  模式: $([ $DRY -eq 1 ] && echo 'DRY-RUN（不动手）' || echo '执行')"
echo "════════════════════════════════════════════════════════════"
echo ""
echo "  生产 MainPID: ${PROD_PID:-未找到}"
echo "  生产 exe:     ${PROD_EXE:-未知}"
echo ""

if [ -z "$PROD_PID" ]; then
  echo "  ⛔ 拿不到生产 MainPID，为安全起见直接退出（不执行任何 kill）"
  exit 2
fi

# ─────────────────────────────────────────────────────────────
# 扫描：只遍历 /proc/<数字>，不用 ps|grep 选目标
# ─────────────────────────────────────────────────────────────
declare -a TO_KILL=()
declare -a TO_KEEP=()

echo "════════ 逐个判定 ════════"
for p in $(ls /proc 2>/dev/null | grep -E '^[0-9]+$' | sort -n); do
  [ -d "/proc/$p" ] || continue

  # 取精确路径；拿不到说明已退出或无权限
  EXE=$(readlink -f "/proc/$p/exe" 2>/dev/null) || continue
  [ -z "$EXE" ] && continue

  # 只看 exe 路径里含 cloudpivot 的（这是唯一的初筛依据）
  case "$EXE" in
    *cloudpivot*) ;;
    *) continue ;;
  esac

  CMD=$(tr '\0' ' ' < "/proc/$p/cmdline" 2>/dev/null || echo "")
  RUN=$(ps -o etime= -p "$p" 2>/dev/null | tr -d ' ')
  PORTS=$(ss -lntp 2>/dev/null | grep "pid=$p," | awk '{print $4}' | tr '\n' ',' | sed 's/,$//')

  # ═══ 闸1：白名单保护 ═══
  if [ "$p" = "$PROD_PID" ] || [ "$EXE" = "$PROD_EXE" ] && [ -n "$PROD_EXE" ]; then
    echo "  ⊙ pid=$p  保留[生产]  运行=$RUN  exe=$EXE"
    TO_KEEP+=("$p")
    continue
  fi

  # ═══ 闸2：黑名单排除 ═══
  case "$EXE" in
    */postgres*|*/node|*/node[0-9]*|*esbuild*|*code-server*|*/nginx*)
      echo "  ⊙ pid=$p  保留[黑名单]  运行=$RUN  exe=$EXE"
      TO_KEEP+=("$p")
      continue
      ;;
  esac

  # ═══ 闸3：归属确认 ═══
  # 注意顺序：临时 target 的判定必须放在 debug 之前，
  # 否则 /root/t97106-target/debug/cloudpivot 会先命中 debug 分支（结果同为待清理，但标签不准确）
  VERDICT=""
  case "$EXE" in
    *target/release/cloudpivot*)
      echo "  ⊙ pid=$p  保留[release版]  运行=$RUN  exe=$EXE"
      TO_KEEP+=("$p")
      continue
      ;;
    */var/tmp/*cloudpivot*)    VERDICT="var-tmp遗留" ;;
    *e11-run*|*e2e*|*regress*) VERDICT="测试遗留" ;;
    */root/*-target/*cloudpivot*|*/[a-z0-9]*-target/*cloudpivot*) VERDICT="临时target" ;;
    *target/debug/cloudpivot*) VERDICT="debug构建" ;;
  esac

  # 端口佐证：测试端口段 180xx 也算
  if echo "$PORTS" | grep -qE ':18[01][0-9]{2}\b'; then
    VERDICT="${VERDICT:+$VERDICT + }测试端口"
  fi

  # 生产端口出现 → 绝不杀
  if echo "$PORTS" | grep -qE ":$PROD_PORT\b"; then
    echo "  ⊙ pid=$p  保留[占用生产端口$PROD_PORT]  运行=$RUN  exe=$EXE"
    TO_KEEP+=("$p")
    continue
  fi

  if [ -n "$VERDICT" ]; then
    echo "  ✗ pid=$p  待清理[$VERDICT]  运行=$RUN  端口=[${PORTS:-无}]"
    echo "       exe=$EXE"
    TO_KILL+=("$p")
  else
    echo "  ? pid=$p  未识别  运行=$RUN  exe=$EXE  端口=[${PORTS:-无}]  → 不处理"
    TO_KEEP+=("$p")
  fi
done

echo ""
echo "════════ 判定汇总 ════════"
echo "  保留: ${#TO_KEEP[@]} 个"
echo "  待清理: ${#TO_KILL[@]} 个 ${TO_KILL[*]:-}"

if [ ${#TO_KILL[@]} -eq 0 ]; then
  echo ""
  echo "  ✓ 没有需要清理的进程"
  exit 0
fi

if [ $DRY -eq 1 ]; then
  echo ""
  echo "  ⓘ DRY-RUN：不执行 kill。确认无误后去掉 --dry-run 重跑。"
  exit 0
fi

# ─────────────────────────────────────────────────────────────
# 清理前基线
# ─────────────────────────────────────────────────────────────
echo ""
echo "════════ 清理前基线 ════════"
set -a; . /etc/cloudpivot/cloudpivot.env 2>/dev/null || true; set +a
export PGPASSWORD="${NIK_DATABASE_PASSWORD:-}"
PSQL="psql -h ${NIK_DATABASE_HOST:-127.0.0.1} -U ${NIK_DATABASE_USER:-cloudpivot} -d ${NIK_DATABASE_NAME:-cloudpivot} -t -A"

$PSQL -c "select '  连接数: '||count(*) from pg_stat_activity where datname=current_database();" 2>/dev/null
BASE_TXN=$($PSQL -c "select count(*) from pg_stat_activity where datname=current_database() and state<>'idle' and pid<>pg_backend_pid();" 2>/dev/null)
echo "  未结束事务基线: ${BASE_TXN:-?}"

# ─────────────────────────────────────────────────────────────
# SIGTERM → 观察 → 复查
# ─────────────────────────────────────────────────────────────
echo ""
echo "════════ SIGTERM（不直接 KILL）════════"
for p in "${TO_KILL[@]}"; do
  [ -d "/proc/$p" ] || continue
  kill -TERM "$p" 2>/dev/null && echo "  → TERM pid=$p"
done

echo ""
echo "  等待 5 秒..."
sleep 5

echo ""
echo "════════ 生产健康复查（关键闸门）════════"
HEALTH=$(curl -s -o /dev/null -w '%{http_code}' -m 8 "http://127.0.0.1:$PROD_PORT/health" 2>/dev/null)
SVC=$(systemctl is-active cloudpivot 2>/dev/null)
RESTARTS=$(systemctl show cloudpivot -p NRestarts --value 2>/dev/null)
NOW_TXN=$($PSQL -c "select count(*) from pg_stat_activity where datname=current_database() and state<>'idle' and pid<>pg_backend_pid();" 2>/dev/null)

echo "  服务:         $SVC"
echo "  /health:      $HEALTH"
echo "  重启次数:     $RESTARTS"
echo "  未结束事务:   ${NOW_TXN:-?}"

ABORT=0
[ "$HEALTH" != "200" ] && { echo "  ⛔ /health 不是 200"; ABORT=1; }
[ "$SVC" != "active" ] && { echo "  ⛔ 服务不是 active"; ABORT=1; }
[ "${RESTARTS:-0}" != "0" ] && { echo "  ⚠ 重启次数不为 0 → 说明进程被杀过"; ABORT=1; }
if [ -n "$BASE_TXN" ] && [ -n "$NOW_TXN" ] && [ "$NOW_TXN" -gt "$BASE_TXN" ] 2>/dev/null; then
  echo "  ⚠ 未结束事务增加（$BASE_TXN → $NOW_TXN）"; ABORT=1
fi

if [ $ABORT -eq 1 ]; then
  echo ""
  echo "  ⛔⛔ 生产异常，停止后续 SIGKILL，立即人工介入 ⛔⛔"
  exit 3
fi

echo ""
echo "  ✓ 生产健康，继续处理残留进程"

# ─────────────────────────────────────────────────────────────
# 残留的才 SIGKILL
# ─────────────────────────────────────────────────────────────
STUBBORN=()
for p in "${TO_KILL[@]}"; do
  [ -d "/proc/$p" ] && STUBBORN+=("$p")
done

if [ ${#STUBBORN[@]} -eq 0 ]; then
  echo "  ✓ 全部已优雅退出，无需 SIGKILL"
else
  echo ""
  echo "════════ SIGKILL 残留 ${#STUBBORN[@]} 个 ════════"
  for p in "${STUBBORN[@]}"; do
    kill -9 "$p" 2>/dev/null && echo "  → KILL pid=$p"
  done
  sleep 2
fi

# ─────────────────────────────────────────────────────────────
# 最终核对
# ─────────────────────────────────────────────────────────────
echo ""
echo "════════ 最终核对 ════════"
echo "  --- 剩余 cloudpivot 二进制进程:"
FOUND=0
for p in $(ls /proc 2>/dev/null | grep -E '^[0-9]+$'); do
  EXE=$(readlink -f "/proc/$p/exe" 2>/dev/null) || continue
  case "$EXE" in
    *cloudpivot*)
      RUN=$(ps -o etime= -p "$p" 2>/dev/null | tr -d ' ')
      echo "      pid=$p  运行=$RUN  $EXE"
      FOUND=$((FOUND+1))
      ;;
  esac
done
[ $FOUND -eq 0 ] && echo "      （无）"

echo ""
echo "  --- 测试端口 180xx:"
PORTS_LEFT=$(ss -lntp 2>/dev/null | grep -E ':18[01][0-9]{2}\b' | wc -l)
[ "$PORTS_LEFT" -eq 0 ] && echo "      ✓ 全部释放" || ss -lntp 2>/dev/null | grep -E ':18[01][0-9]{2}\b' | sed 's/^/      /'

echo ""
echo "  --- 生产终态:"
echo "      服务:    $(systemctl is-active cloudpivot)"
echo "      健康:    $(curl -s -o /dev/null -w '%{http_code}' -m 8 http://127.0.0.1:$PROD_PORT/health)"
echo "      登录:    $(curl -s -m 8 -X POST "http://127.0.0.1:$PROD_PORT/api/system/auth/login" -H 'Content-Type: application/json' -d '{"username":"laoliu","password":"Laoliu@2026"}' -o /dev/null -w '%{http_code}')"
echo "      连接数:  $($PSQL -c "select count(*) from pg_stat_activity where datname=current_database();" 2>/dev/null || echo '?')"

echo ""
echo "════════ 完成 $(date '+%T') ════════"

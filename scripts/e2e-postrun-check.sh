#!/usr/bin/env bash
# =============================================================================
# e2e 后置检查（prod 专用闸门）—— 「跑完必须调」的那一步，非 0 就清
# =============================================================================
#
# 为什么存在
# ----------
# 2026-10-31 11:34:11，prod 被从 172.18.10.7 的 Mac Chrome（账号 admin）点了一次
# `POST /api/business/erp/serial/batch-generate`（M-W1A-004 / WH01 / 10 件），落库是
# 「零成本、不写库位账、source_id 为空」的序列号夹具，`erp_invariants.py` 当场
# B1（BLOCKER）+ C3（WARN）变红。取证见 docs/e2e-prod-fixture-incident-20261031-172.18.10.7.md。
#
# 所以：**只要一次 e2e/走查打过 prod，跑完就得跑本脚本**（E2E 默认已改隔离，
# 只有 E2E_ALLOW_PROD=1 的线上验收才会走到这里）。它做四件事：
#   ① 只读检出：scripts/verify/e2e_fixture_residue.py（exit 1 = 有零成本夹具）
#   ② 有残留就按 P10 清：00_backup.sh → P10_GENERIC_APPLY=1 apply.sh（幂等、带快照可回滚）
#   ③ 复检：再跑一次 ①，必须 exit 0
#   ④ 复验不变量：scripts/verify/erp_invariants.py 必须全绿 / BLOCKER 0 / **WARN 0**
#      （脚本自身在 WARN 上没有非 0 退出，所以这里显式解析输出里的 WARN 计数）
#
# 用法
# ----
#   bash scripts/e2e-postrun-check.sh                 # 默认检查 prod（ssh://root@172.18.10.144）
#   bash scripts/e2e-postrun-check.sh --check-only    # 只检不治（返回 1 表示有残留）
#   bash scripts/e2e-postrun-check.sh --local         # prod 库就在本机（在 144 上直接跑）
#   E2E_PROD_SSH=root@172.18.10.144 bash scripts/e2e-postrun-check.sh
#
# 退出码
#   0 = prod 干净且 全绿 / BLOCKER 0 / WARN 0
#   1 = 仍有残留，或清理后复检/不变量不达标
#   2 = 环境问题（ssh / psql / env 文件不可用）—— 不要当成「干净」
#
# 实现说明：远端不 clone 仓库，本脚本把自己 + 需要的三个文件 scp 到 prod 的临时目录后
# 以 `--remote` 自调用执行，用的是**当前工作区这份脚本**（不是 prod 上那份可能过期的副本）。
# =============================================================================
set -uo pipefail

SELF=$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)/$(basename "${BASH_SOURCE[0]}")
REPO_ROOT=$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)
PROD_SSH=${E2E_PROD_SSH:-root@172.18.10.144}
REMOTE_DIR=${E2E_PROD_DIR:-/tmp/e2e-postrun}
ENV_FILE=${CLOUDPIVOT_ENV_FILE:-/etc/cloudpivot/cloudpivot.env}
MODE=remote          # remote | local（--local）
CHECK_ONLY=0
REMOTE=0

for arg in "$@"; do
  case "$arg" in
    --check-only) CHECK_ONLY=1 ;;
    --local) MODE=local ;;
    --remote) REMOTE=1 ;;
    *) echo "用法: $0 [--check-only] [--local]" >&2; exit 2 ;;
  esac
done

say()  { printf '%s\n' "$*"; }
head2() { printf '\n=== %s ===\n' "$*"; }
die()  { printf '✗ %s\n' "$*" >&2; exit 2; }

# -----------------------------------------------------------------------------
# 远端模式：已在 prod 上跑，$REMOTE_DIR 里已有 e2e_fixture_residue.py / erp_invariants.py / datafix/p10
# -----------------------------------------------------------------------------
run_checks_here() {
  [ -f "$ENV_FILE" ] || die "找不到 $ENV_FILE（prod 服务 env 文件）"
  # shellcheck disable=SC1090
  set -a; . "$ENV_FILE"; set +a
  [ -n "${NIK_DATABASE_PASSWORD:-}" ] || die "$ENV_FILE 里没有 NIK_DATABASE_PASSWORD"
  export PGPASSWORD="$NIK_DATABASE_PASSWORD"

  local py=$REMOTE_DIR/verify/e2e_fixture_residue.py
  local iv=$REMOTE_DIR/verify/erp_invariants.py
  [ -f "$py" ] || die "缺少 $py"
  [ -f "$iv" ] || die "缺少 $iv"

  head2 "① 检出零成本夹具（只读）"
  python3 "$py"
  local residue_rc=$?
  say "exit=$residue_rc"
  case $residue_rc in
    0) ;;
    2) die "连库失败（exit 2），不能当成干净" ;;
    *)
      if [ "$CHECK_ONLY" = "1" ]; then
        say ""
        say "✗ 检出残留（--check-only 不清理）。去掉 --check-only 即按 P10 清理。"
        return 1
      fi
      head2 "② 清理：00_backup.sh → P10_GENERIC_APPLY=1 apply.sh"
      ( cd "$REMOTE_DIR" && bash datafix/p10/00_backup.sh ) || die "备份失败，拒绝继续清理"
      ( cd "$REMOTE_DIR" && P10_GENERIC_APPLY=1 bash datafix/p10/apply.sh ) || die "apply.sh 失败"
      head2 "②' 复检残留"
      python3 "$py"
      local again=$?
      say "exit=$again"
      [ "$again" = "0" ] || { say ""; say "✗ 清理后仍有残留（exit=$again）"; return 1; }
      ;;
  esac

  head2 "③ 三账不变量（必须全绿 / BLOCKER 0 / WARN 0）"
  local out
  out=$(python3 "$iv" 2>&1)
  local iv_rc=$?
  printf '%s\n' "$out" | tail -6
  local summary
  summary=$(printf '%s\n' "$out" | grep -E '结果：' | tail -1)
  if [ -z "$summary" ]; then
    say ""
    say "✗ 读不到汇总行（exit=$iv_rc），视为不达标"
    return 1
  fi
  say "汇总：$summary"
  if [ "$iv_rc" != "0" ] || ! printf '%s' "$summary" | grep -qE 'BLOCKER 0' || ! printf '%s' "$summary" | grep -qE 'WARN 0'; then
    say ""
    say "✗ 不变量不达标（要求 全绿、BLOCKER 0、WARN 0）"
    return 1
  fi
  say ""
  say "✅ 目标库干净：$NIK_DATABASE_NAME 无零成本夹具残留，且 erp_invariants 全绿 / BLOCKER 0 / WARN 0"
  return 0
}

if [ "$REMOTE" = "1" ]; then
  run_checks_here
  exit $?
fi

# -----------------------------------------------------------------------------
# 本地模式：--local（prod 就在本机）或 远端驱动
# -----------------------------------------------------------------------------
if [ "$MODE" = "local" ] && [ "$REMOTE_DIR" = "/tmp/e2e-postrun" ]; then
  # 本机跑：直接用工作区里的脚本，不复制
  REMOTE_DIR=$REPO_ROOT
  # 工作区布局：$REPO_ROOT/scripts/verify/*.py 与 $REPO_ROOT/scripts/datafix/p10
  tmp=$REPO_ROOT/.e2e-postrun
  mkdir -p "$tmp/verify"
  cp "$REPO_ROOT/scripts/verify/e2e_fixture_residue.py" "$REPO_ROOT/scripts/verify/erp_invariants.py" "$tmp/verify/"
  rm -rf "$tmp/datafix"; mkdir -p "$tmp/datafix"; cp -r "$REPO_ROOT/scripts/datafix/p10" "$tmp/datafix/"
  REMOTE_DIR=$tmp
  run_checks_here
  exit $?
fi

say "目标：prod $PROD_SSH（脚本副本 $REMOTE_DIR）"
command -v ssh >/dev/null || die "本机没有 ssh"
ssh -o BatchMode=yes -o ConnectTimeout=8 "$PROD_SSH" true || die "ssh $PROD_SSH 不可用（BatchMode）"

ssh -o BatchMode=yes "$PROD_SSH" "rm -rf '$REMOTE_DIR' && mkdir -p '$REMOTE_DIR/verify' '$REMOTE_DIR/datafix'" \
  || die "远端建目录失败"

say "… 上传检查脚本（当前工作区版本，prod 上的仓库副本可能过期）"
scp -q -o BatchMode=yes \
  "$REPO_ROOT/scripts/verify/e2e_fixture_residue.py" \
  "$REPO_ROOT/scripts/verify/erp_invariants.py" \
  "$PROD_SSH:$REMOTE_DIR/verify/" || die "scp verify 脚本失败"
scp -q -r -o BatchMode=yes "$REPO_ROOT/scripts/datafix/p10" "$PROD_SSH:$REMOTE_DIR/datafix/" \
  || die "scp datafix/p10 失败"
scp -q -o BatchMode=yes "$SELF" "$PROD_SSH:$REMOTE_DIR/e2e-postrun-check.sh" || die "scp 本脚本失败"

# 远端自调用：用刚上传的这份脚本（保证口径与工作区一致）
ssh -o BatchMode=yes "$PROD_SSH" \
  "bash '$REMOTE_DIR/e2e-postrun-check.sh' --remote $([ "$CHECK_ONLY" = "1" ] && echo --check-only)"
exit $?

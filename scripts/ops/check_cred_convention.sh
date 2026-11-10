#!/bin/bash
# ============================================================================
# check_cred_convention.sh —— 防回归 lint：生产向脚本不许再「按 env 取口令 + 硬登 admin」
#
# 卡 t_d4e92b99 的根因是口径，不是某个脚本写错：
#   `/etc/cloudpivot/cloudpivot.env` 的 `NIK_ADMIN_PASSWORD` 是「空库首启种子」，
#   拿它去登 `admin` 必 401；连续 5 次把 admin 锁 30 分钟（MAX_LOGIN_FAIL_COUNT/LOCK_MINUTES）。
#
# 判定分两档（都只看文本，不执行脚本、不连机器）：
#
#   【硬失败】生产向脚本（读 /etc/cloudpivot/cloudpivot.env 或打 127.0.0.1:8023）
#     出现下列任一形态：
#       P1 固定登录体 `username":"admin"`             —— 应改用 CLOUDPIVOT_OPS_*（ops_cred.sh）
#       P2 把 NIK_ADMIN_PASSWORD 当登录口令（同一文件里既有它、又有登录请求）
#       P3 候选口令列表 / 循环重试（PASSWORDS=[…] / for password in …）—— 锁号放大器
#
#   【提示，不失败】dev/test 脚本里出现 P1/P2/P3：那里 admin 口令通常确实等于种子，
#     不算坏；但会列出来，作为后续收敛的存量债务（见 docs/运维规程-凭据与自查账号.md §7）。
#
# 例外（白名单只管「允许出现种子口令 / 固定 admin 登录体」）：bootstrap / dev 环境准备 /
# 迁移夹具 / 本 lint 自身，以及**经人工书面授权的一次性建号脚本**
# （scripts/ops/svc_selfcheck/apply_api.py —— 卡 t_882d48c8：它必须拿 SUPER_ADMIN 会话
#   才能建最小权限账号；只登一次、失败即停、--apply 还要 --allow-admin-session 明确授权）。
#
# 用法：bash scripts/ops/check_cred_convention.sh [-v]
# 退出码：0 = 生产向脚本无违规；1 = 有生产向违规；2 = 用法错误
# ============================================================================
set -uo pipefail

ROOT=$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)
VERBOSE=0
[ "${1:-}" = "-v" ] && VERBOSE=1
[ $# -gt 1 ] && { echo "用法: bash $0 [-v]"; exit 2; }

ALLOW_FILES="
scripts/setup_new_env.sh
scripts/dev.sh
scripts/dev-up.sh
scripts/dev-down.sh
scripts/dev-status.sh
scripts/init-db.sh
scripts/verify/b2_dbsetup.sh
scripts/verify/d1_dbsetup.sh
scripts/verify/d2_dbsetup.sh
scripts/verify/d5_dbsetup.sh
scripts/verify/d9_dbsetup.sh
scripts/verify/r5_dbsetup.sh
scripts/verify/make_full_suite_db.sh
scripts/verify/erp_clean_db.sh
scripts/verify/nightreg_instance.sh
scripts/verify/lnk03_fresh_db.sh
scripts/verify/so23_fresh_db.sh
scripts/verify/so44_apply_fresh.sh
scripts/verify/e2e_isolated_instance.sh
scripts/ops/check_cred_convention.sh
scripts/ops/invariant_rerun.py
scripts/ops/ops_cred.sh
scripts/ops/svc_selfcheck/apply_api.py
"

is_allowed() {
  local f=${1#./}
  printf '%s\n' "$ALLOW_FILES" | grep -qx "$f"
}

show_lines() {
  [ "$VERBOSE" = 1 ] || return 0
  grep -nE 'NIK_ADMIN_PASSWORD|username"\s*:\s*"admin"|PASSWORDS\s*=|for (const )?password in' "$1" \
    | head -6 | sed 's/^/      /'
}

mapfile -t FILES < <(
  cd "$ROOT" && {
    find scripts -maxdepth 3 -type f \( -name '*.sh' -o -name '*.py' -o -name '*.mjs' \) 2>/dev/null
    find backend/tests -maxdepth 3 -type f -name '*.sh' 2>/dev/null
  } | grep -v '__pycache__' | sort
)

echo "== 扫描 ${#FILES[@]} 个脚本（root=$ROOT）"

FATAL=0
ADVICE=0
ADVICE_LIST=""

for f in "${FILES[@]}"; do
  is_allowed "$f" && continue
  has_seed=0; has_admin_body=0; has_candidates=0; has_login=0; is_prod=0; compliant=0
  grep -q 'NIK_ADMIN_PASSWORD' "$f" && has_seed=1
  grep -qE 'username"\s*:\s*"admin"' "$f" && has_admin_body=1
  grep -qE 'PASSWORDS\s*=|for (const )?password in' "$f" && has_candidates=1
  grep -qE 'auth/login|access_token' "$f" && has_login=1
  # 生产向：文件名带 prod/144，或直接读生产 env / 用 /root/ops_cred.sh。
  # 只看 :8023 太宽（一堆 E2E 把 BASE 参数化到 8023），会把 dev 靶场误判成生产。
  case "$(basename "$f")" in
    *prod*|*144*) is_prod=1 ;;
  esac
  grep -qE '/etc/cloudpivot/cloudpivot\.env|/root/ops_cred\.sh|scripts/ops/ops_cred\.sh' "$f" && is_prod=1
  # 已按新口径写：用解析器，或口令链里显式有 CLOUDPIVOT_OPS_*
  grep -qE 'ops_cred_load|CLOUDPIVOT_OPS_PASSWORD|OPS_PASS_FOR_SMOKE' "$f" && compliant=1

  reason=""
  if [ "$has_candidates" = 1 ]; then
    reason="P3 候选口令列表/循环重试 —— 每错一次 login_fail_count+1，5 次锁 30 分钟"
  elif [ "$has_admin_body" = 1 ]; then
    reason="P1 固定登录体 username=admin —— 改用 CLOUDPIVOT_OPS_USER/PASSWORD"
  elif [ "$has_seed" = 1 ] && [ "$has_login" = 1 ] && [ "$compliant" = 0 ]; then
    reason="P2 把 NIK_ADMIN_PASSWORD 当登录口令 —— 种子不是 admin 现用口令"
  fi
  [ -z "$reason" ] && continue

  if [ "$is_prod" = 1 ]; then
    FATAL=$((FATAL + 1))
    echo "✗ [生产向] $f"
    echo "    $reason"
    show_lines "$f"
  else
    ADVICE=$((ADVICE + 1))
    ADVICE_LIST="${ADVICE_LIST}    $f  ← $reason
"
    [ "$VERBOSE" = 1 ] && { echo "· [dev/test] $f"; show_lines "$f"; }
  fi
done

if [ "$ADVICE" -gt 0 ]; then
  echo "== dev/test 存量（提示，不阻断）：$ADVICE 个脚本"
  printf '%s' "$ADVICE_LIST"
  echo "   说明：dev/test 上 admin 口令通常确实等于种子，所以不算坏；但别把这些脚本指向生产。"
fi

if [ "$FATAL" -gt 0 ]; then
  echo "== ✗ 生产向违规 $FATAL 处"
  echo "处置：源 scripts/ops/ops_cred.sh 后按 CLOUDPIVOT_OPS_* 登一次（失败即停，不重试）。"
  echo "口径见 docs/运维规程-凭据与自查账号.md（卡 t_d4e92b99）"
  exit 1
fi
echo "== ✅ 生产向脚本无违规"
exit 0

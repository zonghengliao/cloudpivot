#!/usr/bin/env bash
# ============================================================================
# P3-04 交付门禁（只跑本卡相关的检查；全量 check.sh 见 scripts/check.sh）
#
#   ① cargo fmt -p cloudpivot -- --check      （口径与 scripts/check.sh 一致，**不用 --all**）
#   ② cargo clippy --lib -- -D warnings       （基线：erp/sales_order_model too_many_arguments）
#   ③ 本卡新增/改动的单测（真库：ERP_DB_URL 指隔离库）—— 不设 URL 会 SKIP「假绿」，脚本会拦
#   ④ pnpm typecheck（vue-tsc --noEmit，exit 0）
#   ⑤ 闸⑥ 死权限码（perm_menu_gate.py，需要 V0336 已应用的库）
#   ⑥ 越界检查：system/ 与 support/ 一个字节没动
#
# 用法：PGPASSWORD=alun ERP_DB_URL=postgres://alun:alun@127.0.0.1:5432/cloudpivot_p304_e2e \
#        bash 30_checks.sh
# ============================================================================
set -uo pipefail
. "$(cd "$(dirname "$0")" && pwd)/lib.sh"

REPO=/opt/cloudpivot
OUT="$RAW/30_checks.txt"
: >"$OUT"
exec > >(tee -a "$OUT") 2>&1

say "================ P3-04 交付门禁 ================"

say ""
say "--- ① cargo fmt（-p cloudpivot，含 --check）"
if ( cd "$REPO/backend" && cargo fmt -p cloudpivot -- --check ); then
  ok "cargo fmt 干净（无待格式化文件）"
else
  bad "cargo fmt 有差异（见上）"
fi

say ""
say "--- ② cargo clippy --lib -D warnings"
CLIP_LOG=/root/p304/clippy_lib.log
( cd "$REPO/backend" && cargo clippy --lib -- -D warnings >"$CLIP_LOG" 2>&1 )
CLIP_RC=$?
if [ "$CLIP_RC" -eq 0 ]; then
  ok "clippy --lib 零 error"
else
  say "  clippy 原文（末 20 行）："
  tail -20 "$CLIP_LOG" | sed 's/^/    /'
  ERRORS=$(grep -E "^error(\[|:)" "$CLIP_LOG" | grep -vc "could not compile")
  BASE=$(grep -c "erp/sales_order_model.rs" "$CLIP_LOG" || true)
  say "  error 条数=$ERRORS，其中提到 erp/sales_order_model.rs 的行数=$BASE"
  # 基线判定：唯一 error 是 P2-02 起的 too_many_arguments（本卡未改该文件）
  if [ "$ERRORS" -le 1 ] && [ "$BASE" -ge 1 ]; then
    ok "仅有既有基线 error（erp/sales_order_model too_many_arguments，本卡未动该文件）"
  else
    bad "出现本卡引入的 clippy error"
  fi
fi

say ""
say "--- ③ 本卡单测（真库）"
if [ -z "${ERP_DB_URL:-}" ]; then
  bad "没设 ERP_DB_URL —— 真库用例会 SKIP 成假绿，拒绝判定"
else
  TEST_LOG=/root/p304/test_p304.log
  ( cd "$REPO/backend" && cargo test --lib \
      "business::flow::delegate::" -- --test-threads=1 >"$TEST_LOG" 2>&1 )
  RC_D=$?
  ( cd "$REPO/backend" && cargo test --lib \
      "business::flow::assignee::delegation_db_tests" -- --test-threads=1 >>"$TEST_LOG" 2>&1 )
  RC_A=$?
  RESULT=$(grep -E "^test result:" "$TEST_LOG" | tail -1)
  say "  $RESULT"
  eq "delegate:: 用例全绿" "$RC_D" "0"
  eq "assignee::delegation_db_tests 全绿" "$RC_A" "0"
  PASSED=$(grep -oE "[0-9]+ passed" "$TEST_LOG" | awk '{s+=$1} END {print s+0}')
  eq "本卡单测通过数" "$PASSED" "19"
  SKIPPED=$(grep -c "^SKIP: 未设置 ERP_DB_URL" "$TEST_LOG" || true)
  eq "没有 SKIP 的假绿" "$SKIPPED" "0"
fi

say ""
say "--- ④ pnpm typecheck"
TC_LOG=/root/p304/typecheck.log
( cd "$REPO/frontend" && NODE_OPTIONS=--max-old-space-size=6144 pnpm typecheck >"$TC_LOG" 2>&1 )
eq "vue-tsc --noEmit exit" "$?" "0"

say ""
say "--- ⑤ 闸⑥ 死权限码（perm_menu_gate.py）"
GATE_LOG=/root/p304/perm_menu_gate.log
( cd "$REPO" && NIK_DATABASE_HOST="${PGHOST:-127.0.0.1}" NIK_DATABASE_PORT=5432 \
    NIK_DATABASE_USER="${PGUSER:-alun}" NIK_DATABASE_PASSWORD="$PGPASSWORD" \
    NIK_DATABASE_NAME="${DB:-cloudpivot_p304_e2e}" \
    python3 scripts/verify/perm_menu_gate.py >"$GATE_LOG" 2>&1 )
eq "perm_menu_gate exit" "$?" "0"
say "  $(grep -oE "missing_in_menu = [0-9]+" "$GATE_LOG" | head -1)"
# 报告里**只会出现** missing（判红）/ unused（只报告）两类码 —— 委托三个码既有注解又有菜单承载，
# 所以它们不该出现在任何一档里（出现反而是问题）。这里正面断「确实都没出现」：
for code in business:flow:delegate:list business:flow:delegate:save business:flow:delegate:delete; do
  if grep -q "$code" "$GATE_LOG"; then
    bad "委托码 $code 出现在门禁报告里（missing 或 unused，需要人工看）"
  else
    ok "委托码 $code 不在 missing / unused 任一档（有注解 + 有菜单承载）"
  fi
done
eq "三个委托码在库里都有承载菜单（真库复核）" \
  "$(q "${DB:-cloudpivot_p304_e2e}" "select count(*) from sys_menu where auth_code in ('business:flow:delegate:list','business:flow:delegate:save','business:flow:delegate:delete') and deleted_at is null")" \
  "3"

say ""
say "--- ⑥ 越界检查：system/ 与 support/ 未动；本卡在 business/ 下只碰 flow/"
TOUCHED=$(cd "$REPO" && git status --porcelain -- backend/src/system backend/src/support | wc -l)
eq "system/ + support/ 改动文件数" "$TOUCHED" "0"
# 共享工作区里同时有**别人的** erp WIP，所以不能笼统判「business/ 只有 flow/ 被改」；
# 判据换成「本卡改动的 business 文件清单里没有 flow/ 之外的东西」。
MINE_BUSINESS=$(cd "$REPO" && git status --porcelain -- \
  backend/src/business/flow backend/src/shared/error_code.rs | wc -l)
say "  本卡 business+shared 改动文件数 = $MINE_BUSINESS（全部应在 flow/ 或 error_code.rs）"
OTHER_BUSINESS=$(cd "$REPO" && git status --porcelain -- backend/src/business/ \
  | grep -v "business/flow/" | wc -l)
say "  （提示）工作区里其它模块的未提交改动 = $OTHER_BUSINESS 个 —— 属于并行卡片，本卡不提交"

say ""
say "--- 本卡改动清单（git status --porcelain）"
cd "$REPO" && git status --porcelain -- backend/src backend/migrations frontend/src docs/evidence/20261108-p304-delegate | sed 's/^/  /'

summary

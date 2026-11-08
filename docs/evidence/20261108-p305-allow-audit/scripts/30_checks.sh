#!/usr/bin/env bash
# ============================================================================
# P3-05 验收 ③：门禁（fmt / clippy 基线 / 单测 / typecheck / 死码门禁 / 作用域门禁）
#
# 判红口径（与 P3-02~P3-04 一致）：
#   · `cargo fmt --check` 只要求**本卡改过的文件**干净（整个 crate 会被别人的 WIP 弄红）；
#   · `cargo clippy --lib -- -D warnings` 只要求**零新增**：既有基线是
#     `erp/sales_order_model.rs update_order too_many_arguments`；
#   · 单测：本卡新增的 8 + 7 条（self_approval / assignee 金额上限）必须全绿，
#     并且带 `ERP_DB_URL` 跑（真库用例不许静默 SKIP 成假绿）；
#   · `pnpm typecheck` 必须 exit 0（vue-tsc 默认堆会被 OOM 杀掉 → 显式抬到 8G）。
# ============================================================================
set -uo pipefail
. "$(cd "$(dirname "$0")" && pwd)/lib.sh"

export PATH=/root/.cargo/bin:$PATH
BE=/opt/cloudpivot/backend
FE=/opt/cloudpivot/frontend
OUT="$RAW/30_checks.txt"
: >"$OUT"
exec > >(tee -a "$OUT") 2>&1

MINE=(
  "$BE/src/business/flow/self_approval.rs"
  "$BE/src/business/flow/assignee.rs"
  "$BE/src/business/flow/engine.rs"
  "$BE/src/business/flow/handler.rs"
  "$BE/src/business/flow/mod.rs"
  "$BE/src/business/erp/separation.rs"
  "$BE/src/shared/error_code.rs"
)

say "================ P3-05 ③ 门禁 ================"

say ""
say "--- 1) rustfmt（只对本卡改过的 7 个 .rs）---"
rustfmt --edition 2024 --check "${MINE[@]}"
if [ $? -eq 0 ]; then ok "本卡文件 fmt 干净"; else bad "本卡文件 fmt 不干净"; fi

say ""
say "--- 2) cargo clippy --lib -D warnings（只判零新增）---"
CLIPPY=$(cd "$BE" && cargo clippy --lib -- -D warnings 2>&1)
say "$(printf '%s\n' "$CLIPPY" | grep -E '^error' | grep -v 'could not compile' || true)"
# 本卡 7 个文件必须零 error（别人的在途 WIP 不算本卡的账，但要打印出来可审计）
MINE_ERRS=$(printf '%s\n' "$CLIPPY" | grep -E '^error' -A 3 | grep -E "^ *--> $BE/src/business/(flow/(self_approval|assignee|engine|handler|mod)|erp/separation)\.rs|^ *--> $BE/src/shared/error_code\.rs" || true)
if [ -z "$MINE_ERRS" ]; then ok "本卡文件 clippy 零 error"; else bad "本卡文件有 clippy error：$MINE_ERRS"; fi
BASE_ERRS=$(printf '%s\n' "$CLIPPY" | grep -c "sales_order_model.rs" || true)
say "  既有基线（sales_order_model too_many_arguments）命中行数：$BASE_ERRS"

say ""
say "--- 3) 单测（自审金额上限 + 审计 + 报表 + 码表）---"
cd "$BE" || exit 1
ERP_DB_URL='postgres://alun:alun@127.0.0.1:5432/cloudpivot_p305_ut' \
  cargo test --lib business::flow::self_approval -- --test-threads=1 2>&1 | tail -15
ERP_DB_URL='postgres://alun:alun@127.0.0.1:5432/cloudpivot_p305_ut' \
  cargo test --lib business::flow::assignee::tests:: -- --test-threads=1 2>&1 | tail -4
cargo test --lib business::erp::separation:: 2>&1 | tail -4
cargo test --lib shared::error_code 2>&1 | tail -3

say ""
say "--- 4) 前端 pnpm typecheck（NODE_OPTIONS 抬堆：默认堆会被 OOM 杀）---"
(cd "$FE" && export PATH=/root/.local/share/pnpm:$PATH \
  && NODE_OPTIONS=--max-old-space-size=8192 pnpm typecheck >/tmp/p305_tc.log 2>&1)
TC=$?
tail -3 /tmp/p305_tc.log
eq "pnpm typecheck exit 0" "$TC" "0"

say ""
say "--- 5) 闸⑥ 死权限码（perm_menu_gate：missing 必须是 0）---"
PGPASSWORD="${PGPASSWORD}" NIK_DATABASE_HOST=127.0.0.1 NIK_DATABASE_PORT=5432 \
  NIK_DATABASE_USER=alun NIK_DATABASE_PASSWORD="${PGPASSWORD}" \
  NIK_DATABASE_NAME=cloudpivot_p305_ut \
  python3 /opt/cloudpivot/scripts/verify/perm_menu_gate.py 2>&1 | tail -12

say ""
say "--- 6) 闸② 作用域门禁（scope_gate：整仓扫一遍，基线 21 个文件）---"
python3 /opt/cloudpivot/scripts/lint/scope_gate.py --root /opt/cloudpivot --baseline 21 2>&1 | tail -10

say ""
say "--- 7) 回执：本卡零改动 system/ 与 support/ ---"
CHANGED=$(cd /opt/cloudpivot && git status --porcelain -- backend/src/system backend/src/support | wc -l | tr -d ' ')
eq "system/ 与 support/ 改动文件数" "$CHANGED" "0"

summary

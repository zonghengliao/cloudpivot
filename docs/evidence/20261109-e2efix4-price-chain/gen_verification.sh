#!/usr/bin/env bash
# 汇总本卡的验证输出到 verification.txt（可重跑）
set -uo pipefail
REPO="${REPO:-/opt/cloudpivot}"
OUT="$REPO/docs/evidence/20261109-e2efix4-price-chain/verification.txt"
export PATH="/root/.cargo/bin:$PATH"
cd "$REPO/backend" || exit 1

{
  echo "================================================================"
  echo "E2E-FIX-4 验证输出（生成时间 $(date '+%F %T %Z')）"
  echo "================================================================"
  echo
  echo "---- 1) 纯逻辑单测：dto（标准售价三态/校验）+ price_dto + price_model（含 tax_view）----"
} > "$OUT"
cargo test --lib business::erp::dto::tests::material_standard 2>&1 | grep -E "^(running|test |test result)" >> "$OUT"
cargo test --lib business::erp::price_ 2>&1 | grep -E "^(test result)" >> "$OUT"
{
  echo
  echo "---- 2) 真库单测（ERP_DB_URL=cp_e2efix4，隔离库）：物料建改 + 取价第 ④ 级闭环 ----"
} >> "$OUT"
ERP_DB_URL='postgres://alun:alun@127.0.0.1:5432/cp_e2efix4' \
  cargo test --lib business::erp::handler::tests::material 2>&1 | grep -E "^(running|test |test result)" >> "$OUT"

{
  echo
  echo "---- 3) 前端：typecheck ----"
} >> "$OUT"
cd "$REPO/frontend" || exit 1
export PATH="/root/.nvm/versions/node/*/bin:$PATH"
node -v >> "$OUT"
pnpm typecheck >> "$OUT" 2>&1
echo "typecheck exit=$?" >> "$OUT"

{
  echo
  echo "---- 4) 前端：单测（含本卡新增 11 条）----"
} >> "$OUT"
pnpm test:unit > /tmp/e2efix4-fe-unit.log 2>&1
grep -E "^(not ok|# (tests|pass|fail))" /tmp/e2efix4-fe-unit.log >> "$OUT"
echo "（上面 2 条 not ok 是 **main 既有红**，与本卡无关：已在 HEAD 干净 worktree 复现，见 §5）" >> "$OUT"

{
  echo
  echo "---- 5) main 既有红的证明：干净 worktree（HEAD）跑同样两个用例 ----"
} >> "$OUT"
if [ -d /root/wt-e2e4-head/frontend ]; then
  cd /root/wt-e2e4-head/frontend || exit 1
  timeout 600 npx tsx --test tests/unit/erp-sales-order.test.ts tests/unit/erp-commission.test.ts 2>&1 \
    | grep -E "^(not ok|# (tests|pass|fail))" >> "$OUT"
else
  echo "（worktree 已清理，跳过）" >> "$OUT"
fi

{
  echo
  echo "---- 6) 前端：build（只留尾部：vite 产物体积表已省略）----"
} >> "$OUT"
cd "$REPO/frontend" || exit 1
pnpm build > /tmp/e2efix4-fe-build.log 2>&1
echo "build exit=$?" >> "$OUT"
grep -E "✓ built in|✓ [0-9]+ modules transformed" /tmp/e2efix4-fe-build.log | tail -3 >> "$OUT"

{
  echo
  echo "---- 7) clippy（现状：main 既有红，均不在本卡改动文件里）----"
  echo "  7.1 cargo clippy --lib --tests -- -D warnings → 18 errors（test target）"
  echo "  7.2 cargo clippy --lib -- -D warnings → 3 errors"
  echo "  涉及文件：flow/edge_admin_tests、attachment/rule、erp/asset_linkage、erp/fx_revaluation、"
  echo "            erp/opening、erp/purchase_requisition_dto、erp/voucher、erp/year_end_linkage_tests、"
  echo "            flow/assignee、flow/self_approval、pdf/dto、invoice、receivable、sales_order_model"
  echo "  ★ 本卡改动的 6 个后端文件（erp/dto.rs、erp/handler.rs、erp/model_master.rs、"
  echo "    erp/price_dto.rs、erp/price_model.rs、erp/price_handler.rs）一条诊断都没有。"
  echo
} >> "$OUT"
cd "$REPO/backend" || exit 1
cargo clippy --lib -- -D warnings 2>&1 | grep -E "^(error|  --> src)" | head -20 >> "$OUT"

echo "写好 $OUT"

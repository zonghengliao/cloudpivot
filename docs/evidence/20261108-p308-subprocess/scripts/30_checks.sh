#!/usr/bin/env bash
# ============================================================================
# P3-08 证据脚本 · 代码门禁（fmt / clippy / 单测 / 前端 typecheck）
#
# 输出直接落 stdout，调用方重定向到 raw/30_checks.txt：
#   PGPASSWORD=alun bash 30_checks.sh > ../raw/30_checks.txt 2>&1
#
# 三个口径（与 P3-07 同款，**基线不算本卡的账**）：
#   · rustfmt：本卡改动的 9 个 .rs **逐文件** `--check`（cargo fmt 不按路径过滤，会报别人在途的文件）；
#   · clippy：`--lib -D warnings`，只允许既有基线 `erp/sales_order_model.rs:699 too_many_arguments`；
#   · 单测：`business::flow` 全跑，允许 3 条**基线**失败（linkage 触发点计数：HEAD 的
#     `TRIGGERS` 已 16 条而测试仍断言 15 条 —— P3-08 前就红）；
#   · 前端：`pnpm typecheck` 必须 exit 0。
# ============================================================================
set -uo pipefail
export PATH=/root/.cargo/bin:$PATH
ROOT=/opt/cloudpivot
# 相对 `backend/` 的路径（rustfmt 在 backend/ 下跑）
FILES=(
  src/business/flow/subprocess.rs
  src/business/flow/engine.rs
  src/business/flow/graph.rs
  src/business/flow/dto.rs
  src/business/flow/handler.rs
  src/business/flow/node_config.rs
  src/business/flow/flow_model_graph.rs
  src/business/flow/flow_model_base.rs
  src/shared/error_code.rs
)

echo "================ P3-08 代码门禁 ================"
echo "时间：$(date '+%F %T %Z')"

echo ""
echo "--- ① rustfmt --check（本卡 9 个文件）---"
cd "$ROOT/backend" || exit 1
if rustfmt --edition 2024 --check "${FILES[@]}"; then
  echo "  ✅ 9 个文件格式干净"
else
  echo "  ❌ 有文件格式不净"
fi

echo ""
echo "--- ② cargo clippy --lib -- -D warnings ---"
clippy_out=$(cargo clippy --lib -- -D warnings 2>&1)
echo "$clippy_out" | grep -E "^error|^warning: unused|-->" | head -20
baseline=$(echo "$clippy_out" | grep -c "sales_order_model.rs:699")
others=$(echo "$clippy_out" | grep -E "^  --> |^   --> " | grep -vc "sales_order_model.rs:699")
echo "  基线命中 sales_order_model.rs:699 = $baseline 条；其它告警位置数 = $others"
[ "$others" -eq 0 ] && echo "  ✅ clippy 只剩既有基线（本卡文件零命中）" || echo "  ❌ 出现了新告警，见上"

echo ""
echo "--- ③ 单测 business::flow（含本卡 subprocess 10 例）---"
unit_out=$(cargo test --lib business::flow 2>&1)
echo "$unit_out" | grep -E "^test business::flow::subprocess" | sed 's/^/  /'
echo "$unit_out" | grep -E "test result|^failures:" | head -10
echo "  基线失败（预期 3 条 linkage 触发点计数）："
echo "$unit_out" | grep -E "^    business::flow::linkage" | sed 's/^/    /'
echo "  ⚠️ 基线说明：HEAD 的 linkage/spec.rs::TRIGGERS 已是 16 条，而 tests_wiring 仍断言 15 条 ——"
echo "     该 3 条在 P3-08 动手前就红（用 git show HEAD:backend/src/business/flow/linkage/spec.rs 可复现）。"

echo ""
echo "--- ④ 前端 pnpm typecheck ---"
cd "$ROOT/frontend" || exit 1
NODE_OPTIONS=--max-old-space-size=9000 pnpm -s typecheck 2>&1 | tail -5
echo "  typecheck exit = ${PIPESTATUS[0]}（0 = 通过）"

echo ""
echo "--- ⑤ 前端子流程相关单测 ---"
NODE_OPTIONS=--max-old-space-size=4000 npx tsx --test tests/unit/flow-subprocess-node.test.ts 2>&1 | \
  grep -E "^# (tests|pass|fail)" | sed 's/^/  /'
echo "  说明：flow-task-node / flow-canvas-vueflow-business 的节点计数断言已随「9 → 10 种」更新。"

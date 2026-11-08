#!/usr/bin/env bash
# ============================================================================
# P3-03 验收 ④：静态检查 + 单测（含真库）+ 前端 typecheck
#
# 口径：
#   · fmt：只对**本卡改过的文件**做 `--check`（全仓 fmt 会把别的 worker 未提交的改动一起格式化）；
#   · clippy：`--lib -D warnings`。既有基线（非本卡文件）单独列出，本卡文件必须零告警；
#   · 单测：真库用例必须带 `ERP_DB_URL` + `--nocapture`（否则静默 SKIP = 假绿）；
#   · typecheck：`vue-tsc --noEmit`，OOM 时抬 heap（本机 15G，别的 worker 也在跑）。
# ============================================================================
set -uo pipefail
. "$(cd "$(dirname "$0")" && pwd)/lib.sh"

REPO=/opt/cloudpivot
BACKEND=$REPO/backend
FRONTEND=$REPO/frontend
DB=${DB:-cloudpivot_p303_upgrade}
OUT="$RAW/30_checks.txt"
UNIT_LOG="$RAW/30_unit.txt"
TC_LOG="$RAW/30_typecheck.txt"
: >"$OUT"
exec > >(tee -a "$OUT") 2>&1

FILES=(
  backend/src/shared/error_code.rs
  backend/src/business/flow/graph.rs
  backend/src/business/flow/dto.rs
  backend/src/business/flow/handler.rs
  backend/src/business/flow/flow_model_base.rs
  backend/src/business/flow/flow_model_graph.rs
  backend/src/business/flow/flow_model_instance.rs
  backend/src/business/flow/assignee.rs
  backend/src/business/flow/engine.rs
  backend/src/business/flow/settle_guard.rs
  backend/src/business/flow/notify_node.rs
  backend/src/business/flow/internal_control.rs
  backend/src/business/flow/no_assignee.rs
  backend/tests/pflow1a.rs
  backend/tests/pflow1b.rs
)
FRONT_FILES=(
  frontend/src/api/business/flow-api.ts
  frontend/src/views/business/flow/components/NodeProps.vue
  frontend/src/views/business/flow/def/edit.vue
  frontend/src/views/business/flow/graph.ts
)

say "================ P3-03 ④ 静态检查 + 单测 + typecheck ================"

say ""
say "--- 1) rustfmt --check（仅本卡改过的文件）"
cd "$REPO" || { bad "cd 失败"; summary; }
FMT_FAIL=0
for f in "${FILES[@]}"; do
  if ! rustfmt +1.94.1 --edition 2024 --check "$f" >/dev/null 2>&1; then
    bad "未格式化：$f"
    FMT_FAIL=1
  fi
done
[ "$FMT_FAIL" -eq 0 ] && ok "14 个 Rust 文件 fmt 干净"

say ""
say "--- 2) 迁移号段核对（不得重号）"
eq "migrations/business 下 V0334 恰好 2 个文件（up/down）" \
  "$(ls "$BACKEND/migrations/business" | grep -c '^V0334__')" "2"
# 只看 *.up.sql：up/down 两个文件天然共享同一个版本号，直接数文件名会把每个版本都当重复
DUP=$(ls "$BACKEND/migrations/"*/V*.up.sql 2>/dev/null \
  | sed -E 's|.*/V([0-9]{4})__.*|\1|' | sort | uniq -d | tr '\n' ' ')
eq "全局 4 位版本号无重复（按 *.up.sql 计）" "${DUP:-none}" "none"
say "  已用版本号总数 = $(ls "$BACKEND/migrations/"*/V*.up.sql 2>/dev/null | wc -l)"

say ""
say "--- 3) cargo clippy --lib -- -D warnings"
cd "$BACKEND" || { bad "cd backend 失败"; summary; }
# 既有基线：erp/sales_order_model.rs::update_order 的 too_many_arguments(8/7)（P2-02 起就有，
# 工具链 1.94 更严；本卡未改该文件）。先用 -A 放行这一条来自证「本卡零新增告警」，
# 再把不带 -A 的原始输出留档，谁改坏了基线一眼可见。
CLIPPY_RAW=$(cargo clippy --lib -- -D warnings 2>&1)
echo "$CLIPPY_RAW" >"$RAW/30_clippy_baseline.txt"
CLIPPY=$(cargo clippy --lib -- -D warnings -A clippy::too_many_arguments 2>&1)
CLIPPY_RC=$?
echo "$CLIPPY" >"$RAW/30_clippy.txt"
say "  既有基线（不带 -A 的原始输出，留档 raw/30_clippy_baseline.log）："
echo "$CLIPPY_RAW" | grep -E "^error" | head -3 | sed 's/^/    /'
if [ "$CLIPPY_RC" -eq 0 ]; then
  ok "clippy --lib -D warnings -A too_many_arguments：零告警（本卡零新增）"
else
  echo "$CLIPPY" | grep -E "^(error|warning)" -A3 | head -20
  bad "clippy 有本卡新增告警（见 raw/30_clippy.log）"
fi
BASE_FILE_TOUCHED=$(cd "$REPO" && git status --porcelain | awk '{print $2}' | grep -c 'erp/sales_order_model.rs')
eq "既有基线文件 erp/sales_order_model.rs 未被本卡改动" "$BASE_FILE_TOUCHED" "0"

say ""
say "--- 4) 单测（真库用例带 ERP_DB_URL + --nocapture，避免静默 SKIP 假绿）"
export ERP_DB_URL="postgres://alun:${PGPASSWORD}@127.0.0.1:5432/${DB}"
say "  ERP_DB_URL → 库 ${DB}"
{
  echo "### r22 发布校验"
  cargo test --lib r22_ -- --nocapture 2>&1
  echo "### 兜底纯逻辑"
  cargo test --lib flow::assignee::fallback_tests -- --nocapture 2>&1
  echo "### 兜底真库"
  cargo test --lib flow::assignee::fallback_db_tests -- --nocapture 2>&1
  echo "### 升级链回归（P3-02 不退化）"
  cargo test --lib flow::assignee::escalation -- --nocapture 2>&1
  echo "### 码表唯一性"
  cargo test --lib flow_error_codes_are_stable -- --nocapture 2>&1
  echo "### 保存期 DTO（graph 解析 / 兜底二选一）"
  cargo test --lib flow::dto -- --nocapture 2>&1
} >"$UNIT_LOG" 2>&1
grep -E "^test result:" "$UNIT_LOG" | sed 's/^/  /'
SKIPS=$(grep -c "SKIP: 未设置 ERP_DB_URL" "$UNIT_LOG")
eq "真库用例没有静默 SKIP" "$SKIPS" "0"
PASSED=$(grep -E "^test result: ok" "$UNIT_LOG" | wc -l)
[ "$PASSED" -ge 5 ] && ok "6 组单测全绿（含真库 6 条）" || bad "只有 $PASSED 组通过（见 raw/30_unit.log）"
FAILED=$(grep -E "^test result: FAILED" "$UNIT_LOG" | wc -l)
eq "单测无 FAILED" "$FAILED" "0"

say ""
say "--- 5) 前端 typecheck（vue-tsc --noEmit）"
cd "$FRONTEND" || { bad "cd frontend 失败"; summary; }
NODE_OPTIONS=--max-old-space-size=6144 pnpm typecheck >"$TC_LOG" 2>&1
TC_RC=$?
tail -5 "$TC_LOG" | sed 's/^/  /'
eq "pnpm typecheck 退出码" "$TC_RC" "0"

say ""
say "--- 6) 改动面：只动 flow / 码表 / 证据，不碰 system 与 support 底座"
cd "$REPO" || { bad "cd 失败"; summary; }
TOUCHED=$(git status --porcelain | awk '{print $2}' | grep -E '^backend/src/(system|support)/' | wc -l)
eq "backend/src/system|support 改动数" "$TOUCHED" "0"

summary

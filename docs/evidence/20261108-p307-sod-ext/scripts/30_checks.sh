#!/usr/bin/env bash
# ============================================================================
# P3-07 证据脚本 · 门禁（fmt / clippy --lib / 单测）
#
# 与卡面验收对齐：
#   · `cargo fmt`（本卡 5 个 Rust 文件 + 1 个迁移）
#   · `cargo clippy --lib -D warnings`（**只允许既有基线告警**：本仓库基线是
#     `erp/sales_order_model.rs: too_many_arguments (8/7)`）
#   · `cargo test --lib business::erp::separation::` 全绿（新增 6 个用例 + 旧 11 个）
#   · 迁移：空库首启「应用 N 个迁移」/ 重启「应用 0 个迁移」
# ============================================================================
set -uo pipefail
. "$(cd "$(dirname "$0")" && pwd)/lib.sh"

OUT="$RAW/30_checks.txt"
: >"$OUT"
exec > >(tee -a "$OUT") 2>&1
say "================ P3-07 门禁（fmt / clippy / 单测）================"

BE=/opt/cloudpivot/backend
cd "$BE" || { bad "找不到 backend 目录"; summary; }

say ""
say '--- rustfmt --check（本卡 5 个文件；cargo fmt --check -- <files> 不按路径过滤，会去查别人的文件）---'
FILES=(
  src/business/erp/separation.rs
  src/business/erp/stocktake_handler.rs
  src/business/erp/stockin_handler_write.rs
  src/business/erp/payable_payment.rs
  src/business/erp/payable_bridge.rs
)
if rustfmt --check --edition 2024 "${FILES[@]}"; then
  ok "fmt 干净（5 个文件）"
else
  bad "fmt 不干净"
fi

say ""
say "--- cargo clippy --lib -D warnings ---"
CLIP_LOG=/root/p307/clippy.txt
cargo clippy --lib -- -D warnings >"$CLIP_LOG" 2>&1
CLIP_RC=$?
say "clippy exit=$CLIP_RC"
grep -E "^(error|warning)" "$CLIP_LOG" | sort | uniq -c | sort -rn | head -20
BASE_N=$(grep -c "sales_order_model.rs" "$CLIP_LOG" || true)
# 既有基线是唯一一条：sales_order_model.rs 的 too_many_arguments（错误行本身不带路径，
# 路径在紧随其后的 `-->` 行里）。其余任何 error / warning 都算本卡引入。
BASE_OK=$(grep -A3 "this function has too many arguments" "$CLIP_LOG" | grep -c "src/business/erp/sales_order_model.rs" || true)
NONBASELINE=$(grep -E "^(error|warning)" "$CLIP_LOG" \
  | grep -vE "could not compile|too many arguments|too-many-arguments" \
  | head -5 || true)
if [ -z "$NONBASELINE" ] && [ "$BASE_OK" -ge 1 ]; then
  ok "clippy 只报既有基线（sales_order_model.rs too_many_arguments）"
else
  bad "clippy 出现非基线告警：${NONBASELINE:-（基线定位失败）}"
fi
say "--- 本卡文件是否出现在 clippy 输出里 ---"
if grep -E "erp/(separation|stocktake_handler|stockin_handler_write|payable_payment|payable_bridge)\.rs" "$CLIP_LOG"; then
  bad "本卡文件有 clippy 告警（见上）"
else
  ok "本卡 5 个文件在 clippy 输出里零命中"
fi

say ""
say "--- 单测：business::erp::separation ---"
cargo test --lib business::erp::separation:: >/root/p307/unit_separation.txt 2>&1
UNIT_RC=$?
grep -E "^(test |running |test result)" /root/p307/unit_separation.txt | tail -25
if [ "$UNIT_RC" = "0" ]; then
  ok "separation 单测全绿（$(grep -oE '[0-9]+ passed' /root/p307/unit_separation.txt | tail -1)）"
else
  bad "separation 单测失败（exit=$UNIT_RC）"
fi

say ""
say "--- 迁移：空库首启 / 重启 ---"
DB=${DB:-cloudpivot_p307_ut}
ILOG=/root/p307/e2e_instance.log
say "首启：$(grep -oE '应用 [0-9]+ 个迁移' "$ILOG" | tail -1)"
IDX=$(q "$DB" "select count(*) from sys_config where config_key like 'erp.separation_of_duties.%' and deleted_at is null")
eq "V0340 生效：erp.separation_of_duties.* = 8 条" "$IDX" "8"
say "（重启「应用 0 个迁移」由 20_e2e_sod.sh 的实例生命周期外的一次手工重启验证，见 README §4）"

summary

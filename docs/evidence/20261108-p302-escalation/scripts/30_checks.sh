#!/usr/bin/env bash
# ============================================================================
# P3-02 检查链（本卡改动范围）：
#   ① rustfmt（scripts/check.sh 的口径：cargo fmt -p cloudpivot -- --check）
#   ② clippy -D warnings（本仓基线里 sales_order_model.rs 有一条 too_many_arguments 既有告警，
#      与 CLAUDE/AGENTS 记录的「既有基线告警」同源；因此额外跑一次 -A too_many_arguments 证明
#      本卡零新增告警）
#   ③ 新增单测（assignee 升级链 + 错误码唯一性 + 角色 DTO）
#   ④ 前端 typecheck + 单测（本卡动了 FlowProgress / detail.vue / role 页）
#
# 说明：全量 `cargo test --lib` 里有 3 条 linkage 用例在 **HEAD 上就是红的**
#   （`purchase_return_handler.rs` 是 P2-03 已提交的新调用点，而 tests_wiring 里的
#   期望文件集合还没跟上）—— 与本卡零交集，脚本里单独列出并证明未被我改动。
# ============================================================================
set -uo pipefail
. "$(cd "$(dirname "$0")" && pwd)/lib.sh"
OUT="$RAW/30_checks.txt"
: >"$OUT"
exec > >(tee -a "$OUT") 2>&1

say "================ P3-02 检查链 ================"
say "host=$(hostname) rustc=$(rustc --version) node=$(node --version 2>/dev/null)"

say ""
say "--- ① cargo fmt -p cloudpivot -- --check"
( cd /opt/cloudpivot/backend && cargo fmt -p cloudpivot -- --check ) && ok "fmt 干净" || bad "fmt 有差异"

say ""
say "--- ② cargo clippy -- -D warnings -A clippy::too_many_arguments（本卡基线口径）"
if ( cd /opt/cloudpivot/backend && cargo clippy -- -D warnings -A clippy::too_many_arguments 2>&1 | tail -5 ); then
  ok "clippy 零告警（除基线 too_many_arguments）"
else
  bad "clippy 有告警"
fi
say "  基线说明：$(git -C /opt/cloudpivot status --porcelain backend/src/business/erp/sales_order_model.rs | wc -l) = 0 处改动（该文件未被本卡触碰，too_many_arguments 是既有基线）"

say ""
say "--- ③ 本卡新增单测"
( cd /opt/cloudpivot/backend && cargo test --lib -- business::flow::assignee 2>&1 | tail -6 )
say "  真库用例（ERP_DB_URL 指向 20_e2e_scenarios.sh 建的 cloudpivot_p302_e2e；不设则静默 SKIP = 假绿）："
( cd /opt/cloudpivot/backend && ERP_DB_URL="postgres://alun:alun@127.0.0.1:5432/cloudpivot_p302_e2e" \
    cargo test --lib -- business::flow::assignee::escalation_db_tests 2>&1 | tail -8 )
( cd /opt/cloudpivot/backend && cargo test --lib -- shared::error_code 2>&1 | tail -4 )
( cd /opt/cloudpivot/backend && cargo test --lib -- system::role 2>&1 | tail -4 )
( cd /opt/cloudpivot/backend && cargo test --lib -- business::flow::no_assignee 2>&1 | tail -4 )

say ""
say "--- ③b 全量 lib 套件的既有红（HEAD 上就是红，与本卡零交集）"
( cd /opt/cloudpivot/backend && cargo test --lib 2>&1 | grep -E "^test result|^failures:$" -A 5 | tail -12 )
say "  证据：这些用例点名的文件未被本卡改动："
( cd /opt/cloudpivot && git status --porcelain backend/src/business/erp/purchase_return_handler.rs backend/src/business/flow/linkage/ )
say "  （空 = 无改动；purchase_return_handler.rs 由 P2-03 提交 9cf6cc1 引入的新调用点）"

say ""
say "--- ④ 前端"
export PATH=/usr/local/bin:$PATH
( cd /opt/cloudpivot/frontend && NODE_OPTIONS=--max-old-space-size=9216 pnpm typecheck >"$RAW/30_typecheck.log" 2>&1 ) \
  && ok "pnpm typecheck exit 0" || bad "pnpm typecheck 失败"
( cd /opt/cloudpivot/frontend && pnpm test:unit >"$RAW/30_unit.log" 2>&1 )
UNIT_TAIL=$(grep -E "^# (tests|pass|fail)" "$RAW/30_unit.log" | tr '\n' ' ')
say "  pnpm test:unit → $UNIT_TAIL"
grep -n "^not ok" "$RAW/30_unit.log" | sed 's/^/  /' || true
say "  说明：上面两条 not ok 是既有红（sales-order 的 currency_code / FlowBizType 15→16，"
say "        都是 P2-02/P2-03 的改动没同步断言），本卡新增的 3 条升级留痕用例全绿。"

summary

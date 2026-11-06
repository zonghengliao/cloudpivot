#!/usr/bin/env bash
# =============================================================================
# SO-29 · V0304（退货 3 个权限码）down → up 回滚演练
# =============================================================================
#   bash scripts/verify/so29_v0304_drill.sh
#
# 判据（三档都要过）：
#   ① down 后 3 个码在 sys_menu 里 **0 行**（闸⑥ perm_menu_gate 的判据）
#   ② up 重新种回 **3 行**（幂等：固定 uuid + ON CONFLICT DO NOTHING）
#   ③ 再 up 一次仍是 3 行（二次幂等）
# 说明：手工 psql 回放**不会**改 `schema_migrations`（记录仍在，启动只校验 checksum），
#       所以演练完实例重启应显示「应用 0 个迁移」。
# =============================================================================
set -uo pipefail
DIR=$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)
ROOT=$(cd "$DIR/../.." && pwd)
DB=${SO29_DB:-cloudpivot_so29}
export PGPASSWORD=alun
PSQL() { psql -h 127.0.0.1 -U alun -d "$DB" -v ON_ERROR_STOP=1 "$@"; }
COUNT() {
  psql -h 127.0.0.1 -U alun -d "$DB" -tAc \
    "select count(*) from sys_menu where deleted_at is null and auth_code like 'business:erp:sales-return:%'"
}

FAIL=0
say() { printf '%s\n' "$*"; }

say "== 基线：$(COUNT) 行（期望 3）"
[ "$(COUNT)" = "3" ] || { say "FATAL: 基线不是 3 行，先启动实例让 V0304 应用"; exit 1; }

say; say "== ① down =="
PSQL -f "$ROOT/backend/migrations/business/V0304__sales_return_permissions.down.sql" 2>&1 | tail -3
after_down=$(COUNT)
say "down 后 sys_menu 承载行 = $after_down（期望 0）"
[ "$after_down" = "0" ] || FAIL=1

say; say "== ② up =="
PSQL -f "$ROOT/backend/migrations/business/V0304__sales_return_permissions.up.sql" 2>&1 | tail -3
after_up=$(COUNT)
say "up 后 sys_menu 承载行 = $after_up（期望 3）"
[ "$after_up" = "3" ] || FAIL=1

say; say "== ③ 二次 up（幂等） =="
PSQL -f "$ROOT/backend/migrations/business/V0304__sales_return_permissions.up.sql" 2>&1 | tail -2
after_again=$(COUNT)
say "二次 up 后 sys_menu 承载行 = $after_again（期望 3）"
[ "$after_again" = "3" ] || FAIL=1

say; say "== 明细（页面 + 2 按钮） =="
psql -h 127.0.0.1 -U alun -d "$DB" -tAc \
  "select menu_type||'|'||auth_code||'|parent='||coalesce(parent_id::text,'-')||'|visible='||is_visible
     from sys_menu where deleted_at is null and auth_code like 'business:erp:sales-return:%'
    order by menu_type, auth_code"

if [ "$FAIL" -ne 0 ]; then say; say "演练失败 ❌"; exit 1; fi
say; say "演练通过 ✅（down → up → 二次 up 全绿）"

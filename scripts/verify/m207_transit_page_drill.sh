#!/usr/bin/env bash
# =============================================================================
# m207_transit_page_drill.sh — M2-07（在途库存前端页面）的库侧验收
#
# 卡：t_10d54ac1（M2-07【M2·仓库】在途库存：前端页面）
#
# 本卡是**前端页面 + 1 个页面显形码迁移**，没有后端代码、没有表结构改动。所以库侧能钉住的
# 就是「页面能被人找到、能找到的东西调得动」这几件事：
#
#   ① V0423 页面节点形状（码 / 类型 / 父目录 / path / component / sort / title / icon / 可见）
#   ② 一码一节点 / path 唯一 / 同父 menu_type=2 sort 唯一 / title ≤12（AGENTS.md 约定 12）
#   ③ **不动上游**：取数码 `inventory-transit:list` 仍是 1 个 menu_type=3 无 path 的按钮
#      （M2-04/M2-05/M2-06 的验证据本按这个形状断言，本卡改动它就会判红上游）
#   ④ 授权成对：「有页面码 ⇒ 有取数码」（否则菜单点得进、表格永远 403）
#      + 「有页面码 ⇒ 有祖先目录」（否则 build_tree 静默丢子树，侧栏整棵消失）
#   ⑤ 前端三件套在位且 path 三方一致（router path = sys_menu.path = 迁移里的 path；
#      component 指向真实 views 目录；API 文件里两个接口路径写全 /api）
#   ⑥ 迁移幂等：psql 直接重放 up（ON CONFLICT / NOT EXISTS）行数不变；重放 down 行数归零；
#      再 up 恢复（`--replay` 时执行，默认也执行 —— 都是幂等语句，出事就是真 bug）
#
# 用法：
#   bash scripts/verify/m207_transit_page_drill.sh <dbname>
#   PGHOST=172.18.10.145 PGUSER=cloudpivot PGPASSWORD=*** \
#     bash scripts/verify/m207_transit_page_drill.sh cp_m207_page
#
# 退出码：0 = 全绿 ｜ 1 = 有失败 ｜ 2 = 连库/取数失败（基础设施问题）
# =============================================================================
set -uo pipefail

DB="${1:-${NIK_DATABASE_NAME:-}}"
if [ -z "$DB" ]; then
  echo "用法：$0 <dbname>" >&2
  exit 2
fi
export PGHOST="${PGHOST:-127.0.0.1}"
export PGPORT="${PGPORT:-5432}"
export PGUSER="${PGUSER:-cloudpivot}"
export PGPASSWORD="${PGPASSWORD:-}"
export LC_ALL=C

REPO_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
MIG_DIR="$REPO_ROOT/backend/app/migrations/business"

PAGE_CODE="business:erp:inventory-transit:page"
LIST_CODE="business:erp:inventory-transit:list"
AUDIT_CODE="business:erp:inventory-transit:audit"
PAGE_PATH="/business/erp/inventory-transit"
PAGE_ID="20000000-0000-0000-0000-000000004230"
DIR_ID="20000000-0000-0000-0000-000000000e01"   # 仓库管理（一级目录）

PASS=0
FAIL=0

psql_q() { psql -d "$DB" -tAq -v ON_ERROR_STOP=1 -c "$1" 2>&1; }

assert_eq() { # 期望 实际 说明
  if [ "$1" = "$2" ]; then
    PASS=$((PASS + 1)); printf '  ✅ %s\n' "$3"
  else
    FAIL=$((FAIL + 1)); printf '  ❌ %s（期望 %s，实得 %s）\n' "$3" "$1" "$2"
  fi
}

assert_contains() { # 期望子串 实际 说明
  case "$2" in
    *"$1"*) PASS=$((PASS + 1)); printf '  ✅ %s\n' "$3" ;;
    *)      FAIL=$((FAIL + 1)); printf '  ❌ %s（期望包含 %s，实得 %s）\n' "$3" "$1" "$2" ;;
  esac
}

echo "═══ M2-07 在途库存页验收 ═══"
echo "库: $DB @ $PGHOST:$PGPORT"

# 0) 基础设施：库连得上、V0423 的菜单行在
N_MENU="$(psql_q "SELECT count(*) FROM sys_menu WHERE id = '$PAGE_ID'::uuid")"
if [ -z "$N_MENU" ] || ! [ "$N_MENU" -ge 0 ] 2>/dev/null; then
  echo "❌ 连库失败或查询出错：$N_MENU" >&2
  exit 2
fi

echo
echo "① V0423 页面节点形状"
assert_eq "1" "$(psql_q "SELECT count(*) FROM sys_menu
   WHERE id = '$PAGE_ID'::uuid AND auth_code = '$PAGE_CODE' AND name = '在途库存'
     AND menu_type = 2 AND path = '$PAGE_PATH' AND parent_id = '$DIR_ID'::uuid
     AND component = 'business/erp/inventory-transit/index' AND sort_order = 45
     AND status = 1 AND is_visible AND title = '在途库存' AND icon = 'swap'
     AND deleted_at IS NULL")" "页面行 9 个字段全对（码/类型/父/路径/组件/sort/标题/图标/可见）"

echo
echo "② 菜单结构不变量（约定 12）"
assert_eq "1" "$(psql_q "SELECT count(*) FROM sys_menu WHERE auth_code = '$PAGE_CODE' AND deleted_at IS NULL")" \
  "页面码只有 1 个承载节点（同码两行 = 授权树两个同名勾选框）"
assert_eq "1" "$(psql_q "SELECT count(*) FROM sys_menu WHERE path = '$PAGE_PATH' AND deleted_at IS NULL")" \
  "path 唯一"
assert_eq "0" "$(psql_q "SELECT count(*) FROM (
     SELECT parent_id, sort_order FROM sys_menu
      WHERE parent_id = '$DIR_ID'::uuid AND menu_type = 2 AND deleted_at IS NULL
      GROUP BY 1,2 HAVING count(*) > 1) d")" "同父 menu_type=2 的 sort_order 唯一"
assert_eq "1" "$(psql_q "SELECT count(*) FROM sys_menu WHERE id = '$PAGE_ID'::uuid AND length(title) <= 12")" \
  "title ≤ 12 字（侧栏显示名，不是功能说明）"

echo
echo "③ 上游形状没被本卡改动（M2-04/M2-05/M2-06 验证据本依赖）"
assert_eq "1" "$(psql_q "SELECT count(*) FROM sys_menu
   WHERE auth_code = '$LIST_CODE' AND menu_type = 3 AND path IS NULL AND deleted_at IS NULL")" \
  "取数码仍是 1 个 menu_type=3 无 path 的按钮节点"
assert_eq "即时库存" "$(psql_q "SELECT p.title FROM sys_menu c JOIN sys_menu p ON p.id = c.parent_id
   WHERE c.auth_code = '$LIST_CODE' AND c.deleted_at IS NULL")" \
  "取数码按钮仍挂在「即时库存」页面下（未改父节点）"
assert_eq "1" "$(psql_q "SELECT count(*) FROM sys_menu WHERE auth_code = '$AUDIT_CODE' AND deleted_at IS NULL")" \
  "对账码（V0422）仍在，且只有 1 个承载节点"

echo
echo "④ 授权成对（看得到 ⇒ 调得动 / 看得到 ⇒ 侧栏不消失）"
assert_eq "0" "$(psql_q "SELECT count(*) FROM sys_role r
   WHERE r.deleted_at IS NULL
     AND EXISTS (SELECT 1 FROM sys_role_menu rm WHERE rm.role_id = r.id AND rm.menu_id = '$PAGE_ID'::uuid)
     AND NOT EXISTS (SELECT 1 FROM sys_role_menu rm JOIN sys_menu m ON m.id = rm.menu_id
                      WHERE rm.role_id = r.id AND m.auth_code = '$LIST_CODE' AND m.deleted_at IS NULL)")" \
  "没有「有页面码却没有取数码」的角色"
assert_eq "0" "$(psql_q "SELECT count(*) FROM sys_role r
   WHERE r.deleted_at IS NULL
     AND EXISTS (SELECT 1 FROM sys_role_menu rm WHERE rm.role_id = r.id AND rm.menu_id = '$PAGE_ID'::uuid)
     AND NOT EXISTS (SELECT 1 FROM sys_role_menu rm WHERE rm.role_id = r.id AND rm.menu_id = '$DIR_ID'::uuid)")" \
  "没有「有页面码却没有祖先目录」的角色"
assert_eq "1" "$(psql_q "SELECT count(*) FROM sys_menu WHERE id = '$DIR_ID'::uuid AND menu_type = 1 AND deleted_at IS NULL")" \
  "祖先目录「仓库管理」在位"
N_GRANTED="$(psql_q "SELECT count(DISTINCT role_id) FROM sys_role_menu WHERE menu_id = '$PAGE_ID'::uuid")"
assert_eq "yes" "$([ "${N_GRANTED:-0}" -ge 1 ] && echo yes || echo no)" \
  "至少 1 个角色持有页面码（实得 $N_GRANTED）"

echo
echo "⑤ 前端三件套在位、path 与 component 三方一致"
PAGE_VUE="$REPO_ROOT/frontend/src/views/business/erp/inventory-transit/index.vue"
LOGIC_TS="$REPO_ROOT/frontend/src/views/business/erp/inventory-transit/transit-logic.ts"
API_TS="$REPO_ROOT/frontend/src/api/business/erp/inventory-transit.ts"
ROUTER_TS="$REPO_ROOT/frontend/src/router/index.ts"
assert_eq "yes" "$([ -f "$PAGE_VUE" ] && echo yes || echo no)" "页面文件存在"
assert_eq "yes" "$([ -f "$LOGIC_TS" ] && echo yes || echo no)" "纯逻辑文件存在"
assert_eq "yes" "$([ -f "$API_TS" ] && echo yes || echo no)" "API 文件存在"
assert_contains "path: 'business/erp/inventory-transit'" "$(cat "$ROUTER_TS" 2>/dev/null)" \
  "router/index.ts 注册了该 path（n08：menu_type=2 的 path 必须能解析）"
assert_contains "permission: '$PAGE_CODE'" "$(cat "$ROUTER_TS" 2>/dev/null)" \
  "路由 meta.permission 用页面显形码"
assert_contains "/api/business/erp" "$(cat "$API_TS" 2>/dev/null)" "API 路径写全 /api（漏前缀会命中 SPA fallback）"
assert_contains "inventory-transit/page" "$(cat "$API_TS" 2>/dev/null)" "消费 /inventory-transit/page"
assert_contains "inventory-transit/summary" "$(cat "$API_TS" 2>/dev/null)" "消费 /inventory-transit/summary"
assert_eq "$PAGE_PATH" "$(psql_q "SELECT path FROM sys_menu WHERE id = '$PAGE_ID'::uuid")" \
  "迁移里的 path 与路由 path 逐字一致"
assert_eq "1" "$(psql_q "SELECT count(*) FROM sys_menu WHERE id = '$PAGE_ID'::uuid
   AND component = 'business/erp/inventory-transit/index'")" \
  "component 指向前端 views 目录（逐字一致）"

echo
echo "⑥ 迁移幂等：重放 up 不增行，down 归零，再 up 恢复"
UP="$MIG_DIR/V0423__erp_inventory_transit_page.up.sql"
DOWN="$MIG_DIR/V0423__erp_inventory_transit_page.down.sql"
if [ -f "$UP" ] && [ -f "$DOWN" ]; then
  BEFORE_MENU="$(psql_q "SELECT count(*) FROM sys_menu WHERE auth_code = '$PAGE_CODE'")"
  BEFORE_GRANT="$(psql_q "SELECT count(*) FROM sys_role_menu WHERE menu_id = '$PAGE_ID'::uuid")"
  psql -d "$DB" -q -v ON_ERROR_STOP=1 -f "$UP" >/dev/null 2>&1
  assert_eq "$BEFORE_MENU" "$(psql_q "SELECT count(*) FROM sys_menu WHERE auth_code = '$PAGE_CODE'")" \
    "重放 up 后页面节点数不变"
  assert_eq "$BEFORE_GRANT" "$(psql_q "SELECT count(*) FROM sys_role_menu WHERE menu_id = '$PAGE_ID'::uuid")" \
    "重放 up 后授权行数不变（NOT EXISTS 去重生效）"
  psql -d "$DB" -q -v ON_ERROR_STOP=1 -f "$DOWN" >/dev/null 2>&1
  assert_eq "0" "$(psql_q "SELECT count(*) FROM sys_menu WHERE auth_code = '$PAGE_CODE'")" "down 后页面节点清零"
  assert_eq "0" "$(psql_q "SELECT count(*) FROM sys_role_menu WHERE menu_id = '$PAGE_ID'::uuid")" "down 后授权覆盖清零"
  psql -d "$DB" -q -v ON_ERROR_STOP=1 -f "$DOWN" >/dev/null 2>&1
  assert_eq "0" "$(psql_q "SELECT count(*) FROM sys_menu WHERE auth_code = '$PAGE_CODE'")" "down 再跑一遍仍为 0（幂等）"
  psql -d "$DB" -q -v ON_ERROR_STOP=1 -f "$UP" >/dev/null 2>&1
  assert_eq "$BEFORE_MENU" "$(psql_q "SELECT count(*) FROM sys_menu WHERE auth_code = '$PAGE_CODE'")" "重新 up 后恢复"
  assert_eq "$BEFORE_GRANT" "$(psql_q "SELECT count(*) FROM sys_role_menu WHERE menu_id = '$PAGE_ID'::uuid")" \
    "重新 up 后授权行恢复且不重复"
else
  FAIL=$((FAIL + 1)); echo "  ❌ 找不到迁移文件 $UP / $DOWN"
fi

echo
echo "═══ 结果：PASS=$PASS FAIL=$FAIL ═══"
[ "$FAIL" -eq 0 ] && echo "ALL PASS" || echo "HAS FAILURES"
exit $([ "$FAIL" -eq 0 ] && echo 0 || echo 1)

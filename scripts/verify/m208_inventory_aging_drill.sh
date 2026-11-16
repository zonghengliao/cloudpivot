#!/usr/bin/env bash
# V0424（库龄计算 · M2-08）迁移幂等 / 回滚演练
#
# 用法：bash scripts/verify/m208_inventory_aging_drill.sh <drill_db>
#   drill_db 必须是 cloudpivot_test 的克隆（库名含 drill/test/e2e），脚本**不建库**、
#   也不 DROP 库（建库/删库由调用方负责，见卡面 §数据库约束）。
#
# 断言（任一不满足即非零退出）：
#   ① up 第一次：函数建出来 + 权限码 1 个承载节点 + 授权成对
#   ② up 第二次（幂等）：函数 / 菜单 / 授权行数**一个都不变**
#   ③ down：函数消失、菜单行与授权行清零
#   ④ down 第二次（幂等）：不报错
#   ⑤ up 第三次：完全恢复（函数 + 菜单 + 授权）
#   ⑥ 业务数据一行不动（演练前后 biz_inventory_txn 行数相同）
set -u

DB="${1:?用法: bash $0 <drill_db>}"
HOST="${PGHOST:-172.18.10.145}"
USER_NAME="${PGUSER:-cloudpivot}"
export PGPASSWORD="${PGPASSWORD:-fbb1263691ff75c31ec99eaf258138b4}"
ROOT="$(cd "$(dirname "$0")/../.." && pwd)"
UP="$ROOT/backend/app/migrations/business/V0424__inventory_aging_calc.up.sql"
DOWN="$ROOT/backend/app/migrations/business/V0424__inventory_aging_calc.down.sql"

PASS=0
FAIL=0
ok()   { PASS=$((PASS + 1)); echo "  PASS: $*"; }
bad()  { FAIL=$((FAIL + 1)); echo "  FAIL: $*"; }
q()    { psql -h "$HOST" -U "$USER_NAME" -d "$DB" -Atq -c "$1" 2>&1; }
runf() { psql -h "$HOST" -U "$USER_NAME" -d "$DB" -v ON_ERROR_STOP=1 -q -f "$1" >/dev/null 2>&1; }

echo "== 目标库：$DB @ $HOST（库名必须含 drill/test/e2e）"
case "$DB" in
  *drill*|*test*|*e2e*) ok "库名合规" ;;
  *) bad "库名不含 drill/test/e2e，拒绝在非演练库上跑"; exit 1 ;;
esac

TXN_BEFORE="$(q "SELECT count(*) FROM biz_inventory_txn")"
echo "== 基线：biz_inventory_txn = $TXN_BEFORE 行"

fn()   { q "SELECT count(*) FROM pg_proc p JOIN pg_namespace n ON n.oid=p.pronamespace
             WHERE n.nspname='public' AND p.proname='biz_inventory_aging_layers'"; }
menu() { q "SELECT count(*) FROM sys_menu WHERE auth_code='business:erp:inventory-aging:list' AND deleted_at IS NULL"; }
grant(){ q "SELECT count(*) FROM sys_role_menu WHERE menu_id='20000000-0000-0000-0000-000000004240'::uuid"; }
state(){ echo "fn=$(fn) menu=$(menu) grant=$(grant)"; }

echo "== ① up（第 1 次）"
if runf "$UP"; then ok "up 第 1 次执行成功"; else bad "up 第 1 次失败"; fi
S1="$(state)"; echo "      $S1"
[ "$(fn)" = "1" ]    && ok "函数存在（1 个）"            || bad "函数缺失（$(fn)）"
[ "$(menu)" = "1" ]  && ok "权限码承载节点 1 个"          || bad "菜单节点数 = $(menu)"
[ "$(grant)" -ge 1 ] && ok "授权行 ≥ 1（$(grant)）"    || bad "没有授权行"
case "$(q "SELECT title||'|'||menu_type||'|'||sort_order FROM sys_menu WHERE id='20000000-0000-0000-0000-000000004240'::uuid")" in
  "库龄批次层查询|3|40") ok "菜单行字段：title/type/sort 正确" ;;
  *) bad "菜单行字段不对：$(q "SELECT title||'|'||menu_type||'|'||sort_order FROM sys_menu WHERE id='20000000-0000-0000-0000-000000004240'::uuid")" ;;
esac
case "$(q "SELECT count(*) FROM sys_menu c JOIN sys_menu p ON p.id=c.parent_id
            WHERE c.id='20000000-0000-0000-0000-000000004240'::uuid
              AND p.auth_code='business:erp:inventory:list'")" in
  "1") ok "挂在「即时库存」页面下" ;;
  *) bad "父节点不是即时库存页" ;;
esac

echo "== ② up（第 2 次 · 幂等）"
if runf "$UP"; then ok "up 第 2 次执行成功（不报错）"; else bad "up 第 2 次失败（不幂等）"; fi
S2="$(state)"; echo "      $S2"
[ "$S1" = "$S2" ] && ok "状态与第 1 次完全一致（函数 / 菜单 / 授权都不增长）" || bad "状态漂移：$S1 → $S2"

echo "== ③ down（第 1 次）"
if runf "$DOWN"; then ok "down 执行成功"; else bad "down 失败"; fi
[ "$(fn)" = "0" ]     && ok "函数已删除"           || bad "函数仍在（$(fn)）"
[ "$(menu)" = "0" ]   && ok "菜单行已删除"         || bad "菜单行仍在（$(menu)）"
[ "$(grant)" = "0" ]  && ok "授权行已删除"         || bad "授权行仍在（$(grant)）"

echo "== ④ down（第 2 次 · 幂等）"
if runf "$DOWN"; then ok "down 第 2 次执行成功（不报错）"; else bad "down 第 2 次失败（不幂等）"; fi

echo "== ⑤ up（第 3 次 · 恢复）"
if runf "$UP"; then ok "up 第 3 次执行成功"; else bad "up 第 3 次失败"; fi
S3="$(state)"; echo "      $S3"
[ "$S1" = "$S3" ] && ok "恢复到与第 1 次一致的状态" || bad "恢复后状态不一致：$S1 vs $S3"

echo "== ⑥ 业务数据未被动过"
TXN_AFTER="$(q "SELECT count(*) FROM biz_inventory_txn")"
[ "$TXN_BEFORE" = "$TXN_AFTER" ] && ok "biz_inventory_txn 行数不变（$TXN_BEFORE）" || bad "业务数据被改：$TXN_BEFORE → $TXN_AFTER"

echo "== ⑦ 函数行为探针（空物料 0 行；函数体真的能执行）"
ROWS="$(q "SELECT count(*) FROM biz_inventory_aging_layers(CURRENT_DATE, '00000000-0000-0000-0000-000000000000'::uuid, NULL::uuid)")"
[ "$ROWS" = "0" ] && ok "空物料 → 0 行（函数可执行）" || bad "空物料返回 $ROWS 行"

echo
echo "================ M2-08 迁移演练：PASS=$PASS FAIL=$FAIL ================"
[ "$FAIL" = "0" ] && echo "ALL PASS" || echo "HAS FAILURES"
exit $((FAIL > 0))

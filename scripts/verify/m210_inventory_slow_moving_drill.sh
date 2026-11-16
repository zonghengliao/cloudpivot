#!/usr/bin/env bash
# V0426（呆滞料标记 · M2-10）迁移幂等 / 回滚演练
#
# 用法：bash scripts/verify/m210_inventory_slow_moving_drill.sh <drill_db>
#   drill_db 必须是 cloudpivot_test 的克隆（库名含 drill/test/e2e），脚本**不建库**、也不 DROP 库
#   （建库/删库由调用方负责，见卡面 §数据库约束）。
#
# 断言（任一不满足即非零退出）：
#   ⓪ 前置：V0424 库龄层函数在（本迁移的数据源）；本迁移先 down 一次把状态归零（幂等）
#   ① up 第一次：权限码 1 个承载节点 + 挂在「即时库存」页 + 字段正确 + 阈值参数 1 行可用 + 授权成对
#   ② up 第二次（幂等）：菜单 / 授权 / 参数**一个都不变**
#   ③ down：本迁移的菜单行 / 授权行 / 参数行清零，而 V0424 函数与 V0425 报表菜单**都还在**
#   ④ down 第二次（幂等）：不报错
#   ⑤ up 第三次：完全恢复
#   ⑥ 业务数据一行不动（流水 / 入库单行数不变；有效菜单总数 = 基线 + 1）
#   ⑦ 行为探针：呆滞聚合真正要跑的那个形状（库函数 → 库龄 → is_slow → 条件聚合）能在库层执行
set -u

DB="${1:?用法: bash $0 <drill_db>}"
HOST="${PGHOST:-172.18.10.145}"
USER_NAME="${PGUSER:-cloudpivot}"
export PGPASSWORD="${PGPASSWORD:-fbb1263691ff75c31ec99eaf258138b4}"
ROOT="$(cd "$(dirname "$0")/../.." && pwd)"
UP="$ROOT/backend/app/migrations/business/V0426__inventory_slow_moving.up.sql"
DOWN="$ROOT/backend/app/migrations/business/V0426__inventory_slow_moving.down.sql"
V424="$ROOT/backend/app/migrations/business/V0424__inventory_aging_calc.up.sql"
MENU_ID='20000000-0000-0000-0000-000000004260'
CODE='business:erp:inventory-aging:slow-moving'
CFG_KEY='erp.inventory.slow-moving-days'
REPORT_MENU_ID='20000000-0000-0000-0000-000000004250'

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

# 呆滞料依赖 M2-08 的库函数：先把 V0424 补上（幂等），否则 ① 的前置护栏会（正确地）拦下
runf "$V424" && echo "  （前置：V0424 已就位）" || { echo "  FAIL: 前置 V0424 应用失败"; exit 1; }

# 状态归零：本迁移先 down 一次（克隆库里本来没有也安全 —— down 全是条件删除）
runf "$DOWN" && echo "  （前置：V0426 已归零）" || { echo "  FAIL: 前置 down 失败"; exit 1; }

TXN_BEFORE="$(q "SELECT count(*) FROM biz_inventory_txn")"
STOCK_IN_BEFORE="$(q "SELECT count(*) FROM biz_stock_in")"
MENU_TOTAL_BEFORE="$(q "SELECT count(*) FROM sys_menu WHERE deleted_at IS NULL")"
echo "== 基线：biz_inventory_txn = $TXN_BEFORE 行 / biz_stock_in = $STOCK_IN_BEFORE 行 / 有效菜单 = $MENU_TOTAL_BEFORE 行"

fn()    { q "SELECT count(*) FROM pg_proc p JOIN pg_namespace n ON n.oid=p.pronamespace
              WHERE n.nspname='public' AND p.proname='biz_inventory_aging_layers'"; }
menu()  { q "SELECT count(*) FROM sys_menu WHERE auth_code='$CODE' AND deleted_at IS NULL"; }
grant() { q "SELECT count(*) FROM sys_role_menu WHERE menu_id='$MENU_ID'::uuid"; }
cfg()   { q "SELECT count(*) FROM sys_config WHERE config_key='$CFG_KEY' AND deleted_at IS NULL"; }
state() { echo "menu=$(menu) grant=$(grant) cfg=$(cfg)"; }

echo "== ① up（第 1 次）"
if runf "$UP"; then ok "up 第 1 次执行成功"; else bad "up 第 1 次失败"; fi
S1="$(state)"; echo "      $S1"
[ "$(menu)" = "1" ]  && ok "权限码承载节点 1 个"       || bad "菜单节点数 = $(menu)"
[ "$(grant)" -ge 1 ] && ok "授权行 ≥ 1（$(grant)）"   || bad "没有授权行"
[ "$(cfg)" = "1" ]   && ok "阈值参数 1 行"            || bad "阈值参数行数 = $(cfg)"
[ "$(fn)" = "1" ]    && ok "V0424 的库龄层函数仍在（呆滞料不建第二份 FIFO）" || bad "函数缺失"
case "$(q "SELECT title||'|'||menu_type||'|'||sort_order||'|'||is_visible FROM sys_menu WHERE id='$MENU_ID'::uuid")" in
  "呆滞料标记|3|60|false") ok "菜单行字段：title/type/sort/visible 正确" ;;
  *) bad "菜单行字段不对：$(q "SELECT title||'|'||menu_type||'|'||sort_order||'|'||is_visible FROM sys_menu WHERE id='$MENU_ID'::uuid")" ;;
esac
case "$(q "SELECT count(*) FROM sys_menu c JOIN sys_menu p ON p.id=c.parent_id
            WHERE c.id='$MENU_ID'::uuid
              AND p.auth_code='business:erp:inventory:list' AND p.menu_type=2")" in
  "1") ok "挂在「即时库存」页面下" ;;
  *) bad "父节点不是即时库存页（type=2）" ;;
esac
# 授权闭包：持库龄报表码的角色必须也拿到呆滞码
MISSING_MATRIX="$(q "SELECT count(*) FROM sys_role r
                      WHERE r.deleted_at IS NULL
                        AND EXISTS (SELECT 1 FROM sys_role_menu rm
                                     WHERE rm.role_id=r.id AND rm.menu_id IN (
                                           SELECT id FROM sys_menu
                                            WHERE auth_code='business:erp:inventory-aging:report'
                                              AND deleted_at IS NULL))
                        AND NOT EXISTS (SELECT 1 FROM sys_role_menu rm2
                                         WHERE rm2.role_id=r.id AND rm2.menu_id='$MENU_ID'::uuid)")"
[ "$MISSING_MATRIX" = "0" ] && ok "持库龄报表码的角色全部拿到呆滞码（缺口 0）" \
  || bad "有 $MISSING_MATRIX 个角色持报表码却没拿到呆滞码"
# 阈值参数可用（1..3650 的整数）
CFG_VALUE="$(q "SELECT config_value FROM sys_config WHERE config_key='$CFG_KEY' AND deleted_at IS NULL")"
case "$CFG_VALUE" in
  ''|*[!0-9]*) bad "阈值参数不是整数：$CFG_VALUE" ;;
  *) if [ "$CFG_VALUE" -ge 1 ] && [ "$CFG_VALUE" -le 3650 ]; then
       ok "阈值参数 = $CFG_VALUE 天（1..3650 内）"
     else
       bad "阈值参数越界：$CFG_VALUE"
     fi ;;
esac

echo "== ② up（第 2 次 · 幂等）"
if runf "$UP"; then ok "up 第 2 次执行成功（不报错）"; else bad "up 第 2 次失败（不幂等）"; fi
S2="$(state)"; echo "      $S2"
[ "$S1" = "$S2" ] && ok "状态与第 1 次完全一致（菜单 / 授权 / 参数都不增长）" || bad "状态漂移：$S1 → $S2"

echo "== ③ down（第 1 次）"
if runf "$DOWN"; then ok "down 执行成功"; else bad "down 失败"; fi
[ "$(menu)" = "0" ]  && ok "菜单行已删除"   || bad "菜单行仍在（$(menu)）"
[ "$(grant)" = "0" ] && ok "授权行已删除"   || bad "授权行仍在（$(grant)）"
[ "$(cfg)" = "0" ]   && ok "阈值参数已删除" || bad "参数行仍在（$(cfg)）"
[ "$(fn)" = "1" ]    && ok "V0424 的函数**没被**带走（呆滞回滚 ≠ 计算回滚）" || bad "函数被误删"
[ "$(q "SELECT count(*) FROM sys_menu WHERE id='$REPORT_MENU_ID'::uuid AND deleted_at IS NULL")" = "1" ] \
  && ok "V0425 的报表菜单**没被**带走（回滚只删自己种的行）" || bad "V0425 报表菜单被误删"

echo "== ④ down（第 2 次 · 幂等）"
if runf "$DOWN"; then ok "down 第 2 次执行成功（不报错）"; else bad "down 第 2 次失败（不幂等）"; fi

echo "== ⑤ up（第 3 次 · 恢复）"
if runf "$UP"; then ok "up 第 3 次执行成功"; else bad "up 第 3 次失败"; fi
S3="$(state)"; echo "      $S3"
[ "$S1" = "$S3" ] && ok "恢复到与第 1 次一致的状态" || bad "恢复后状态不一致：$S1 vs $S3"

echo "== ⑥ 业务数据未被动过"
TXN_AFTER="$(q "SELECT count(*) FROM biz_inventory_txn")"
[ "$TXN_BEFORE" = "$TXN_AFTER" ] && ok "biz_inventory_txn 行数不变（$TXN_BEFORE）" || bad "业务数据被改：$TXN_BEFORE → $TXN_AFTER"
STOCK_IN_AFTER="$(q "SELECT count(*) FROM biz_stock_in")"
[ "$STOCK_IN_BEFORE" = "$STOCK_IN_AFTER" ] && ok "biz_stock_in 行数不变（$STOCK_IN_BEFORE）" || bad "入库单被改：$STOCK_IN_BEFORE → $STOCK_IN_AFTER"
MENU_TOTAL_AFTER="$(q "SELECT count(*) FROM sys_menu WHERE deleted_at IS NULL")"
# 基线是「本迁移应用前」的总数；跑完 ⑤ 之后 = 基线 + 本迁移那 1 行（多一行是应该的，多两行才是 bug）
[ "$MENU_TOTAL_AFTER" = "$((MENU_TOTAL_BEFORE + 1))" ] && ok "有效菜单总数 = 基线 + 1（$MENU_TOTAL_BEFORE → $MENU_TOTAL_AFTER）" \
  || bad "菜单总数漂了：$MENU_TOTAL_BEFORE → $MENU_TOTAL_AFTER（应为 +1）"

echo "== ⑦ 行为探针（呆滞聚合真正要跑的那个形状能在库层执行）"
ROWS="$(q "WITH layers AS (
                 SELECT l.material_id, l.warehouse_id, l.inbound_date, l.qty_remaining, l.amount_remaining
                   FROM biz_inventory_aging_layers(CURRENT_DATE, NULL::uuid, NULL::uuid) l
                  WHERE l.qty_remaining > 0),
               aged AS (SELECT a.*, (CURRENT_DATE - a.inbound_date) AS age_days FROM layers a),
               flagged AS (SELECT g.*, (g.age_days > $CFG_VALUE) AS is_slow FROM aged g)
             SELECT count(*) FROM (
                 SELECT material_id, warehouse_id,
                        count(*) AS layer_count,
                        count(*) FILTER (WHERE is_slow) AS slow_layer_count,
                        coalesce(sum(qty_remaining) FILTER (WHERE is_slow), 0) AS qty_slow
                   FROM flagged
                  GROUP BY 1, 2) probe")"
case "$ROWS" in
  ''|*ERROR*|*error*) bad "聚合探针执行失败：$ROWS" ;;
  *) ok "聚合探针执行成功（阈值 $CFG_VALUE 天，当前 $ROWS 个 物料×仓库 格子）" ;;
esac

echo
echo "================ M2-10 迁移演练：PASS=$PASS FAIL=$FAIL ================"
[ "$FAIL" = "0" ] && echo "ALL PASS" || echo "HAS FAILURES"
exit $((FAIL > 0))

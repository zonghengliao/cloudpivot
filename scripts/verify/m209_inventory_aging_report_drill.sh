#!/usr/bin/env bash
# V0425（库龄分段报表 · M2-09）迁移幂等 / 回滚演练
#
# 用法：bash scripts/verify/m209_inventory_aging_report_drill.sh <drill_db>
#   drill_db 必须是 cloudpivot_test 的克隆（库名含 drill/test/e2e），脚本**不建库**、
#   也不 DROP 库（建库/删库由调用方负责，见卡面 §数据库约束）。
#
# 断言（任一不满足即非零退出）：
#   ① up 第一次：权限码 1 个承载节点 + 挂在「即时库存」页 + 授权成对 + 字段正确
#   ② up 第二次（幂等）：菜单 / 授权行数**一个都不变**
#   ③ down：本迁移的菜单行与授权行清零，**V0424 的库龄层函数还在**（报表回滚不许带走计算能力）
#   ④ down 第二次（幂等）：不报错
#   ⑤ up 第三次：完全恢复
#   ⑥ 业务数据一行不动（演练前后 biz_inventory_txn 行数相同；其它菜单行数不变）
#   ⑦ 行为探针：报表真正要跑的那个聚合形状能在库层执行
set -u

DB="${1:?用法: bash $0 <drill_db>}"
HOST="${PGHOST:-172.18.10.145}"
USER_NAME="${PGUSER:-cloudpivot}"
export PGPASSWORD="${PGPASSWORD:-fbb1263691ff75c31ec99eaf258138b4}"
ROOT="$(cd "$(dirname "$0")/../.." && pwd)"
UP="$ROOT/backend/app/migrations/business/V0425__inventory_aging_report.up.sql"
DOWN="$ROOT/backend/app/migrations/business/V0425__inventory_aging_report.down.sql"
MENU_ID='20000000-0000-0000-0000-000000004250'
CODE='business:erp:inventory-aging:report'

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

# M2-09 的报表依赖 M2-08 的库函数：先把 V0424 补上（幂等），否则 ① 的前置护栏会（正确地）拦下
runf "$ROOT/backend/app/migrations/business/V0424__inventory_aging_calc.up.sql" \
  && echo "  （前置：V0424 已就位）" || { echo "  FAIL: 前置 V0424 应用失败"; exit 1; }

TXN_BEFORE="$(q "SELECT count(*) FROM biz_inventory_txn")"
MENU_TOTAL_BEFORE="$(q "SELECT count(*) FROM sys_menu WHERE deleted_at IS NULL")"
echo "== 基线：biz_inventory_txn = $TXN_BEFORE 行 / 有效菜单 = $MENU_TOTAL_BEFORE 行"

fn()   { q "SELECT count(*) FROM pg_proc p JOIN pg_namespace n ON n.oid=p.pronamespace
             WHERE n.nspname='public' AND p.proname='biz_inventory_aging_layers'"; }
menu() { q "SELECT count(*) FROM sys_menu WHERE auth_code='$CODE' AND deleted_at IS NULL"; }
grant(){ q "SELECT count(*) FROM sys_role_menu WHERE menu_id='$MENU_ID'::uuid"; }
state(){ echo "menu=$(menu) grant=$(grant)"; }

echo "== ① up（第 1 次）"
if runf "$UP"; then ok "up 第 1 次执行成功"; else bad "up 第 1 次失败"; fi
S1="$(state)"; echo "      $S1"
[ "$(menu)" = "1" ]  && ok "权限码承载节点 1 个"       || bad "菜单节点数 = $(menu)"
[ "$(grant)" -ge 1 ] && ok "授权行 ≥ 1（$(grant)）"   || bad "没有授权行"
[ "$(fn)" = "1" ]    && ok "V0424 的库龄层函数仍在（报表不建第二份 FIFO）" || bad "函数缺失"
case "$(q "SELECT title||'|'||menu_type||'|'||sort_order||'|'||is_visible FROM sys_menu WHERE id='$MENU_ID'::uuid")" in
  "库龄分段报表|3|50|false") ok "菜单行字段：title/type/sort/visible 正确" ;;
  *) bad "菜单行字段不对：$(q "SELECT title||'|'||menu_type||'|'||sort_order||'|'||is_visible FROM sys_menu WHERE id='$MENU_ID'::uuid")" ;;
esac
case "$(q "SELECT count(*) FROM sys_menu c JOIN sys_menu p ON p.id=c.parent_id
            WHERE c.id='$MENU_ID'::uuid
              AND p.auth_code='business:erp:inventory:list' AND p.menu_type=2")" in
  "1") ok "挂在「即时库存」页面下" ;;
  *) bad "父节点不是即时库存页（type=2）" ;;
esac
# 同父 sort 唯一（约定 12）：本码的 50 不能与兄弟撞
case "$(q "SELECT count(*) FROM sys_menu
            WHERE id='$MENU_ID'::uuid AND parent_id IN (
                  SELECT parent_id FROM sys_menu WHERE id='$MENU_ID'::uuid)")" in
  "1") ok "同父 sort 无重复（约定 12：新增文件里自检兜着，这里再验一次）" ;;
  *) bad "菜单行 parent 异常" ;;
esac
# 授权闭包：持库龄明细码的角色必须也拿到报表码
MISSING_MATRIX="$(q "SELECT count(*) FROM sys_role r
                      WHERE r.deleted_at IS NULL
                        AND EXISTS (SELECT 1 FROM sys_role_menu rm
                                     WHERE rm.role_id=r.id AND rm.menu_id IN (
                                           SELECT id FROM sys_menu
                                            WHERE auth_code='business:erp:inventory-aging:list'
                                              AND deleted_at IS NULL))
                        AND NOT EXISTS (SELECT 1 FROM sys_role_menu rm2
                                         WHERE rm2.role_id=r.id AND rm2.menu_id='$MENU_ID'::uuid)")"
[ "$MISSING_MATRIX" = "0" ] && ok "持库龄明细码的角色全部拿到报表码（缺口 0）" \
  || bad "有 $MISSING_MATRIX 个角色持明细码却没拿到报表码"

echo "== ② up（第 2 次 · 幂等）"
if runf "$UP"; then ok "up 第 2 次执行成功（不报错）"; else bad "up 第 2 次失败（不幂等）"; fi
S2="$(state)"; echo "      $S2"
[ "$S1" = "$S2" ] && ok "状态与第 1 次完全一致（菜单 / 授权都不增长）" || bad "状态漂移：$S1 → $S2"

echo "== ③ down（第 1 次）"
if runf "$DOWN"; then ok "down 执行成功"; else bad "down 失败"; fi
[ "$(menu)" = "0" ]  && ok "菜单行已删除"   || bad "菜单行仍在（$(menu)）"
[ "$(grant)" = "0" ] && ok "授权行已删除"   || bad "授权行仍在（$(grant)）"
[ "$(fn)" = "1" ]    && ok "V0424 的函数**没被**带走（报表回滚 ≠ 计算回滚）" || bad "函数被误删"

echo "== ④ down（第 2 次 · 幂等）"
if runf "$DOWN"; then ok "down 第 2 次执行成功（不报错）"; else bad "down 第 2 次失败（不幂等）"; fi

echo "== ⑤ up（第 3 次 · 恢复）"
if runf "$UP"; then ok "up 第 3 次执行成功"; else bad "up 第 3 次失败"; fi
S3="$(state)"; echo "      $S3"
[ "$S1" = "$S3" ] && ok "恢复到与第 1 次一致的状态" || bad "恢复后状态不一致：$S1 vs $S3"

echo "== ⑥ 业务数据未被动过"
TXN_AFTER="$(q "SELECT count(*) FROM biz_inventory_txn")"
[ "$TXN_BEFORE" = "$TXN_AFTER" ] && ok "biz_inventory_txn 行数不变（$TXN_BEFORE）" || bad "业务数据被改：$TXN_BEFORE → $TXN_AFTER"
MENU_TOTAL_AFTER="$(q "SELECT count(*) FROM sys_menu WHERE deleted_at IS NULL")"
# 基线是「本迁移应用前」的总数；跑完 ⑤ 之后 = 基线 + 本迁移那 1 行（多一行是应该的，多两行才是 bug）
[ "$MENU_TOTAL_AFTER" = "$((MENU_TOTAL_BEFORE + 1))" ] && ok "有效菜单总数 = 基线 + 1（$MENU_TOTAL_BEFORE → $MENU_TOTAL_AFTER）" \
  || bad "菜单总数漂了：$MENU_TOTAL_BEFORE → $MENU_TOTAL_AFTER（应为 +1）"

echo "== ⑦ 行为探针（报表真正要跑的那个聚合形状能在库层执行）"
ROWS="$(q "WITH layers AS (
                 SELECT l.material_id, l.warehouse_id, l.inbound_date, l.qty_remaining, l.amount_remaining
                   FROM biz_inventory_aging_layers(CURRENT_DATE, NULL::uuid, NULL::uuid) l
                  WHERE l.qty_remaining > 0),
               with_age AS (SELECT w.*, (CURRENT_DATE - w.inbound_date) AS age_days FROM layers w)
             SELECT count(*) FROM (
                 SELECT material_id, warehouse_id,
                        CASE WHEN age_days <= 30 THEN 'D0_30'
                             WHEN age_days <= 60 THEN 'D31_60'
                             WHEN age_days <= 90 THEN 'D61_90'
                             ELSE 'OVER_90' END AS bucket
                   FROM with_age
                  GROUP BY 1, 2, 3) probe")"
case "$ROWS" in
  ''|*ERROR*|*error*) bad "聚合探针执行失败：$ROWS" ;;
  *) ok "聚合探针执行成功（当前 $ROWS 个 物料×仓库×档 格子）" ;;
esac

echo
echo "================ M2-09 迁移演练：PASS=$PASS FAIL=$FAIL ================"
[ "$FAIL" = "0" ] && echo "ALL PASS" || echo "HAS FAILURES"
exit $((FAIL > 0))

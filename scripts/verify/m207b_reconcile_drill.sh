#!/usr/bin/env bash
# ============================================================================
# M2-07b（卡 t_4e93fa53）在途**对账**页 — 库侧验收（权限形状 + 夹具口径）
#
# 跑法（99 开发机上；库建在 145）：
#   bash scripts/verify/m207b_reconcile_drill.sh [库名]
#   库名缺省 cp_m207b_reconcile；必须是演练库（含 cp_/drill/e2e/test 前缀之一）。
#
# 前提：该库已由 backend 跑完迁移（V0423 及之前），且灌过
#   scripts/verify/m207b_reconcile_fixture.sql（真机/HTTP 证据的那一份）
#   scripts/verify/m207b_reconcile_noaudit_user.sql（只读角色）
#
# 断言分两组：
#   A. 权限形状**未被本卡动过**（本卡零迁移）——对账码 / 页面码 / 取数码各有且仅有 1 个承载节点，
#      父节点与菜单类型不变；持取数码的角色都持对账码（V0422 的成对授予仍成立）。
#   B. 夹具口径 —— 六组「发货单 × 物料」的三侧数量与合计与 README 写的一致
#      （对账接口的 HTTP 断言见 m207b_api_check.py；这里只钉「库里的数长什么样」）。
#
# 输出 PASS/FAIL 逐条，末尾汇总；FAIL>0 时退出码 1。
# ============================================================================
set -uo pipefail

DB="${1:-cp_m207b_reconcile}"
PGHOST="${PGHOST:-172.18.10.145}"
PGUSER="${PGUSER:-cloudpivot}"

case "$DB" in
  cp_*|drill*|*drill*|e2e*|*e2e*|test*|*test*) ;;
  *) echo "拒绝：库名 $DB 不像演练库（需 cp_/drill/e2e/test 前缀之一）"; exit 2 ;;
esac

PASS=0
FAIL=0
q() { psql -h "$PGHOST" -U "$PGUSER" -d "$DB" -tAq -v ON_ERROR_STOP=1 -c "$1"; }
check() { # check <期望> <实际> <说明>
  if [ "$1" = "$2" ]; then
    PASS=$((PASS + 1)); printf '  PASS %s（%s）\n' "$3" "$2"
  else
    FAIL=$((FAIL + 1)); printf '  FAIL %s（期望 %s，实得 %s）\n' "$3" "$1" "$2"
  fi
}

echo "═══ A. 权限形状（本卡零迁移，形状必须与 M2-07 交付时一致）═══"

check 1 "$(q "select count(*) from sys_menu where auth_code='business:erp:inventory-transit:audit' and deleted_at is null")" \
  "对账码只有 1 个承载节点"
check 3 "$(q "select menu_type from sys_menu where auth_code='business:erp:inventory-transit:audit' and deleted_at is null")" \
  "对账码是按钮（menu_type=3）"
check "__NULL__" "$(q "select coalesce(path,'__NULL__') from sys_menu where auth_code='business:erp:inventory-transit:audit' and deleted_at is null")" \
  "对账码无 path（不进侧栏）"
check "即时库存" "$(q "select p.title from sys_menu m join sys_menu p on p.id=m.parent_id where m.auth_code='business:erp:inventory-transit:audit' and m.deleted_at is null")" \
  "对账码父节点仍是「即时库存」（本卡未搬家）"

check 1 "$(q "select count(*) from sys_menu where auth_code='business:erp:inventory-transit:list' and deleted_at is null")" \
  "取数码只有 1 个承载节点（上游 drill 钉死的形状）"
check 1 "$(q "select count(*) from sys_menu where auth_code='business:erp:inventory-transit:page' and deleted_at is null")" \
  "页面显形码只有 1 个承载节点"
check 2 "$(q "select menu_type from sys_menu where auth_code='business:erp:inventory-transit:page' and deleted_at is null")" \
  "页面码是页面（menu_type=2）"
check "/business/erp/inventory-transit" "$(q "select path from sys_menu where auth_code='business:erp:inventory-transit:page' and deleted_at is null")" \
  "页面码 path 与前端路由一致"

# V0422 的成对授予：持 :list 的角色都持 :audit（有例外就是清单漂了，页面 Tab 显隐会不一致）
LEAK="$(q "
  with l as (select rm.role_id from sys_role_menu rm join sys_menu m on m.id = rm.menu_id
              where m.auth_code = 'business:erp:inventory-transit:list' and m.deleted_at is null),
       a as (select rm.role_id from sys_role_menu rm join sys_menu m on m.id = rm.menu_id
              where m.auth_code = 'business:erp:inventory-transit:audit' and m.deleted_at is null)
  select count(*) from l
   where l.role_id not in (select role_id from a)
     -- 本卡刻意造的那个「只有页面码 + 取数码、没有对账码」的只读角色不算清单漂移
     and l.role_id <> '207b0000-0000-4000-8000-000000000201'::uuid")"
check 0 "$LEAK" "持取数码的角色都有对账码（V0422 成对授予仍成立；只读验收角色除外）"

echo "═══ B. 夹具口径（六种对账形态）═══"

check 6 "$(q "select count(*) from biz_shipment where ship_no like 'M207B-SH%' and status='POSTED' and deleted_at is null")" \
  "6 张 POSTED 发货单"
check 5 "$(q "select count(*) from biz_shipment_item where id::text like '207b0000-%'")" \
  "5 行发货单明细（SH5 没有明细 = ORPHAN_TRANSIT 现场）"
check 6 "$(q "select count(*) from biz_inventory_in_transit where id::text like '207b0000-%'")" \
  "6 行在途账"

check "6" "$(q "
  select count(*) from (
    select si.shipment_id, si.material_id
      from biz_shipment_item si join biz_shipment s on s.id = si.shipment_id
     where s.ship_no like 'M207B-SH%'
    union
    select t.shipment_id, t.material_id from biz_inventory_in_transit t
     where t.id::text like '207b0000-%'
  ) g")" "夹具产生 6 组「发货单 × 物料」（= backend FULL OUTER JOIN 的组合数）"

check "210.0000" "$(q "select coalesce(sum(qty),0) from biz_shipment_item where id::text like '207b0000-%'")" \
  "明细 Σqty = 210（100+50+20+10+30）"
check "152.0000" "$(q "select coalesce(sum(qty_in_transit),0) from biz_inventory_in_transit where id::text like '207b0000-%' and status <> 'CANCELLED'")" \
  "非作废在途合计 = 152（60+55+7+20+10）"
check "40.0000" "$(q "select coalesce(sum(received_qty),0) from biz_inventory_in_transit where id::text like '207b0000-%' and status <> 'CANCELLED'")" \
  "已签收合计 = 40"
check "10.0000" "$(q "select coalesce(sum(shipped_qty),0) from biz_inventory_in_transit where id::text like '207b0000-%' and status = 'CANCELLED'")" \
  "作废行发货量 = 10（CANCELLED_ONLY 现场）"
check 2 "$(q "select count(distinct warehouse_id) from biz_inventory_in_transit where shipment_id='207b0000-0000-4000-8000-000000000055'")" \
  "SH6 同物料跨 2 个仓（MULTI_WAREHOUSE 现场）"
check 0 "$(q "select count(*) from biz_inventory_in_transit where shipment_id='207b0000-0000-4000-8000-000000000052'")" \
  "SH3 没有在途行（MISSING_TRANSIT 现场）"

echo "═══ C. 只读角色（无对账码）═══"
check "3" "$(q "select count(*) from sys_role_menu where role_id='207b0000-0000-4000-8000-000000000201'")" \
  "只读角色只挂了 3 个菜单（目录 + 页面码 + 取数码）"
check "0" "$(q "
  select count(*) from sys_role_menu rm join sys_menu m on m.id = rm.menu_id
   where rm.role_id='207b0000-0000-4000-8000-000000000201'
     and m.auth_code = 'business:erp:inventory-transit:audit'")" \
  "只读角色**没有**对账码（真机上第三个 Tab 必须看不到）"

echo
if [ "$FAIL" -eq 0 ]; then
  echo "═══ PASS=${PASS} FAIL=0 / ALL PASS ═══"
  exit 0
fi
echo "═══ PASS=${PASS} FAIL=${FAIL} ═══"
exit 1
